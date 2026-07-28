-- Reversibly remove HTML items whose page declared a syntactically parseable
-- but structurally malformed canonical such as `https://https/...`. The
-- crawler now records that declaration for audit and uses the independently
-- fetched final URL as canonical identity. An explicit archive/category H1 is
-- still rejected, so repairing canonical identity cannot make a taxonomy page
-- into an article.

CREATE TEMP TABLE malformed_declared_canonical_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at,
    'malformed_declared_canonical'::text AS reason
FROM feed_items AS item
WHERE
    NOT item.is_private
    AND item.source_kind IN ('html', 'browser')
    AND item.canonical_url ~* '^https?://(http|https)(/|$)';

CREATE TEMP TABLE malformed_declared_canonical_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.malformed_declared_canonical_repair_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM malformed_declared_canonical_items
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM malformed_declared_canonical_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM malformed_declared_canonical_items
                ),
            'policy', 'malformed-canonical.v1',
            'migration',
                '0065_quarantine_malformed_declared_canonical_items'
        )
    WHERE EXISTS (
        SELECT 1 FROM malformed_declared_canonical_items
    )
    RETURNING id
)
INSERT INTO malformed_declared_canonical_wave (event_id)
SELECT id FROM repair_started;

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
            'policy', 'malformed-canonical.v1',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    malformed_declared_canonical_items AS repair
    CROSS JOIN malformed_declared_canonical_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM malformed_declared_canonical_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

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
        'policy', 'malformed-canonical.v1',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0065_quarantine_malformed_declared_canonical_items'
    )
FROM
    malformed_declared_canonical_items AS repair
    CROSS JOIN malformed_declared_canonical_wave AS wave;
