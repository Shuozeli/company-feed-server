-- Reversibly remove category, utility, pagination, and content-hub pages that
-- older generic HTML recipe crawls normalized as individual articles.
--
-- The crawler now rejects explicit CMS taxonomy paths and terminal collection
-- routes, recognizes pagination labels and a provider-neutral vocabulary of
-- section/framework headings, and applies a stricter bounded density test when
-- only the generic paragraph-cluster fallback supports an undated page.

CREATE TEMP TABLE taxonomy_pagination_and_section_artifacts
ON COMMIT DROP AS
WITH base AS (
    SELECT
        item.*,
        split_part(item.canonical_url, '?', 1) AS url_without_query,
        lower(btrim(regexp_replace(
            item.title,
            '[[:space:]]+',
            ' ',
            'g'
        ))) AS normalized_title,
        lower(regexp_replace(
            regexp_replace(
                regexp_replace(
                    split_part(item.canonical_url, '?', 1),
                    '/$',
                    ''
                ),
                '^.*/',
                ''
            ),
            '\.(html?|aspx?|php)$',
            '',
            'i'
        )) AS terminal_segment,
        CASE
            WHEN item.content_processing->>'link_count' ~ '^[0-9]+$'
            THEN (item.content_processing->>'link_count')::bigint
            ELSE 0
        END AS link_count,
        CASE
            WHEN item.raw->>'sanitized_content_chars' ~ '^[0-9]+$'
            THEN (item.raw->>'sanitized_content_chars')::bigint
            ELSE length(item.body_text)
        END AS content_chars
    FROM feed_items AS item
    WHERE
        NOT item.is_private
        AND item.source_kind IN ('html', 'browser')
),
classified AS (
    SELECT
        base.*,
        CASE
            WHEN
                base.normalized_title ~ '^show page [0-9]+$'
                OR base.normalized_title ~ '^show [0-9]+ per page$'
            THEN 'pagination_control_title'
            WHEN base.url_without_query ~*
                '/(cat|categor(y|ia)|categories|categorias)/'
            THEN 'explicit_taxonomy_path'
            WHEN base.terminal_segment = ANY (ARRAY[
                'code-examples',
                'corporate-governance',
                'customer-stories',
                'events',
                'events-and-presentations',
                'latest-press-releases',
                'media-contacts',
                'media-library',
                'media-resources',
                'news-and-stories',
                'podcasts',
                'product-updates',
                'resources',
                'sec-filings',
                'subscribe',
                'subscriptions',
                'sustainability'
            ])
            THEN 'terminal_collection_path'
            WHEN
                base.published_at IS NULL
                AND base.raw->>'article_body_selector' =
                    'generic:paragraph-cluster.v1'
                AND array_length(
                    regexp_split_to_array(
                        base.normalized_title,
                        '[[:space:]]+'
                    ),
                    1
                ) <= 6
                AND base.link_count >= 50
                AND base.link_count * 1000 >= base.content_chars * 8
            THEN 'weak_generic_paragraph_collection'
            WHEN base.normalized_title = ANY (ARRAY[
                'agreement manager',
                'all industries',
                'app center',
                'artificial intelligence',
                'artificial intelligence insights',
                'audited financial statements',
                'blackrock investment institute',
                'building',
                'cautionary statement',
                'common api tasks',
                'common pool problems',
                'contract lifecycle management',
                'cookie policy',
                'cricut contact information',
                'customer experience insights',
                'cybersecurity',
                'data center',
                'data-driven finance',
                'delaware',
                'developer support articles',
                'developer tools',
                'developer trending topics',
                'digital transformation insights',
                'document generation',
                'electronic signature',
                'enode news',
                'environment',
                'esignature',
                'evolving energy',
                'family of brands',
                'gunshot detection',
                'healthy spaces podcast',
                'home care business',
                'home care marketing',
                'home care technology',
                'identify',
                'inspiration',
                'investment ideas',
                'investment team voices',
                'investor learning',
                'know more',
                'life@gen',
                'lifestyle',
                'lucid tips and updates',
                'market signals podcast',
                'maryland',
                'media and news',
                'media resources arrow_forward',
                'natural disasters',
                'newborn skincare',
                'news releases details',
                'oncology',
                'our story',
                'our thinking',
                'people & impact',
                'poolsmart',
                'product and innovation',
                'research and reports',
                'safety & security',
                'sdks and tools',
                'sign up for our investor news alerts',
                'skin conditions',
                'solutions & innovation',
                'stories and perspectives',
                'street view video',
                'the honeylove edit',
                'trends & ideas',
                'vanta release notes',
                'view code examples on github',
                'virginia',
                'water treatment',
                'weekly market performance',
                'west virginia',
                'woodside fact checker',
                'workflow builder'
            ])
                OR base.normalized_title ~
                    '^contact[[:space:]]*[|–—-][[:space:]]*[^[:space:]]+'
            THEN 'generic_section_or_chrome_title'
            ELSE NULL
        END AS reason
    FROM base
)
SELECT
    classified.id AS feed_item_id,
    classified.raw_crawl_item_id,
    classified.company_id,
    classified.source_id,
    classified.canonical_url,
    classified.title,
    classified.published_at,
    classified.reason
FROM classified
WHERE classified.reason IS NOT NULL;

CREATE TEMP TABLE taxonomy_pagination_and_section_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.taxonomy_pagination_and_section_repair_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM taxonomy_pagination_and_section_artifacts
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM taxonomy_pagination_and_section_artifacts
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM taxonomy_pagination_and_section_artifacts
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM taxonomy_pagination_and_section_artifacts
                        GROUP BY reason
                    ) AS counts
                ),
            'policy', 'recipe-listing-artifact.v19',
            'migration',
                '0063_quarantine_taxonomy_pagination_and_section_artifacts'
        )
    WHERE EXISTS (
        SELECT 1 FROM taxonomy_pagination_and_section_artifacts
    )
    RETURNING id
)
INSERT INTO taxonomy_pagination_and_section_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v19',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    taxonomy_pagination_and_section_artifacts AS repair
    CROSS JOIN taxonomy_pagination_and_section_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM taxonomy_pagination_and_section_artifacts AS repair
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
        'policy', 'recipe-listing-artifact.v19',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0063_quarantine_taxonomy_pagination_and_section_artifacts'
    )
FROM
    taxonomy_pagination_and_section_artifacts AS repair
    CROSS JOIN taxonomy_pagination_and_section_wave AS wave;
