-- Quarantine historical HTML/browser rows that are static investor-relations
-- utilities rather than individual editorial articles. Exact generic labels
-- cover utility pages on any host. On conventional IR subdomains, bounded
-- governance, shareholder, overview, and financial-information namespaces are
-- also non-editorial unless the URL has an explicit news/blog/press segment.
-- The quarantine is reversible if a later crawl extracts a substantive article
-- from the same canonical URL.

CREATE TEMP TABLE residual_ir_utility_items
ON COMMIT DROP AS
WITH classified AS (
    SELECT DISTINCT ON (item.id)
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        recipe.id AS recipe_id,
        item.url,
        item.canonical_url,
        item.title,
        item.published_at
    FROM
        feed_items AS item
        CROSS JOIN LATERAL (
            SELECT
                lower(regexp_replace(
                    btrim(item.title),
                    '[[:space:]]+',
                    ' ',
                    'g'
                )) AS normalized_title,
                lower(split_part(
                    regexp_replace(
                        item.canonical_url,
                        '^https?://',
                        '',
                        'i'
                    ),
                    '/',
                    1
                )) AS normalized_host,
                lower(regexp_replace(
                    split_part(
                        split_part(item.canonical_url, '?', 1),
                        '#',
                        1
                    ),
                    '^https?://[^/]+',
                    '',
                    'i'
                )) AS normalized_path
        ) AS normalized
        LEFT JOIN LATERAL (
            SELECT candidate.id
            FROM company_news_recipes AS candidate
            WHERE candidate.source_id = item.source_id
            ORDER BY
                CASE candidate.status
                    WHEN 'active' THEN 0
                    WHEN 'stale' THEN 1
                    WHEN 'superseded' THEN 2
                    WHEN 'draft' THEN 3
                    ELSE 4
                END,
                candidate.created_at DESC,
                candidate.id
            LIMIT 1
        ) AS recipe ON true
    WHERE
        NOT item.is_private
        AND item.source_kind IN ('html', 'browser')
        AND (
            normalized.normalized_title = ANY(ARRAY[
                'corporate backgrounder',
                'leadership team',
                'events & financial calendar',
                'events and financial calendar',
                'legal notes',
                'financial highlights',
                'listings',
                'share monitor tool',
                'shareholder remuneration',
                'history and profile',
                'ownership breakdown',
                'property portfolio'
            ])
            OR (
                normalized.normalized_host
                    ~ '^(ir|ri|investor|investors)\.'
                AND normalized.normalized_path
                    !~ '/(blog|blogs|changelog|changelogs|company-news|engineering|insights|news|newsroom|press|press-release|press-releases|pressrelease|pressreleases|product-updates|release-notes|research|stories|updates|what-s-new|whats-new)(/|$)'
                AND (
                    normalized.normalized_path
                        ~ '/(corporate-governance|financial-information|governance|informacoes-aos-acionistas|investor-information|overview|shareholder-information|shareholders|stock-information)(/|$)'
                    OR normalized.normalized_title
                        ~ '^why invest in [^[:digit:]]{1,100}[?]?$'
                )
            )
        )
    ORDER BY item.id
)
SELECT * FROM classified;

CREATE TEMP TABLE residual_ir_utility_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.residual_ir_utility_backfill_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM residual_ir_utility_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM residual_ir_utility_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM residual_ir_utility_items
                ),
            'policy', 'recipe-listing-artifact.v50',
            'migration', '0128_quarantine_residual_ir_utility_pages'
        )
    WHERE EXISTS (SELECT 1 FROM residual_ir_utility_items)
    RETURNING id
)
INSERT INTO residual_ir_utility_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'generic_ir_utility_page',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v50',
            'recipe_id', repair.recipe_id,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP,
            'migration', '0128_quarantine_residual_ir_utility_pages'
        )
    )
FROM
    residual_ir_utility_items AS repair
    CROSS JOIN residual_ir_utility_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: generic_ir_utility_page',
    normalized_feed_item_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM residual_ir_utility_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'raw_crawl_item_id', repair.raw_crawl_item_id,
        'recipe_id', repair.recipe_id,
        'url', repair.url,
        'canonical_url', repair.canonical_url,
        'title', repair.title,
        'published_at', repair.published_at,
        'reason', 'generic_ir_utility_page',
        'reversible', true,
        'policy', 'recipe-listing-artifact.v50',
        'repair_wave_event_id', wave.event_id,
        'migration', '0128_quarantine_residual_ir_utility_pages'
    )
FROM
    residual_ir_utility_items AS repair
    CROSS JOIN residual_ir_utility_wave AS wave;
