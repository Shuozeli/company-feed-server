-- Recipe crawls now reject taxonomy/collection URLs before fetching and repair
-- repeated site-wide page titles from substantive listing-link titles. Retain
-- historical rows and raw evidence, but remove the residual proven artifacts
-- from public reads. The named policy can be released by a later successful
-- normalization of the same canonical item.

CREATE TEMP TABLE recipe_collection_title_artifacts ON COMMIT DROP AS
WITH latest_recipe_source AS (
    SELECT DISTINCT ON (recipe.source_id)
        recipe.company_id,
        recipe.source_id
    FROM company_news_recipes AS recipe
    ORDER BY recipe.source_id, recipe.version DESC
), recipe_items AS (
    SELECT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.canonical_url,
        item.title,
        lower(split_part(split_part(item.canonical_url, '?', 1), '#', 1))
            AS canonical_resource,
        lower(split_part(split_part(item.url, '?', 1), '#', 1))
            AS fetched_resource,
        lower(btrim(item.title)) AS title_key
    FROM latest_recipe_source AS recipe
    JOIN feed_items AS item ON item.source_id = recipe.source_id
    WHERE NOT item.is_private
), path_artifacts AS (
    SELECT item.*
    FROM recipe_items AS item
    WHERE
        item.canonical_resource ~
            '/(archive|author|authors|category|collection|collections|page|pagination|search|series|tag|tags|topic|topics)(/|$)'
        OR item.fetched_resource ~
            '/(archive|author|authors|category|collection|collections|page|pagination|search|series|tag|tags|topic|topics)(/|$)'
        OR item.canonical_resource ~
            '/(announcement|announcements|article|articles|blog|blogs|company-news|engineering|insights|investor-news|media-center|media-centre|news|news-release|news-releases|newsroom|press|press-release|press-releases|pressroom|research|stories|updates|what-s-new|whats-new)(/(default|index)(\.(asp|aspx|htm|html|php))?)?/?$'
        OR item.fetched_resource ~
            '/(announcement|announcements|article|articles|blog|blogs|company-news|engineering|insights|investor-news|media-center|media-centre|news|news-release|news-releases|newsroom|press|press-release|press-releases|pressroom|research|stories|updates|what-s-new|whats-new)(/(default|index)(\.(asp|aspx|htm|html|php))?)?/?$'
), remaining_items AS (
    SELECT item.*
    FROM recipe_items AS item
    LEFT JOIN path_artifacts AS path ON path.feed_item_id = item.feed_item_id
    WHERE path.feed_item_id IS NULL
), low_diversity_sources AS (
    SELECT source_id
    FROM remaining_items
    GROUP BY source_id
    HAVING
        count(*) >= 5
        AND count(DISTINCT title_key) * 2 < count(*)
), repeated_titles AS (
    SELECT item.source_id, item.title_key
    FROM remaining_items AS item
    JOIN low_diversity_sources AS source ON source.source_id = item.source_id
    GROUP BY item.source_id, item.title_key
    HAVING count(*) >= 3
), repeated_title_artifacts AS (
    SELECT item.*
    FROM remaining_items AS item
    JOIN repeated_titles AS repeated ON
        repeated.source_id = item.source_id
        AND repeated.title_key = item.title_key
)
SELECT
    path.feed_item_id,
    path.raw_crawl_item_id,
    path.company_id,
    path.source_id,
    path.canonical_url,
    path.title,
    'taxonomy_or_collection_page'::text AS reason
FROM path_artifacts AS path
UNION ALL
SELECT
    repeated.feed_item_id,
    repeated.raw_crawl_item_id,
    repeated.company_id,
    repeated.source_id,
    repeated.canonical_url,
    repeated.title,
    'repeated_sitewide_title'::text AS reason
FROM repeated_title_artifacts AS repeated;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', artifact.reason,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v1',
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM recipe_collection_title_artifacts AS artifact
WHERE item.id = artifact.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || artifact.reason,
    normalized_feed_item_id = NULL
FROM recipe_collection_title_artifacts AS artifact
WHERE raw.id = artifact.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    artifact.company_id,
    artifact.source_id,
    jsonb_build_object(
        'feed_item_id', artifact.feed_item_id,
        'canonical_url', artifact.canonical_url,
        'title', artifact.title,
        'reason', artifact.reason,
        'reversible', true,
        'policy', 'recipe-listing-artifact.v1',
        'repair_wave_event_id', 62635,
        'migration', '0013_quarantine_recipe_collection_and_repeated_title_artifacts'
    )
FROM recipe_collection_title_artifacts AS artifact;
