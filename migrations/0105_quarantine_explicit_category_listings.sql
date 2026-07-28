-- Shallow editorial routes whose normalized body explicitly announces that
-- they are showing a set of posts are category listings, not articles.

CREATE TEMP TABLE explicit_category_listing_items
ON COMMIT DROP AS
SELECT DISTINCT ON (item.id)
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    recipe.id AS recipe_id,
    item.url,
    item.canonical_url,
    item.title,
    item.published_at
FROM
    feed_items AS item
    JOIN company_news_recipes AS recipe
        ON recipe.source_id = item.source_id
WHERE
    NOT item.is_private
    AND recipe.status IN ('active', 'stale')
    AND lower(btrim(item.body_text)) LIKE 'showing posts %'
    AND COALESCE(
        (item.content_processing ->> 'link_count')::integer,
        0
    ) >= 10
ORDER BY
    item.id,
    (recipe.status = 'active') DESC,
    recipe.created_at DESC,
    recipe.id;

CREATE TEMP TABLE explicit_category_listing_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.explicit_category_listing_backfill_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM explicit_category_listing_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM explicit_category_listing_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM explicit_category_listing_items
                ),
            'policy', 'recipe-listing-artifact.v36',
            'migration', '0105_quarantine_explicit_category_listings'
        )
    WHERE EXISTS (SELECT 1 FROM explicit_category_listing_items)
    RETURNING id
)
INSERT INTO explicit_category_listing_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'explicit_category_listing',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v36',
            'recipe_id', repair.recipe_id,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    explicit_category_listing_items AS repair
    CROSS JOIN explicit_category_listing_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: explicit_category_listing',
    normalized_feed_item_id = NULL
FROM explicit_category_listing_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'raw_crawl_item_id', repair.raw_crawl_item_id,
        'recipe_id', repair.recipe_id,
        'url', repair.url,
        'canonical_url', repair.canonical_url,
        'title', repair.title,
        'published_at', repair.published_at,
        'reason', 'explicit_category_listing',
        'reversible', true,
        'policy', 'recipe-listing-artifact.v36',
        'repair_wave_event_id', wave.event_id,
        'migration', '0105_quarantine_explicit_category_listings'
    )
FROM
    explicit_category_listing_items AS repair
    CROSS JOIN explicit_category_listing_wave AS wave;
