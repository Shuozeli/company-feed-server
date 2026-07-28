-- Direct evidence URLs now receive the same obvious-listing-path gate as
-- recipe links, even when a listing page emits misleading article metadata.
-- Year archive pages are additionally identified by their path and title.
-- Retain the bounded historical evidence while hiding it from public reads.

CREATE TEMP TABLE recipe_strong_signal_listing_artifacts ON COMMIT DROP AS
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
        lower(split_part(split_part(item.canonical_url, '?', 1), '#', 1))
            AS canonical_resource,
        lower(btrim(item.title)) AS title_key
    FROM latest_recipe_source AS recipe
    JOIN feed_items AS item ON item.source_id = recipe.source_id
    WHERE NOT item.is_private
), classified AS (
    SELECT
        item.*,
        CASE
            WHEN
                item.canonical_resource ~
                    '/(archive|archives|author|authors|category|categories|collection|collections|complete-archive|page|pagination|pillar|search|series|tag|tags|tagged|topic|topics)(/|$)'
                OR item.canonical_resource ~
                    '/(author|category|tag|topic)-[^/]+(/|$)'
                OR (
                    item.canonical_url !~ '\?'
                    AND item.canonical_resource ~
                        '/(announcement|announcements|article|articles|blog|blogs|company-news|engineering|insights|in-the-news|investor-relations|investor-news|latest-news|market-announcements|media-center|media-centre|media-room|news|news-and-events|news-events|news-release|news-releases|newsroom|press|press-center|press-centre|press-release|press-releases|pressreleases|pressroom|research|stories|updates|what-s-new|whats-new|coverage|events|fact-center|news-press-center|past-events|social-media|social_media\.html|webinars)/?$'
                )
                THEN 'obvious_listing_path'
            WHEN
                item.canonical_resource ~ '/[0-9]{4}/?$'
                AND item.title_key ~
                    '^(all news|announcements|blog|company news|investor news|latest news|media center|media centre|news|news archive|news archives|newsroom|press release|press releases|archive|archives|year archive)[[:space:]]*[|:–—-][[:space:]]*[0-9]{4}$'
                THEN 'year_archive_collection'
            ELSE NULL
        END AS reason
    FROM recipe_items AS item
)
SELECT
    feed_item_id,
    raw_crawl_item_id,
    company_id,
    source_id,
    canonical_url,
    title,
    reason
FROM classified
WHERE reason IS NOT NULL;

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
FROM recipe_strong_signal_listing_artifacts AS artifact
WHERE item.id = artifact.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || artifact.reason,
    normalized_feed_item_id = NULL
FROM recipe_strong_signal_listing_artifacts AS artifact
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
        'repair_wave_event_id', 101457,
        'migration', '0018_quarantine_strong_signal_listing_artifacts'
    )
FROM recipe_strong_signal_listing_artifacts AS artifact;
