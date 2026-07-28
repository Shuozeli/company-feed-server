-- "Engineering", "developer", and "media" are useful editorial markers at a
-- publication root, but they also occur on product, service, industry, and
-- career pages. The recipe builder now rejects commercial parent paths unless
-- they contain an explicit editorial collection segment. Organizational paths
-- without such a segment must prove that they expose a dated collection of at
-- least three distinct stories. Supersede existing active recipes that fail
-- the same bounded rule; their stored items remain reversible but disappear
-- from public output because the recipe is no longer active.

CREATE TEMP TABLE non_editorial_parent_recipes
ON COMMIT DROP AS
WITH recipe_path AS (
    SELECT
        recipe.id AS recipe_id,
        recipe.company_id,
        recipe.source_id,
        source.url AS source_url,
        lower(regexp_replace(
            COALESCE(recipe.spec->>'publication_url', source.url),
            '^https?://[^/]+',
            ''
        )) AS path
    FROM
        company_news_recipes AS recipe
        JOIN sources AS source ON source.id = recipe.source_id
        LEFT JOIN company_news_recipe_state AS state
            ON state.recipe_id = recipe.id
    WHERE
        recipe.status = 'active'
        AND NOT COALESCE(state.rebuild_required, false)
),
classified AS (
    SELECT
        recipe.*,
        EXISTS (
            SELECT 1
            FROM regexp_split_to_table(
                trim(both '/' FROM recipe.path),
                '/'
            ) AS segment
            WHERE
                regexp_replace(segment, '\.(html?|aspx)$', '') = ANY (
                    ARRAY[
                        'announcements',
                        'articles',
                        'blog',
                        'blogs',
                        'changelog',
                        'changelogs',
                        'company-news',
                        'insights',
                        'news',
                        'newsroom',
                        'press',
                        'press-release',
                        'press-releases',
                        'publications',
                        'release-notes',
                        'research',
                        'stories',
                        'updates',
                        'what-s-new',
                        'whats-new'
                    ]
                )
                OR regexp_replace(segment, '\.(html?|aspx)$', '')
                    ~ '[-_]blog$'
        ) AS has_explicit_editorial_segment,
        EXISTS (
            SELECT 1
            FROM regexp_split_to_table(
                trim(both '/' FROM recipe.path),
                '/'
            ) AS segment
            WHERE segment = ANY (
                ARRAY[
                    'capabilities',
                    'capability',
                    'expertise',
                    'industries',
                    'industry',
                    'our-services',
                    'product',
                    'products',
                    'service',
                    'services',
                    'solutions'
                ]
            )
        ) AS has_commercial_parent,
        EXISTS (
            SELECT 1
            FROM regexp_split_to_table(
                trim(both '/' FROM recipe.path),
                '/'
            ) AS segment
            WHERE segment = ANY (
                ARRAY[
                    'careers',
                    'departments',
                    'jobs',
                    'team',
                    'teams'
                ]
            )
        ) AS has_organizational_parent
    FROM recipe_path AS recipe
),
metrics AS (
    SELECT
        recipe.*,
        count(item.id) FILTER (WHERE NOT item.is_private) AS item_count,
        count(DISTINCT lower(btrim(regexp_replace(
            item.title,
            '[[:space:]]+',
            ' ',
            'g'
        )))) FILTER (WHERE NOT item.is_private) AS distinct_title_count,
        count(item.published_at) FILTER (WHERE NOT item.is_private)
            AS dated_item_count
    FROM
        classified AS recipe
        LEFT JOIN feed_items AS item ON item.source_id = recipe.source_id
    GROUP BY
        recipe.recipe_id,
        recipe.company_id,
        recipe.source_id,
        recipe.source_url,
        recipe.path,
        recipe.has_explicit_editorial_segment,
        recipe.has_commercial_parent,
        recipe.has_organizational_parent
)
SELECT
    recipe_id,
    company_id,
    source_id,
    source_url,
    path,
    item_count,
    distinct_title_count,
    dated_item_count,
    CASE
        WHEN has_commercial_parent
            THEN 'commercial_parent_without_editorial_collection'
        ELSE 'organizational_page_lacks_editorial_collection_evidence'
    END AS reason
FROM metrics
WHERE
    NOT has_explicit_editorial_segment
    AND (
        has_commercial_parent
        OR (
            has_organizational_parent
            AND (
                item_count < 3
                OR distinct_title_count < 3
                OR dated_item_count = 0
            )
        )
    );

UPDATE company_news_recipes AS recipe
SET
    status = 'superseded',
    stale_at = NULL,
    stale_reason = NULL
FROM non_editorial_parent_recipes AS invalid
WHERE
    recipe.id = invalid.recipe_id
    AND recipe.status = 'active';

UPDATE company_news_recipe_state AS state
SET
    rebuild_required = false,
    reason = invalid.reason,
    metadata = state.metadata || jsonb_build_object(
        'supersession',
        jsonb_build_object(
            'policy', 'company-news-editorial-parent-scope.v1',
            'reason', invalid.reason,
            'source_url', invalid.source_url,
            'path', invalid.path,
            'item_count', invalid.item_count,
            'distinct_title_count', invalid.distinct_title_count,
            'dated_item_count', invalid.dated_item_count,
            'migration',
                '0091_supersede_non_editorial_parent_recipes'
        )
    )
FROM non_editorial_parent_recipes AS invalid
WHERE state.recipe_id = invalid.recipe_id;

UPDATE sources AS source
SET metadata = source.metadata - 'active_recipe_id' - 'recipe_schema_version'
FROM non_editorial_parent_recipes AS invalid
WHERE
    source.id = invalid.source_id
    AND source.metadata ->> 'active_recipe_id' = invalid.recipe_id::text;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'non-editorial parent recipe superseded'
FROM non_editorial_parent_recipes AS invalid
WHERE
    job.job_type = 'crawl_source'
    AND job.status = 'pending'
    AND job.source_id = invalid.source_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_superseded',
    invalid.company_id,
    invalid.source_id,
    jsonb_build_object(
        'recipe_id', invalid.recipe_id,
        'reason', invalid.reason,
        'policy', 'company-news-editorial-parent-scope.v1',
        'source_url', invalid.source_url,
        'path', invalid.path,
        'item_count', invalid.item_count,
        'distinct_title_count', invalid.distinct_title_count,
        'dated_item_count', invalid.dated_item_count,
        'migration', '0091_supersede_non_editorial_parent_recipes'
    )
FROM non_editorial_parent_recipes AS invalid;
