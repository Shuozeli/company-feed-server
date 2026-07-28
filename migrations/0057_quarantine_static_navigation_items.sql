-- Quarantine HTML rows that are demonstrably static navigation destinations,
-- not individual editorial items. The matching vocabulary is shared with the
-- generic recipe crawler and stays independent of company identity.

CREATE TEMP TABLE static_navigation_artifacts ON COMMIT DROP AS
WITH normalized AS (
    SELECT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.canonical_url,
        item.title,
        regexp_replace(
            regexp_replace(
                regexp_replace(
                    lower(split_part(item.canonical_url, '?', 1)),
                    '/(default|index)(\.(asp|aspx|htm|html|php))?/?$',
                    ''
                ),
                '/$',
                ''
            ),
            '^.*/',
            ''
        ) AS raw_slug
    FROM feed_items AS item
    JOIN sources AS source ON source.id = item.source_id
    WHERE
        NOT item.is_private
        AND source.kind IN ('html', 'browser')
),
classified AS (
    SELECT
        normalized.*,
        regexp_replace(
            raw_slug,
            '\.(asp|aspx|htm|html|php)$',
            ''
        ) AS navigation_slug
    FROM normalized
)
SELECT
    feed_item_id,
    raw_crawl_item_id,
    company_id,
    source_id,
    canonical_url,
    title,
    navigation_slug,
    CASE
        WHEN lower(title) LIKE '%. select to filter.'
        THEN 'listing_filter_label'
        WHEN
            navigation_slug = 'faq'
            AND title ~* '^(january|february|march|april|may|june|july|august|september|october|november|december)[[:space:]]+[0-9]{1,2},[[:space:]]+[0-9]{4}$'
        THEN 'date_only_page_title'
        ELSE 'static_navigation_path'
    END AS reason
FROM classified
WHERE
    navigation_slug = ANY(ARRAY[
        'about-us',
        'annual-general-meeting-materials',
        'annual-meetings',
        'annual-reports-and-proxy-statements',
        'audited-financial-statements',
        'brand-guides',
        'clawback-policy',
        'committees',
        'company-presentations',
        'company-voices',
        'composites-value-proposition',
        'contact-the-board',
        'cookie-policy',
        'document-center',
        'earnings-and-news',
        'email-alerts-and-rss-newsfeeds',
        'emergency-resource-center',
        'executive-management',
        'faqs',
        'featured-videos',
        'filings-and-reports',
        'governance-overview',
        'industrial-business',
        'informative-resources',
        'media',
        'media-and-analyst-contacts',
        'media-and-tpa-contacts',
        'media-center-archives',
        'media-centre-archives',
        'media-inquiry-form',
        'media-logos',
        'media-relations',
        'media-toolkit',
        'news-and-insights',
        'news-and-media',
        'news-articles',
        'news-archive',
        'officers-directors',
        'our-approach',
        'our-solutions',
        'overview',
        'performance-and-aftermarket',
        'performance-report',
        'presentations-events',
        'presentations-reports',
        'press-contacts',
        'press-release-archive',
        'product-pipeline',
        'product-pipeline-media-resources',
        'quarterly-results',
        'reports-and-releases',
        'senior-management',
        'spaces-media',
        'stay-connected',
        'stockholder-faqs',
        'stock-tax-information',
        'tax-documents',
        'tech-and-engineering',
        'thought-leadership',
        'why-invest'
    ])
    OR navigation_slug LIKE 'news-archive-%'
    OR (
        lower(canonical_url) ~ '/(investor-relations|investors)/'
        AND (
            navigation_slug LIKE 'why-own-%'
            OR (
                navigation_slug LIKE '%-ownership-restriction'
                AND array_length(
                    regexp_split_to_array(navigation_slug, '-'),
                    1
                ) <= 4
            )
        )
    )
    OR lower(title) LIKE '%. select to filter.'
    OR (
        navigation_slug = 'faq'
        AND title ~* '^(january|february|march|april|may|june|july|august|september|october|november|december)[[:space:]]+[0-9]{1,2},[[:space:]]+[0-9]{4}$'
    )
    OR (
        navigation_slug = 'webinar'
        AND lower(btrim(title)) = 'webinars'
    );

CREATE TEMP TABLE static_navigation_repair_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.static_navigation_repair_started',
        jsonb_build_object(
            'item_count', (SELECT count(*) FROM static_navigation_artifacts),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM static_navigation_artifacts
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM static_navigation_artifacts
                ),
            'policy', 'recipe-listing-artifact.v13',
            'migration', '0057_quarantine_static_navigation_items'
        )
    WHERE EXISTS (SELECT 1 FROM static_navigation_artifacts)
    RETURNING id
)
INSERT INTO static_navigation_repair_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'navigation_slug', repair.navigation_slug,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v13',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    static_navigation_artifacts AS repair
    CROSS JOIN static_navigation_repair_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM static_navigation_artifacts AS repair
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
        'reason', repair.reason,
        'navigation_slug', repair.navigation_slug,
        'reversible', true,
        'policy', 'recipe-listing-artifact.v13',
        'repair_wave_event_id', wave.event_id,
        'migration', '0057_quarantine_static_navigation_items'
    )
FROM
    static_navigation_artifacts AS repair
    CROSS JOIN static_navigation_repair_wave AS wave;
