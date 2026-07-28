-- Migration 0115 intentionally audited every historical explicit adapter
-- boundary. That proved too conservative for issuer brands and acronyms that
-- are not recoverable from name heuristics alone. Stop the blanket audit,
-- preserve hard reviewed exclusions, and restore the three valid publications
-- it had already superseded.

CREATE TEMP TABLE cancelled_boundary_revalidation_targets
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec ->> 'publication_url' AS publication_url
FROM
    company_news_recipes AS recipe
    JOIN company_news_recipe_state AS state
        ON state.recipe_id = recipe.id
WHERE
    recipe.status = 'active'
    AND NOT state.rebuild_required
    AND state.reason = 'publication_host_policy_revalidation_required';

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error =
        'cancelled after restoring explicit adapter publication boundaries',
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL
WHERE
    job.job_type = 'crawl_source'
    AND job.status = 'pending'
    AND job.priority >= 8192
    AND job.source_id IN (
        SELECT source_id
        FROM cancelled_boundary_revalidation_targets
    );

UPDATE company_news_recipe_state AS state
SET
    consecutive_correctness_failures = 0,
    freshness_status = CASE
        WHEN state.last_item_published_at IS NOT NULL
            AND state.last_item_published_at
                < CURRENT_TIMESTAMP
                    - (
                        (
                            recipe.spec
                                #>> '{freshness,content_stale_after_seconds}'
                        )::bigint * INTERVAL '1 second'
                    )
            THEN 'content_stale'
        WHEN COALESCE(
            state.last_correct_at,
            recipe.verified_at,
            recipe.created_at
        ) + (
            (
                recipe.spec
                    #>> '{freshness,crawl_interval_seconds}'
            )::bigint * INTERVAL '1 second'
        ) <= CURRENT_TIMESTAMP
            THEN 'overdue'
        WHEN state.last_correct_at IS NOT NULL
            THEN 'fresh'
        ELSE 'unknown'
    END,
    correctness_status = CASE
        WHEN state.last_correct_at IS NOT NULL THEN 'passing'
        ELSE 'unknown'
    END,
    rebuild_required = false,
    reason = NULL,
    metadata = state.metadata || jsonb_build_object(
        'publication_boundary_revalidation_cancelled',
        jsonb_build_object(
            'reason',
                'explicit_adapter_boundary_preserved_without_reviewed_exclusion',
            'policy', 'company-publication-host-policy.v3',
            'cancelled_at', CURRENT_TIMESTAMP,
            'migration', '0117_restore_explicit_adapter_boundaries'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    cancelled_boundary_revalidation_targets AS target
    JOIN company_news_recipes AS recipe
        ON recipe.id = target.recipe_id
WHERE state.recipe_id = target.recipe_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_publication_boundary_revalidation_cancelled',
    target.company_id,
    target.source_id,
    jsonb_build_object(
        'recipe_id', target.recipe_id,
        'publication_url', target.publication_url,
        'reason',
            'explicit_adapter_boundary_preserved_without_reviewed_exclusion',
        'policy', 'company-publication-host-policy.v3',
        'migration', '0117_restore_explicit_adapter_boundaries'
    )
FROM cancelled_boundary_revalidation_targets AS target;

CREATE TEMP TABLE verified_brand_host_policies (
    company_key text PRIMARY KEY,
    verified_host text NOT NULL
) ON COMMIT DROP;

INSERT INTO verified_brand_host_policies (company_key, verified_host)
VALUES
    ('brc-inc-class-a-common-stock', 'blackriflecoffee.com'),
    (
        'procter-gamble-company-the-common-stock',
        'pg.com'
    );

UPDATE companies AS company
SET
    metadata = jsonb_set(
        company.metadata,
        '{publication_host_policy}',
        COALESCE(company.metadata -> 'publication_host_policy', '{}'::jsonb)
            || jsonb_build_object(
                'verified_hosts',
                    COALESCE(
                        company.metadata
                            #> '{publication_host_policy,verified_hosts}',
                        '[]'::jsonb
                    ) || jsonb_build_array(policy.verified_host),
                'policy', 'company-publication-host-policy.v3',
                'reviewed_at', CURRENT_TIMESTAMP,
                'migration', '0117_restore_explicit_adapter_boundaries'
            ),
        true
    ),
    updated_at = CURRENT_TIMESTAMP
FROM verified_brand_host_policies AS policy
WHERE company.company_key = policy.company_key;

INSERT INTO event_log (event_type, company_id, payload)
SELECT
    'company.publication_host_policy_updated',
    company.id,
    jsonb_build_object(
        'verified_host', policy.verified_host,
        'reason', 'verified_issuer_brand_or_acronym_publication',
        'policy', 'company-publication-host-policy.v3',
        'migration', '0117_restore_explicit_adapter_boundaries'
    )
FROM
    verified_brand_host_policies AS policy
    JOIN companies AS company ON company.company_key = policy.company_key;

CREATE TEMP TABLE valid_brand_recipes_to_restore
ON COMMIT DROP AS
WITH expected (company_key, publication_url) AS (
    VALUES
        (
            'brc-inc-class-a-common-stock',
            'https://ir.blackriflecoffee.com/news-events/press-releases'
        ),
        (
            'procter-gamble-company-the-common-stock',
            'https://us.pg.com/newsroom/news-releases'
        ),
        (
            'procter-gamble-company-the-common-stock',
            'https://us.pg.com/blogs'
        )
)
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec ->> 'publication_url' AS publication_url,
    latest.id AS invalidating_run_id
FROM
    expected
    JOIN companies AS company
        ON company.company_key = expected.company_key
    JOIN company_news_recipes AS recipe
        ON recipe.company_id = company.id
        AND public_url_identity_key(recipe.spec ->> 'publication_url')
            = public_url_identity_key(expected.publication_url)
    JOIN sources AS source
        ON source.id = recipe.source_id
        AND source.status = 'approved'
    JOIN LATERAL (
        SELECT run.id, run.started_at, run.reasons
        FROM company_news_recipe_runs AS run
        WHERE run.recipe_id = recipe.id
        ORDER BY run.started_at DESC
        LIMIT 1
    ) AS latest ON true
WHERE
    recipe.status = 'superseded'
    AND latest.started_at >= '2026-07-24T21:03:00Z'::timestamptz
    AND latest.reasons ? 'company_scope_relevance_below_minimum'
    AND NOT EXISTS (
        SELECT 1
        FROM company_news_recipes AS active
        WHERE
            active.source_id = recipe.source_id
            AND active.status = 'active'
    );

UPDATE company_news_recipes AS recipe
SET
    status = 'active',
    stale_at = NULL,
    stale_reason = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM valid_brand_recipes_to_restore AS repair
WHERE recipe.id = repair.recipe_id;

UPDATE company_news_recipe_state AS state
SET
    consecutive_correctness_failures = 0,
    freshness_status = 'unknown',
    correctness_status = 'passing',
    rebuild_required = false,
    reason = NULL,
    metadata = state.metadata || jsonb_build_object(
        'publication_boundary_restoration',
        jsonb_build_object(
            'invalidating_run_id', repair.invalidating_run_id,
            'reason', 'valid_issuer_brand_or_acronym_publication',
            'policy', 'company-publication-host-policy.v3',
            'restored_at', CURRENT_TIMESTAMP,
            'migration', '0117_restore_explicit_adapter_boundaries'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM valid_brand_recipes_to_restore AS repair
WHERE state.recipe_id = repair.recipe_id;

UPDATE sources AS source
SET
    metadata = source.metadata || jsonb_build_object(
        'active_recipe_id', recipe.id,
        'recipe_schema_version', recipe.schema_version
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    valid_brand_recipes_to_restore AS repair
    JOIN company_news_recipes AS recipe
        ON recipe.id = repair.recipe_id
WHERE source.id = repair.source_id;

UPDATE source_state AS state
SET
    consecutive_failures = 0,
    backoff_until = NULL,
    last_error = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM valid_brand_recipes_to_restore AS repair
WHERE state.source_id = repair.source_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_restored',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'recipe_id', repair.recipe_id,
        'publication_url', repair.publication_url,
        'invalidating_run_id', repair.invalidating_run_id,
        'reason', 'valid_issuer_brand_or_acronym_publication',
        'policy', 'company-publication-host-policy.v3',
        'migration', '0117_restore_explicit_adapter_boundaries'
    )
FROM valid_brand_recipes_to_restore AS repair;
