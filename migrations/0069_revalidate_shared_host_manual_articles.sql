-- Revalidate historical direct HTML evidence collected from shared
-- multi-company press-wire and market-news hosts.
--
-- The manual recipe builder now filters every directly suggested article on
-- these hosts by company identity before normalization. Older imports predate
-- that gate, so make their public rows private until the same canonical item
-- is observed again through the new filter. A successful scoped replay
-- releases this versioned quarantine; off-company rows remain private.

CREATE TEMP TABLE shared_host_manual_articles
ON COMMIT DROP AS
WITH shared_domains(domain) AS (
    VALUES
        ('accessnewswire.com'),
        ('barchart.com'),
        ('benzinga.com'),
        ('biospace.com'),
        ('bloomberg.com'),
        ('businesswire.com'),
        ('einpresswire.com'),
        ('finance.yahoo.com'),
        ('forbes.com'),
        ('globenewswire.com'),
        ('investing.com'),
        ('marketbeat.com'),
        ('marketscreener.com'),
        ('marketwatch.com'),
        ('msn.com'),
        ('nasdaq.com'),
        ('newsfilecorp.com'),
        ('prnewswire.com'),
        ('reuters.com'),
        ('seekingalpha.com'),
        ('stocktitan.net'),
        ('tipranks.com'),
        ('tradingview.com')
),
manual_items AS (
    SELECT DISTINCT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.canonical_url,
        item.title,
        item.published_at,
        lower(split_part(
            split_part(btrim(source.url), '://', 2),
            '/',
            1
        )) AS source_host
    FROM
        feed_items AS item
        JOIN raw_crawl_items AS raw
            ON raw.id = item.raw_crawl_item_id
        JOIN crawl_runs AS crawl
            ON crawl.id = raw.crawl_run_id
        JOIN sources AS source
            ON source.id = item.source_id
    WHERE
        NOT item.is_private
        AND item.source_kind IN ('html', 'browser')
        AND crawl.metadata->>'ingestion_mode'
            = 'manual_company_news_import'
)
SELECT
    manual.feed_item_id,
    manual.raw_crawl_item_id,
    manual.company_id,
    manual.source_id,
    manual.canonical_url,
    manual.title,
    manual.published_at,
    'shared_host_manual_item_requires_revalidation'::text AS reason
FROM
    manual_items AS manual
    JOIN shared_domains AS shared
        ON manual.source_host = shared.domain
        OR manual.source_host LIKE '%.' || shared.domain;

CREATE TEMP TABLE shared_host_manual_articles_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.shared_host_manual_article_revalidation_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM shared_host_manual_articles),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM shared_host_manual_articles
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM shared_host_manual_articles
                ),
            'policy', 'shared-direct-scope.v1',
            'migration',
                '0069_revalidate_shared_host_manual_articles'
        )
    WHERE EXISTS (SELECT 1 FROM shared_host_manual_articles)
    RETURNING id
)
INSERT INTO shared_host_manual_articles_wave (event_id)
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
            'policy', 'shared-direct-scope.v1',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    shared_host_manual_articles AS repair
    CROSS JOIN shared_host_manual_articles_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM shared_host_manual_articles AS repair
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
        'policy', 'shared-direct-scope.v1',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0069_revalidate_shared_host_manual_articles'
    )
FROM
    shared_host_manual_articles AS repair
    CROSS JOIN shared_host_manual_articles_wave AS wave;
