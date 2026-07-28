-- Quarantine deterministic collection pages that older crawls retained as
-- articles: semantic terminal collection routes, generic section headings,
-- explicit filter/navigation bodies, and taxonomy archives whose heading
-- exactly matches the terminal slug.

CREATE TEMP TABLE residual_collection_items
ON COMMIT DROP AS
WITH base AS (
    SELECT
        item.*,
        lower(regexp_replace(
            split_part(split_part(
                COALESCE(NULLIF(item.canonical_url, ''), item.url),
                '?',
                1
            ), '#', 1),
            '/$',
            ''
        )) AS clean_url,
        lower(regexp_replace(
            btrim(item.title),
            '[[:space:]]+',
            ' ',
            'g'
        )) AS normalized_title
    FROM feed_items AS item
    WHERE NOT item.is_private
),
classified AS (
    SELECT
        base.*,
        CASE
            WHEN clean_url ~ '/(brand-articles|community-articles|company-articles|corporate-financial|insights-blog|latest-articles|media-and-news|research-library|west-in-the-news|world-events)(\.(aspx|html|asp|htm|php))?$'
            THEN 'non_editorial_terminal_collection'
            WHEN normalized_title IN (
                'corporate & financial',
                'corporate and financial',
                'engineering spotlight',
                'explore topics',
                'insights blog',
                'other group companies',
                'people & culture',
                'role spotlights',
                'team asana',
                'view our latest articles'
            )
            OR (
                normalized_title
                    ~ '^all [^|[:digit:]]+ articles( \| .+)?$'
                AND cardinality(regexp_split_to_array(
                    split_part(normalized_title, ' | ', 1),
                    '[[:space:]]+'
                )) <= 4
            )
            OR (
                btrim(normalized_title, '.') LIKE '% media coverage'
                AND cardinality(regexp_split_to_array(
                    btrim(normalized_title, '.'),
                    '[[:space:]]+'
                )) <= 4
            )
            THEN 'non_editorial_generic_collection_title'
            WHEN lower(base.body_text)
                    LIKE '%there is a lack of results to match selected filters%'
                AND lower(base.body_text)
                    LIKE '%please adjust the filter options to broaden results%'
            THEN 'non_editorial_filter_collection'
            WHEN lower(btrim(base.body_text))
                    LIKE 'news releases topics media contacts %'
                AND COALESCE(
                    (base.content_processing ->> 'link_count')::integer,
                    0
                ) >= 20
                AND COALESCE(
                    (base.raw ->> 'article_element_count')::integer,
                    0
                ) = 0
                AND cardinality(regexp_split_to_array(
                    normalized_title,
                    '[[:space:]]+'
                )) <= 6
            THEN 'non_editorial_navigation_collection'
            WHEN normalized_title ~ ' archives (\||-|–|—) '
                AND regexp_replace(
                    regexp_replace(
                        split_part(normalized_title, ' archives ', 1),
                        '[^a-z0-9]+',
                        '-',
                        'g'
                    ),
                    '(^-|-$)',
                    '',
                    'g'
                ) = regexp_replace(clean_url, '^.*/', '')
            THEN 'non_editorial_slug_matched_archive'
        END AS reason
    FROM base
),
repairable AS (
    SELECT
        classified.*,
        CASE reason
            WHEN 'non_editorial_terminal_collection'
            THEN 'recipe-listing-artifact.v41'
            WHEN 'non_editorial_generic_collection_title'
            THEN 'recipe-listing-artifact.v42'
            WHEN 'non_editorial_filter_collection'
            THEN 'recipe-listing-artifact.v43'
            WHEN 'non_editorial_navigation_collection'
            THEN 'recipe-listing-artifact.v44'
            ELSE 'recipe-listing-artifact.v45'
        END AS policy
    FROM classified
    WHERE reason IS NOT NULL
)
SELECT DISTINCT ON (repairable.id)
    repairable.id AS feed_item_id,
    repairable.raw_crawl_item_id,
    repairable.company_id,
    repairable.source_id,
    recipe.id AS recipe_id,
    repairable.url,
    repairable.canonical_url,
    repairable.title,
    repairable.published_at,
    repairable.reason,
    repairable.policy
FROM
    repairable
    LEFT JOIN LATERAL (
        SELECT candidate.id, candidate.status, candidate.created_at
        FROM company_news_recipes AS candidate
        WHERE candidate.source_id = repairable.source_id
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
ORDER BY repairable.id;

CREATE TEMP TABLE residual_collection_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.residual_collection_backfill_started',
        jsonb_build_object(
            'item_count', (SELECT count(*) FROM residual_collection_items),
            'recipe_count',
                (
                    SELECT count(DISTINCT recipe_id)
                    FROM residual_collection_items
                    WHERE recipe_id IS NOT NULL
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM residual_collection_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM residual_collection_items
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM residual_collection_items
                        GROUP BY reason
                    ) AS counts
                ),
            'migration',
                '0108_quarantine_residual_taxonomy_and_navigation_collections'
        )
    WHERE EXISTS (SELECT 1 FROM residual_collection_items)
    RETURNING id
)
INSERT INTO residual_collection_wave (event_id)
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
    residual_collection_items AS repair
    CROSS JOIN residual_collection_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM residual_collection_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

CREATE TEMP TABLE residual_collection_targets
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    count(*) AS item_count,
    array_agg(DISTINCT repair.reason ORDER BY repair.reason) AS reasons
FROM
    residual_collection_items AS repair
    JOIN company_news_recipes AS recipe
        ON recipe.source_id = repair.source_id
        AND recipe.status = 'active'
    LEFT JOIN company_news_recipe_state AS state
        ON state.recipe_id = recipe.id
WHERE NOT COALESCE(state.rebuild_required, false)
GROUP BY
    recipe.id,
    recipe.company_id,
    recipe.source_id;

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
                '0108_quarantine_residual_taxonomy_and_navigation_collections'
        )
    )
FROM
    residual_collection_targets AS target
    CROSS JOIN residual_collection_wave AS wave
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
            '0108_quarantine_residual_taxonomy_and_navigation_collections'
    )
FROM
    residual_collection_items AS repair
    CROSS JOIN residual_collection_wave AS wave;

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
            '0108_quarantine_residual_taxonomy_and_navigation_collections'
    )
FROM
    residual_collection_targets AS target
    CROSS JOIN residual_collection_wave AS wave;
