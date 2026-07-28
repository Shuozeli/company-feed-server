-- Source discovery produces untrusted candidates. Validation is a separate,
-- durable stage that may automatically activate a technically valid official
-- RSS/Atom source or route an ambiguous result to operator review.

ALTER TABLE jobs
DROP CONSTRAINT jobs_type_valid;

ALTER TABLE jobs
ADD COLUMN candidate_id uuid REFERENCES source_candidates(id) ON DELETE CASCADE;

ALTER TABLE jobs
ADD CONSTRAINT jobs_type_valid CHECK (
    job_type IN (
        'discover_company',
        'validate_candidate',
        'crawl_source',
        'export_target',
        'normalize_backfill'
    )
);

CREATE INDEX jobs_candidate_idx
ON jobs (candidate_id, created_at DESC)
WHERE candidate_id IS NOT NULL;

CREATE TABLE candidate_validation_runs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    candidate_id uuid NOT NULL REFERENCES source_candidates(id) ON DELETE CASCADE,
    job_id uuid REFERENCES jobs(id) ON DELETE SET NULL,
    started_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at timestamptz,
    status text NOT NULL DEFAULT 'running',
    detected_kind text,
    final_url text,
    http_status integer,
    item_count integer NOT NULL DEFAULT 0,
    titled_item_count integer NOT NULL DEFAULT 0,
    latest_item_at timestamptz,
    policy_reasons jsonb NOT NULL DEFAULT '[]'::jsonb,
    error text,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT candidate_validation_runs_status_valid CHECK (
        status IN (
            'running',
            'valid',
            'needs_review',
            'invalid',
            'failed',
            'cancelled'
        )
    ),
    CONSTRAINT candidate_validation_runs_kind_valid CHECK (
        detected_kind IS NULL OR detected_kind IN ('rss', 'atom')
    ),
    CONSTRAINT candidate_validation_runs_http_status_valid CHECK (
        http_status IS NULL OR http_status BETWEEN 100 AND 599
    ),
    CONSTRAINT candidate_validation_runs_item_count_nonnegative CHECK (item_count >= 0),
    CONSTRAINT candidate_validation_runs_titled_count_nonnegative CHECK (
        titled_item_count >= 0
    ),
    CONSTRAINT candidate_validation_runs_titled_count_bounded CHECK (
        titled_item_count <= item_count
    ),
    CONSTRAINT candidate_validation_runs_policy_reasons_array CHECK (
        jsonb_typeof(policy_reasons) = 'array'
    ),
    CONSTRAINT candidate_validation_runs_finished_state CHECK (
        (status = 'running' AND finished_at IS NULL)
        OR (status <> 'running' AND finished_at IS NOT NULL)
    )
);

CREATE INDEX candidate_validation_runs_candidate_started_idx
ON candidate_validation_runs (candidate_id, started_at DESC, id DESC);

CREATE UNIQUE INDEX candidate_validation_runs_one_running_idx
ON candidate_validation_runs (candidate_id)
WHERE status = 'running';

CREATE TABLE candidate_decisions (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    candidate_id uuid NOT NULL REFERENCES source_candidates(id) ON DELETE CASCADE,
    source_id uuid REFERENCES sources(id) ON DELETE SET NULL,
    decision text NOT NULL,
    decision_mode text NOT NULL,
    actor text NOT NULL,
    reason text NOT NULL,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT candidate_decisions_decision_valid CHECK (
        decision IN ('activated', 'rejected', 'kept_for_review')
    ),
    CONSTRAINT candidate_decisions_mode_valid CHECK (
        decision_mode IN ('automatic', 'operator')
    ),
    CONSTRAINT candidate_decisions_actor_not_blank CHECK (btrim(actor) <> ''),
    CONSTRAINT candidate_decisions_reason_not_blank CHECK (btrim(reason) <> '')
);

CREATE INDEX candidate_decisions_candidate_created_idx
ON candidate_decisions (candidate_id, created_at DESC, id DESC);
