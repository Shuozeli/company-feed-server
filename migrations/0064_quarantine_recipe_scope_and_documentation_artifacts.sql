-- Retire HTML recipes that are rooted at a documentation/help detail page,
-- and support-host recipes whose fetched articles escaped their explicit path
-- boundary after a redirect or canonical rewrite. The runtime crawler now
-- checks both final and canonical article URLs against the effective recipe
-- scope, while recipe admission still permits explicit editorial listings on
-- docs/help hosts and named help-center release sections.

CREATE TEMP TABLE invalid_documentation_and_scope_recipes
ON COMMIT DROP AS
WITH base AS (
    SELECT
        recipe.id AS recipe_id,
        recipe.company_id,
        recipe.source_id,
        recipe.spec,
        recipe.spec->>'publication_url' AS publication_url,
        split_part(
            lower(split_part(recipe.spec->>'publication_url', '://', 2)),
            '/',
            1
        ) AS publication_host,
        regexp_split_to_array(
            trim(
                BOTH '/'
                FROM lower(
                    regexp_replace(
                        split_part(
                            split_part(
                                recipe.spec->>'publication_url',
                                '?',
                                1
                            ),
                            '#',
                            1
                        ),
                        '^https?://[^/]+',
                        ''
                    )
                )
            ),
            '/'
        ) AS path_segments
    FROM company_news_recipes AS recipe
    WHERE recipe.status = 'active'
),
labeled AS (
    SELECT
        base.*,
        split_part(
            regexp_replace(base.publication_host, '^www\.', ''),
            '.',
            1
        ) IN ('help', 'helpcenter', 'knowledgebase', 'support')
            AS is_help_host,
        base.path_segments && ARRAY[
            'blog',
            'blogs',
            'changelog',
            'changelogs',
            'company-news',
            'engineering',
            'insights',
            'news',
            'newsroom',
            'press',
            'press-releases',
            'product-updates',
            'release-notes',
            'releases',
            'research',
            'stories',
            'updates',
            'what-s-new',
            'whats-new'
        ] AS has_editorial_listing,
        base.path_segments && ARRAY[
            'api-reference',
            'docs',
            'documentation',
            'knowledge-base',
            'reference',
            'references'
        ] AS has_documentation_scope
    FROM base
),
classified AS (
    SELECT
        labeled.*,
        CASE
            WHEN
                labeled.is_help_host
                AND labeled.path_segments && ARRAY['article', 'articles']
            THEN 'non_editorial_help_article_scope'
            WHEN
                NOT labeled.has_editorial_listing
                AND labeled.has_documentation_scope
            THEN 'non_editorial_documentation_scope'
            WHEN
                labeled.is_help_host
                AND NOT labeled.has_editorial_listing
                AND NOT (
                    labeled.path_segments && ARRAY['section', 'sections']
                    AND EXISTS (
                        SELECT 1
                        FROM unnest(labeled.path_segments) AS path(segment)
                        WHERE
                            path.segment LIKE '%changelog%'
                            OR path.segment LIKE '%news%'
                            OR path.segment LIKE '%release%'
                            OR path.segment LIKE '%update%'
                    )
                )
            THEN 'non_editorial_help_scope'
            WHEN
                labeled.is_help_host
                AND jsonb_array_length(
                    COALESCE(
                        labeled.spec->'include_path_prefixes',
                        '[]'::jsonb
                    )
                ) > 0
                AND jsonb_array_length(
                    COALESCE(
                        labeled.spec->'evidence_article_urls',
                        '[]'::jsonb
                    )
                ) = 0
                AND EXISTS (
                    SELECT 1
                    FROM feed_items AS item
                    WHERE
                        item.source_id = labeled.source_id
                        AND NOT item.is_private
                        AND NOT EXISTS (
                            SELECT 1
                            FROM jsonb_array_elements_text(
                                labeled.spec->'include_path_prefixes'
                            ) AS allowed(prefix)
                            WHERE
                                regexp_replace(
                                    split_part(
                                        split_part(
                                            item.canonical_url,
                                            '?',
                                            1
                                        ),
                                        '#',
                                        1
                                    ),
                                    '^https?://[^/]+',
                                    ''
                                ) LIKE allowed.prefix || '%'
                        )
                )
            THEN 'article_outside_recipe_scope'
            ELSE NULL
        END AS reason
    FROM labeled
)
SELECT
    classified.recipe_id,
    classified.company_id,
    classified.source_id,
    classified.publication_url,
    classified.spec,
    classified.reason
FROM classified
WHERE classified.reason IS NOT NULL;

CREATE TEMP TABLE invalid_documentation_and_scope_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at,
    repair.reason
FROM invalid_documentation_and_scope_recipes AS repair
JOIN feed_items AS item ON item.source_id = repair.source_id
WHERE
    NOT item.is_private
    AND (
        repair.reason <> 'article_outside_recipe_scope'
        OR NOT EXISTS (
            SELECT 1
            FROM jsonb_array_elements_text(
                repair.spec->'include_path_prefixes'
            ) AS allowed(prefix)
            WHERE
                regexp_replace(
                    split_part(
                        split_part(item.canonical_url, '?', 1),
                        '#',
                        1
                    ),
                    '^https?://[^/]+',
                    ''
                ) LIKE allowed.prefix || '%'
        )
    );

CREATE TEMP TABLE invalid_documentation_and_scope_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.documentation_and_scope_repair_started',
        jsonb_build_object(
            'recipe_count',
                (
                    SELECT count(*)
                    FROM invalid_documentation_and_scope_recipes
                ),
            'item_count',
                (
                    SELECT count(*)
                    FROM invalid_documentation_and_scope_items
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM invalid_documentation_and_scope_recipes
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, recipe_count)
                    FROM (
                        SELECT reason, count(*) AS recipe_count
                        FROM invalid_documentation_and_scope_recipes
                        GROUP BY reason
                    ) AS counts
                ),
            'policy', 'recipe-listing-artifact.v20',
            'migration',
                '0064_quarantine_recipe_scope_and_documentation_artifacts'
        )
    WHERE EXISTS (
        SELECT 1 FROM invalid_documentation_and_scope_recipes
    )
    RETURNING id
)
INSERT INTO invalid_documentation_and_scope_wave (event_id)
SELECT id FROM repair_started;

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = repair.reason
FROM invalid_documentation_and_scope_recipes AS repair
WHERE recipe.id = repair.recipe_id;

UPDATE company_news_recipe_state AS state
SET
    correctness_status = 'failing',
    rebuild_required = true,
    consecutive_correctness_failures = GREATEST(
        state.consecutive_correctness_failures,
        3
    ),
    reason = repair.reason,
    metadata = state.metadata || jsonb_build_object(
        'documentation_and_scope_repair',
        jsonb_build_object(
            'policy', 'recipe-listing-artifact.v20',
            'repair_wave_event_id', wave.event_id,
            'repaired_at', CURRENT_TIMESTAMP
        )
    )
FROM
    invalid_documentation_and_scope_recipes AS repair
    CROSS JOIN invalid_documentation_and_scope_wave AS wave
WHERE state.recipe_id = repair.recipe_id;

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
            'policy', 'recipe-listing-artifact.v20',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    invalid_documentation_and_scope_items AS repair
    CROSS JOIN invalid_documentation_and_scope_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM invalid_documentation_and_scope_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error =
        'cancelled: recipe staled by documentation/scope correctness repair'
FROM invalid_documentation_and_scope_recipes AS repair
WHERE
    job.source_id = repair.source_id
    AND job.job_type = 'crawl_source'
    AND job.status = 'pending';

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_stale',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'recipe_id', repair.recipe_id,
        'publication_url', repair.publication_url,
        'reason', repair.reason,
        'rebuild_required', true,
        'policy', 'recipe-listing-artifact.v20',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0064_quarantine_recipe_scope_and_documentation_artifacts'
    )
FROM
    invalid_documentation_and_scope_recipes AS repair
    CROSS JOIN invalid_documentation_and_scope_wave AS wave;

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
        'policy', 'recipe-listing-artifact.v20',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0064_quarantine_recipe_scope_and_documentation_artifacts'
    )
FROM
    invalid_documentation_and_scope_items AS repair
    CROSS JOIN invalid_documentation_and_scope_wave AS wave;
