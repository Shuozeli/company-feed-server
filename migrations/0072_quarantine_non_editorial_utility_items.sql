-- Utility forms and policy pages are not company news even when an approved
-- feed or an HTML listing links to them. The shared normalizer now rejects
-- these conservative title/path signatures for every ingestion mode.

CREATE TEMP TABLE non_editorial_utility_items
ON COMMIT DROP AS
WITH normalized AS (
    SELECT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.canonical_url,
        item.title,
        item.published_at,
        lower(btrim(regexp_replace(
            item.title,
            '[[:space:]]+',
            ' ',
            'g'
        ))) AS normalized_title,
        lower(regexp_replace(
            split_part(split_part(item.canonical_url, '?', 1), '#', 1),
            '/+$',
            ''
        )) AS normalized_url
    FROM feed_items AS item
    WHERE NOT item.is_private
)
SELECT
    normalized.feed_item_id,
    normalized.raw_crawl_item_id,
    normalized.company_id,
    normalized.source_id,
    normalized.canonical_url,
    normalized.title,
    normalized.published_at,
    'non_editorial_utility_item'::text AS reason
FROM normalized
WHERE
    normalized.normalized_title = ANY (ARRAY[
        'chain reaction newsletter archive',
        'cookies policy (opens in new window)',
        'investor communications sign up',
        'learn more : gts north america',
        'newsletter sign-up',
        'read more data and research articles',
        'sign up for our investors news alert',
        'subscribe to visualize',
        'subscribe using the form below'
    ])
    OR normalized.normalized_url ~
        '/(cookie-policy|cookies-policy|newsletter-sign-up|newsletter-signup|personal-data|privacy-policy|subscribe|subscription|terms-of-use)$'
    OR (
        normalized.normalized_url ~ '/none$'
        AND normalized.normalized_title LIKE '%coming soon'
    );

CREATE TEMP TABLE non_editorial_utility_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.non_editorial_utility_repair_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM non_editorial_utility_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM non_editorial_utility_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM non_editorial_utility_items
                ),
            'policy', 'non-editorial-utility-item.v1',
            'migration',
                '0072_quarantine_non_editorial_utility_items'
        )
    WHERE EXISTS (SELECT 1 FROM non_editorial_utility_items)
    RETURNING id
)
INSERT INTO non_editorial_utility_wave (event_id)
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
            'policy', 'non-editorial-utility-item.v1',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    non_editorial_utility_items AS repair
    CROSS JOIN non_editorial_utility_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM non_editorial_utility_items AS repair
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
        'policy', 'non-editorial-utility-item.v1',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0072_quarantine_non_editorial_utility_items'
    )
FROM
    non_editorial_utility_items AS repair
    CROSS JOIN non_editorial_utility_wave AS wave;
