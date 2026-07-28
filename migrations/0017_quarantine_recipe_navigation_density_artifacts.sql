-- Recipe article validation now rejects explicit navigation-hub paths and
-- short weak-signal pages whose link density is characteristic of a
-- collection rather than an article. Retain the historical rows and raw
-- evidence while excluding this bounded repair set from public reads.

CREATE TEMP TABLE recipe_navigation_density_artifacts ON COMMIT DROP AS
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
        COALESCE(
            (item.content_processing->>'link_count')::integer,
            0
        ) AS link_count,
        COALESCE(
            (raw.payload->'payload'->>'sanitized_content_chars')::integer,
            0
        ) AS content_chars,
        lower(split_part(split_part(item.canonical_url, '?', 1), '#', 1))
            AS canonical_resource
    FROM latest_recipe_source AS recipe
    JOIN feed_items AS item ON item.source_id = recipe.source_id
    LEFT JOIN raw_crawl_items AS raw ON raw.id = item.raw_crawl_item_id
    WHERE NOT item.is_private
), classified AS (
    SELECT
        item.*,
        CASE
            WHEN
                item.canonical_resource ~ '/(pillar|tagged)(/|$)'
                OR item.canonical_resource ~
                    '/(events|fact-center|news-press-center|past-events|social-media|social_media\.html|webinars)/?$'
                THEN 'explicit_navigation_hub'
            WHEN
                item.extraction->'article_signals' =
                    '["article_like_path_with_h1"]'::jsonb
                AND array_length(
                    regexp_split_to_array(btrim(item.title), E'\\s+'),
                    1
                ) <= 6
                AND item.link_count >= 20
                AND item.link_count::bigint * 1000
                    >= item.content_chars::bigint * 15
                THEN 'high_link_density_collection'
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
FROM recipe_navigation_density_artifacts AS artifact
WHERE item.id = artifact.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || artifact.reason,
    normalized_feed_item_id = NULL
FROM recipe_navigation_density_artifacts AS artifact
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
        'repair_wave_event_id', 66155,
        'migration', '0017_quarantine_recipe_navigation_density_artifacts'
    )
FROM recipe_navigation_density_artifacts AS artifact;
