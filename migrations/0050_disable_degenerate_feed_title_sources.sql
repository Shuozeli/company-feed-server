-- Editorial RSS/Atom feeds must not expose the same title for every item in a
-- meaningful sample. A feed that repeats one title across at least five
-- distinct stored items is structurally degenerate, even when its host is
-- official and the XML is syntactically valid. Disable existing sources that
-- meet this conservative condition and reversibly quarantine their items.

CREATE TEMP TABLE degenerate_title_sources ON COMMIT DROP AS
SELECT
    source.id AS source_id,
    source.company_id,
    source.url,
    min(item.title) AS repeated_title,
    count(*) AS item_count
FROM sources AS source
JOIN feed_items AS item ON item.source_id = source.id
WHERE
    source.status = 'approved'
    AND source.kind IN ('rss', 'atom')
    AND NOT item.is_private
GROUP BY source.id
HAVING
    count(*) >= 5
    AND count(
        DISTINCT lower(btrim(regexp_replace(item.title, '[[:space:]]+', ' ', 'g')))
    ) = 1;

CREATE TEMP TABLE degenerate_title_candidates ON COMMIT DROP AS
SELECT
    candidate.id AS candidate_id,
    source.source_id,
    source.company_id,
    source.url
FROM degenerate_title_sources AS source
JOIN source_candidates AS candidate
    ON candidate.accepted_source_id = source.source_id;

CREATE TEMP TABLE degenerate_title_items ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title
FROM degenerate_title_sources AS source
JOIN feed_items AS item ON item.source_id = source.source_id
WHERE NOT item.is_private;

CREATE TEMP TABLE degenerate_title_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'source.feed_title_diversity_repair_started',
        jsonb_build_object(
            'source_count', (SELECT count(*) FROM degenerate_title_sources),
            'candidate_count', (SELECT count(*) FROM degenerate_title_candidates),
            'item_count', (SELECT count(*) FROM degenerate_title_items),
            'policy', 'feed-title-diversity.v1',
            'migration', '0050_disable_degenerate_feed_title_sources'
        )
    WHERE EXISTS (SELECT 1 FROM degenerate_title_sources)
    RETURNING id
)
INSERT INTO degenerate_title_wave (event_id)
SELECT id FROM repair_started;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata = source.metadata || jsonb_build_object(
        'quality_disable',
        jsonb_build_object(
            'reason', 'degenerate_feed_title_diversity',
            'repeated_title', repair.repeated_title,
            'sample_item_count', repair.item_count,
            'reversible', true,
            'policy', 'feed-title-diversity.v1',
            'repair_wave_event_id', wave.event_id,
            'disabled_at', CURRENT_TIMESTAMP
        )
    )
FROM
    degenerate_title_sources AS repair
    CROSS JOIN degenerate_title_wave AS wave
WHERE source.id = repair.source_id;

UPDATE source_candidates AS candidate
SET
    status = 'rejected',
    accepted_source_id = NULL
FROM degenerate_title_candidates AS repair
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
    'migration:0050',
    'feed repeats one title across at least five distinct items',
    jsonb_build_object(
        'policy', 'feed-title-diversity.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0050_disable_degenerate_feed_title_sources'
    )
FROM
    degenerate_title_candidates AS repair
    CROSS JOIN degenerate_title_wave AS wave;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'degenerate_feed_title_diversity',
            'reversible', true,
            'policy', 'feed-title-diversity.v1',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    degenerate_title_items AS repair
    CROSS JOIN degenerate_title_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: degenerate_feed_title_diversity',
    normalized_feed_item_id = NULL
FROM degenerate_title_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled: feed disabled by title-diversity quality policy'
FROM degenerate_title_sources AS repair
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
        'repeated_title', repair.repeated_title,
        'item_count', repair.item_count,
        'reason', 'degenerate_feed_title_diversity',
        'reversible', true,
        'policy', 'feed-title-diversity.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0050_disable_degenerate_feed_title_sources'
    )
FROM
    degenerate_title_sources AS repair
    CROSS JOIN degenerate_title_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source_candidate.rejected',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'candidate_id', repair.candidate_id,
        'disabled_accepted_source', true,
        'reason', 'degenerate_feed_title_diversity',
        'decision_mode', 'automatic',
        'actor', 'migration:0050',
        'policy', 'feed-title-diversity.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0050_disable_degenerate_feed_title_sources'
    )
FROM
    degenerate_title_candidates AS repair
    CROSS JOIN degenerate_title_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'canonical_url', repair.canonical_url,
        'title', repair.title,
        'reason', 'degenerate_feed_title_diversity',
        'reversible', true,
        'policy', 'feed-title-diversity.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0050_disable_degenerate_feed_title_sources'
    )
FROM
    degenerate_title_items AS repair
    CROSS JOIN degenerate_title_wave AS wave;
