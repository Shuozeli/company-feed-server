-- Official hosts can expose RSS for documentation, forum replies/topics, or
-- operational alerts. Those are useful resources but are not company news
-- publications. Disable accepted feeds with a strong non-editorial feed URL
-- or an 80% non-editorial latest-item sample, preserving legitimate
-- community-hosted blogs and press-release documents.

CREATE TEMP TABLE non_editorial_feed_sources ON COMMIT DROP AS
WITH ranked_items AS (
    SELECT
        source.id AS source_id,
        source.company_id,
        source.url AS source_url,
        item.title,
        item.canonical_url,
        row_number() OVER (
            PARTITION BY source.id
            ORDER BY
                COALESCE(item.published_at, item.fetched_at) DESC,
                item.id
        ) AS sample_rank
    FROM sources AS source
    JOIN feed_items AS item
        ON item.source_id = source.id
        AND NOT item.is_private
    WHERE
        source.status = 'approved'
        AND source.kind IN ('rss', 'atom')
),
scope_metrics AS (
    SELECT
        source_id,
        company_id,
        source_url,
        count(*) FILTER (WHERE sample_rank <= 20) AS sample_item_count,
        count(*) FILTER (
            WHERE
                sample_rank <= 20
                AND (
                    lower(title) LIKE 'forum post:%'
                    OR lower(canonical_url)
                        LIKE '%/cadence_technology_forums/%'
                    OR lower(canonical_url) ~ '/(forum|forums)/'
                    OR lower(canonical_url) LIKE '%/bc-p/%'
                    OR (
                        lower(canonical_url)
                            ~ '/(docs|documentation|reference)/'
                        AND lower(canonical_url) NOT LIKE '%pressrelease%'
                        AND lower(canonical_url) NOT LIKE '%press-release%'
                        AND lower(canonical_url) NOT LIKE '%/news/%'
                        AND lower(canonical_url) NOT LIKE '%/blog/%'
                    )
                )
        ) AS non_editorial_item_count
    FROM ranked_items
    GROUP BY source_id, company_id, source_url
)
SELECT
    source_id,
    company_id,
    source_url AS url,
    sample_item_count,
    non_editorial_item_count
FROM scope_metrics
WHERE
    lower(source_url) LIKE '%/boardmessages%'
    OR lower(source_url) LIKE '%/feed/topics%'
    OR lower(source_url) LIKE '%/trust/alerts/feed%'
    OR (
        sample_item_count >= 5
        AND non_editorial_item_count * 5 >= sample_item_count * 4
    );

CREATE TEMP TABLE non_editorial_feed_candidates ON COMMIT DROP AS
SELECT
    candidate.id AS candidate_id,
    source.source_id,
    source.company_id,
    source.url
FROM non_editorial_feed_sources AS source
JOIN source_candidates AS candidate
    ON candidate.accepted_source_id = source.source_id;

CREATE TEMP TABLE non_editorial_feed_items ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title
FROM non_editorial_feed_sources AS source
JOIN feed_items AS item ON item.source_id = source.source_id
WHERE NOT item.is_private;

CREATE TEMP TABLE non_editorial_feed_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'source.non_editorial_scope_repair_started',
        jsonb_build_object(
            'source_count', (SELECT count(*) FROM non_editorial_feed_sources),
            'candidate_count', (SELECT count(*) FROM non_editorial_feed_candidates),
            'item_count', (SELECT count(*) FROM non_editorial_feed_items),
            'policy', 'non-editorial-feed-scope.v1',
            'migration', '0054_disable_non_editorial_feed_scopes'
        )
    WHERE EXISTS (SELECT 1 FROM non_editorial_feed_sources)
    RETURNING id
)
INSERT INTO non_editorial_feed_wave (event_id)
SELECT id FROM repair_started;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata = source.metadata || jsonb_build_object(
        'quality_disable',
        jsonb_build_object(
            'reason', 'non_editorial_feed_item_scope',
            'sample_item_count', repair.sample_item_count,
            'non_editorial_item_count', repair.non_editorial_item_count,
            'reversible', true,
            'policy', 'non-editorial-feed-scope.v1',
            'repair_wave_event_id', wave.event_id,
            'disabled_at', CURRENT_TIMESTAMP
        )
    )
FROM
    non_editorial_feed_sources AS repair
    CROSS JOIN non_editorial_feed_wave AS wave
WHERE source.id = repair.source_id;

UPDATE source_candidates AS candidate
SET
    status = 'rejected',
    accepted_source_id = NULL
FROM non_editorial_feed_candidates AS repair
WHERE candidate.id = repair.candidate_id;

INSERT INTO candidate_decisions (
    candidate_id,
    source_id,
    decision,
    decision_mode,
    actor,
    reason,
    metadata
)
SELECT
    repair.candidate_id,
    repair.source_id,
    'rejected',
    'automatic',
    'migration:0054',
    'feed is dominated by documentation, forum, comment, or operational items',
    jsonb_build_object(
        'policy', 'non-editorial-feed-scope.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0054_disable_non_editorial_feed_scopes'
    )
FROM
    non_editorial_feed_candidates AS repair
    CROSS JOIN non_editorial_feed_wave AS wave;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'non_editorial_feed_item_scope',
            'reversible', true,
            'policy', 'non-editorial-feed-scope.v1',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    non_editorial_feed_items AS repair
    CROSS JOIN non_editorial_feed_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: non_editorial_feed_item_scope',
    normalized_feed_item_id = NULL
FROM non_editorial_feed_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled: source disabled by editorial-scope quality policy'
FROM non_editorial_feed_sources AS repair
WHERE
    job.source_id = repair.source_id
    AND job.job_type = 'crawl_source'
    AND job.status = 'pending';

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.quality_disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.url,
        'sample_item_count', repair.sample_item_count,
        'non_editorial_item_count', repair.non_editorial_item_count,
        'reason', 'non_editorial_feed_item_scope',
        'reversible', true,
        'policy', 'non-editorial-feed-scope.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0054_disable_non_editorial_feed_scopes'
    )
FROM
    non_editorial_feed_sources AS repair
    CROSS JOIN non_editorial_feed_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source_candidate.rejected',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'candidate_id', repair.candidate_id,
        'disabled_accepted_source', true,
        'reason', 'non_editorial_feed_item_scope',
        'decision_mode', 'automatic',
        'actor', 'migration:0054',
        'policy', 'non-editorial-feed-scope.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0054_disable_non_editorial_feed_scopes'
    )
FROM
    non_editorial_feed_candidates AS repair
    CROSS JOIN non_editorial_feed_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'canonical_url', repair.canonical_url,
        'title', repair.title,
        'reason', 'non_editorial_feed_item_scope',
        'reversible', true,
        'policy', 'non-editorial-feed-scope.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0054_disable_non_editorial_feed_scopes'
    )
FROM
    non_editorial_feed_items AS repair
    CROSS JOIN non_editorial_feed_wave AS wave;
