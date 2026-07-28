-- Reversibly remove generic collection pages that older recipe crawls
-- normalized as articles:
--   * taxonomy, locale, and pagination query variants;
--   * breadcrumb-prefixed media-library leaves;
--   * bounded taxonomy pages whose titles expose aggregate item counts;
--   * static governance/privacy destinations and empty CMS detail IDs.
--
-- The crawler now keeps only bounded resource-identifying query fields,
-- rejects these collection shapes after independent page inspection, and
-- retains the original rows for audit and later healthy recrawl release.

CREATE TEMP TABLE query_and_media_collection_artifacts ON COMMIT DROP AS
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
        CASE
            WHEN
                item.content_processing->>'link_count' ~ '^[0-9]+$'
            THEN (item.content_processing->>'link_count')::bigint
            ELSE 0
        END AS link_count
    FROM feed_items AS item
    WHERE
        NOT item.is_private
        AND item.source_kind IN ('html', 'browser')
),
classified AS (
    SELECT
        base.*,
        CASE
            WHEN base.canonical_url ~* '[?&]category=[^&#]*'
            THEN 'taxonomy_filter_query'
            WHEN
                base.normalized_title LIKE 'newsroom media %'
                AND base.url_without_query ~*
                    '/newsroom/media/[^/]+/?$'
            THEN 'breadcrumb_media_collection'
            WHEN
                base.url_without_query ~*
                    '/(committees-board|donnees-personnelles)/?$'
            THEN 'static_terminal_path'
            WHEN
                base.url_without_query ~*
                    '/(news|newsroom|press|blog)/[a-z]{2}([-_][a-z]{2})?/$'
                AND (
                    base.canonical_url ~*
                        '[?&](lang|language|language_id|locale)='
                    OR base.link_count >= 20
                    OR base.normalized_title ~
                        '(blog|newsroom|press|press room|presse|espace presse)$'
                )
            THEN 'localized_publication_root'
            WHEN
                base.url_without_query ~*
                    '/(news|newsroom|press|blog)/[^/]+/?$'
                AND base.normalized_title ~
                    '\(([0-9]{1,3}|1000)\)$'
                AND array_length(
                    regexp_split_to_array(
                        base.normalized_title,
                        '[[:space:]]+'
                    ),
                    1
                ) <= 6
                AND base.link_count >= 100
            THEN 'counted_taxonomy_collection'
            WHEN
                base.url_without_query ~* '/(recent-posts?|posts?)/?$'
                AND base.canonical_url ~* '[?&]page=[0-9]+'
            THEN 'pagination_query'
            WHEN
                base.canonical_url ~*
                    '[?&](id|cid|content|content_id|contentid|news_id|newsid|nid|p|post|post_id|postid)=(&|#|$)'
            THEN 'empty_resource_query'
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

CREATE TEMP TABLE query_and_media_collection_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.query_and_media_collection_repair_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM query_and_media_collection_artifacts),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM query_and_media_collection_artifacts
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM query_and_media_collection_artifacts
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM query_and_media_collection_artifacts
                        GROUP BY reason
                    ) AS counts
                ),
            'policy', 'recipe-listing-artifact.v15',
            'migration',
                '0059_quarantine_query_and_media_collection_artifacts'
        )
    WHERE EXISTS (SELECT 1 FROM query_and_media_collection_artifacts)
    RETURNING id
)
INSERT INTO query_and_media_collection_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v15',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    query_and_media_collection_artifacts AS repair
    CROSS JOIN query_and_media_collection_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM query_and_media_collection_artifacts AS repair
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
        'policy', 'recipe-listing-artifact.v15',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0059_quarantine_query_and_media_collection_artifacts'
    )
FROM
    query_and_media_collection_artifacts AS repair
    CROSS JOIN query_and_media_collection_wave AS wave;
