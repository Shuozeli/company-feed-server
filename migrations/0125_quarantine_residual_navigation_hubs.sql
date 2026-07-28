-- Align historical HTML/browser rows with the shared non-editorial utility
-- policy. These exact short labels identify navigation or media collection
-- pages rather than individual editorial articles. The quarantine remains
-- reversible if a later crawl extracts a substantive article from the same
-- canonical URL.

CREATE TEMP TABLE residual_navigation_hub_items
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
        item.published_at
    FROM
        feed_items AS item
        LEFT JOIN LATERAL (
            SELECT candidate.id
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
        AND item.source_kind IN ('html', 'browser')
        AND (
            lower(regexp_replace(
                btrim(item.title),
                '[[:space:]]+',
                ' ',
                'g'
            )) = 'photos & videos'
            OR (
                lower(regexp_replace(
                    btrim(item.title),
                    '[[:space:]]+',
                    ' ',
                    'g'
                )) = 'timeline'
                AND lower(
                    regexp_replace(
                        split_part(
                            split_part(item.canonical_url, '#', 1),
                            '?',
                            1
                        ),
                        '/+$',
                        ''
                    )
                ) ~ '/timeline$'
            )
            OR lower(regexp_replace(
                btrim(item.title),
                '[[:space:]]+',
                ' ',
                'g'
            )) ~ '^view more from [^[:digit:]]{1,100}$'
        )
    ORDER BY item.id
)
SELECT * FROM classified;

CREATE TEMP TABLE residual_navigation_hub_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.residual_navigation_hub_backfill_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM residual_navigation_hub_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM residual_navigation_hub_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM residual_navigation_hub_items
                ),
            'policy', 'recipe-listing-artifact.v48',
            'migration', '0125_quarantine_residual_navigation_hubs'
        )
    WHERE EXISTS (SELECT 1 FROM residual_navigation_hub_items)
    RETURNING id
)
INSERT INTO residual_navigation_hub_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'generic_listing_title',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v48',
            'recipe_id', repair.recipe_id,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP,
            'migration', '0125_quarantine_residual_navigation_hubs'
        )
    )
FROM
    residual_navigation_hub_items AS repair
    CROSS JOIN residual_navigation_hub_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: generic_listing_title',
    normalized_feed_item_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM residual_navigation_hub_items AS repair
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
        'reason', 'generic_listing_title',
        'reversible', true,
        'policy', 'recipe-listing-artifact.v48',
        'repair_wave_event_id', wave.event_id,
        'migration', '0125_quarantine_residual_navigation_hubs'
    )
FROM
    residual_navigation_hub_items AS repair
    CROSS JOIN residual_navigation_hub_wave AS wave;
