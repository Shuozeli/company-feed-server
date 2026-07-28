-- Revalidate active recipe output against three deterministic shared rules:
-- one sanitized body cannot represent multiple differently titled articles,
-- shallow short-title pages dominated by repeated "Read More" cards are
-- collections, and static advertising/prime-rate pages are reference utilities.

CREATE TEMP TABLE repeated_content_items
ON COMMIT DROP AS
WITH eligible_recipes AS (
    SELECT
        recipe.id AS recipe_id,
        recipe.company_id,
        recipe.source_id
    FROM
        company_news_recipes AS recipe
        LEFT JOIN company_news_recipe_state AS state
            ON state.recipe_id = recipe.id
    WHERE
        recipe.status = 'active'
        AND NOT COALESCE(state.rebuild_required, false)
),
repeated_groups AS (
    SELECT
        item.source_id,
        md5(btrim(COALESCE(
            NULLIF(item.body_text, ''),
            NULLIF(item.summary, ''),
            item.title
        ))) AS body_hash
    FROM
        feed_items AS item
        JOIN eligible_recipes AS recipe
            ON recipe.source_id = item.source_id
    WHERE NOT item.is_private
    GROUP BY
        item.source_id,
        md5(btrim(COALESCE(
            NULLIF(item.body_text, ''),
            NULLIF(item.summary, ''),
            item.title
        )))
    HAVING
        count(*) > 1
        AND count(DISTINCT lower(btrim(item.title))) > 1
)
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    recipe.recipe_id,
    item.url,
    item.canonical_url,
    item.title,
    item.published_at,
    'repeated_sanitized_content'::text AS reason,
    'recipe-content-diversity.v2'::text AS policy
FROM
    feed_items AS item
    JOIN eligible_recipes AS recipe
        ON recipe.source_id = item.source_id
    JOIN repeated_groups AS repeated
        ON repeated.source_id = item.source_id
        AND repeated.body_hash = md5(btrim(COALESCE(
            NULLIF(item.body_text, ''),
            NULLIF(item.summary, ''),
            item.title
        )))
WHERE NOT item.is_private;

CREATE TEMP TABLE repeated_cta_collection_items
ON COMMIT DROP AS
WITH eligible_recipes AS (
    SELECT
        recipe.id AS recipe_id,
        recipe.company_id,
        recipe.source_id
    FROM
        company_news_recipes AS recipe
        LEFT JOIN company_news_recipe_state AS state
            ON state.recipe_id = recipe.id
    WHERE
        recipe.status = 'active'
        AND NOT COALESCE(state.rebuild_required, false)
)
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    recipe.recipe_id,
    item.url,
    item.canonical_url,
    item.title,
    item.published_at,
    'repeated_read_more_collection'::text AS reason,
    'recipe-listing-artifact.v37'::text AS policy
FROM
    feed_items AS item
    JOIN eligible_recipes AS recipe
        ON recipe.source_id = item.source_id
WHERE
    NOT item.is_private
    AND NOT EXISTS (
        SELECT 1
        FROM repeated_content_items AS repeated
        WHERE repeated.feed_item_id = item.id
    )
    AND cardinality(regexp_split_to_array(
        btrim(item.title),
        '[[:space:]]+'
    )) <= 6
    AND item.title !~ '[[:digit:]]'
    AND lower(regexp_replace(
        split_part(split_part(item.url, '?', 1), '#', 1),
        '/$',
        ''
    )) ~ '/(announcement|announcements|article|articles|blog|blog-post|blog-posts|blogs|changelog|changelogs|company-news|engineering|insights|in-the-news|investor-news|journal|latest-news|market-announcements|media-center|media-centre|media-room|news|news-and-events|news-events|news-release|news-releases|newsroom|press|press-center|press-centre|press-release|press-releases|pressreleases|pressroom|post|posts|research|stories|updates|what-s-new|whats-new)/[^/]+$'
    AND item.raw ->> 'article_body_selector'
        = 'generic:paragraph-cluster.v1'
    AND COALESCE(
        (item.raw ->> 'article_element_count')::integer,
        0
    ) = 0
    AND COALESCE(
        (item.raw ->> 'article_elements_with_h1')::integer,
        0
    ) = 0
    AND COALESCE(
        (item.content_processing ->> 'link_count')::integer,
        0
    ) >= 8
    AND (
        length(lower(item.body_text))
        - length(replace(lower(item.body_text), 'read more', ''))
    ) / length('read more') >= 7;

CREATE TEMP TABLE static_reference_utility_items
ON COMMIT DROP AS
WITH eligible_recipes AS (
    SELECT
        recipe.id AS recipe_id,
        recipe.company_id,
        recipe.source_id
    FROM
        company_news_recipes AS recipe
        LEFT JOIN company_news_recipe_state AS state
            ON state.recipe_id = recipe.id
    WHERE
        recipe.status = 'active'
        AND NOT COALESCE(state.rebuild_required, false)
)
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    recipe.recipe_id,
    item.url,
    item.canonical_url,
    item.title,
    item.published_at,
    'non_editorial_static_reference_utility'::text AS reason,
    'recipe-listing-artifact.v38'::text AS policy
FROM
    feed_items AS item
    JOIN eligible_recipes AS recipe
        ON recipe.source_id = item.source_id
WHERE
    NOT item.is_private
    AND NOT EXISTS (
        SELECT 1
        FROM repeated_content_items AS repeated
        WHERE repeated.feed_item_id = item.id
    )
    AND NOT EXISTS (
        SELECT 1
        FROM repeated_cta_collection_items AS collection
        WHERE collection.feed_item_id = item.id
    )
    AND lower(regexp_replace(
        split_part(split_part(item.url, '?', 1), '#', 1),
        '/$',
        ''
    )) ~ '/(advertising-practices|prime-rate-information)(\.(aspx|html|asp|htm|php))?$';

CREATE TEMP TABLE recipe_quality_repair_items
ON COMMIT DROP AS
SELECT * FROM repeated_content_items
UNION ALL
SELECT * FROM repeated_cta_collection_items
UNION ALL
SELECT * FROM static_reference_utility_items;

CREATE TEMP TABLE recipe_quality_repair_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.recipe_quality_revalidation_backfill_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM recipe_quality_repair_items),
            'recipe_count',
                (
                    SELECT count(DISTINCT recipe_id)
                    FROM recipe_quality_repair_items
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM recipe_quality_repair_items
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM recipe_quality_repair_items
                        GROUP BY reason
                    ) AS counts
                ),
            'migration',
                '0106_quarantine_repeated_content_and_reference_utilities'
        )
    WHERE EXISTS (SELECT 1 FROM recipe_quality_repair_items)
    RETURNING id
)
INSERT INTO recipe_quality_repair_wave (event_id)
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
            'policy', repair.policy,
            'recipe_id', repair.recipe_id,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    recipe_quality_repair_items AS repair
    CROSS JOIN recipe_quality_repair_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM recipe_quality_repair_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

CREATE TEMP TABLE recipe_quality_revalidation_targets
ON COMMIT DROP AS
SELECT
    repair.recipe_id,
    repair.company_id,
    repair.source_id,
    count(*) AS item_count,
    array_agg(DISTINCT repair.reason ORDER BY repair.reason) AS reasons
FROM recipe_quality_repair_items AS repair
GROUP BY
    repair.recipe_id,
    repair.company_id,
    repair.source_id;

INSERT INTO company_news_recipe_state (
    recipe_id,
    consecutive_correctness_failures,
    freshness_status,
    correctness_status,
    rebuild_required,
    reason,
    metadata
)
SELECT
    target.recipe_id,
    1,
    'unknown',
    'failing',
    false,
    'quality_revalidation_required',
    jsonb_build_object(
        'quality_revalidation',
        jsonb_build_object(
            'item_count', target.item_count,
            'reasons', to_jsonb(target.reasons),
            'repair_wave_event_id', wave.event_id,
            'migration',
                '0106_quarantine_repeated_content_and_reference_utilities'
        )
    )
FROM
    recipe_quality_revalidation_targets AS target
    CROSS JOIN recipe_quality_repair_wave AS wave
ON CONFLICT (recipe_id) DO UPDATE
SET
    consecutive_correctness_failures = GREATEST(
        company_news_recipe_state.consecutive_correctness_failures,
        1
    ),
    correctness_status = 'failing',
    rebuild_required = false,
    reason = 'quality_revalidation_required',
    metadata = company_news_recipe_state.metadata || EXCLUDED.metadata,
    updated_at = CURRENT_TIMESTAMP;

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
        'reason', repair.reason,
        'reversible', true,
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0106_quarantine_repeated_content_and_reference_utilities'
    )
FROM
    recipe_quality_repair_items AS repair
    CROSS JOIN recipe_quality_repair_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_quality_revalidation_required',
    target.company_id,
    target.source_id,
    jsonb_build_object(
        'recipe_id', target.recipe_id,
        'item_count', target.item_count,
        'reasons', to_jsonb(target.reasons),
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0106_quarantine_repeated_content_and_reference_utilities'
    )
FROM
    recipe_quality_revalidation_targets AS target
    CROSS JOIN recipe_quality_repair_wave AS wave;
