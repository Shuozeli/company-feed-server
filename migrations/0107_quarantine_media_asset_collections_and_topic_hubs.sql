-- Media-download collections and short "... news and updates | ... Blog"
-- topic pages are navigation resources rather than individual editorial
-- articles. Quarantine historical rows from every source lifecycle state, then
-- queue healthy active recipes so the shared runtime rule proves the repair.

CREATE TEMP TABLE non_editorial_collection_items
ON COMMIT DROP AS
WITH classified AS (
    SELECT DISTINCT ON (item.id)
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        recipe.id AS recipe_id,
        item.url,
        item.canonical_url,
        item.title,
        item.published_at,
        CASE
            WHEN lower(regexp_replace(
                split_part(split_part(
                    COALESCE(NULLIF(item.canonical_url, ''), item.url),
                    '?',
                    1
                ), '#', 1),
                '/$',
                ''
            )) ~ '/(b-roll|brand-assets|corporate-video|historical-photos|management-photos|materials-for-media|media-materials|press-assets)(\.(aspx|html|asp|htm|php))?$'
            THEN 'non_editorial_media_asset_collection'
            ELSE 'non_editorial_news_updates_topic_hub'
        END AS reason,
        CASE
            WHEN lower(regexp_replace(
                split_part(split_part(
                    COALESCE(NULLIF(item.canonical_url, ''), item.url),
                    '?',
                    1
                ), '#', 1),
                '/$',
                ''
            )) ~ '/(b-roll|brand-assets|corporate-video|historical-photos|management-photos|materials-for-media|media-materials|press-assets)(\.(aspx|html|asp|htm|php))?$'
            THEN 'recipe-listing-artifact.v39'
            ELSE 'recipe-listing-artifact.v40'
        END AS policy
    FROM
        feed_items AS item
        LEFT JOIN LATERAL (
            SELECT candidate.id, candidate.status, candidate.created_at
            FROM company_news_recipes AS candidate
            WHERE candidate.source_id = item.source_id
            ORDER BY
                CASE candidate.status
                    WHEN 'active' THEN 0
                    WHEN 'stale' THEN 1
                    WHEN 'superseded' THEN 2
                    WHEN 'draft' THEN 3
                    ELSE 4
                END,
                candidate.created_at DESC,
                candidate.id
            LIMIT 1
        ) AS recipe ON true
    WHERE
        NOT item.is_private
        AND (
            lower(regexp_replace(
                split_part(split_part(
                    COALESCE(NULLIF(item.canonical_url, ''), item.url),
                    '?',
                    1
                ), '#', 1),
                '/$',
                ''
            )) ~ '/(b-roll|brand-assets|corporate-video|historical-photos|management-photos|materials-for-media|media-materials|press-assets)(\.(aspx|html|asp|htm|php))?$'
            OR lower(regexp_replace(
                btrim(item.title),
                '[[:space:]]+',
                ' ',
                'g'
            )) ~ '^[^|[:digit:]]+ news and updates \| ([^|[:space:]]+[[:space:]]+){0,3}blog$'
        )
    ORDER BY item.id
)
SELECT * FROM classified;

CREATE TEMP TABLE non_editorial_collection_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.non_editorial_collection_backfill_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM non_editorial_collection_items),
            'recipe_count',
                (
                    SELECT count(DISTINCT recipe_id)
                    FROM non_editorial_collection_items
                    WHERE recipe_id IS NOT NULL
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM non_editorial_collection_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM non_editorial_collection_items
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM non_editorial_collection_items
                        GROUP BY reason
                    ) AS counts
                ),
            'migration',
                '0107_quarantine_media_asset_collections_and_topic_hubs'
        )
    WHERE EXISTS (SELECT 1 FROM non_editorial_collection_items)
    RETURNING id
)
INSERT INTO non_editorial_collection_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', repair.policy,
            'recipe_id', repair.recipe_id,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    non_editorial_collection_items AS repair
    CROSS JOIN non_editorial_collection_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM non_editorial_collection_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

CREATE TEMP TABLE non_editorial_collection_targets
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    count(*) AS item_count,
    array_agg(DISTINCT repair.reason ORDER BY repair.reason) AS reasons
FROM
    non_editorial_collection_items AS repair
    JOIN company_news_recipes AS recipe
        ON recipe.source_id = repair.source_id
        AND recipe.status = 'active'
    LEFT JOIN company_news_recipe_state AS state
        ON state.recipe_id = recipe.id
WHERE NOT COALESCE(state.rebuild_required, false)
GROUP BY
    recipe.id,
    recipe.company_id,
    recipe.source_id;

INSERT INTO company_news_recipe_state (
    recipe_id,
    consecutive_correctness_failures,
    freshness_status,
    correctness_status,
    rebuild_required,
    reason,
    metadata
)
SELECT
    target.recipe_id,
    1,
    'unknown',
    'failing',
    false,
    'quality_revalidation_required',
    jsonb_build_object(
        'quality_revalidation',
        jsonb_build_object(
            'item_count', target.item_count,
            'reasons', to_jsonb(target.reasons),
            'repair_wave_event_id', wave.event_id,
            'migration',
                '0107_quarantine_media_asset_collections_and_topic_hubs'
        )
    )
FROM
    non_editorial_collection_targets AS target
    CROSS JOIN non_editorial_collection_wave AS wave
ON CONFLICT (recipe_id) DO UPDATE
SET
    consecutive_correctness_failures = GREATEST(
        company_news_recipe_state.consecutive_correctness_failures,
        1
    ),
    correctness_status = 'failing',
    rebuild_required = false,
    reason = 'quality_revalidation_required',
    metadata = company_news_recipe_state.metadata || EXCLUDED.metadata,
    updated_at = CURRENT_TIMESTAMP;

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
        'reason', repair.reason,
        'reversible', true,
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0107_quarantine_media_asset_collections_and_topic_hubs'
    )
FROM
    non_editorial_collection_items AS repair
    CROSS JOIN non_editorial_collection_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_quality_revalidation_required',
    target.company_id,
    target.source_id,
    jsonb_build_object(
        'recipe_id', target.recipe_id,
        'item_count', target.item_count,
        'reasons', to_jsonb(target.reasons),
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0107_quarantine_media_asset_collections_and_topic_hubs'
    )
FROM
    non_editorial_collection_targets AS target
    CROSS JOIN non_editorial_collection_wave AS wave;
