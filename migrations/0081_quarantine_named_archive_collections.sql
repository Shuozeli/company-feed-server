-- Quarantine year-named archives and bounded branded archive/story indexes.
-- These URL/title forms are now rejected by the shared crawler and content
-- policy, while the historical rows remain replay-safe if a path later becomes
-- a valid detail page.

CREATE TEMP TABLE named_archive_collection_items
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
        lower(split_part(
            split_part(item.canonical_url, '?', 1),
            '#',
            1
        )) AS canonical_resource,
        lower(btrim(item.title)) AS normalized_title,
        CASE
            WHEN COALESCE(item.raw->>'article_element_count', '') ~
                '^[0-9]+$'
            THEN (item.raw->>'article_element_count')::integer
            ELSE 0
        END AS article_element_count
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
        WHEN candidates.canonical_resource ~
            '/employee-stories/?$'
        THEN 'employee_stories_collection'
        WHEN candidates.canonical_resource ~
            '/[0-9]{4}-news-archive/?$'
            OR candidates.canonical_resource ~
                '/(news|press)-releases?-[0-9]{4}/?$'
            OR candidates.canonical_resource ~
                '/[0-9]{4}-(news|press)-releases?/?$'
        THEN 'year_named_archive'
        ELSE 'branded_archives_title'
    END AS reason
FROM candidates
WHERE
    candidates.canonical_resource ~ '/employee-stories/?$'
    OR candidates.canonical_resource ~
        '/[0-9]{4}-news-archive/?$'
    OR candidates.canonical_resource ~
        '/(news|press)-releases?-[0-9]{4}/?$'
    OR candidates.canonical_resource ~
        '/[0-9]{4}-(news|press)-releases?/?$'
    OR (
        candidates.article_element_count >= 4
        AND candidates.normalized_title ~
            '^[[:alnum:]&'' ]{1,60} archives[[:space:]]*[|–—-][[:space:]].{1,60}$'
    );

CREATE TEMP TABLE named_archive_collection_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.named_archive_collection_repair_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM named_archive_collection_items
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM named_archive_collection_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM named_archive_collection_items
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM named_archive_collection_items
                        GROUP BY reason
                    ) AS counts
                ),
            'policy', 'recipe-listing-artifact.v30',
            'migration',
                '0081_quarantine_named_archive_collections'
        )
    WHERE EXISTS (
        SELECT 1 FROM named_archive_collection_items
    )
    RETURNING id
)
INSERT INTO named_archive_collection_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v30',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    named_archive_collection_items AS repair
    CROSS JOIN named_archive_collection_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM named_archive_collection_items AS repair
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
        'policy', 'recipe-listing-artifact.v30',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0081_quarantine_named_archive_collections'
    )
FROM
    named_archive_collection_items AS repair
    CROSS JOIN named_archive_collection_wave AS wave;
