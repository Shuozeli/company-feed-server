-- Some CMS collection hubs expose article metadata and a plausible date even
-- though their title is only a short breadcrumb made entirely from collection
-- labels, for example "ANF Blog | Insights | News". The generic article
-- crawler now rejects this title shape. Quarantine the already-normalized
-- recipe items so API/export visibility matches the runtime contract.

CREATE TEMP TABLE collection_breadcrumb_items
ON COMMIT DROP AS
WITH recipe_sources AS (
    SELECT DISTINCT recipe.source_id
    FROM company_news_recipes AS recipe
)
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at
FROM
    feed_items AS item
    JOIN recipe_sources AS recipe ON recipe.source_id = item.source_id
    JOIN sources AS source ON source.id = item.source_id
WHERE
    NOT item.is_private
    AND source.kind = 'html'
    AND lower(
        btrim(regexp_replace(item.title, '[[:space:]]+', ' ', 'g'))
    ) ~
        '^[^|0-9]{1,60} blogs?[[:space:]]*\|[[:space:]]*(additional insights|insights|news)([[:space:]]*\|[[:space:]]*(additional insights|insights|news)){0,2}$';

CREATE TEMP TABLE collection_breadcrumb_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.collection_breadcrumb_repair_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM collection_breadcrumb_items
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM collection_breadcrumb_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM collection_breadcrumb_items
                ),
            'policy', 'recipe-listing-artifact.v31',
            'migration',
                '0087_quarantine_collection_breadcrumb_titles'
        )
    WHERE EXISTS (
        SELECT 1 FROM collection_breadcrumb_items
    )
    RETURNING id
)
INSERT INTO collection_breadcrumb_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'generic_listing_title',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v31',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    collection_breadcrumb_items AS repair
    CROSS JOIN collection_breadcrumb_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: generic_listing_title',
    normalized_feed_item_id = NULL
FROM collection_breadcrumb_items AS repair
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
        'reason', 'generic_listing_title',
        'reversible', true,
        'policy', 'recipe-listing-artifact.v31',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0087_quarantine_collection_breadcrumb_titles'
    )
FROM
    collection_breadcrumb_items AS repair
    CROSS JOIN collection_breadcrumb_wave AS wave;
