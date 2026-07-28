-- A year/month archive is a valid source of evidence but not a durable
-- publication recipe: it stops discovering new stories when the calendar
-- advances. The builder now normalizes those adapter URLs to a stable blog or
-- newsroom root. Marketing role and use-case pages likewise expose curated
-- subsets of a real blog rather than independent publications. Supersede the
-- three existing active recipes matching these high-confidence shapes.

CREATE TEMP TABLE temporal_and_persona_recipes
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
        recipe.path ~ '/(19|20)[0-9]{2}/(0?[1-9]|1[0-2])/?$'
            AS has_temporal_archive_suffix,
        EXISTS (
            SELECT 1
            FROM regexp_split_to_table(
                trim(both '/' FROM recipe.path),
                '/'
            ) AS segment
            WHERE segment = ANY (
                ARRAY[
                    'role',
                    'roles',
                    'use-case',
                    'use-cases'
                ]
            )
        ) AS has_marketing_persona_parent,
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
        ) AS has_explicit_editorial_segment
    FROM recipe_path AS recipe
)
SELECT
    recipe_id,
    company_id,
    source_id,
    source_url,
    path,
    CASE
        WHEN has_temporal_archive_suffix
            THEN 'temporal_archive_publication'
        ELSE 'marketing_persona_without_editorial_collection'
    END AS reason
FROM classified
WHERE
    has_temporal_archive_suffix
    OR (
        has_marketing_persona_parent
        AND NOT has_explicit_editorial_segment
    );

UPDATE company_news_recipes AS recipe
SET
    status = 'superseded',
    stale_at = NULL,
    stale_reason = NULL
FROM temporal_and_persona_recipes AS invalid
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
            'policy', 'company-news-stable-publication-root.v1',
            'reason', invalid.reason,
            'source_url', invalid.source_url,
            'path', invalid.path,
            'migration',
                '0092_supersede_temporal_and_persona_recipes'
        )
    )
FROM temporal_and_persona_recipes AS invalid
WHERE state.recipe_id = invalid.recipe_id;

UPDATE sources AS source
SET metadata = source.metadata - 'active_recipe_id' - 'recipe_schema_version'
FROM temporal_and_persona_recipes AS invalid
WHERE
    source.id = invalid.source_id
    AND source.metadata ->> 'active_recipe_id' = invalid.recipe_id::text;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'unstable publication recipe superseded'
FROM temporal_and_persona_recipes AS invalid
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
        'policy', 'company-news-stable-publication-root.v1',
        'source_url', invalid.source_url,
        'path', invalid.path,
        'migration', '0092_supersede_temporal_and_persona_recipes'
    )
FROM temporal_and_persona_recipes AS invalid;
