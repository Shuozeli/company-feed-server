-- A publication recipe must survive the calendar advancing and a base company
-- domain must not become editorial merely because its registered-domain label
-- contains text such as "tech" or "media". The builder now normalizes both
-- year/month and year-only archives, recognizes editorial markers only on true
-- subdomains, and lets otherwise ambiguous URLs activate only after they prove
-- a collection of at least three distinct company stories.

CREATE TEMP TABLE unstable_publication_recipes
ON COMMIT DROP AS
WITH recipe_url AS (
    SELECT
        recipe.id AS recipe_id,
        recipe.company_id,
        recipe.source_id,
        source.url AS source_url,
        COALESCE(recipe.spec->>'publication_url', source.url)
            AS publication_url,
        regexp_replace(
            lower(split_part(split_part(
                COALESCE(recipe.spec->>'publication_url', source.url),
                '://',
                2
            ), '/', 1)),
            '^www\.',
            ''
        ) AS host,
        lower(split_part(split_part(regexp_replace(
            COALESCE(recipe.spec->>'publication_url', source.url),
            '^https?://[^/]+',
            ''
        ), '?', 1), '#', 1)) AS path
    FROM
        company_news_recipes AS recipe
        JOIN sources AS source ON source.id = recipe.source_id
        LEFT JOIN company_news_recipe_state AS state
            ON state.recipe_id = recipe.id
    WHERE
        recipe.status = 'active'
        AND NOT COALESCE(state.rebuild_required, false)
),
host_shape AS (
    SELECT
        recipe.*,
        regexp_split_to_array(recipe.host, '\.') AS host_labels
    FROM recipe_url AS recipe
),
classified AS (
    SELECT
        recipe.*,
        recipe.path ~ '/(19|20)[0-9]{2}/?$'
            AS has_year_archive_suffix,
        recipe.host_labels[1] ~
            '(blog|builder|developer|engineering|journal|labs|media|news|press|research|stories|tech|updates)'
            AS base_label_has_editorial_marker,
        cardinality(recipe.host_labels) <
            CASE
                WHEN
                    length(recipe.host_labels[cardinality(recipe.host_labels)]) = 2
                    AND recipe.host_labels[cardinality(recipe.host_labels) - 1]
                        = ANY (
                            ARRAY['ac', 'co', 'com', 'edu', 'gov', 'net', 'org']
                        )
                    THEN 4
                ELSE 3
            END AS marker_is_on_registered_domain,
        EXISTS (
            SELECT 1
            FROM regexp_split_to_table(
                trim(both '/' FROM recipe.path),
                '/'
            ) AS segment
            WHERE segment ~
                '(announcement|article|blog|case-study|changelog|content|customer-stories|developer|engineering|episode|featured|insight|innovation|journal|knowledge|learn|library|latest|media|message|news|notice|paper|perspective|podcast|post|publication|press|release|report|research|resource|review|stories|tech|thought-leadership|updates|what-s-new|whats-new)'
        ) AS path_has_editorial_marker
    FROM host_shape AS recipe
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
        )))) FILTER (WHERE NOT item.is_private) AS distinct_title_count
    FROM
        classified AS recipe
        LEFT JOIN feed_items AS item ON item.source_id = recipe.source_id
    GROUP BY
        recipe.recipe_id,
        recipe.company_id,
        recipe.source_id,
        recipe.source_url,
        recipe.publication_url,
        recipe.host,
        recipe.path,
        recipe.host_labels,
        recipe.has_year_archive_suffix,
        recipe.base_label_has_editorial_marker,
        recipe.marker_is_on_registered_domain,
        recipe.path_has_editorial_marker
)
SELECT
    recipe_id,
    company_id,
    source_id,
    source_url,
    publication_url,
    path,
    item_count,
    distinct_title_count,
    CASE
        WHEN has_year_archive_suffix
            THEN 'temporal_year_archive_publication'
        ELSE 'base_domain_marker_lacks_editorial_collection_evidence'
    END AS reason
FROM metrics
WHERE
    has_year_archive_suffix
    OR (
        host <> 'globenewswire.com'
        AND base_label_has_editorial_marker
        AND marker_is_on_registered_domain
        AND NOT path_has_editorial_marker
        AND (
            item_count < 3
            OR distinct_title_count < 3
        )
    );

UPDATE company_news_recipes AS recipe
SET
    status = 'superseded',
    stale_at = NULL,
    stale_reason = NULL
FROM unstable_publication_recipes AS invalid
WHERE
    recipe.id = invalid.recipe_id
    AND recipe.status = 'active';

UPDATE company_news_recipe_state AS state
SET
    rebuild_required = false,
    reason = invalid.reason,
    metadata = COALESCE(state.metadata, '{}'::jsonb) || jsonb_build_object(
        'supersession',
        jsonb_build_object(
            'policy', 'company-news-stable-editorial-origin.v2',
            'reason', invalid.reason,
            'source_url', invalid.source_url,
            'publication_url', invalid.publication_url,
            'path', invalid.path,
            'item_count', invalid.item_count,
            'distinct_title_count', invalid.distinct_title_count,
            'migration',
                '0093_supersede_year_archives_and_unproven_base_markers'
        )
    )
FROM unstable_publication_recipes AS invalid
WHERE state.recipe_id = invalid.recipe_id;

UPDATE sources AS source
SET metadata = source.metadata - 'active_recipe_id' - 'recipe_schema_version'
FROM unstable_publication_recipes AS invalid
WHERE
    source.id = invalid.source_id
    AND source.metadata ->> 'active_recipe_id' = invalid.recipe_id::text;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'unstable publication recipe superseded'
FROM unstable_publication_recipes AS invalid
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
        'policy', 'company-news-stable-editorial-origin.v2',
        'source_url', invalid.source_url,
        'publication_url', invalid.publication_url,
        'path', invalid.path,
        'item_count', invalid.item_count,
        'distinct_title_count', invalid.distinct_title_count,
        'migration',
            '0093_supersede_year_archives_and_unproven_base_markers'
    )
FROM unstable_publication_recipes AS invalid;
