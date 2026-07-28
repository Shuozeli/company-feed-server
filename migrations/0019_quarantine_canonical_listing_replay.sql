-- A detail-looking fetched URL can canonicalize to an obvious publication
-- root. Canonical URLs now receive the same listing-path gate as final URLs.
-- Quarantine the single row that was replayed between the preceding repair
-- migration and deployment of that canonical gate.

CREATE TEMP TABLE recipe_canonical_listing_artifacts ON COMMIT DROP AS
WITH latest_recipe_source AS (
    SELECT DISTINCT ON (recipe.source_id)
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
        raw.fetched_url,
        lower(split_part(split_part(item.canonical_url, '?', 1), '#', 1))
            AS canonical_resource
    FROM latest_recipe_source AS recipe
    JOIN feed_items AS item ON item.source_id = recipe.source_id
    JOIN raw_crawl_items AS raw ON raw.id = item.raw_crawl_item_id
    WHERE NOT item.is_private
)
SELECT
    feed_item_id,
    raw_crawl_item_id,
    company_id,
    source_id,
    canonical_url,
    title,
    'canonical_obvious_listing_path'::text AS reason
FROM recipe_items
WHERE
    fetched_url <> canonical_url
    AND canonical_url !~ '\?'
    AND canonical_resource ~
        '/(announcement|announcements|article|articles|blog|blogs|company-news|engineering|insights|in-the-news|investor-relations|investor-news|latest-news|market-announcements|media-center|media-centre|media-room|news|news-and-events|news-events|news-release|news-releases|newsroom|press|press-center|press-centre|press-release|press-releases|pressreleases|pressroom|research|stories|updates|what-s-new|whats-new|coverage|events|fact-center|news-press-center|past-events|social-media|social_media\.html|webinars)/?$';

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
FROM recipe_canonical_listing_artifacts AS artifact
WHERE item.id = artifact.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || artifact.reason,
    normalized_feed_item_id = NULL
FROM recipe_canonical_listing_artifacts AS artifact
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
        'repair_wave_event_id', 101808,
        'migration', '0019_quarantine_canonical_listing_replay'
    )
FROM recipe_canonical_listing_artifacts AS artifact;
