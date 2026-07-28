-- A pre-guard HTML recipe crawl could normalize a publication listing as an
-- article when the page exposed article-like markup or generic social titles.
-- The crawler now rejects these pages before persistence. Quarantine only the
-- residual proven artifacts from recipe sources while retaining both
-- the normalized row and raw crawl evidence for reversible audit.

CREATE TEMP TABLE recipe_listing_artifacts ON COMMIT DROP AS
WITH recipe_sources AS (
    SELECT DISTINCT ON (recipe.source_id)
        recipe.company_id,
        recipe.source_id,
        recipe.spec ->> 'publication_url' AS publication_url
    FROM company_news_recipes AS recipe
    ORDER BY recipe.source_id, recipe.version DESC
), classified AS (
    SELECT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.canonical_url,
        item.title,
        (
            lower(regexp_replace(
                regexp_replace(
                    regexp_replace(
                        split_part(split_part(item.canonical_url, '?', 1), '#', 1),
                        '^https?://(www\.)?',
                        '',
                        'i'
                    ),
                    '/(default|index)(\.(asp|aspx|htm|html|php))?/?$',
                    '',
                    'i'
                ),
                '/+$',
                ''
            )) = lower(regexp_replace(
                regexp_replace(
                    regexp_replace(
                        split_part(split_part(recipe.publication_url, '?', 1), '#', 1),
                        '^https?://(www\.)?',
                        '',
                        'i'
                    ),
                    '/(default|index)(\.(asp|aspx|htm|html|php))?/?$',
                    '',
                    'i'
                ),
                '/+$',
                ''
            ))
            OR lower(regexp_replace(
                regexp_replace(
                    regexp_replace(
                        split_part(split_part(item.url, '?', 1), '#', 1),
                        '^https?://(www\.)?',
                        '',
                        'i'
                    ),
                    '/(default|index)(\.(asp|aspx|htm|html|php))?/?$',
                    '',
                    'i'
                ),
                '/+$',
                ''
            )) = lower(regexp_replace(
                regexp_replace(
                    regexp_replace(
                        split_part(split_part(recipe.publication_url, '?', 1), '#', 1),
                        '^https?://(www\.)?',
                        '',
                        'i'
                    ),
                    '/(default|index)(\.(asp|aspx|htm|html|php))?/?$',
                    '',
                    'i'
                ),
                '/+$',
                ''
            ))
        ) AS publication_self_page,
        lower(btrim(item.title)) ~
            '^(all news|announcements|blog|company news|company news and press releases|investor news|latest news|market announcements|media center|media centre|news|news & events|news & press releases|news and events|news and press releases|news release|news releases|newsroom|press release|press releases)([[:space:]]*[|:–—-][[:space:]]*.*)?$'
            AS generic_listing_title
    FROM recipe_sources AS recipe
    JOIN feed_items AS item ON item.source_id = recipe.source_id
    WHERE NOT item.is_private
)
SELECT
    feed_item_id,
    raw_crawl_item_id,
    company_id,
    source_id,
    canonical_url,
    title,
    CASE
        WHEN publication_self_page THEN 'publication_page_returned_as_article'
        ELSE 'generic_listing_title'
    END AS reason
FROM classified
WHERE publication_self_page OR generic_listing_title;

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
FROM recipe_listing_artifacts AS artifact
WHERE item.id = artifact.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || artifact.reason,
    normalized_feed_item_id = NULL
FROM recipe_listing_artifacts AS artifact
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
        'migration', '0012_quarantine_recipe_listing_artifacts'
    )
FROM recipe_listing_artifacts AS artifact;
