-- Reversibly remove short section, category, utility, and branded-publication
-- headings that older generic HTML crawls emitted as article titles.
--
-- The crawler now treats these labels as generic. On a real detail page this
-- lets a substantive OpenGraph, document-title, or semantic-heading headline
-- win; on a collection page the candidate remains rejected.

CREATE TEMP TABLE generic_section_and_category_titles
ON COMMIT DROP AS
WITH base AS (
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
        ))) AS normalized_title
    FROM feed_items AS item
    WHERE
        NOT item.is_private
        AND item.source_kind IN ('html', 'browser')
),
classified AS (
    SELECT
        base.*,
        CASE
            WHEN base.normalized_title = ANY (ARRAY[
            '404 error',
            '404 page not found',
            'about us',
            'archives',
            'bra talk',
            'brochure',
            'calendar',
            'clientes',
            'coming soon',
            'corporate',
            'customer',
            'dashboard',
            'dividends',
            'downloads',
            'earnings',
            'ecommerce',
            'embedded',
            'esg news',
            'facebook',
            'features',
            'footwear',
            'heritage',
            'images',
            'images .',
            'industry',
            'insights',
            'investor',
            'latest articles',
            'lighting',
            'newer posts',
            'news releases',
            'older posts',
            'overview',
            'page not found',
            'previous posts',
            'producto',
            'results',
            'results:',
            'see more',
            'shipping',
            'shoptalk',
            'subscribe',
            'tax forms',
            'templates',
            'vaccines',
            'webinars'
            ])
            THEN 'generic_section_or_category_title'
            WHEN base.normalized_title ~
                '^([[:alpha:]][[:space:]]){4,}[[:alpha:]]$'
            THEN 'letter_spaced_title'
            WHEN base.normalized_title ~
                '^(january|february|march|april|may|june|july|august|september|october|november|december)( [0-9]{1,2},)? [0-9]{4}$'
            THEN 'date_only_title'
            WHEN base.normalized_title ~
                '^day: [0-9]{1,2} (january|february|march|april|may|june|july|august|september|october|november|december) [0-9]{4}$'
            THEN 'day_archive_title'
            WHEN
                array_length(
                    regexp_split_to_array(
                        base.normalized_title,
                        '[[:space:]]+'
                    ),
                    1
                ) <= 3
                AND (
                    left(base.normalized_title, 1) = '|'
                    OR right(base.normalized_title, 1) = '|'
                )
            THEN 'incomplete_separator_title'
            ELSE NULL
        END AS reason
    FROM base
)
SELECT
    feed_item_id,
    raw_crawl_item_id,
    company_id,
    source_id,
    canonical_url,
    title,
    published_at,
    reason
FROM classified
WHERE reason IS NOT NULL;

CREATE TEMP TABLE generic_section_and_category_titles_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.generic_section_and_category_title_repair_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM generic_section_and_category_titles
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM generic_section_and_category_titles
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM generic_section_and_category_titles
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM generic_section_and_category_titles
                        GROUP BY reason
                    ) AS counts
                ),
            'policy', 'recipe-listing-artifact.v23',
            'migration',
                '0067_quarantine_generic_section_and_category_titles'
        )
    WHERE EXISTS (
        SELECT 1 FROM generic_section_and_category_titles
    )
    RETURNING id
)
INSERT INTO generic_section_and_category_titles_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v23',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    generic_section_and_category_titles AS repair
    CROSS JOIN generic_section_and_category_titles_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM generic_section_and_category_titles AS repair
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
        'policy', 'recipe-listing-artifact.v23',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0067_quarantine_generic_section_and_category_titles'
    )
FROM
    generic_section_and_category_titles AS repair
    CROSS JOIN generic_section_and_category_titles_wave AS wave;
