-- Quarantine residual collection pages whose CMS templates present them as
-- articles, plus valid article titles contaminated by leaked React wrapper
-- data. The collection pages remain private on replay; title-contaminated
-- articles are released after the normalizer stores their cleaned titles.

CREATE TEMP TABLE collection_title_and_framework_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at,
    CASE
        WHEN item.title LIKE '% self.__wrap_%'
        THEN 'framework_script_title_suffix'
        WHEN lower(btrim(item.title)) IN (
            'consumer behavior',
            'industry trends'
        )
        THEN 'short_multi_article_collection'
        ELSE 'generic_collection_title'
    END AS reason
FROM
    feed_items AS item
    JOIN sources AS source ON source.id = item.source_id
WHERE
    NOT item.is_private
    AND source.kind = 'html'
    AND (
        item.title LIKE '% self.__wrap_%'
        OR lower(btrim(item.title)) IN (
            'gen blogs | impact',
            'general information',
            'guides & articles',
            'the latest product offerings'
        )
        OR (
            lower(btrim(item.title)) = 'consumer behavior'
            AND lower(item.canonical_url) ~
                '/insights/consumer-behavior/?$'
        )
        OR (
            lower(btrim(item.title)) = 'industry trends'
            AND lower(item.canonical_url) ~
                '/insights/industry-trends/?$'
        )
    );

CREATE TEMP TABLE collection_title_and_framework_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.collection_title_and_framework_repair_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM collection_title_and_framework_items
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM collection_title_and_framework_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM collection_title_and_framework_items
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM collection_title_and_framework_items
                        GROUP BY reason
                    ) AS counts
                ),
            'policy', 'recipe-listing-artifact.v26',
            'migration',
                '0077_quarantine_collection_titles_and_framework_suffixes'
        )
    WHERE EXISTS (
        SELECT 1 FROM collection_title_and_framework_items
    )
    RETURNING id
)
INSERT INTO collection_title_and_framework_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v26',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    collection_title_and_framework_items AS repair
    CROSS JOIN collection_title_and_framework_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM collection_title_and_framework_items AS repair
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
        'policy', 'recipe-listing-artifact.v26',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0077_quarantine_collection_titles_and_framework_suffixes'
    )
FROM
    collection_title_and_framework_items AS repair
    CROSS JOIN collection_title_and_framework_wave AS wave;
