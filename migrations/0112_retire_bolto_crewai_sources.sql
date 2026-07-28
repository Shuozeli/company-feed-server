-- Bolto used "Crew AI" as a historical launch name, but CrewAI at crewai.com
-- is a separate agent-platform company. Retire every remaining CrewAI source
-- and candidate attached to Bolto, and suppress the ambiguous historical alias
-- from future discovery requests.

CREATE TEMP TABLE wrong_bolto_crewai_sources
ON COMMIT DROP AS
SELECT
    source.id AS source_id,
    source.company_id,
    source.url,
    'CrewAI'::text AS claiming_company_name,
    'publication_owned_by_different_company'::text AS reason
FROM
    companies AS company
    JOIN sources AS source ON source.company_id = company.id
WHERE
    company.company_key = 'yc-bolto'
    AND source.status = 'approved'
    AND lower(source.url) ~ '^https?://([^/]+[.])?crewai[.]com(/|$)';

CREATE TEMP TABLE wrong_bolto_crewai_recipes
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec->>'publication_url' AS publication_url,
    repair.claiming_company_name,
    repair.reason
FROM
    wrong_bolto_crewai_sources AS repair
    JOIN company_news_recipes AS recipe
        ON recipe.source_id = repair.source_id
WHERE recipe.status IN ('active', 'stale');

CREATE TEMP TABLE wrong_bolto_crewai_candidates
ON COMMIT DROP AS
SELECT
    candidate.id AS candidate_id,
    candidate.company_id,
    candidate.accepted_source_id,
    candidate.candidate_url,
    candidate.status AS prior_status,
    'CrewAI'::text AS claiming_company_name,
    'publication_owned_by_different_company'::text AS reason
FROM
    companies AS company
    JOIN source_candidates AS candidate
        ON candidate.company_id = company.id
WHERE
    company.company_key = 'yc-bolto'
    AND candidate.status <> 'rejected'
    AND lower(candidate.candidate_url)
        ~ '^https?://([^/]+[.])?crewai[.]com(/|$)';

CREATE TEMP TABLE wrong_bolto_crewai_items
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
    wrong_bolto_crewai_sources AS repair
    JOIN feed_items AS item ON item.source_id = repair.source_id
WHERE NOT item.is_private;

CREATE TEMP TABLE wrong_bolto_crewai_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.wrong_cross_domain_repair_started',
        jsonb_build_object(
            'source_count',
                (SELECT count(*) FROM wrong_bolto_crewai_sources),
            'recipe_count',
                (SELECT count(*) FROM wrong_bolto_crewai_recipes),
            'candidate_count',
                (SELECT count(*) FROM wrong_bolto_crewai_candidates),
            'item_count',
                (SELECT count(*) FROM wrong_bolto_crewai_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM wrong_bolto_crewai_sources
                ),
            'policy', 'cross-domain-company-ownership.v2',
            'migration', '0112_retire_bolto_crewai_sources'
        )
    WHERE
        EXISTS (SELECT 1 FROM wrong_bolto_crewai_sources)
        OR EXISTS (SELECT 1 FROM wrong_bolto_crewai_candidates)
    RETURNING id
)
INSERT INTO wrong_bolto_crewai_wave (event_id)
SELECT id FROM repair_started;

UPDATE companies
SET
    aliases = aliases - 'Crew' - 'Crew AI',
    metadata = metadata || jsonb_build_object(
        'alias_correction',
        jsonb_build_object(
            'removed_aliases', jsonb_build_array('Crew', 'Crew AI'),
            'reason', 'historical_alias_conflicts_with_distinct_crewai_company',
            'claiming_company_name', 'CrewAI',
            'policy', 'cross-domain-company-ownership.v2',
            'corrected_at', CURRENT_TIMESTAMP,
            'migration', '0112_retire_bolto_crewai_sources'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
WHERE company_key = 'yc-bolto';

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = repair.reason
FROM wrong_bolto_crewai_recipes AS repair
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
            'policy', 'cross-domain-company-ownership.v2',
            'claiming_company_name', repair.claiming_company_name,
            'repair_wave_event_id', wave.event_id,
            'repaired_at', CURRENT_TIMESTAMP,
            'migration', '0112_retire_bolto_crewai_sources'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    wrong_bolto_crewai_recipes AS repair
    CROSS JOIN wrong_bolto_crewai_wave AS wave
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
                'policy', 'cross-domain-company-ownership.v2',
                'claiming_company_name', repair.claiming_company_name,
                'repair_wave_event_id', wave.event_id,
                'disabled_at', CURRENT_TIMESTAMP,
                'migration', '0112_retire_bolto_crewai_sources'
            )
        )
FROM
    wrong_bolto_crewai_sources AS repair
    CROSS JOIN wrong_bolto_crewai_wave AS wave
WHERE source.id = repair.source_id;

UPDATE source_candidates AS candidate
SET
    status = 'rejected',
    accepted_source_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM wrong_bolto_crewai_candidates AS repair
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
    'migration:0112',
    'candidate belongs to the distinct CrewAI company',
    jsonb_build_object(
        'prior_status', repair.prior_status,
        'claiming_company_name', repair.claiming_company_name,
        'policy', 'cross-domain-company-ownership.v2',
        'repair_wave_event_id', wave.event_id,
        'migration', '0112_retire_bolto_crewai_sources'
    )
FROM
    wrong_bolto_crewai_candidates AS repair
    CROSS JOIN wrong_bolto_crewai_wave AS wave;

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
            SELECT source_id FROM wrong_bolto_crewai_sources
        )
        OR job.candidate_id IN (
            SELECT candidate_id FROM wrong_bolto_crewai_candidates
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
            'policy', 'cross-domain-company-ownership.v2',
            'claiming_company_name', repair.claiming_company_name,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    wrong_bolto_crewai_items AS repair
    CROSS JOIN wrong_bolto_crewai_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM wrong_bolto_crewai_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

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
        'policy', 'cross-domain-company-ownership.v2',
        'repair_wave_event_id', wave.event_id,
        'migration', '0112_retire_bolto_crewai_sources'
    )
FROM
    wrong_bolto_crewai_sources AS repair
    CROSS JOIN wrong_bolto_crewai_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source_candidate.rejected',
    repair.company_id,
    repair.accepted_source_id,
    jsonb_build_object(
        'candidate_id', repair.candidate_id,
        'candidate_url', repair.candidate_url,
        'prior_status', repair.prior_status,
        'claiming_company_name', repair.claiming_company_name,
        'reason', repair.reason,
        'decision_mode', 'automatic',
        'actor', 'migration:0112',
        'policy', 'cross-domain-company-ownership.v2',
        'repair_wave_event_id', wave.event_id,
        'migration', '0112_retire_bolto_crewai_sources'
    )
FROM
    wrong_bolto_crewai_candidates AS repair
    CROSS JOIN wrong_bolto_crewai_wave AS wave;

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
        'policy', 'cross-domain-company-ownership.v2',
        'repair_wave_event_id', wave.event_id,
        'migration', '0112_retire_bolto_crewai_sources'
    )
FROM
    wrong_bolto_crewai_items AS repair
    CROSS JOIN wrong_bolto_crewai_wave AS wave;
