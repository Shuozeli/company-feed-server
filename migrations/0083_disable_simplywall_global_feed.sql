-- Simply Wall St's /news/rss endpoint is a global market-news stream, not a
-- company-scoped feed. It predates the shared-host company-relevance gate.

CREATE TEMP TABLE wrong_simplywall_feed_sources
ON COMMIT DROP AS
SELECT
    source.id AS source_id,
    source.company_id,
    source.url,
    'shared_global_feed_not_company_scoped'::text AS reason
FROM sources AS source
WHERE
    source.status = 'approved'
    AND source.kind IN ('rss', 'atom')
    AND public_url_identity_key(source.url)
        = public_url_identity_key('https://simplywall.st/news/rss');

CREATE TEMP TABLE wrong_simplywall_feed_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at,
    source.reason
FROM feed_items AS item
JOIN wrong_simplywall_feed_sources AS source
    ON source.source_id = item.source_id
WHERE NOT item.is_private;

CREATE TEMP TABLE wrong_simplywall_feed_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.shared_global_feed_scope_repair_started',
        jsonb_build_object(
            'item_count', (SELECT count(*) FROM wrong_simplywall_feed_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM wrong_simplywall_feed_sources
                ),
            'source_count', (SELECT count(*) FROM wrong_simplywall_feed_sources),
            'policy', 'shared-feed-scope.v2',
            'migration', '0083_disable_simplywall_global_feed'
        )
    WHERE EXISTS (SELECT 1 FROM wrong_simplywall_feed_sources)
    RETURNING id
)
INSERT INTO wrong_simplywall_feed_wave (event_id)
SELECT id FROM repair_started;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata = source.metadata || jsonb_build_object(
        'quality_disable',
        jsonb_build_object(
            'reason', repair.reason,
            'reversible', true,
            'policy', 'shared-feed-scope.v2',
            'repair_wave_event_id', wave.event_id,
            'disabled_at', CURRENT_TIMESTAMP
        )
    )
FROM
    wrong_simplywall_feed_sources AS repair
    CROSS JOIN wrong_simplywall_feed_wave AS wave
WHERE source.id = repair.source_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled because source is an unscoped shared global feed',
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL
FROM wrong_simplywall_feed_sources AS repair
WHERE
    job.source_id = repair.source_id
    AND job.status IN ('pending', 'running');

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'shared-feed-scope.v2',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    wrong_simplywall_feed_items AS repair
    CROSS JOIN wrong_simplywall_feed_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM wrong_simplywall_feed_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.shared_global_feed_disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.url,
        'reason', repair.reason,
        'reversible', true,
        'policy', 'shared-feed-scope.v2',
        'repair_wave_event_id', wave.event_id,
        'migration', '0083_disable_simplywall_global_feed'
    )
FROM
    wrong_simplywall_feed_sources AS repair
    CROSS JOIN wrong_simplywall_feed_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'canonical_url', repair.canonical_url,
        'title', repair.title,
        'published_at', repair.published_at,
        'reason', repair.reason,
        'reversible', true,
        'policy', 'shared-feed-scope.v2',
        'repair_wave_event_id', wave.event_id,
        'migration', '0083_disable_simplywall_global_feed'
    )
FROM
    wrong_simplywall_feed_items AS repair
    CROSS JOIN wrong_simplywall_feed_wave AS wave;
