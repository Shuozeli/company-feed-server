-- Some collection hubs changed their exposed title when replayed: listing
-- evidence replaced an original generic heading with another category label.
-- Quarantine those repaired variants while the runtime now evaluates the
-- shallow page structure and selected body, not only the first title string.

CREATE TEMP TABLE repaired_collection_hub_items
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
        WHEN lower(btrim(item.title)) IN ('category', 'categories')
            OR lower(item.title) ~
                '^[^|]{1,40} blogs \| [^|]{1,60}$'
        THEN 'generic_collection_title'
        WHEN company.company_key =
            'niq-global-intelligence-plc-ordinary-shares'
        THEN 'shallow_card_grid_collection'
        ELSE 'featured_article_collection'
    END AS reason
FROM
    feed_items AS item
    JOIN sources AS source ON source.id = item.source_id
    JOIN companies AS company ON company.id = item.company_id
WHERE
    NOT item.is_private
    AND source.kind = 'html'
    AND (
        lower(btrim(item.title)) IN ('category', 'categories')
        OR lower(item.title) ~
            '^[^|]{1,40} blogs \| [^|]{1,60}$'
        OR (
            company.company_key =
                'niq-global-intelligence-plc-ordinary-shares'
            AND lower(item.canonical_url) ~
                '/insights/(brand-strategy|consumer-behavior|data-science-analytics|e-commerce|health-wellness|industry-trends|pricing-and-promotion|product-innovation)/?$'
        )
        OR (
            company.company_key =
                'james-hardie-industries-plc-ordinary-shares'
            AND lower(item.canonical_url) ~
                '/press-releases/(performance|products)/?$'
        )
    );

CREATE TEMP TABLE repaired_collection_hub_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.repaired_collection_hub_repair_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM repaired_collection_hub_items
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM repaired_collection_hub_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM repaired_collection_hub_items
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM repaired_collection_hub_items
                        GROUP BY reason
                    ) AS counts
                ),
            'policy', 'recipe-listing-artifact.v27',
            'migration',
                '0078_quarantine_repaired_collection_hub_titles'
        )
    WHERE EXISTS (
        SELECT 1 FROM repaired_collection_hub_items
    )
    RETURNING id
)
INSERT INTO repaired_collection_hub_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v27',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    repaired_collection_hub_items AS repair
    CROSS JOIN repaired_collection_hub_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM repaired_collection_hub_items AS repair
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
        'policy', 'recipe-listing-artifact.v27',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0078_quarantine_repaired_collection_hub_titles'
    )
FROM
    repaired_collection_hub_items AS repair
    CROSS JOIN repaired_collection_hub_wave AS wave;
