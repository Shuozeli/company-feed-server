-- Retryable transport failures do not prove that a recipe's structure or
-- company scope has drifted. Restore recipes that were staled solely by HTTP
-- 408, 429, or 5xx responses and reset the counters that individual durable-job
-- retries incorrectly advanced.

CREATE TEMP TABLE transient_recipe_failure_repairs ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    COALESCE(state.reason, recipe.stale_reason) AS prior_reason
FROM company_news_recipes AS recipe
JOIN company_news_recipe_state AS state ON state.recipe_id = recipe.id
WHERE
    COALESCE(state.reason, recipe.stale_reason, '')
        ~ '^HTTP (408|429|5[0-9]{2}) '
    AND state.last_correct_at IS NOT NULL
    AND (
        recipe.status = 'active'
        OR (
            recipe.status = 'stale'
            AND NOT EXISTS (
                SELECT 1
                FROM company_news_recipes AS active_recipe
                WHERE
                    active_recipe.source_id = recipe.source_id
                    AND active_recipe.status = 'active'
                    AND active_recipe.id <> recipe.id
            )
        )
    );

UPDATE company_news_recipes AS recipe
SET
    status = 'active',
    stale_at = NULL,
    stale_reason = NULL
FROM transient_recipe_failure_repairs AS repair
WHERE recipe.id = repair.recipe_id;

UPDATE company_news_recipe_state AS state
SET
    consecutive_failures = 0,
    consecutive_empty_runs = 0,
    consecutive_correctness_failures = 0,
    freshness_status = 'overdue',
    correctness_status = 'passing',
    rebuild_required = false,
    reason = 'transient_transport_failure_repaired',
    metadata = state.metadata || jsonb_build_object(
        'transient_failure_repair',
        jsonb_build_object(
            'policy', 'company-news-recipe-health.v2',
            'prior_reason', repair.prior_reason,
            'repaired_at', CURRENT_TIMESTAMP,
            'migration', '0044_repair_transient_recipe_failures'
        )
    )
FROM transient_recipe_failure_repairs AS repair
WHERE state.recipe_id = repair.recipe_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.transient_failure_repaired',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'recipe_id', repair.recipe_id,
        'prior_reason', repair.prior_reason,
        'policy', 'company-news-recipe-health.v2',
        'migration', '0044_repair_transient_recipe_failures'
    )
FROM transient_recipe_failure_repairs AS repair;

-- Also preserve the official Platinum Group Metals profile so future explicit
-- recipe builds can prefer the company publication over third-party coverage.
--
-- Primary evidence:
-- https://platinumgroupmetals.net/
-- https://platinumgroupmetals.net/news/

WITH target AS (
    SELECT id
    FROM companies
    WHERE company_key = 'platinum-group-metals-ltd-ordinary-shares-canada'
),
alias_values(alias) AS (
    VALUES
        ('Platinum Group'),
        ('Platinum Group Metals'),
        ('Platinum Group Metals Ltd.')
),
merged_aliases AS (
    SELECT
        target.id,
        jsonb_agg(DISTINCT alias ORDER BY alias) AS aliases
    FROM target
    CROSS JOIN LATERAL (
        SELECT jsonb_array_elements_text(company.aliases) AS alias
        FROM companies AS company
        WHERE company.id = target.id
        UNION ALL
        SELECT alias
        FROM alias_values
    ) AS values
    GROUP BY target.id
)
UPDATE companies AS company
SET
    aliases = merged_aliases.aliases,
    homepage_url = COALESCE(
        company.homepage_url,
        'https://platinumgroupmetals.net/'
    ),
    investor_relations_url = COALESCE(
        company.investor_relations_url,
        'https://platinumgroupmetals.net/investors/'
    ),
    newsroom_url = COALESCE(
        company.newsroom_url,
        'https://platinumgroupmetals.net/news/'
    ),
    metadata = company.metadata || jsonb_build_object(
        'profile_enrichment',
        jsonb_build_object(
            'reason', 'official_legal_name_domain',
            'source', 'official_company_website',
            'source_url', 'https://platinumgroupmetals.net/'
        )
    )
FROM merged_aliases
WHERE company.id = merged_aliases.id;

INSERT INTO event_log (event_type, company_id, payload)
SELECT
    'company.profile_enriched',
    company.id,
    jsonb_build_object(
        'policy', 'company-profile-enrichment.v1',
        'reason', 'official_legal_name_domain',
        'canonical_name', company.name,
        'aliases', company.aliases,
        'source_url', 'https://platinumgroupmetals.net/',
        'migration', '0044_repair_transient_recipe_failures'
    )
FROM companies AS company
WHERE
    company.company_key =
        'platinum-group-metals-ltd-ordinary-shares-canada';
