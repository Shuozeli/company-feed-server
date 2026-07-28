-- Quarantine obvious navigation labels and short taxonomy/card-grid pages that
-- were accepted because their templates emitted misleading page-level Article
-- metadata. The shared item policy now rejects the same navigation titles for
-- RSS/Atom and HTML ingestion, while the HTML crawler additionally requires a
-- short slug-matched page with many article cards to have an individual body.

CREATE TEMP TABLE navigation_and_multi_article_items
ON COMMIT DROP AS
WITH normalized AS (
    SELECT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.canonical_url,
        item.title,
        item.published_at,
        source.kind AS source_kind,
        lower(btrim(regexp_replace(
            item.title,
            '[[:space:]]+',
            ' ',
            'g'
        ))) AS normalized_title,
        cardinality(regexp_split_to_array(
            btrim(item.title),
            '[[:space:]]+'
        )) AS title_word_count,
        NULLIF(
            item.raw->>'article_element_count',
            ''
        )::integer AS article_element_count,
        NULLIF(
            item.raw->>'sanitized_content_chars',
            ''
        )::integer AS sanitized_content_chars,
        NULLIF(
            item.content_processing->>'link_count',
            ''
        )::integer AS link_count,
        regexp_replace(
            split_part(
                split_part(item.canonical_url, '?', 1),
                '#',
                1
            ),
            '^.*/([^/]+)/?$',
            '\1'
        ) AS terminal_segment
    FROM
        feed_items AS item
        JOIN sources AS source ON source.id = item.source_id
    WHERE NOT item.is_private
),
keyed AS (
    SELECT
        normalized.*,
        regexp_replace(
            regexp_replace(
                lower(replace(normalized.title, '&', ' ')),
                '(^|[^a-z0-9])and([^a-z0-9]|$)',
                '\1\2',
                'g'
            ),
            '[^a-z0-9]+',
            '',
            'g'
        ) AS title_key,
        regexp_replace(
            regexp_replace(
                lower(replace(normalized.terminal_segment, '&', ' ')),
                '(^|[^a-z0-9])and([^a-z0-9]|$)',
                '\1\2',
                'g'
            ),
            '[^a-z0-9]+',
            '',
            'g'
        ) AS terminal_key
    FROM normalized
),
classified AS (
    SELECT
        keyed.*,
        CASE
            WHEN
                keyed.normalized_title = ANY (ARRAY[
                    'blog post',
                    'corporate',
                    'homepage',
                    'key ratio',
                    'linkedin',
                    'next page',
                    'no title',
                    'previous',
                    'qwe',
                    'resources',
                    'rss feeds',
                    'sec test',
                    'test'
                ])
                OR (
                    keyed.title_word_count <= 4
                    AND keyed.normalized_title LIKE 'about %'
                    AND keyed.normalized_title !~ '[0-9]'
                )
            THEN 'generic_navigation_title'
            WHEN
                keyed.source_kind = 'html'
                AND lower(keyed.canonical_url) ~
                    '/(filter-blog-|category[.-])'
            THEN 'explicit_taxonomy_path'
            WHEN
                keyed.source_kind = 'html'
                AND COALESCE(keyed.article_element_count, 0) >= 4
                AND keyed.title_word_count <= 3
                AND keyed.normalized_title !~ '[0-9]'
                AND keyed.title_key <> ''
                AND keyed.title_key = keyed.terminal_key
                AND (
                    COALESCE(keyed.sanitized_content_chars, 0) < 500
                    OR (
                        keyed.article_element_count >= 10
                        AND COALESCE(keyed.link_count, 0) >= 20
                    )
                )
            THEN 'short_multi_article_collection'
            ELSE NULL
        END AS reason
    FROM keyed
)
SELECT
    classified.feed_item_id,
    classified.raw_crawl_item_id,
    classified.company_id,
    classified.source_id,
    classified.canonical_url,
    classified.title,
    classified.published_at,
    classified.reason
FROM classified
WHERE classified.reason IS NOT NULL;

CREATE TEMP TABLE navigation_and_multi_article_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.navigation_and_multi_article_repair_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM navigation_and_multi_article_items
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM navigation_and_multi_article_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM navigation_and_multi_article_items
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM navigation_and_multi_article_items
                        GROUP BY reason
                    ) AS counts
                ),
            'policy', 'recipe-listing-artifact.v25',
            'migration',
                '0074_quarantine_navigation_and_multi_article_collections'
        )
    WHERE EXISTS (
        SELECT 1 FROM navigation_and_multi_article_items
    )
    RETURNING id
)
INSERT INTO navigation_and_multi_article_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v25',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    navigation_and_multi_article_items AS repair
    CROSS JOIN navigation_and_multi_article_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM navigation_and_multi_article_items AS repair
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
        'policy', 'recipe-listing-artifact.v25',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0074_quarantine_navigation_and_multi_article_collections'
    )
FROM
    navigation_and_multi_article_items AS repair
    CROSS JOIN navigation_and_multi_article_wave AS wave;
