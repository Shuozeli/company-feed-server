-- Re-run every healthy adapter-generated recipe that persisted an explicit
-- publication boundary before the runtime began re-evaluating that boundary
-- from the current company publication-host policy. Public visibility remains
-- available while this audit runs because rebuild_required stays false.

CREATE TEMP TABLE publication_boundary_revalidation_targets
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
    AND recipe.generated_by_run_id IS NOT NULL
    AND recipe.spec ->> 'item_scope' = 'publication_boundary'
    AND NOT COALESCE(state.rebuild_required, false);

CREATE TEMP TABLE publication_boundary_revalidation_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH audit_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.publication_boundary_revalidation_started',
        jsonb_build_object(
            'recipe_count',
                (SELECT count(*) FROM publication_boundary_revalidation_targets),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM publication_boundary_revalidation_targets
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM publication_boundary_revalidation_targets
                ),
            'policy', 'company-publication-host-policy.v1',
            'migration', '0115_revalidate_adapter_publication_boundaries'
        )
    WHERE EXISTS (SELECT 1 FROM publication_boundary_revalidation_targets)
    RETURNING id
)
INSERT INTO publication_boundary_revalidation_wave (event_id)
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
    'publication_host_policy_revalidation_required',
    jsonb_build_object(
        'publication_host_policy_revalidation',
        jsonb_build_object(
            'policy', 'company-publication-host-policy.v1',
            'publication_url', target.publication_url,
            'repair_wave_event_id', wave.event_id,
            'migration', '0115_revalidate_adapter_publication_boundaries'
        )
    )
FROM
    publication_boundary_revalidation_targets AS target
    CROSS JOIN publication_boundary_revalidation_wave AS wave
ON CONFLICT (recipe_id) DO UPDATE
SET
    consecutive_correctness_failures = GREATEST(
        company_news_recipe_state.consecutive_correctness_failures,
        1
    ),
    freshness_status = 'unknown',
    correctness_status = 'failing',
    rebuild_required = false,
    reason = 'publication_host_policy_revalidation_required',
    metadata = company_news_recipe_state.metadata || EXCLUDED.metadata,
    updated_at = CURRENT_TIMESTAMP;

-- Preserve any already-active crawl job, but make this bounded audit outrank
-- ordinary scheduled crawling.
UPDATE jobs AS job
SET
    priority = GREATEST(job.priority, 8192),
    run_after = LEAST(job.run_after, CURRENT_TIMESTAMP),
    updated_at = CURRENT_TIMESTAMP
WHERE
    job.job_type = 'crawl_source'
    AND job.status IN ('pending', 'running')
    AND job.source_id IN (
        SELECT source_id
        FROM publication_boundary_revalidation_targets
    );

INSERT INTO jobs (
    job_type,
    job_key,
    status,
    priority,
    run_after,
    max_attempts,
    company_id,
    source_id,
    payload
)
SELECT
    'crawl_source',
    'source:' || target.source_id::text,
    'pending',
    8192,
    CURRENT_TIMESTAMP,
    5,
    target.company_id,
    target.source_id,
    jsonb_build_object(
        'source_id', target.source_id,
        'reason', 'publication_host_policy_revalidation',
        'policy', 'company-publication-host-policy.v1'
    )
FROM publication_boundary_revalidation_targets AS target
WHERE NOT EXISTS (
    SELECT 1
    FROM jobs AS active_job
    WHERE
        active_job.job_type = 'crawl_source'
        AND active_job.job_key = 'source:' || target.source_id::text
        AND active_job.status IN ('pending', 'running')
)
ON CONFLICT (job_type, job_key)
    WHERE status IN ('pending', 'running')
DO NOTHING;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_publication_boundary_revalidation_required',
    target.company_id,
    target.source_id,
    jsonb_build_object(
        'recipe_id', target.recipe_id,
        'publication_url', target.publication_url,
        'priority', 8192,
        'policy', 'company-publication-host-policy.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0115_revalidate_adapter_publication_boundaries'
    )
FROM
    publication_boundary_revalidation_targets AS target
    CROSS JOIN publication_boundary_revalidation_wave AS wave;
