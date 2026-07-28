-- Disable approved RSS/Atom aliases whose complete public item set is already
-- present in another approved feed for the same company. Distinct product,
-- engineering, regional, and topical feeds remain active; these exact targets
-- are format aliases or strict subsets verified from canonical item overlap.
-- Candidate validation now rejects future feeds only when at least three
-- sampled identities are all covered by existing approved feeds.

CREATE TEMP TABLE redundant_feed_aliases
ON COMMIT DROP AS
WITH targets (company_key, redundant_url, replacement_url) AS (
    VALUES
        (
            'american-healthcare-reit-inc-common-stock',
            'https://americanhealthcarereit.com/category/press-releases/feed/',
            'https://americanhealthcarereit.com/feed/'
        ),
        (
            'arcosa-inc-common-stock',
            'https://arcosalightweight.com/insights/feed/',
            'https://arcosalightweight.com/feed/'
        ),
        (
            'argan-inc-common-stock',
            'https://arganinc.com/category/news/feed/',
            'https://arganinc.com/feed/'
        ),
        (
            'yc-clupp',
            'https://clupp.com.mx/feed/',
            'https://clupp.com.mx/blog/feed/'
        ),
        (
            'coterra-energy-inc-common-stock',
            'https://wellsaidcoterra.com//feed/',
            'https://wellsaidcoterra.com/feed/'
        ),
        (
            'firefly-aerospace-inc-common-stock',
            'https://fireflyspace.com/news/category/blog/feed/',
            'https://fireflyspace.com/feed/'
        ),
        (
            'kadant-inc-common-stock',
            'https://kadant.com/en/blog/feed/atom/blog?format=feed',
            'https://kadant.com/en/blog/feed/rss/blog?format=feed'
        ),
        (
            'moderna-inc-common-stock',
            'https://news.modernatx.com/feed/atom',
            'https://news.modernatx.com/feed/rss2'
        ),
        (
            'protagonist-therapeutics-inc-common-stock',
            'http://www.protagonist-inc.com/feed/atom',
            'http://www.protagonist-inc.com/feed/rss2'
        ),
        (
            'target-corporation-common-stock',
            'https://tech.target.com/rss/atom.xml',
            'https://tech.target.com/rss/feed.xml'
        ),
        (
            'teledyne-technologies-incorporated-common-stock',
            'https://blog.teledynelecroy.com/feeds/posts/default',
            'https://blog.teledynelecroy.com/feeds/posts/default?alt=rss'
        ),
        (
            'west-pharmaceutical-services-inc-common-stock',
            'https://www.westpharma.com/blog/blog-entries-2',
            'https://www.westpharma.com/blog/blog-entries'
        )
)
SELECT
    source.id AS source_id,
    source.company_id,
    source.url,
    replacement.id AS replacement_source_id,
    replacement.url AS replacement_url,
    'fully_redundant_feed_alias'::text AS reason
FROM
    targets AS target
    JOIN companies AS company
        ON company.company_key = target.company_key
    JOIN sources AS source
        ON source.company_id = company.id
        AND source.status = 'approved'
        AND source.kind IN ('rss', 'atom')
        AND source.url = target.redundant_url
    JOIN sources AS replacement
        ON replacement.company_id = company.id
        AND replacement.status = 'approved'
        AND replacement.kind IN ('rss', 'atom')
        AND replacement.url = target.replacement_url;

CREATE TEMP TABLE redundant_feed_alias_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.redundant_feed_alias_repair_started',
        jsonb_build_object(
            'source_count', (SELECT count(*) FROM redundant_feed_aliases),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM redundant_feed_aliases
                ),
            'policy', 'feed-overlap.v1',
            'migration',
                '0073_disable_fully_redundant_feed_aliases'
        )
    WHERE EXISTS (SELECT 1 FROM redundant_feed_aliases)
    RETURNING id
)
INSERT INTO redundant_feed_alias_wave (event_id)
SELECT id FROM repair_started;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata = source.metadata || jsonb_build_object(
        'quality_disable',
        jsonb_build_object(
            'reason', repair.reason,
            'replacement_source_id', repair.replacement_source_id,
            'replacement_url', repair.replacement_url,
            'reversible', true,
            'policy', 'feed-overlap.v1',
            'repair_wave_event_id', wave.event_id,
            'disabled_at', CURRENT_TIMESTAMP
        )
    )
FROM
    redundant_feed_aliases AS repair
    CROSS JOIN redundant_feed_alias_wave AS wave
WHERE source.id = repair.source_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error =
        'cancelled because source is fully covered by another approved feed',
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL
FROM redundant_feed_aliases AS repair
WHERE
    job.source_id = repair.source_id
    AND job.status IN ('pending', 'running');

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.redundant_feed_disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.url,
        'reason', repair.reason,
        'replacement_source_id', repair.replacement_source_id,
        'replacement_url', repair.replacement_url,
        'reversible', true,
        'policy', 'feed-overlap.v1',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0073_disable_fully_redundant_feed_aliases'
    )
FROM
    redundant_feed_aliases AS repair
    CROSS JOIN redundant_feed_alias_wave AS wave;
