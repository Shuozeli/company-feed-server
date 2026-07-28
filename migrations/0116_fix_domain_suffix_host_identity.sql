-- A terminal DNS suffix in a company name (for example, "Perpetuals.com")
-- used to match the same suffix on every unrelated host. Retire the confirmed
-- Las Vegas Sun expansion, then revalidate every remaining active recipe for
-- a domain-branded company under the corrected host-label identity rule.

UPDATE companies AS company
SET
    metadata = jsonb_set(
        company.metadata,
        '{publication_host_policy}',
        COALESCE(company.metadata -> 'publication_host_policy', '{}'::jsonb)
            || jsonb_build_object(
                'excluded_hosts',
                    COALESCE(
                        company.metadata
                            #> '{publication_host_policy,excluded_hosts}',
                        '[]'::jsonb
                    ) || jsonb_build_array('lasvegassun.com'),
                'policy', 'company-publication-host-policy.v2',
                'reviewed_at', CURRENT_TIMESTAMP,
                'migration', '0116_fix_domain_suffix_host_identity'
            ),
        true
    ),
    updated_at = CURRENT_TIMESTAMP
WHERE
    company.company_key =
        'perpetuals-com-ltd-american-depositary-shares';

CREATE TEMP TABLE wrong_domain_suffix_sources
ON COMMIT DROP AS
SELECT
    source.id AS source_id,
    source.company_id,
    source.url,
    'Las Vegas Sun'::text AS claiming_company_name,
    'publication_owned_by_different_company'::text AS reason
FROM
    companies AS company
    JOIN sources AS source ON source.company_id = company.id
WHERE
    company.company_key =
        'perpetuals-com-ltd-american-depositary-shares'
    AND source.status = 'approved'
    AND lower(source.url)
        ~ '^https?://([^/]+[.])?lasvegassun[.]com(/|$)';

CREATE TEMP TABLE wrong_domain_suffix_recipes
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec ->> 'publication_url' AS publication_url,
    repair.claiming_company_name,
    repair.reason
FROM
    wrong_domain_suffix_sources AS repair
    JOIN company_news_recipes AS recipe
        ON recipe.source_id = repair.source_id
WHERE recipe.status IN ('active', 'stale');

CREATE TEMP TABLE wrong_domain_suffix_candidates
ON COMMIT DROP AS
SELECT
    candidate.id AS candidate_id,
    candidate.company_id,
    candidate.accepted_source_id,
    candidate.candidate_url,
    candidate.status AS prior_status
FROM
    companies AS company
    JOIN source_candidates AS candidate
        ON candidate.company_id = company.id
WHERE
    company.company_key =
        'perpetuals-com-ltd-american-depositary-shares'
    AND candidate.status <> 'rejected'
    AND lower(candidate.candidate_url)
        ~ '^https?://([^/]+[.])?lasvegassun[.]com(/|$)';

CREATE TEMP TABLE wrong_domain_suffix_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at,
    repair.claiming_company_name,
    repair.reason
FROM
    wrong_domain_suffix_sources AS repair
    JOIN feed_items AS item ON item.source_id = repair.source_id
WHERE NOT item.is_private;

CREATE TEMP TABLE wrong_domain_suffix_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.wrong_cross_domain_repair_started',
        jsonb_build_object(
            'source_count',
                (SELECT count(*) FROM wrong_domain_suffix_sources),
            'recipe_count',
                (SELECT count(*) FROM wrong_domain_suffix_recipes),
            'candidate_count',
                (SELECT count(*) FROM wrong_domain_suffix_candidates),
            'item_count',
                (SELECT count(*) FROM wrong_domain_suffix_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM wrong_domain_suffix_sources
                ),
            'policy', 'cross-domain-company-ownership.v5',
            'migration', '0116_fix_domain_suffix_host_identity'
        )
    WHERE
        EXISTS (SELECT 1 FROM wrong_domain_suffix_sources)
        OR EXISTS (SELECT 1 FROM wrong_domain_suffix_candidates)
    RETURNING id
)
INSERT INTO wrong_domain_suffix_wave (event_id)
SELECT id FROM repair_started;

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = repair.reason
FROM wrong_domain_suffix_recipes AS repair
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
        'ownership_repair',
        jsonb_build_object(
            'policy', 'cross-domain-company-ownership.v5',
            'claiming_company_name', repair.claiming_company_name,
            'repair_wave_event_id', wave.event_id,
            'repaired_at', CURRENT_TIMESTAMP,
            'migration', '0116_fix_domain_suffix_host_identity'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    wrong_domain_suffix_recipes AS repair
    CROSS JOIN wrong_domain_suffix_wave AS wave
WHERE state.recipe_id = repair.recipe_id;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata =
        source.metadata - 'active_recipe_id' - 'recipe_schema_version'
        || jsonb_build_object(
            'quality_disable',
            jsonb_build_object(
                'reason', repair.reason,
                'reversible', false,
                'policy', 'cross-domain-company-ownership.v5',
                'claiming_company_name', repair.claiming_company_name,
                'repair_wave_event_id', wave.event_id,
                'disabled_at', CURRENT_TIMESTAMP,
                'migration', '0116_fix_domain_suffix_host_identity'
            )
        )
FROM
    wrong_domain_suffix_sources AS repair
    CROSS JOIN wrong_domain_suffix_wave AS wave
WHERE source.id = repair.source_id;

UPDATE source_candidates AS candidate
SET
    status = 'rejected',
    accepted_source_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM wrong_domain_suffix_candidates AS repair
WHERE candidate.id = repair.candidate_id;

INSERT INTO candidate_decisions (
    candidate_id,
    source_id,
    decision,
    decision_mode,
    actor,
    reason,
    metadata
)
SELECT
    repair.candidate_id,
    repair.accepted_source_id,
    'rejected',
    'automatic',
    'migration:0116',
    'candidate expands a different company publication',
    jsonb_build_object(
        'prior_status', repair.prior_status,
        'claiming_company_name', 'Las Vegas Sun',
        'policy', 'cross-domain-company-ownership.v5',
        'repair_wave_event_id', wave.event_id,
        'migration', '0116_fix_domain_suffix_host_identity'
    )
FROM
    wrong_domain_suffix_candidates AS repair
    CROSS JOIN wrong_domain_suffix_wave AS wave;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled because publication belongs to a different company',
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL
WHERE
    job.status IN ('pending', 'running')
    AND (
        job.source_id IN (
            SELECT source_id
            FROM wrong_domain_suffix_sources
        )
        OR job.candidate_id IN (
            SELECT candidate_id
            FROM wrong_domain_suffix_candidates
        )
    );

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'original_published_at', repair.published_at,
            'reversible', false,
            'policy', 'cross-domain-company-ownership.v5',
            'claiming_company_name', repair.claiming_company_name,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    wrong_domain_suffix_items AS repair
    CROSS JOIN wrong_domain_suffix_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM wrong_domain_suffix_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_stale',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'recipe_id', repair.recipe_id,
        'publication_url', repair.publication_url,
        'claiming_company_name', repair.claiming_company_name,
        'reason', repair.reason,
        'rebuild_required', true,
        'policy', 'cross-domain-company-ownership.v5',
        'repair_wave_event_id', wave.event_id,
        'migration', '0116_fix_domain_suffix_host_identity'
    )
FROM
    wrong_domain_suffix_recipes AS repair
    CROSS JOIN wrong_domain_suffix_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.quality_disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.url,
        'claiming_company_name', repair.claiming_company_name,
        'reason', repair.reason,
        'reversible', false,
        'policy', 'cross-domain-company-ownership.v5',
        'repair_wave_event_id', wave.event_id,
        'migration', '0116_fix_domain_suffix_host_identity'
    )
FROM
    wrong_domain_suffix_sources AS repair
    CROSS JOIN wrong_domain_suffix_wave AS wave;

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
        'claiming_company_name', repair.claiming_company_name,
        'reason', repair.reason,
        'reversible', false,
        'policy', 'cross-domain-company-ownership.v5',
        'repair_wave_event_id', wave.event_id,
        'migration', '0116_fix_domain_suffix_host_identity'
    )
FROM
    wrong_domain_suffix_items AS repair
    CROSS JOIN wrong_domain_suffix_wave AS wave;

CREATE TEMP TABLE domain_suffix_identity_revalidation_targets
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec ->> 'publication_url' AS publication_url
FROM
    company_news_recipes AS recipe
    JOIN companies AS company ON company.id = recipe.company_id
    LEFT JOIN company_news_recipe_state AS state
        ON state.recipe_id = recipe.id
WHERE
    recipe.status = 'active'
    AND recipe.generated_by_run_id IS NOT NULL
    AND NOT COALESCE(state.rebuild_required, false)
    AND (
        lower(company.name)
            ~ '[.](com|net|org|co|io|ai)([^a-z]|$)'
        OR EXISTS (
            SELECT 1
            FROM jsonb_array_elements_text(company.aliases) AS alias(value)
            WHERE lower(alias.value)
                ~ '[.](com|net|org|co|io|ai)([^a-z]|$)'
        )
    );

CREATE TEMP TABLE domain_suffix_identity_revalidation_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH audit_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.domain_suffix_identity_revalidation_started',
        jsonb_build_object(
            'recipe_count',
                (
                    SELECT count(*)
                    FROM domain_suffix_identity_revalidation_targets
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM domain_suffix_identity_revalidation_targets
                ),
            'policy', 'company-host-identity.v2',
            'migration', '0116_fix_domain_suffix_host_identity'
        )
    WHERE EXISTS (
        SELECT 1
        FROM domain_suffix_identity_revalidation_targets
    )
    RETURNING id
)
INSERT INTO domain_suffix_identity_revalidation_wave (event_id)
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
    'domain_suffix_identity_revalidation_required',
    jsonb_build_object(
        'domain_suffix_identity_revalidation',
        jsonb_build_object(
            'policy', 'company-host-identity.v2',
            'publication_url', target.publication_url,
            'repair_wave_event_id', wave.event_id,
            'migration', '0116_fix_domain_suffix_host_identity'
        )
    )
FROM
    domain_suffix_identity_revalidation_targets AS target
    CROSS JOIN domain_suffix_identity_revalidation_wave AS wave
ON CONFLICT (recipe_id) DO UPDATE
SET
    consecutive_correctness_failures = GREATEST(
        company_news_recipe_state.consecutive_correctness_failures,
        1
    ),
    freshness_status = 'unknown',
    correctness_status = 'failing',
    rebuild_required = false,
    reason = 'domain_suffix_identity_revalidation_required',
    metadata = company_news_recipe_state.metadata || EXCLUDED.metadata,
    updated_at = CURRENT_TIMESTAMP;

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
        FROM domain_suffix_identity_revalidation_targets
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
        'reason', 'domain_suffix_identity_revalidation',
        'policy', 'company-host-identity.v2'
    )
FROM domain_suffix_identity_revalidation_targets AS target
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
    'company_news.recipe_domain_suffix_identity_revalidation_required',
    target.company_id,
    target.source_id,
    jsonb_build_object(
        'recipe_id', target.recipe_id,
        'publication_url', target.publication_url,
        'priority', 8192,
        'policy', 'company-host-identity.v2',
        'repair_wave_event_id', wave.event_id,
        'migration', '0116_fix_domain_suffix_host_identity'
    )
FROM
    domain_suffix_identity_revalidation_targets AS target
    CROSS JOIN domain_suffix_identity_revalidation_wave AS wave;
