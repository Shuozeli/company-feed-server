-- Different official, investor-relations, regional, and syndication URLs can
-- expose the same underlying publication while rewriting every article URL.
-- URL identity alone cannot detect those mirrors. Keep the preferred active
-- recipe and supersede only a company-scoped candidate with at least three
-- exact normalized-title/publication-day matches covering at least 80% of its
-- public items. A larger publication wins; equal-sized publications retain the
-- recipe verified first. This deliberately preserves publications that share
-- only occasional cross-posts, such as separate product and engineering blogs.

CREATE TEMP TABLE redundant_title_date_recipe_mirrors
ON COMMIT DROP AS
WITH active_item AS (
    SELECT
        recipe.id AS recipe_id,
        recipe.company_id,
        recipe.source_id,
        public_url_identity_key(item.canonical_url) AS item_identity,
        lower(btrim(regexp_replace(
            item.title,
            '[[:space:]]+',
            ' ',
            'g'
        ))) AS title_key,
        (item.published_at AT TIME ZONE 'UTC')::date AS published_day
    FROM
        company_news_recipes AS recipe
        JOIN sources AS source ON source.id = recipe.source_id
        LEFT JOIN company_news_recipe_state AS state
            ON state.recipe_id = recipe.id
        JOIN feed_items AS item ON item.source_id = recipe.source_id
    WHERE
        recipe.status = 'active'
        AND source.status = 'approved'
        AND NOT COALESCE(state.rebuild_required, false)
        AND NOT item.is_private
        AND btrim(item.title) <> ''
),
active_recipe AS (
    SELECT
        recipe.id AS recipe_id,
        recipe.company_id,
        recipe.source_id,
        source.url AS source_url,
        COALESCE(recipe.verified_at, recipe.created_at) AS preference_at,
        count(DISTINCT item.item_identity) AS item_count
    FROM
        company_news_recipes AS recipe
        JOIN sources AS source ON source.id = recipe.source_id
        JOIN active_item AS item ON item.recipe_id = recipe.id
    GROUP BY
        recipe.id,
        recipe.company_id,
        recipe.source_id,
        source.url,
        COALESCE(recipe.verified_at, recipe.created_at)
),
overlap AS (
    SELECT
        candidate.recipe_id,
        candidate.company_id,
        candidate.source_id,
        candidate.source_url,
        candidate.item_count,
        preferred.recipe_id AS superseded_by_recipe_id,
        preferred.source_url AS preferred_source_url,
        preferred.item_count AS preferred_item_count,
        preferred.preference_at AS preferred_at,
        count(DISTINCT candidate_item.item_identity) AS matched_item_count
    FROM
        active_recipe AS candidate
        JOIN active_recipe AS preferred
            ON preferred.company_id = candidate.company_id
            AND preferred.recipe_id <> candidate.recipe_id
            AND (
                preferred.item_count > candidate.item_count
                OR (
                    preferred.item_count = candidate.item_count
                    AND (
                        preferred.preference_at,
                        preferred.recipe_id
                    ) < (
                        candidate.preference_at,
                        candidate.recipe_id
                    )
                )
            )
        JOIN active_item AS candidate_item
            ON candidate_item.recipe_id = candidate.recipe_id
        JOIN active_item AS preferred_item
            ON preferred_item.recipe_id = preferred.recipe_id
            AND preferred_item.title_key = candidate_item.title_key
            AND preferred_item.published_day
                IS NOT DISTINCT FROM candidate_item.published_day
    GROUP BY
        candidate.recipe_id,
        candidate.company_id,
        candidate.source_id,
        candidate.source_url,
        candidate.item_count,
        preferred.recipe_id,
        preferred.source_url,
        preferred.item_count,
        preferred.preference_at
    HAVING
        count(DISTINCT candidate_item.item_identity) >= 3
        AND count(DISTINCT candidate_item.item_identity) * 100
            >= candidate.item_count * 80
)
SELECT DISTINCT ON (recipe_id)
    recipe_id,
    company_id,
    source_id,
    source_url,
    item_count,
    superseded_by_recipe_id,
    preferred_source_url,
    preferred_item_count,
    matched_item_count
FROM overlap
ORDER BY
    recipe_id,
    preferred_item_count DESC,
    preferred_at,
    superseded_by_recipe_id;

UPDATE company_news_recipes AS recipe
SET
    status = 'superseded',
    stale_at = NULL,
    stale_reason = NULL
FROM redundant_title_date_recipe_mirrors AS mirror
WHERE
    recipe.id = mirror.recipe_id
    AND recipe.status = 'active';

UPDATE company_news_recipe_state AS state
SET
    rebuild_required = false,
    reason = 'superseded_by_title_date_recipe_mirror',
    metadata = state.metadata || jsonb_build_object(
        'supersession',
        jsonb_build_object(
            'policy', 'company-news-title-date-mirror.v1',
            'superseded_by_recipe_id', mirror.superseded_by_recipe_id,
            'source_url', mirror.source_url,
            'preferred_source_url', mirror.preferred_source_url,
            'item_count', mirror.item_count,
            'preferred_item_count', mirror.preferred_item_count,
            'matched_item_count', mirror.matched_item_count,
            'overlap_ratio_bps',
                mirror.matched_item_count * 10000 / mirror.item_count,
            'migration',
                '0090_supersede_redundant_title_date_recipe_mirrors'
        )
    )
FROM redundant_title_date_recipe_mirrors AS mirror
WHERE state.recipe_id = mirror.recipe_id;

UPDATE sources AS source
SET metadata = source.metadata - 'active_recipe_id' - 'recipe_schema_version'
FROM redundant_title_date_recipe_mirrors AS mirror
WHERE
    source.id = mirror.source_id
    AND source.metadata ->> 'active_recipe_id' = mirror.recipe_id::text;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'redundant title/date mirror recipe superseded'
FROM redundant_title_date_recipe_mirrors AS mirror
WHERE
    job.job_type = 'crawl_source'
    AND job.status = 'pending'
    AND job.source_id = mirror.source_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_superseded',
    mirror.company_id,
    mirror.source_id,
    jsonb_build_object(
        'recipe_id', mirror.recipe_id,
        'superseded_by_recipe_id', mirror.superseded_by_recipe_id,
        'reason', 'overlaps_preferred_active_recipe',
        'policy', 'company-news-title-date-mirror.v1',
        'source_url', mirror.source_url,
        'preferred_source_url', mirror.preferred_source_url,
        'item_count', mirror.item_count,
        'preferred_item_count', mirror.preferred_item_count,
        'matched_item_count', mirror.matched_item_count,
        'overlap_ratio_bps',
            mirror.matched_item_count * 10000 / mirror.item_count,
        'migration', '0090_supersede_redundant_title_date_recipe_mirrors'
    )
FROM redundant_title_date_recipe_mirrors AS mirror;
