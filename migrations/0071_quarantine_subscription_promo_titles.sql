-- A Webflow subscription promo can be the most substantive visible H1 even on
-- a real article page. The crawler now treats this exact promo as page chrome
-- so social metadata, the document title, or listing evidence can supply the
-- actual headline.

CREATE TEMP TABLE subscription_promo_title_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at,
    'subscription_promo_title'::text AS reason
FROM feed_items AS item
WHERE
    NOT item.is_private
    AND item.source_kind IN ('html', 'browser')
    AND lower(btrim(regexp_replace(
        item.title,
        '[[:space:]]+',
        ' ',
        'g'
    ))) =
        'never miss an update: sign up for updates, exclusive insights, and product releases.';

CREATE TEMP TABLE subscription_promo_title_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.subscription_promo_title_repair_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM subscription_promo_title_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM subscription_promo_title_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM subscription_promo_title_items
                ),
            'policy', 'recipe-listing-artifact.v24',
            'migration',
                '0071_quarantine_subscription_promo_titles'
        )
    WHERE EXISTS (SELECT 1 FROM subscription_promo_title_items)
    RETURNING id
)
INSERT INTO subscription_promo_title_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v24',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    subscription_promo_title_items AS repair
    CROSS JOIN subscription_promo_title_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM subscription_promo_title_items AS repair
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
        'policy', 'recipe-listing-artifact.v24',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0071_quarantine_subscription_promo_titles'
    )
FROM
    subscription_promo_title_items AS repair
    CROSS JOIN subscription_promo_title_wave AS wave;
