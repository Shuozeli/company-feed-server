-- Quarantine historical HTML/browser rows that are market quote/profile
-- utilities rather than individual editorial articles. The classifier
-- requires both a bounded quote-profile URL namespace and a stock/share
-- price-or-quote title, so ordinary market reporting remains eligible.
--
-- The quarantine is reversible if a later crawl extracts a substantive
-- editorial article from the same canonical URL.

CREATE TEMP TABLE market_quote_profile_items
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
            normalized.normalized_path
                ~ '(^|/)(equities|quote|quotes)(/|$)'
            OR normalized.normalized_path
                ~ '/(investing/stock|market-activity/stocks|markets/stocks)(/|$)'
        )
        AND (
            normalized.normalized_title
                ~ '(^|[[:space:][:punct:]])(stock|share)[[:space:]]+quote([[:space:][:punct:]]|$)'
            OR (
                normalized.normalized_title
                    ~ '(^|[[:space:][:punct:]])(stock|share)[[:space:]]+price([[:space:][:punct:]]|$)'
                AND normalized.normalized_title
                    ~ '(^|[[:space:][:punct:]])(chart|finance|history|live|market[[:space:]]+cap|quote|real[ -]time|today|lse|nasdaq|nyse)([[:space:][:punct:]]|$)'
            )
        )
    ORDER BY item.id
)
SELECT * FROM classified;

CREATE TEMP TABLE market_quote_profile_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.market_quote_profile_backfill_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM market_quote_profile_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM market_quote_profile_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM market_quote_profile_items
                ),
            'policy', 'market-quote-profile.v1',
            'migration', '0130_quarantine_market_quote_profiles'
        )
    WHERE EXISTS (SELECT 1 FROM market_quote_profile_items)
    RETURNING id
)
INSERT INTO market_quote_profile_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'market_quote_profile',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'market-quote-profile.v1',
            'recipe_id', repair.recipe_id,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP,
            'migration', '0130_quarantine_market_quote_profiles'
        )
    )
FROM
    market_quote_profile_items AS repair
    CROSS JOIN market_quote_profile_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: market_quote_profile',
    normalized_feed_item_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM market_quote_profile_items AS repair
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
        'reason', 'market_quote_profile',
        'reversible', true,
        'policy', 'market-quote-profile.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0130_quarantine_market_quote_profiles'
    )
FROM
    market_quote_profile_items AS repair
    CROSS JOIN market_quote_profile_wave AS wave;
