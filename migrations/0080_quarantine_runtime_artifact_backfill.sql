-- Backfill the narrow historical set now covered by deterministic runtime
-- article gates. These rows can survive a runtime replay when the publication
-- has stopped linking the old collection URL, so retain their evidence in the
-- same reversible listing-artifact quarantine used by earlier cleanup waves.

CREATE TEMP TABLE runtime_artifact_backfill_items
ON COMMIT DROP AS
WITH candidates AS (
    SELECT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.canonical_url,
        item.title,
        item.published_at,
        CASE
            WHEN COALESCE(item.raw->>'article_element_count', '') ~
                '^[0-9]+$'
            THEN (item.raw->>'article_element_count')::integer
            ELSE 0
        END AS article_element_count,
        CASE
            WHEN COALESCE(item.raw->>'article_elements_with_h1', '') ~
                '^[0-9]+$'
            THEN (item.raw->>'article_elements_with_h1')::integer
            ELSE 0
        END AS article_elements_with_h1,
        CASE
            WHEN COALESCE(item.raw->>'sanitized_content_chars', '') ~
                '^[0-9]+$'
            THEN (item.raw->>'sanitized_content_chars')::integer
            ELSE 999999
        END AS sanitized_content_chars,
        lower(split_part(
            split_part(item.canonical_url, '?', 1),
            '#',
            1
        )) AS canonical_resource,
        lower(btrim(item.title)) AS normalized_title
    FROM
        feed_items AS item
        JOIN sources AS source ON source.id = item.source_id
    WHERE
        NOT item.is_private
        AND source.status = 'approved'
        AND source.kind IN ('html', 'browser')
        AND EXISTS (
            SELECT 1
            FROM company_news_recipes AS recipe
            WHERE recipe.source_id = item.source_id
        )
)
SELECT
    candidates.feed_item_id,
    candidates.raw_crawl_item_id,
    candidates.company_id,
    candidates.source_id,
    candidates.canonical_url,
    candidates.title,
    candidates.published_at,
    CASE
        WHEN candidates.canonical_resource ~ '/glossary/?$'
        THEN 'non_editorial_glossary'
        WHEN candidates.article_element_count >= 10
            AND candidates.article_elements_with_h1 >= 4
            AND candidates.sanitized_content_chars < 1500
        THEN 'multi_heading_card_grid'
        WHEN candidates.normalized_title =
            'company & portfolio news'
        THEN 'generic_collection_title'
        ELSE 'short_stories_collection'
    END AS reason
FROM candidates
WHERE
    candidates.canonical_resource ~ '/glossary/?$'
    OR (
        candidates.article_element_count >= 10
        AND candidates.article_elements_with_h1 >= 4
        AND candidates.sanitized_content_chars < 1500
    )
    OR candidates.normalized_title =
        'company & portfolio news'
    OR (
        candidates.article_element_count >= 4
        AND (
            candidates.normalized_title ~
                '^[[:alnum:]&'' ]{1,60} stories$'
            OR candidates.normalized_title ~
                '^[[:alnum:]&'' ]{1,60} stories[[:space:]]*[|–—-][[:space:]].{1,60}$'
        )
    );

CREATE TEMP TABLE runtime_artifact_backfill_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.runtime_artifact_backfill_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM runtime_artifact_backfill_items
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM runtime_artifact_backfill_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM runtime_artifact_backfill_items
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM runtime_artifact_backfill_items
                        GROUP BY reason
                    ) AS counts
                ),
            'policy', 'recipe-listing-artifact.v29',
            'migration',
                '0080_quarantine_runtime_artifact_backfill'
        )
    WHERE EXISTS (
        SELECT 1 FROM runtime_artifact_backfill_items
    )
    RETURNING id
)
INSERT INTO runtime_artifact_backfill_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v29',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    runtime_artifact_backfill_items AS repair
    CROSS JOIN runtime_artifact_backfill_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM runtime_artifact_backfill_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'canonical_url', repair.canonical_url,
        'title', repair.title,
        'published_at', repair.published_at,
        'reason', repair.reason,
        'reversible', true,
        'policy', 'recipe-listing-artifact.v29',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0080_quarantine_runtime_artifact_backfill'
    )
FROM
    runtime_artifact_backfill_items AS repair
    CROSS JOIN runtime_artifact_backfill_wave AS wave;
