-- Retire two host associations whose current publication contents belong to
-- a distinct imported company:
--
-- * Public Storage sources assigned to National Storage Affiliates;
-- * Lattice sources assigned to Maven.
--
-- Ownership quarantines are intentionally non-replay-safe. The retained rows
-- remain available for audit, while the excluded-host profile prevents a
-- later adapter result from reviving the same association.

CREATE TEMP TABLE wrong_company_host_sources
ON COMMIT DROP AS
WITH source_scope AS (
    SELECT
        source.id AS source_id,
        source.company_id,
        source.url AS source_url,
        company.company_key,
        lower(
            regexp_replace(
                split_part(split_part(source.url, '://', 2), '/', 1),
                '^www\.',
                ''
            )
        ) AS source_host
    FROM
        sources AS source
        JOIN companies AS company ON company.id = source.company_id
    WHERE source.status = 'approved'
)
SELECT
    source_id,
    company_id,
    source_url,
    source_host,
    CASE
        WHEN source_host = 'publicstorage.com'
            OR source_host LIKE '%.publicstorage.com'
        THEN 'Public Storage Common Stock'
        ELSE 'Lattice'
    END AS claiming_company_name,
    'publication_owned_by_different_company'::text AS reason,
    'cross-company-source-ownership.v3'::text AS policy
FROM source_scope
WHERE
    (
        company_key =
            'national-storage-affiliates-trust-common-shares-of-beneficial-interest'
        AND (
            source_host = 'publicstorage.com'
            OR source_host LIKE '%.publicstorage.com'
        )
    )
    OR (
        company_key = 'yc-maven'
        AND (
            source_host = 'lattice.com'
            OR source_host LIKE '%.lattice.com'
        )
    );

CREATE TEMP TABLE wrong_company_host_candidates
ON COMMIT DROP AS
SELECT
    candidate.id AS candidate_id,
    candidate.accepted_source_id AS source_id,
    repair.company_id,
    candidate.candidate_url,
    repair.claiming_company_name,
    repair.reason,
    repair.policy
FROM
    wrong_company_host_sources AS repair
    JOIN source_candidates AS candidate
        ON candidate.accepted_source_id = repair.source_id
WHERE candidate.status = 'accepted';

CREATE TEMP TABLE wrong_company_host_recipes
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec ->> 'publication_url' AS publication_url,
    repair.claiming_company_name,
    repair.reason,
    repair.policy
FROM
    wrong_company_host_sources AS repair
    JOIN company_news_recipes AS recipe
        ON recipe.source_id = repair.source_id
WHERE recipe.status IN ('active', 'stale');

CREATE TEMP TABLE wrong_company_host_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.url,
    item.canonical_url,
    item.title,
    item.published_at,
    repair.claiming_company_name,
    repair.reason,
    repair.policy
FROM
    wrong_company_host_sources AS repair
    JOIN feed_items AS item ON item.source_id = repair.source_id;

CREATE TEMP TABLE wrong_company_host_exclusions
ON COMMIT DROP AS
SELECT DISTINCT
    company_id,
    CASE
        WHEN source_host = 'publicstorage.com'
            OR source_host LIKE '%.publicstorage.com'
        THEN 'publicstorage.com'
        ELSE 'lattice.com'
    END AS excluded_host
FROM wrong_company_host_sources;

CREATE TEMP TABLE wrong_company_host_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.wrong_company_host_repair_started',
        jsonb_build_object(
            'source_count', (SELECT count(*) FROM wrong_company_host_sources),
            'candidate_count',
                (SELECT count(*) FROM wrong_company_host_candidates),
            'recipe_count', (SELECT count(*) FROM wrong_company_host_recipes),
            'item_count', (SELECT count(*) FROM wrong_company_host_items),
            'public_item_count',
                (
                    SELECT count(*)
                    FROM
                        wrong_company_host_items AS repair
                        JOIN feed_items AS item
                            ON item.id = repair.feed_item_id
                    WHERE NOT item.is_private
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM wrong_company_host_sources
                ),
            'reversible', false,
            'policy', 'cross-company-source-ownership.v3',
            'migration', '0123_quarantine_cross_company_host_collisions'
        )
    WHERE EXISTS (SELECT 1 FROM wrong_company_host_sources)
    RETURNING id
)
INSERT INTO wrong_company_host_wave (event_id)
SELECT id FROM repair_started;

UPDATE companies AS company
SET
    metadata = company.metadata || jsonb_build_object(
        'publication_host_policy',
        coalesce(
            company.metadata -> 'publication_host_policy',
            '{}'::jsonb
        ) || jsonb_build_object(
            'policy', 'company-publication-host-policy.v5',
            'excluded_hosts',
                CASE
                    WHEN coalesce(
                        company.metadata
                            -> 'publication_host_policy'
                            -> 'excluded_hosts',
                        '[]'::jsonb
                    ) @> jsonb_build_array(exclusion.excluded_host)
                    THEN coalesce(
                        company.metadata
                            -> 'publication_host_policy'
                            -> 'excluded_hosts',
                        '[]'::jsonb
                    )
                    ELSE coalesce(
                        company.metadata
                            -> 'publication_host_policy'
                            -> 'excluded_hosts',
                        '[]'::jsonb
                    ) || jsonb_build_array(exclusion.excluded_host)
                END,
            'direct_evidence_excluded_hosts',
                CASE
                    WHEN coalesce(
                        company.metadata
                            -> 'publication_host_policy'
                            -> 'direct_evidence_excluded_hosts',
                        '[]'::jsonb
                    ) @> jsonb_build_array(exclusion.excluded_host)
                    THEN coalesce(
                        company.metadata
                            -> 'publication_host_policy'
                            -> 'direct_evidence_excluded_hosts',
                        '[]'::jsonb
                    )
                    ELSE coalesce(
                        company.metadata
                            -> 'publication_host_policy'
                            -> 'direct_evidence_excluded_hosts',
                        '[]'::jsonb
                    ) || jsonb_build_array(exclusion.excluded_host)
                END,
            'reviewed_at', CURRENT_TIMESTAMP,
            'migration', '0123_quarantine_cross_company_host_collisions'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM wrong_company_host_exclusions AS exclusion
WHERE company.id = exclusion.company_id;

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = coalesce(recipe.stale_at, CURRENT_TIMESTAMP),
    stale_reason = repair.reason,
    updated_at = CURRENT_TIMESTAMP
FROM wrong_company_host_recipes AS repair
WHERE recipe.id = repair.recipe_id;

UPDATE company_news_recipe_state AS state
SET
    correctness_status = 'failing',
    rebuild_required = true,
    consecutive_correctness_failures = greatest(
        state.consecutive_correctness_failures,
        3
    ),
    reason = repair.reason,
    metadata = state.metadata || jsonb_build_object(
        'ownership_repair',
        jsonb_build_object(
            'policy', repair.policy,
            'claiming_company_name', repair.claiming_company_name,
            'repair_wave_event_id', wave.event_id,
            'repaired_at', CURRENT_TIMESTAMP,
            'migration', '0123_quarantine_cross_company_host_collisions'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    wrong_company_host_recipes AS repair
    CROSS JOIN wrong_company_host_wave AS wave
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
                'policy', repair.policy,
                'claiming_company_name', repair.claiming_company_name,
                'repair_wave_event_id', wave.event_id,
                'disabled_at', CURRENT_TIMESTAMP,
                'migration', '0123_quarantine_cross_company_host_collisions'
            )
        ),
    updated_at = CURRENT_TIMESTAMP
FROM
    wrong_company_host_sources AS repair
    CROSS JOIN wrong_company_host_wave AS wave
WHERE source.id = repair.source_id;

UPDATE source_state AS state
SET
    last_error = 'disabled: publication_owned_by_different_company',
    backoff_until = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM wrong_company_host_sources AS repair
WHERE state.source_id = repair.source_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled because source belongs to a different company',
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM wrong_company_host_sources AS repair
WHERE
    job.source_id = repair.source_id
    AND job.status IN ('pending', 'running');

UPDATE source_candidates AS candidate
SET
    status = 'rejected',
    accepted_source_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM wrong_company_host_candidates AS repair
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
    repair.source_id,
    'rejected',
    'automatic',
    'migration:0123',
    'publication host and content belong to a distinct imported company',
    jsonb_build_object(
        'reason', repair.reason,
        'reversible', false,
        'policy', repair.policy,
        'claiming_company_name', repair.claiming_company_name,
        'repair_wave_event_id', wave.event_id,
        'migration', '0123_quarantine_cross_company_host_collisions'
    )
FROM
    wrong_company_host_candidates AS repair
    CROSS JOIN wrong_company_host_wave AS wave;

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
            'policy', repair.policy,
            'claiming_company_name', repair.claiming_company_name,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP,
            'migration', '0123_quarantine_cross_company_host_collisions'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    wrong_company_host_items AS repair
    CROSS JOIN wrong_company_host_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: publication_owned_by_different_company',
    normalized_feed_item_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM wrong_company_host_sources AS repair
WHERE raw.source_id = repair.source_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.quality_disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.source_url,
        'reason', repair.reason,
        'reversible', false,
        'policy', repair.policy,
        'claiming_company_name', repair.claiming_company_name,
        'repair_wave_event_id', wave.event_id,
        'migration', '0123_quarantine_cross_company_host_collisions'
    )
FROM
    wrong_company_host_sources AS repair
    CROSS JOIN wrong_company_host_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_stale',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'recipe_id', repair.recipe_id,
        'publication_url', repair.publication_url,
        'reason', repair.reason,
        'rebuild_required', true,
        'reversible', false,
        'policy', repair.policy,
        'claiming_company_name', repair.claiming_company_name,
        'repair_wave_event_id', wave.event_id,
        'migration', '0123_quarantine_cross_company_host_collisions'
    )
FROM
    wrong_company_host_recipes AS repair
    CROSS JOIN wrong_company_host_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'raw_crawl_item_id', repair.raw_crawl_item_id,
        'url', repair.url,
        'canonical_url', repair.canonical_url,
        'title', repair.title,
        'published_at', repair.published_at,
        'reason', repair.reason,
        'reversible', false,
        'policy', repair.policy,
        'claiming_company_name', repair.claiming_company_name,
        'repair_wave_event_id', wave.event_id,
        'migration', '0123_quarantine_cross_company_host_collisions'
    )
FROM
    wrong_company_host_items AS repair
    CROSS JOIN wrong_company_host_wave AS wave;
