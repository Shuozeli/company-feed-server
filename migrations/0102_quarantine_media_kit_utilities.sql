-- Press, media, and brand kits are resource hubs rather than editorial
-- articles. Quarantine historical terminal kit routes and retire recipes whose
-- only public output was one of these utilities.

CREATE TEMP TABLE media_kit_utility_items
ON COMMIT DROP AS
WITH candidate_items AS (
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
            lower(split_part(split_part(item.url, '?', 1), '#', 1))
                ~ '/(brand|media|press)[-_]?kit/?$'
            OR lower(split_part(
                split_part(item.canonical_url, '?', 1),
                '#',
                1
            )) ~ '/(brand|media|press)[-_]?kit/?$'
        )
    ORDER BY
        item.id,
        (recipe.status = 'active') DESC,
        recipe.created_at DESC,
        recipe.id
)
SELECT * FROM candidate_items;

CREATE TEMP TABLE media_kit_utility_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.media_kit_utility_backfill_started',
        jsonb_build_object(
            'item_count', (SELECT count(*) FROM media_kit_utility_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM media_kit_utility_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM media_kit_utility_items
                ),
            'policy', 'recipe-listing-artifact.v33',
            'migration', '0102_quarantine_media_kit_utilities'
        )
    WHERE EXISTS (SELECT 1 FROM media_kit_utility_items)
    RETURNING id
)
INSERT INTO media_kit_utility_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'non_editorial_media_kit_utility',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v33',
            'recipe_id', repair.recipe_id,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    media_kit_utility_items AS repair
    CROSS JOIN media_kit_utility_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: non_editorial_media_kit_utility',
    normalized_feed_item_id = NULL
FROM media_kit_utility_items AS repair
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
        'reason', 'non_editorial_media_kit_utility',
        'reversible', true,
        'policy', 'recipe-listing-artifact.v33',
        'repair_wave_event_id', wave.event_id,
        'migration', '0102_quarantine_media_kit_utilities'
    )
FROM
    media_kit_utility_items AS repair
    CROSS JOIN media_kit_utility_wave AS wave;

CREATE TEMP TABLE media_kit_utility_only_recipes
ON COMMIT DROP AS
SELECT DISTINCT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    source.url AS source_url,
    recipe.status AS previous_status
FROM
    company_news_recipes AS recipe
    JOIN sources AS source ON source.id = recipe.source_id
    JOIN media_kit_utility_items AS repair
        ON repair.recipe_id = recipe.id
WHERE
    recipe.status IN ('active', 'stale')
    AND NOT EXISTS (
        SELECT 1
        FROM feed_items AS remaining
        WHERE
            remaining.source_id = recipe.source_id
            AND NOT remaining.is_private
            AND NOT EXISTS (
                SELECT 1
                FROM media_kit_utility_items AS quarantined
                WHERE quarantined.feed_item_id = remaining.id
            )
    );

UPDATE company_news_recipes AS recipe
SET
    status = 'superseded',
    stale_at = NULL,
    stale_reason = NULL
FROM media_kit_utility_only_recipes AS invalid
WHERE
    recipe.id = invalid.recipe_id
    AND recipe.status IN ('active', 'stale');

UPDATE company_news_recipe_state AS state
SET
    rebuild_required = false,
    reason = 'non_editorial_media_kit_utility_only_recipe',
    metadata = COALESCE(state.metadata, '{}'::jsonb) || jsonb_build_object(
        'supersession',
        jsonb_build_object(
            'policy', 'company-news-utility-scope.v4',
            'reason', 'non_editorial_media_kit_utility_only_recipe',
            'source_url', invalid.source_url,
            'previous_status', invalid.previous_status,
            'migration', '0102_quarantine_media_kit_utilities'
        )
    )
FROM media_kit_utility_only_recipes AS invalid
WHERE state.recipe_id = invalid.recipe_id;

UPDATE sources AS source
SET metadata = source.metadata - 'active_recipe_id' - 'recipe_schema_version'
FROM media_kit_utility_only_recipes AS invalid
WHERE
    source.id = invalid.source_id
    AND source.metadata ->> 'active_recipe_id' = invalid.recipe_id::text;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'non-editorial media-kit utility-only recipe superseded'
FROM media_kit_utility_only_recipes AS invalid
WHERE
    job.job_type = 'crawl_source'
    AND job.status = 'pending'
    AND job.source_id = invalid.source_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_superseded',
    invalid.company_id,
    invalid.source_id,
    jsonb_build_object(
        'recipe_id', invalid.recipe_id,
        'reason', 'non_editorial_media_kit_utility_only_recipe',
        'policy', 'company-news-utility-scope.v4',
        'source_url', invalid.source_url,
        'previous_status', invalid.previous_status,
        'migration', '0102_quarantine_media_kit_utilities'
    )
FROM media_kit_utility_only_recipes AS invalid;
