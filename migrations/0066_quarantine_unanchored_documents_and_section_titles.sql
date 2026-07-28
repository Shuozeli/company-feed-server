-- Reversibly remove two generic extraction artifacts:
--
-- 1. direct PDF links that older recipe crawls treated as both the stable
--    article identity and their own `official-listing-document.v1` fallback;
-- 2. an article page whose generic H1 section label ("Finance") shadowed its
--    independently declared article headline.
--
-- Document-backed releases remain supported when a listing card contains a
-- distinct HTML detail link and an exact same-title PDF.

CREATE TEMP TABLE unanchored_documents_and_section_titles
ON COMMIT DROP AS
WITH classified AS (
    SELECT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.canonical_url,
        item.title,
        item.published_at,
        CASE
            WHEN
                item.raw->>'extraction_contract' =
                    'official-listing-document.v1'
                AND public_url_identity_key(
                    item.raw->>'requested_article_url'
                ) = public_url_identity_key(item.raw->>'document_url')
            THEN 'document_without_distinct_article_identity'
            WHEN
                lower(btrim(regexp_replace(
                    item.title,
                    '[[:space:]]+',
                    ' ',
                    'g'
                ))) = 'finance'
                AND item.raw->>'title_source' = 'h1'
            THEN 'generic_section_title'
            ELSE NULL
        END AS reason
    FROM feed_items AS item
    WHERE
        NOT item.is_private
        AND item.source_kind IN ('html', 'browser')
)
SELECT *
FROM classified
WHERE reason IS NOT NULL;

CREATE TEMP TABLE unanchored_documents_and_section_titles_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.unanchored_document_and_section_title_repair_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM unanchored_documents_and_section_titles
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM unanchored_documents_and_section_titles
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM unanchored_documents_and_section_titles
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM unanchored_documents_and_section_titles
                        GROUP BY reason
                    ) AS counts
                ),
            'policy', 'recipe-listing-artifact.v22',
            'migration',
                '0066_quarantine_unanchored_documents_and_section_titles'
        )
    WHERE EXISTS (
        SELECT 1 FROM unanchored_documents_and_section_titles
    )
    RETURNING id
)
INSERT INTO unanchored_documents_and_section_titles_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v22',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    unanchored_documents_and_section_titles AS repair
    CROSS JOIN unanchored_documents_and_section_titles_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM unanchored_documents_and_section_titles AS repair
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
        'policy', 'recipe-listing-artifact.v22',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0066_quarantine_unanchored_documents_and_section_titles'
    )
FROM
    unanchored_documents_and_section_titles AS repair
    CROSS JOIN unanchored_documents_and_section_titles_wave AS wave;
