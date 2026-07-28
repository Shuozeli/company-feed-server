-- Some CMS listing pages wrap their whole card grid in one `<article>`, which
-- can combine page-level article metadata with a listing-anchor title and look
-- like a detail page. The runtime crawler now rejects the bounded structural
-- signature: a shallow editorial page, at most one article wrapper, no article
-- heading, a small wrapper body, many links, and four or more "Read article"
-- cards. Quarantine already-normalized HTML items with the same high-confidence
-- signature.

CREATE TEMP TABLE wrapped_article_card_collections
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at
FROM
    feed_items AS item
    JOIN sources AS source ON source.id = item.source_id
WHERE
    NOT item.is_private
    AND source.kind = 'html'
    AND item.raw->>'title_source' = 'listing_anchor'
    AND COALESCE((item.raw->>'article_element_count')::integer, 0) <= 1
    AND COALESCE(
        (item.raw->>'article_elements_with_h1')::integer,
        0
    ) = 0
    AND COALESCE(
        (item.raw->>'max_article_content_chars')::integer,
        0
    ) < 1000
    AND COALESCE(
        (item.raw->>'sanitized_content_chars')::integer,
        0
    ) < 2000
    AND COALESCE(
        (item.content_processing->>'link_count')::integer,
        0
    ) >= 15
    AND (
        length(lower(item.body_text))
        - length(replace(lower(item.body_text), 'read article', ''))
    ) / length('read article') >= 4;

CREATE TEMP TABLE wrapped_article_card_collection_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.wrapped_article_card_collection_repair_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM wrapped_article_card_collections
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM wrapped_article_card_collections
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM wrapped_article_card_collections
                ),
            'policy', 'recipe-listing-artifact.v33',
            'migration',
                '0089_quarantine_wrapped_article_card_collections'
        )
    WHERE EXISTS (
        SELECT 1 FROM wrapped_article_card_collections
    )
    RETURNING id
)
INSERT INTO wrapped_article_card_collection_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'multiple_article_collection',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v33',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    wrapped_article_card_collections AS repair
    CROSS JOIN wrapped_article_card_collection_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: multiple_article_collection',
    normalized_feed_item_id = NULL
FROM wrapped_article_card_collections AS repair
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
        'reason', 'multiple_article_collection',
        'reversible', true,
        'policy', 'recipe-listing-artifact.v33',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0089_quarantine_wrapped_article_card_collections'
    )
FROM
    wrapped_article_card_collections AS repair
    CROSS JOIN wrapped_article_card_collection_wave AS wave;
