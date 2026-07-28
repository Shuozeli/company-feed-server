-- Align historical HTML/browser items with the shared generic-title policy.
-- These short labels identify collection or utility resources, not individual
-- editorial articles. The quarantine is reversible: a future crawl that
-- extracts a substantive article title may release the same canonical item.

CREATE TEMP TABLE utility_collection_label_items
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
            )) IN (
                'all articles',
                'clinical case studies',
                'fixed income',
                'insights library',
                'media request',
                'new product announcements',
                'other archives',
                'people and culture',
                'sign up today',
                'site-seeing gallery',
                'submit media request'
            )
            OR lower(regexp_replace(
                btrim(item.title),
                '[[:space:]]+',
                ' ',
                'g'
            )) ~ '^(all articles|media request|submit media request)[[:space:]]*[|][[:space:]].+$'
            OR lower(regexp_replace(
                btrim(item.title),
                '[[:space:]]+',
                ' ',
                'g'
            )) ~ '^contact ([^|[:space:]]+[[:space:]]+){0,3}media relations([[:space:]]*[|].*)?$'
        )
    ORDER BY item.id
)
SELECT * FROM classified;

CREATE TEMP TABLE utility_collection_label_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.utility_collection_label_backfill_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM utility_collection_label_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM utility_collection_label_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM utility_collection_label_items
                ),
            'policy', 'recipe-listing-artifact.v47',
            'migration', '0119_quarantine_utility_collection_labels'
        )
    WHERE EXISTS (SELECT 1 FROM utility_collection_label_items)
    RETURNING id
)
INSERT INTO utility_collection_label_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v47',
            'recipe_id', repair.recipe_id,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    utility_collection_label_items AS repair
    CROSS JOIN utility_collection_label_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: generic_listing_title',
    normalized_feed_item_id = NULL
FROM utility_collection_label_items AS repair
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
        'policy', 'recipe-listing-artifact.v47',
        'repair_wave_event_id', wave.event_id,
        'migration', '0119_quarantine_utility_collection_labels'
    )
FROM
    utility_collection_label_items AS repair
    CROSS JOIN utility_collection_label_wave AS wave;
