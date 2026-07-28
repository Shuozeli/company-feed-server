-- Recipe link selection now ranks article-detail URLs ahead of navigation and
-- the article gate rejects plural/prefixed taxonomy paths plus weak path+H1
-- collection pages. Retain all historical rows and raw evidence while hiding
-- the bounded residual artifacts from public reads.

CREATE TEMP TABLE recipe_navigation_collection_artifacts ON COMMIT DROP AS
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
        raw.payload->'payload' AS extraction,
        lower(split_part(split_part(item.canonical_url, '?', 1), '#', 1))
            AS canonical_resource,
        lower(btrim(item.title)) AS title_key
    FROM latest_recipe_source AS recipe
    JOIN feed_items AS item ON item.source_id = recipe.source_id
    LEFT JOIN raw_crawl_items AS raw ON raw.id = item.raw_crawl_item_id
    WHERE NOT item.is_private
), classified AS (
    SELECT
        item.*,
        CASE
            WHEN
                item.canonical_resource ~ '/categories(/|$)'
                OR item.canonical_resource ~
                    '/(author|category|tag|topic)-[^/]+(/|$)'
                OR item.canonical_resource ~ '/coverage/?$'
                THEN 'taxonomy_or_coverage_page'
            WHEN
                item.extraction->'article_signals' =
                    '["article_like_path_with_h1"]'::jsonb
                AND array_length(
                    regexp_split_to_array(btrim(item.title), E'\\s+'),
                    1
                ) <= 6
                AND item.title !~ '[0-9]'
                AND (
                    item.title_key = 'the blog'
                    OR item.title_key LIKE 'browse all %'
                    OR item.title_key LIKE 'recent %'
                    OR item.title_key LIKE 'see all %'
                    OR item.title_key LIKE 'view all %'
                    OR (
                        item.title_key NOT LIKE '% | %'
                        AND item.title_key NOT LIKE '% - %'
                        AND item.title_key NOT LIKE '% – %'
                        AND item.title_key NOT LIKE '% — %'
                        AND item.title_key NOT LIKE '%: %'
                        AND item.title_key ~
                            '( articles| blog| coverage| news| press releases| stories)$'
                    )
                    OR (
                        item.title_key ~ '^[^[:space:]]+ - .+ blog$'
                        AND replace(
                            regexp_replace(
                                rtrim(item.canonical_resource, '/'),
                                '^.*/',
                                ''
                            ),
                            '-',
                            ' '
                        ) = split_part(item.title_key, ' - ', 1)
                    )
                )
                THEN 'weak_collection_title'
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
FROM recipe_navigation_collection_artifacts AS artifact
WHERE item.id = artifact.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || artifact.reason,
    normalized_feed_item_id = NULL
FROM recipe_navigation_collection_artifacts AS artifact
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
        'repair_wave_event_id', 65033,
        'migration', '0016_quarantine_navigation_collection_artifacts'
    )
FROM recipe_navigation_collection_artifacts AS artifact;
