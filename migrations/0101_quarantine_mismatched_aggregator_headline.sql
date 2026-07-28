-- A Zacks article page about Smith+Nephew exposed an unrelated recommendation
-- card H1 ahead of its real headline. The article body and canonical URL were
-- company-scoped, but the stored public title was not. Hold this bounded row
-- in the replay-safe shared-host quarantine until the corrected generic title
-- selector observes the same canonical article again.

CREATE TEMP TABLE mismatched_aggregator_headlines
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at,
    'shared_host_manual_item_requires_revalidation'::text AS reason
FROM
    feed_items AS item
    JOIN companies AS company ON company.id = item.company_id
    JOIN sources AS source ON source.id = item.source_id
WHERE
    NOT item.is_private
    AND company.name = 'Smith & Nephew SNATS Inc. Common Stock'
    AND lower(split_part(
        split_part(btrim(source.url), '://', 2),
        '/',
        1
    )) IN ('zacks.com', 'www.zacks.com')
    AND public_url_identity_key(item.canonical_url)
        = public_url_identity_key(
            'https://www.zacks.com/stock/news/2959634/'
            'smith-nephew-expands-asc-platform-to-support-value-based-care'
        )
    AND item.title
        = 'S&P 500 Q2 Earnings: Stripping Out Outsized Impact of GOOGL & MU';

CREATE TEMP TABLE mismatched_aggregator_headline_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.aggregator_headline_revalidation_started',
        jsonb_build_object(
            'item_count', count(*),
            'policy', 'shared-direct-scope.v2',
            'migration', '0101_quarantine_mismatched_aggregator_headline'
        )
    FROM mismatched_aggregator_headlines
    HAVING count(*) > 0
    RETURNING id
)
INSERT INTO mismatched_aggregator_headline_wave (event_id)
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
            'policy', 'shared-direct-scope.v2',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    mismatched_aggregator_headlines AS repair
    CROSS JOIN mismatched_aggregator_headline_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM mismatched_aggregator_headlines AS repair
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
        'policy', 'shared-direct-scope.v2',
        'repair_wave_event_id', wave.event_id,
        'migration', '0101_quarantine_mismatched_aggregator_headline'
    )
FROM
    mismatched_aggregator_headlines AS repair
    CROSS JOIN mismatched_aggregator_headline_wave AS wave;
