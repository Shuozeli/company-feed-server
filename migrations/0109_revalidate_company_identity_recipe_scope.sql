-- Re-run every currently healthy company-identity recipe once under the
-- current article-level ownership gate. These are inferred or shared-host
-- publications, so a sample with less than 50% company-relevant items is now
-- superseded immediately instead of remaining live through a failure streak.

CREATE TEMP TABLE company_identity_revalidation_targets
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec ->> 'publication_url' AS publication_url
FROM
    company_news_recipes AS recipe
    LEFT JOIN company_news_recipe_state AS state
        ON state.recipe_id = recipe.id
WHERE
    recipe.status = 'active'
    AND recipe.spec ->> 'item_scope' = 'company_identity'
    AND NOT COALESCE(state.rebuild_required, false);

CREATE TEMP TABLE company_identity_revalidation_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH audit_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.company_identity_revalidation_started',
        jsonb_build_object(
            'recipe_count',
                (SELECT count(*) FROM company_identity_revalidation_targets),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM company_identity_revalidation_targets
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM company_identity_revalidation_targets
                ),
            'policy', 'company-scope-relevance.v3',
            'migration', '0109_revalidate_company_identity_recipe_scope'
        )
    WHERE EXISTS (SELECT 1 FROM company_identity_revalidation_targets)
    RETURNING id
)
INSERT INTO company_identity_revalidation_wave (event_id)
SELECT id FROM audit_started;

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
    'company_scope_revalidation_required',
    jsonb_build_object(
        'company_scope_revalidation',
        jsonb_build_object(
            'policy', 'company-scope-relevance.v3',
            'publication_url', target.publication_url,
            'repair_wave_event_id', wave.event_id,
            'migration', '0109_revalidate_company_identity_recipe_scope'
        )
    )
FROM
    company_identity_revalidation_targets AS target
    CROSS JOIN company_identity_revalidation_wave AS wave
ON CONFLICT (recipe_id) DO UPDATE
SET
    consecutive_correctness_failures = GREATEST(
        company_news_recipe_state.consecutive_correctness_failures,
        1
    ),
    correctness_status = 'failing',
    rebuild_required = false,
    reason = 'company_scope_revalidation_required',
    metadata = company_news_recipe_state.metadata || EXCLUDED.metadata,
    updated_at = CURRENT_TIMESTAMP;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_company_scope_revalidation_required',
    target.company_id,
    target.source_id,
    jsonb_build_object(
        'recipe_id', target.recipe_id,
        'publication_url', target.publication_url,
        'policy', 'company-scope-relevance.v3',
        'repair_wave_event_id', wave.event_id,
        'migration', '0109_revalidate_company_identity_recipe_scope'
    )
FROM
    company_identity_revalidation_targets AS target
    CROSS JOIN company_identity_revalidation_wave AS wave;
