CREATE FUNCTION set_updated_at()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.updated_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$;

CREATE TABLE companies (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    ticker text NOT NULL UNIQUE,
    name text NOT NULL,
    homepage_url text,
    investor_relations_url text,
    newsroom_url text,
    blog_url text,
    hints jsonb NOT NULL DEFAULT '[]'::jsonb,
    discovery_cadence_seconds integer NOT NULL DEFAULT 604800,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT companies_ticker_not_blank CHECK (btrim(ticker) <> ''),
    CONSTRAINT companies_ticker_uppercase CHECK (ticker = upper(ticker)),
    CONSTRAINT companies_name_not_blank CHECK (btrim(name) <> ''),
    CONSTRAINT companies_discovery_cadence_positive CHECK (discovery_cadence_seconds > 0)
);

CREATE TRIGGER companies_set_updated_at
BEFORE UPDATE ON companies
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE sources (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_id text NOT NULL UNIQUE,
    company_id uuid NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    kind text NOT NULL,
    url text NOT NULL,
    status text NOT NULL DEFAULT 'approved',
    freshness_slo_seconds integer NOT NULL DEFAULT 3600,
    browser_required boolean NOT NULL DEFAULT false,
    public_export_allowed boolean NOT NULL DEFAULT false,
    discovery_confidence double precision,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT sources_source_id_not_blank CHECK (btrim(source_id) <> ''),
    CONSTRAINT sources_url_not_blank CHECK (btrim(url) <> ''),
    CONSTRAINT sources_kind_valid CHECK (kind IN ('rss', 'atom', 'html', 'browser')),
    CONSTRAINT sources_status_valid CHECK (status IN ('approved', 'disabled')),
    CONSTRAINT sources_freshness_slo_positive CHECK (freshness_slo_seconds > 0),
    CONSTRAINT sources_discovery_confidence_range CHECK (
        discovery_confidence IS NULL
        OR discovery_confidence BETWEEN 0.0 AND 1.0
    ),
    UNIQUE (company_id, url)
);

CREATE INDEX sources_company_idx ON sources (company_id);
CREATE INDEX sources_approved_idx
    ON sources (company_id, freshness_slo_seconds)
    WHERE status = 'approved';

CREATE TRIGGER sources_set_updated_at
BEFORE UPDATE ON sources
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE source_state (
    source_id uuid PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
    last_attempt_at timestamptz,
    last_success_at timestamptz,
    last_error text,
    consecutive_failures integer NOT NULL DEFAULT 0,
    backoff_until timestamptz,
    cursor jsonb NOT NULL DEFAULT '{}'::jsonb,
    consecutive_zero_runs integer NOT NULL DEFAULT 0,
    total_successful_runs bigint NOT NULL DEFAULT 0,
    total_items bigint NOT NULL DEFAULT 0,
    last_nonzero_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT source_state_failures_nonnegative CHECK (consecutive_failures >= 0),
    CONSTRAINT source_state_zero_runs_nonnegative CHECK (consecutive_zero_runs >= 0),
    CONSTRAINT source_state_successful_runs_nonnegative CHECK (total_successful_runs >= 0),
    CONSTRAINT source_state_total_items_nonnegative CHECK (total_items >= 0)
);

CREATE TRIGGER source_state_set_updated_at
BEFORE UPDATE ON source_state
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE export_targets (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    target_id text NOT NULL UNIQUE,
    repo_url text NOT NULL,
    local_path text NOT NULL,
    branch text NOT NULL DEFAULT 'main',
    format text NOT NULL,
    layout text NOT NULL,
    cadence_seconds integer NOT NULL DEFAULT 3600,
    enabled boolean NOT NULL DEFAULT true,
    push_enabled boolean NOT NULL DEFAULT false,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    last_scheduled_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT export_targets_target_id_not_blank CHECK (btrim(target_id) <> ''),
    CONSTRAINT export_targets_repo_url_not_blank CHECK (btrim(repo_url) <> ''),
    CONSTRAINT export_targets_local_path_not_blank CHECK (btrim(local_path) <> ''),
    CONSTRAINT export_targets_branch_not_blank CHECK (btrim(branch) <> ''),
    CONSTRAINT export_targets_format_valid CHECK (format IN ('markdown_json', 'jsonl')),
    CONSTRAINT export_targets_layout_valid CHECK (layout IN ('by_company_date')),
    CONSTRAINT export_targets_cadence_positive CHECK (cadence_seconds > 0)
);

CREATE INDEX export_targets_enabled_idx
    ON export_targets (last_scheduled_at)
    WHERE enabled;

CREATE TRIGGER export_targets_set_updated_at
BEFORE UPDATE ON export_targets
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE jobs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    job_type text NOT NULL,
    job_key text NOT NULL,
    status text NOT NULL DEFAULT 'pending',
    priority smallint NOT NULL DEFAULT 0,
    run_after timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    locked_by text,
    locked_at timestamptz,
    heartbeat_at timestamptz,
    lease_until timestamptz,
    lease_token uuid,
    attempt_count integer NOT NULL DEFAULT 0,
    max_attempts integer NOT NULL DEFAULT 5,
    company_id uuid REFERENCES companies(id) ON DELETE CASCADE,
    source_id uuid REFERENCES sources(id) ON DELETE CASCADE,
    export_target_id uuid REFERENCES export_targets(id) ON DELETE CASCADE,
    payload jsonb NOT NULL DEFAULT '{}'::jsonb,
    last_error text,
    completed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT jobs_type_valid CHECK (
        job_type IN (
            'discover_company',
            'crawl_source',
            'export_target',
            'normalize_backfill'
        )
    ),
    CONSTRAINT jobs_key_not_blank CHECK (btrim(job_key) <> ''),
    CONSTRAINT jobs_status_valid CHECK (
        status IN ('pending', 'running', 'completed', 'failed', 'cancelled')
    ),
    CONSTRAINT jobs_attempt_count_nonnegative CHECK (attempt_count >= 0),
    CONSTRAINT jobs_max_attempts_positive CHECK (max_attempts > 0),
    CONSTRAINT jobs_attempt_count_bounded CHECK (attempt_count <= max_attempts),
    CONSTRAINT jobs_running_has_lease CHECK (
        status <> 'running'
        OR (
            locked_by IS NOT NULL
            AND locked_at IS NOT NULL
            AND heartbeat_at IS NOT NULL
            AND lease_until IS NOT NULL
            AND lease_token IS NOT NULL
        )
    )
);

CREATE UNIQUE INDEX jobs_one_active_key_idx
    ON jobs (job_type, job_key)
    WHERE status IN ('pending', 'running');

CREATE INDEX jobs_due_idx
    ON jobs (priority DESC, run_after, created_at)
    WHERE status = 'pending';

CREATE INDEX jobs_expired_lease_idx
    ON jobs (lease_until)
    WHERE status = 'running';

CREATE TRIGGER jobs_set_updated_at
BEFORE UPDATE ON jobs
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE discovery_runs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id uuid NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    job_id uuid REFERENCES jobs(id) ON DELETE SET NULL,
    started_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at timestamptz,
    status text NOT NULL DEFAULT 'running',
    candidate_count integer NOT NULL DEFAULT 0,
    error text,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT discovery_runs_status_valid CHECK (
        status IN ('running', 'completed', 'failed', 'cancelled')
    ),
    CONSTRAINT discovery_runs_candidate_count_nonnegative CHECK (candidate_count >= 0),
    CONSTRAINT discovery_runs_finished_state CHECK (
        (status = 'running' AND finished_at IS NULL)
        OR (status <> 'running' AND finished_at IS NOT NULL)
    )
);

CREATE INDEX discovery_runs_company_started_idx
    ON discovery_runs (company_id, started_at DESC);

CREATE TABLE source_candidates (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id uuid NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    discovery_run_id uuid REFERENCES discovery_runs(id) ON DELETE SET NULL,
    candidate_url text NOT NULL,
    candidate_kind text NOT NULL,
    confidence double precision NOT NULL,
    evidence jsonb NOT NULL DEFAULT '{}'::jsonb,
    status text NOT NULL DEFAULT 'new',
    accepted_source_id uuid REFERENCES sources(id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT source_candidates_url_not_blank CHECK (btrim(candidate_url) <> ''),
    CONSTRAINT source_candidates_kind_valid CHECK (
        candidate_kind IN ('rss', 'atom', 'html', 'browser')
    ),
    CONSTRAINT source_candidates_confidence_range CHECK (confidence BETWEEN 0.0 AND 1.0),
    CONSTRAINT source_candidates_status_valid CHECK (status IN ('new', 'accepted', 'rejected')),
    CONSTRAINT source_candidates_acceptance_consistent CHECK (
        (status = 'accepted' AND accepted_source_id IS NOT NULL)
        OR (status <> 'accepted' AND accepted_source_id IS NULL)
    ),
    UNIQUE (company_id, candidate_url, candidate_kind)
);

CREATE INDEX source_candidates_review_idx
    ON source_candidates (company_id, confidence DESC, created_at)
    WHERE status = 'new';

CREATE TRIGGER source_candidates_set_updated_at
BEFORE UPDATE ON source_candidates
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE crawl_runs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_id uuid NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    job_id uuid REFERENCES jobs(id) ON DELETE SET NULL,
    started_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at timestamptz,
    status text NOT NULL DEFAULT 'running',
    item_count integer NOT NULL DEFAULT 0,
    new_item_count integer NOT NULL DEFAULT 0,
    error text,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT crawl_runs_status_valid CHECK (
        status IN ('running', 'completed', 'failed', 'cancelled')
    ),
    CONSTRAINT crawl_runs_item_count_nonnegative CHECK (item_count >= 0),
    CONSTRAINT crawl_runs_new_item_count_nonnegative CHECK (new_item_count >= 0),
    CONSTRAINT crawl_runs_new_item_count_bounded CHECK (new_item_count <= item_count),
    CONSTRAINT crawl_runs_finished_state CHECK (
        (status = 'running' AND finished_at IS NULL)
        OR (status <> 'running' AND finished_at IS NOT NULL)
    )
);

CREATE INDEX crawl_runs_source_started_idx
    ON crawl_runs (source_id, started_at DESC);

CREATE TABLE raw_crawl_items (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    crawl_run_id uuid NOT NULL REFERENCES crawl_runs(id) ON DELETE CASCADE,
    source_id uuid NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    source_item_key text NOT NULL,
    external_id text,
    fetched_url text NOT NULL,
    canonical_url text,
    payload jsonb NOT NULL DEFAULT '{}'::jsonb,
    fetched_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    processing_status text NOT NULL DEFAULT 'pending',
    normalization_attempt_count integer NOT NULL DEFAULT 0,
    normalization_error text,
    normalized_feed_item_id uuid,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT raw_crawl_items_key_not_blank CHECK (btrim(source_item_key) <> ''),
    CONSTRAINT raw_crawl_items_url_not_blank CHECK (btrim(fetched_url) <> ''),
    CONSTRAINT raw_crawl_items_status_valid CHECK (
        processing_status IN ('pending', 'normalized', 'failed', 'skipped')
    ),
    CONSTRAINT raw_crawl_items_attempts_nonnegative CHECK (
        normalization_attempt_count >= 0
    ),
    UNIQUE (crawl_run_id, source_item_key)
);

CREATE INDEX raw_crawl_items_processing_idx
    ON raw_crawl_items (processing_status, created_at)
    WHERE processing_status IN ('pending', 'failed');

CREATE INDEX raw_crawl_items_source_idx
    ON raw_crawl_items (source_id, fetched_at DESC);

CREATE TRIGGER raw_crawl_items_set_updated_at
BEFORE UPDATE ON raw_crawl_items
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE feed_items (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id uuid NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    source_id uuid NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    raw_crawl_item_id uuid REFERENCES raw_crawl_items(id) ON DELETE SET NULL,
    external_id text NOT NULL,
    url text NOT NULL,
    canonical_url text NOT NULL,
    title text NOT NULL,
    summary text NOT NULL DEFAULT '',
    body_text text NOT NULL DEFAULT '',
    body_html text NOT NULL DEFAULT '',
    body_markdown text NOT NULL DEFAULT '',
    published_at timestamptz,
    fetched_at timestamptz NOT NULL,
    content_hash text NOT NULL,
    source_kind text NOT NULL,
    raw jsonb NOT NULL DEFAULT '{}'::jsonb,
    normalized jsonb NOT NULL DEFAULT '{}'::jsonb,
    content_processing jsonb NOT NULL DEFAULT '{}'::jsonb,
    is_private boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT feed_items_external_id_not_blank CHECK (btrim(external_id) <> ''),
    CONSTRAINT feed_items_url_not_blank CHECK (btrim(url) <> ''),
    CONSTRAINT feed_items_canonical_url_not_blank CHECK (btrim(canonical_url) <> ''),
    CONSTRAINT feed_items_title_not_blank CHECK (btrim(title) <> ''),
    CONSTRAINT feed_items_content_hash_not_blank CHECK (btrim(content_hash) <> ''),
    CONSTRAINT feed_items_source_kind_valid CHECK (
        source_kind IN ('rss', 'atom', 'html', 'browser')
    ),
    UNIQUE (source_id, external_id),
    UNIQUE (source_id, canonical_url),
    UNIQUE (content_hash)
);

ALTER TABLE raw_crawl_items
ADD CONSTRAINT raw_crawl_items_normalized_feed_item_fk
FOREIGN KEY (normalized_feed_item_id)
REFERENCES feed_items(id)
ON DELETE SET NULL;

CREATE INDEX feed_items_company_published_idx
    ON feed_items (company_id, published_at DESC NULLS LAST, fetched_at DESC);

CREATE INDEX feed_items_source_fetched_idx
    ON feed_items (source_id, fetched_at DESC);

CREATE TRIGGER feed_items_set_updated_at
BEFORE UPDATE ON feed_items
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE export_runs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    export_target_id uuid NOT NULL REFERENCES export_targets(id) ON DELETE CASCADE,
    job_id uuid REFERENCES jobs(id) ON DELETE SET NULL,
    started_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at timestamptz,
    status text NOT NULL DEFAULT 'running',
    item_count integer NOT NULL DEFAULT 0,
    commit_sha text,
    pushed boolean NOT NULL DEFAULT false,
    error text,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT export_runs_status_valid CHECK (
        status IN ('running', 'completed', 'failed', 'cancelled')
    ),
    CONSTRAINT export_runs_item_count_nonnegative CHECK (item_count >= 0),
    CONSTRAINT export_runs_finished_state CHECK (
        (status = 'running' AND finished_at IS NULL)
        OR (status <> 'running' AND finished_at IS NOT NULL)
    )
);

CREATE INDEX export_runs_target_started_idx
    ON export_runs (export_target_id, started_at DESC);

CREATE TABLE exported_items (
    target_id uuid NOT NULL REFERENCES export_targets(id) ON DELETE CASCADE,
    feed_item_id uuid NOT NULL REFERENCES feed_items(id) ON DELETE CASCADE,
    exported_path text NOT NULL,
    exported_content_hash text NOT NULL,
    exported_commit text,
    exported_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (target_id, feed_item_id),
    CONSTRAINT exported_items_path_not_blank CHECK (btrim(exported_path) <> ''),
    CONSTRAINT exported_items_hash_not_blank CHECK (btrim(exported_content_hash) <> '')
);

CREATE TABLE event_log (
    id bigserial PRIMARY KEY,
    event_type text NOT NULL,
    company_id uuid REFERENCES companies(id) ON DELETE SET NULL,
    source_id uuid REFERENCES sources(id) ON DELETE SET NULL,
    job_id uuid REFERENCES jobs(id) ON DELETE SET NULL,
    payload jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT event_log_type_not_blank CHECK (btrim(event_type) <> '')
);

CREATE INDEX event_log_created_idx ON event_log (created_at DESC);
CREATE INDEX event_log_company_idx
    ON event_log (company_id, created_at DESC)
    WHERE company_id IS NOT NULL;
CREATE INDEX event_log_source_idx
    ON event_log (source_id, created_at DESC)
    WHERE source_id IS NOT NULL;
