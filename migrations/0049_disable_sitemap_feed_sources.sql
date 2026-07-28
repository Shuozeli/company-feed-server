-- Sitemap resources enumerate site URLs; they are not editorial RSS/Atom
-- publications even when a CMS serializes them with RSS syntax. Disable
-- previously accepted sitemap sources and reversibly quarantine their stored
-- items so navigation pages cannot appear in public news.

CREATE TEMP TABLE invalid_sitemap_sources ON COMMIT DROP AS
SELECT
    source.id AS source_id,
    source.company_id,
    source.url
FROM sources AS source
WHERE
    source.status = 'approved'
    AND source.kind IN ('rss', 'atom')
    AND lower(
        regexp_replace(source.url, '^https?://[^/?#]+', '', 'i')
    ) LIKE '%sitemap%';

CREATE TEMP TABLE invalid_sitemap_candidates ON COMMIT DROP AS
SELECT
    candidate.id AS candidate_id,
    source.source_id,
    source.company_id,
    source.url
FROM invalid_sitemap_sources AS source
JOIN source_candidates AS candidate
    ON candidate.accepted_source_id = source.source_id;

CREATE TEMP TABLE invalid_sitemap_items ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title
FROM invalid_sitemap_sources AS source
JOIN feed_items AS item ON item.source_id = source.source_id
WHERE NOT item.is_private;

CREATE TEMP TABLE invalid_sitemap_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'source.sitemap_repair_started',
        jsonb_build_object(
            'source_count', (SELECT count(*) FROM invalid_sitemap_sources),
            'candidate_count', (SELECT count(*) FROM invalid_sitemap_candidates),
            'item_count', (SELECT count(*) FROM invalid_sitemap_items),
            'policy', 'non-editorial-sitemap-source.v1',
            'migration', '0049_disable_sitemap_feed_sources'
        )
    WHERE EXISTS (SELECT 1 FROM invalid_sitemap_sources)
    RETURNING id
)
INSERT INTO invalid_sitemap_wave (event_id)
SELECT id FROM repair_started;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata = source.metadata || jsonb_build_object(
        'quality_disable',
        jsonb_build_object(
            'reason', 'non_editorial_sitemap_feed',
            'reversible', true,
            'policy', 'non-editorial-sitemap-source.v1',
            'repair_wave_event_id', wave.event_id,
            'disabled_at', CURRENT_TIMESTAMP
        )
    )
FROM
    invalid_sitemap_sources AS repair
    CROSS JOIN invalid_sitemap_wave AS wave
WHERE source.id = repair.source_id;

UPDATE source_candidates AS candidate
SET
    status = 'rejected',
    accepted_source_id = NULL
FROM invalid_sitemap_candidates AS repair
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
    'migration:0049',
    'sitemap resources are URL inventories, not editorial RSS/Atom feeds',
    jsonb_build_object(
        'policy', 'non-editorial-sitemap-source.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0049_disable_sitemap_feed_sources'
    )
FROM
    invalid_sitemap_candidates AS repair
    CROSS JOIN invalid_sitemap_wave AS wave;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'non_editorial_sitemap_feed',
            'reversible', true,
            'policy', 'non-editorial-sitemap-source.v1',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    invalid_sitemap_items AS repair
    CROSS JOIN invalid_sitemap_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: non_editorial_sitemap_feed',
    normalized_feed_item_id = NULL
FROM invalid_sitemap_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled: sitemap source disabled by quality policy'
FROM invalid_sitemap_sources AS repair
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
        'reason', 'non_editorial_sitemap_feed',
        'reversible', true,
        'policy', 'non-editorial-sitemap-source.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0049_disable_sitemap_feed_sources'
    )
FROM
    invalid_sitemap_sources AS repair
    CROSS JOIN invalid_sitemap_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source_candidate.rejected',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'candidate_id', repair.candidate_id,
        'disabled_accepted_source', true,
        'reason', 'non_editorial_sitemap_feed',
        'decision_mode', 'automatic',
        'actor', 'migration:0049',
        'policy', 'non-editorial-sitemap-source.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0049_disable_sitemap_feed_sources'
    )
FROM
    invalid_sitemap_candidates AS repair
    CROSS JOIN invalid_sitemap_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'canonical_url', repair.canonical_url,
        'title', repair.title,
        'reason', 'non_editorial_sitemap_feed',
        'reversible', true,
        'policy', 'non-editorial-sitemap-source.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0049_disable_sitemap_feed_sources'
    )
FROM
    invalid_sitemap_items AS repair
    CROSS JOIN invalid_sitemap_wave AS wave;
