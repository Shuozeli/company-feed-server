-- HTML pages that explain or enumerate RSS subscriptions are navigation
-- utilities, not articles. Actual RSS/Atom sources remain unaffected.

CREATE TEMP TABLE rss_directory_utility_items
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
    AND (
        lower(regexp_replace(
            split_part(split_part(item.url, '?', 1), '#', 1),
            '/$',
            ''
        )) ~ '/(all-)?rss(-feeds?)?$'
        OR lower(regexp_replace(
            split_part(split_part(item.canonical_url, '?', 1), '#', 1),
            '/$',
            ''
        )) ~ '/(all-)?rss(-feeds?)?$'
    )
ORDER BY
    item.id,
    (recipe.status = 'active') DESC,
    recipe.created_at DESC,
    recipe.id;

CREATE TEMP TABLE rss_directory_utility_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.rss_directory_utility_backfill_started',
        jsonb_build_object(
            'item_count', (SELECT count(*) FROM rss_directory_utility_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM rss_directory_utility_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM rss_directory_utility_items
                ),
            'policy', 'recipe-listing-artifact.v34',
            'migration', '0103_quarantine_rss_directory_utilities'
        )
    WHERE EXISTS (SELECT 1 FROM rss_directory_utility_items)
    RETURNING id
)
INSERT INTO rss_directory_utility_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'non_editorial_rss_directory_utility',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v34',
            'recipe_id', repair.recipe_id,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    rss_directory_utility_items AS repair
    CROSS JOIN rss_directory_utility_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: non_editorial_rss_directory_utility',
    normalized_feed_item_id = NULL
FROM rss_directory_utility_items AS repair
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
        'reason', 'non_editorial_rss_directory_utility',
        'reversible', true,
        'policy', 'recipe-listing-artifact.v34',
        'repair_wave_event_id', wave.event_id,
        'migration', '0103_quarantine_rss_directory_utilities'
    )
FROM
    rss_directory_utility_items AS repair
    CROSS JOIN rss_directory_utility_wave AS wave;
