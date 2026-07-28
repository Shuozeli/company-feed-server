-- Extend the provider-neutral navigation-title quarantine for exact labels
-- found during the live recipe campaign. These labels are section links, not
-- individual news articles. Raw crawl evidence remains available and every
-- quarantine is reversible.

CREATE TEMP TABLE additional_navigation_title_items ON COMMIT DROP AS
WITH recipe_sources AS (
    SELECT DISTINCT source_id
    FROM company_news_recipes
)
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    'generic_navigation_title'::text AS reason
FROM recipe_sources AS recipe
JOIN feed_items AS item ON item.source_id = recipe.source_id
WHERE
    NOT item.is_private
    AND lower(btrim(regexp_replace(item.title, '[[:space:]]+', ' ', 'g')))
        IN (
            'annual reports & proxies',
            'annual reports and proxies',
            'contact info',
            'webcasts & presentations',
            'webcasts and presentations'
        );

CREATE TEMP TABLE additional_navigation_title_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.navigation_title_repair_started',
        jsonb_build_object(
            'item_count', count(*),
            'source_count', count(DISTINCT source_id),
            'policy', 'recipe-listing-artifact.v7',
            'migration', '0032_quarantine_additional_navigation_titles'
        )
    FROM additional_navigation_title_items
    HAVING count(*) > 0
    RETURNING id
)
INSERT INTO additional_navigation_title_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v7',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    additional_navigation_title_items AS repair
    CROSS JOIN additional_navigation_title_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM additional_navigation_title_items AS repair
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
        'reason', repair.reason,
        'reversible', true,
        'policy', 'recipe-listing-artifact.v7',
        'repair_wave_event_id', wave.event_id,
        'migration', '0032_quarantine_additional_navigation_titles'
    )
FROM
    additional_navigation_title_items AS repair
    CROSS JOIN additional_navigation_title_wave AS wave;
