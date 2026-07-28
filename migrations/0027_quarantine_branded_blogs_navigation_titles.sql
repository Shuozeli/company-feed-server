-- Short "<brand> Blogs" and "<brand> Stories - Newsroom" titles identify
-- taxonomy landing pages rather than individual posts. This also closes the
-- small localized/navigation tail found by auditing the preceding repair's
-- reversible releases. Keep the follow-up separate from the already-applied
-- remaining-navigation migration.

CREATE TEMP TABLE recipe_branded_blogs_artifacts ON COMMIT DROP AS
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
        lower(btrim(regexp_replace(item.title, '[[:space:]]+', ' ', 'g'))) AS title_key,
        regexp_replace(
            lower(split_part(split_part(item.canonical_url, '?', 1), '#', 1)),
            '\.(asp|aspx|htm|html|php)/?$',
            ''
        ) AS canonical_resource
    FROM latest_recipe_source AS recipe
    JOIN feed_items AS item ON item.source_id = recipe.source_id
    WHERE NOT item.is_private
), classified AS (
    SELECT
        item.*,
        CASE
            WHEN item.title_key ~ '^[[:alnum:]&._-]+ blogs$'
                THEN 'branded_blogs_navigation_title'
            WHEN item.title_key ~
                '^[^0-9|:–—-]+ stories[[:space:]]+-[[:space:]]+newsroom$'
                THEN 'branded_stories_navigation_title'
            WHEN item.title_key = ANY (ARRAY[
                'financial news',
                'general news',
                'infographics',
                'view our webcasts',
                'voir tout'
            ])
                THEN 'generic_navigation_title'
            WHEN item.title_key !~ '[0-9]'
                AND array_length(regexp_split_to_array(item.title_key, '[[:space:]]+'), 1) <= 4
                AND item.title_key ~ ' glossary$'
                THEN 'branded_glossary_navigation_title'
            WHEN item.canonical_resource ~
                '/(communiques-de-presse|financial-news|general-news|infographics|our-events)/?$'
                THEN 'terminal_navigation_hub'
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

CREATE TEMP TABLE branded_blogs_repair_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.residual_navigation_repair_started',
        jsonb_build_object(
            'item_count', count(*),
            'source_count', count(DISTINCT source_id),
            'policy', 'recipe-listing-artifact.v5',
            'migration', '0027_quarantine_branded_blogs_navigation_titles'
        )
    FROM recipe_branded_blogs_artifacts
    HAVING count(*) > 0
    RETURNING id
)
INSERT INTO branded_blogs_repair_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', artifact.reason,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v5',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    recipe_branded_blogs_artifacts AS artifact
    CROSS JOIN branded_blogs_repair_wave AS wave
WHERE item.id = artifact.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || artifact.reason,
    normalized_feed_item_id = NULL
FROM recipe_branded_blogs_artifacts AS artifact
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
        'policy', 'recipe-listing-artifact.v5',
        'repair_wave_event_id', wave.event_id,
        'migration', '0027_quarantine_branded_blogs_navigation_titles'
    )
FROM
    recipe_branded_blogs_artifacts AS artifact
    CROSS JOIN branded_blogs_repair_wave AS wave;
