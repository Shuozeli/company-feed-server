-- Quarantine high-confidence multi-card category pages accepted by templates
-- that stamp page-level Article metadata onto the collection. Shared
-- multi-company news hosts are excluded from this historical classifier
-- because their short release pages can legitimately carry many related cards.
-- The runtime now checks for a real primary article element directly.

CREATE TEMP TABLE high_confidence_card_grid_items
ON COMMIT DROP AS
WITH candidates AS (
    SELECT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.canonical_url,
        item.title,
        item.published_at,
        COALESCE(
            NULLIF(item.raw->>'article_element_count', '')::integer,
            0
        ) AS article_element_count,
        COALESCE(
            NULLIF(item.raw->>'sanitized_content_chars', '')::integer,
            999999
        ) AS sanitized_content_chars,
        cardinality(regexp_split_to_array(
            btrim(item.title),
            '[[:space:]]+'
        )) AS title_word_count,
        regexp_replace(
            regexp_replace(
                lower(item.canonical_url),
                '^https?://',
                ''
            ),
            '[:/].*$',
            ''
        ) AS canonical_host
    FROM
        feed_items AS item
        JOIN sources AS source ON source.id = item.source_id
    WHERE
        NOT item.is_private
        AND source.status = 'approved'
        AND source.kind = 'html'
)
SELECT
    candidates.feed_item_id,
    candidates.raw_crawl_item_id,
    candidates.company_id,
    candidates.source_id,
    candidates.canonical_url,
    candidates.title,
    candidates.published_at,
    'multi_card_collection_without_primary_article'::text AS reason
FROM candidates
WHERE
    candidates.article_element_count >= 10
    AND candidates.sanitized_content_chars < 1000
    AND candidates.title_word_count <= 6
    AND candidates.title !~ '[0-9]'
    AND candidates.canonical_host !~
        '(accessnewswire|barchart|benzinga|biospace|bloomberg|businesswire|einpresswire|finance\.yahoo|forbes|globenewswire|investing|marketbeat|marketscreener|marketwatch|msn|nasdaq|newsfilecorp|prnewswire|reuters|seekingalpha|stocktitan|tipranks|tradingview)';

CREATE TEMP TABLE high_confidence_card_grid_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.high_confidence_card_grid_repair_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM high_confidence_card_grid_items
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM high_confidence_card_grid_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM high_confidence_card_grid_items
                ),
            'policy', 'recipe-listing-artifact.v28',
            'migration',
                '0079_quarantine_high_confidence_card_grids'
        )
    WHERE EXISTS (
        SELECT 1 FROM high_confidence_card_grid_items
    )
    RETURNING id
)
INSERT INTO high_confidence_card_grid_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v28',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    high_confidence_card_grid_items AS repair
    CROSS JOIN high_confidence_card_grid_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM high_confidence_card_grid_items AS repair
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
        'policy', 'recipe-listing-artifact.v28',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0079_quarantine_high_confidence_card_grids'
    )
FROM
    high_confidence_card_grid_items AS repair
    CROSS JOIN high_confidence_card_grid_wave AS wave;
