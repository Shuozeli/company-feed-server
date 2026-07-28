-- Close the remaining content-integrity gaps found by the terminal campaign
-- audit.
--
-- * Discourse discussion/category URLs are community forums, not company news
--   publications. Disable every currently accepted `/discuss/` source and
--   quarantine its historical rows under the generic non-editorial policy.
-- * Four reviewed first-party publications have lost editorial integrity.
--   Disable their affected sources and retain all observed rows privately.
-- * RADCOM's WordPress feed was compromised from 2026-07-24 19:19 UTC through
--   2026-07-27 08:14 UTC, then recovered. Quarantine only that exact observed
--   attack window, retain the ten clean company articles, keep the recovered
--   source approved, and revalidate it with the runtime compromise guard.

CREATE TEMP TABLE audited_quality_sources (
    source_id uuid PRIMARY KEY,
    company_id uuid NOT NULL,
    previous_status text NOT NULL,
    url text NOT NULL,
    reason text NOT NULL,
    policy text NOT NULL,
    disable_source boolean NOT NULL
) ON COMMIT DROP;

INSERT INTO audited_quality_sources (
    source_id,
    company_id,
    previous_status,
    url,
    reason,
    policy,
    disable_source
)
SELECT
    source.id,
    source.company_id,
    source.status,
    source.url,
    'non_editorial_discussion_scope',
    'non-editorial-feed-scope.v2',
    true
FROM sources AS source
WHERE
    source.status <> 'disabled'
    AND lower(source.url) ~ '/discuss(/|$)';

INSERT INTO audited_quality_sources (
    source_id,
    company_id,
    previous_status,
    url,
    reason,
    policy,
    disable_source
)
SELECT
    source.id,
    source.company_id,
    source.status,
    source.url,
    'publication_topic_compromise_detected',
    'publication-topic-compromise.v1',
    true
FROM sources AS source
WHERE lower(regexp_replace(source.url, '/+$', '')) IN (
    'https://cel-sci.com/feed',
    'https://infina.vn',
    'https://www.cerus.com/feed',
    'https://www.reitar.io/feed'
)
ON CONFLICT (source_id) DO NOTHING;

-- RADCOM recovered after the reviewed attack window, so this incident source
-- participates in quarantine and audit events without being disabled.
INSERT INTO audited_quality_sources (
    source_id,
    company_id,
    previous_status,
    url,
    reason,
    policy,
    disable_source
)
SELECT
    source.id,
    source.company_id,
    source.status,
    source.url,
    'publication_topic_compromise_attack_window',
    'publication-topic-compromise.v1',
    false
FROM sources AS source
WHERE
    lower(regexp_replace(source.url, '/+$', ''))
        = 'https://radcom.com/feed'
ON CONFLICT (source_id) DO NOTHING;

CREATE TEMP TABLE audited_quality_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    audit.reason,
    audit.policy,
    item.url,
    item.canonical_url,
    item.title,
    item.published_at
FROM
    feed_items AS item
    JOIN audited_quality_sources AS audit
        ON audit.source_id = item.source_id
WHERE
    NOT item.is_private
    AND (
        audit.disable_source
        OR (
            lower(regexp_replace(audit.url, '/+$', ''))
                = 'https://radcom.com/feed'
            AND item.published_at >= timestamptz '2026-07-24 19:00:00+00'
            AND item.published_at < timestamptz '2026-07-28 00:00:00+00'
        )
    );

CREATE TEMP TABLE audited_quality_candidates
ON COMMIT DROP AS
SELECT
    candidate.id AS candidate_id,
    audit.source_id,
    audit.company_id,
    audit.reason,
    audit.policy
FROM
    audited_quality_sources AS audit
    JOIN source_candidates AS candidate
        ON candidate.accepted_source_id = audit.source_id
WHERE audit.disable_source;

CREATE TEMP TABLE audited_quality_recipes
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    audit.source_id,
    audit.company_id,
    audit.reason,
    audit.policy
FROM
    audited_quality_sources AS audit
    JOIN company_news_recipes AS recipe
        ON recipe.source_id = audit.source_id
WHERE
    audit.disable_source
    AND recipe.status IN ('active', 'stale');

CREATE TEMP TABLE audited_quality_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH audit_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.terminal_quality_repair_started',
        jsonb_build_object(
            'source_count',
                (SELECT count(*) FROM audited_quality_sources),
            'disabled_source_count',
                (
                    SELECT count(*)
                    FROM audited_quality_sources
                    WHERE disable_source
                ),
            'item_count',
                (SELECT count(*) FROM audited_quality_items),
            'discussion_source_count',
                (
                    SELECT count(*)
                    FROM audited_quality_sources
                    WHERE reason = 'non_editorial_discussion_scope'
                ),
            'topic_compromise_source_count',
                (
                    SELECT count(*)
                    FROM audited_quality_sources
                    WHERE reason LIKE 'publication_topic_compromise%'
                ),
            'migration',
                '0135_quarantine_discussion_and_compromised_publications'
        )
    WHERE EXISTS (
        SELECT 1 FROM audited_quality_sources
    )
    RETURNING id
)
INSERT INTO audited_quality_wave (event_id)
SELECT id FROM audit_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = COALESCE(
        item.content_processing,
        '{}'::jsonb
    ) || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', repair.policy,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP,
            'migration',
                '0135_quarantine_discussion_and_compromised_publications'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    audited_quality_items AS repair
    CROSS JOIN audited_quality_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM audited_quality_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

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
        'reversible', true,
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0135_quarantine_discussion_and_compromised_publications'
    )
FROM
    audited_quality_items AS repair
    CROSS JOIN audited_quality_wave AS wave;

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = COALESCE(recipe.stale_at, CURRENT_TIMESTAMP),
    stale_reason = repair.reason,
    updated_at = CURRENT_TIMESTAMP
FROM audited_quality_recipes AS repair
WHERE
    recipe.id = repair.recipe_id
    AND recipe.status = 'active';

UPDATE company_news_recipe_state AS state
SET
    correctness_status = 'failing',
    rebuild_required = true,
    consecutive_correctness_failures =
        GREATEST(state.consecutive_correctness_failures, 3),
    reason = repair.reason,
    metadata = state.metadata || jsonb_build_object(
        'terminal_quality_repair',
        jsonb_build_object(
            'reason', repair.reason,
            'policy', repair.policy,
            'detected_at', CURRENT_TIMESTAMP,
            'migration',
                '0135_quarantine_discussion_and_compromised_publications'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM audited_quality_recipes AS repair
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
                'reversible', true,
                'policy', repair.policy,
                'previous_status', repair.previous_status,
                'repair_wave_event_id', wave.event_id,
                'disabled_at', CURRENT_TIMESTAMP,
                'migration',
                    '0135_quarantine_discussion_and_compromised_publications'
            )
        ),
    updated_at = CURRENT_TIMESTAMP
FROM
    audited_quality_sources AS repair
    CROSS JOIN audited_quality_wave AS wave
WHERE
    source.id = repair.source_id
    AND repair.disable_source;

-- Keep a structured record on the recovered RADCOM source without disabling
-- it or disturbing its current healthy source state.
UPDATE sources AS source
SET
    metadata = source.metadata || jsonb_build_object(
        'publication_quality_incident',
        jsonb_build_object(
            'state', 'recovered_pending_revalidation',
            'reason', repair.reason,
            'policy', repair.policy,
            'attack_window_start', '2026-07-24T19:00:00Z',
            'attack_window_end', '2026-07-28T00:00:00Z',
            'repair_wave_event_id', wave.event_id,
            'recorded_at', CURRENT_TIMESTAMP,
            'migration',
                '0135_quarantine_discussion_and_compromised_publications'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    audited_quality_sources AS repair
    CROSS JOIN audited_quality_wave AS wave
WHERE
    source.id = repair.source_id
    AND NOT repair.disable_source;

UPDATE source_state AS state
SET
    last_attempt_at = CURRENT_TIMESTAMP,
    last_error = repair.reason,
    consecutive_failures = GREATEST(state.consecutive_failures, 1),
    backoff_until = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM audited_quality_sources AS repair
WHERE
    state.source_id = repair.source_id
    AND repair.disable_source;

UPDATE source_candidates AS candidate
SET
    status = 'rejected',
    accepted_source_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM audited_quality_candidates AS repair
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
    'migration:0135',
    repair.reason,
    jsonb_build_object(
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0135_quarantine_discussion_and_compromised_publications'
    )
FROM
    audited_quality_candidates AS repair
    CROSS JOIN audited_quality_wave AS wave;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled: source disabled by terminal quality repair',
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM audited_quality_sources AS repair
WHERE
    job.source_id = repair.source_id
    AND job.job_type = 'crawl_source'
    AND job.status IN ('pending', 'running')
    AND repair.disable_source;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.quality_disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.url,
        'previous_status', repair.previous_status,
        'reason', repair.reason,
        'reversible', true,
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0135_quarantine_discussion_and_compromised_publications'
    )
FROM
    audited_quality_sources AS repair
    CROSS JOIN audited_quality_wave AS wave
WHERE repair.disable_source;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.publication_topic_compromise_window_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.url,
        'attack_window_start', '2026-07-24T19:00:00Z',
        'attack_window_end', '2026-07-28T00:00:00Z',
        'quarantined_item_count',
            (
                SELECT count(*)
                FROM audited_quality_items AS item
                WHERE item.source_id = repair.source_id
            ),
        'reason', repair.reason,
        'reversible', true,
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0135_quarantine_discussion_and_compromised_publications'
    )
FROM
    audited_quality_sources AS repair
    CROSS JOIN audited_quality_wave AS wave
WHERE NOT repair.disable_source;

-- Revalidate the recovered RADCOM feed immediately. Existing active work is
-- promoted instead of duplicated.
UPDATE jobs AS job
SET
    priority = GREATEST(job.priority, 16384),
    run_after = LEAST(job.run_after, CURRENT_TIMESTAMP),
    payload = job.payload || jsonb_build_object(
        'trigger', 'publication_topic_compromise_recovery_revalidation',
        'policy', 'publication-topic-compromise.v1',
        'migration',
            '0135_quarantine_discussion_and_compromised_publications'
    ),
    updated_at = CURRENT_TIMESTAMP
FROM audited_quality_sources AS repair
WHERE
    job.source_id = repair.source_id
    AND job.job_type = 'crawl_source'
    AND job.status IN ('pending', 'running')
    AND NOT repair.disable_source;

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
    'source:' || repair.source_id::text,
    'pending',
    16384,
    CURRENT_TIMESTAMP,
    5,
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'source_id', repair.source_id,
        'trigger', 'publication_topic_compromise_recovery_revalidation',
        'policy', repair.policy,
        'migration',
            '0135_quarantine_discussion_and_compromised_publications'
    )
FROM audited_quality_sources AS repair
WHERE
    NOT repair.disable_source
    AND NOT EXISTS (
        SELECT 1
        FROM jobs AS active_job
        WHERE
            active_job.job_type = 'crawl_source'
            AND active_job.job_key = 'source:' || repair.source_id::text
            AND active_job.status IN ('pending', 'running')
    )
ON CONFLICT (job_type, job_key)
    WHERE status IN ('pending', 'running')
DO NOTHING;
