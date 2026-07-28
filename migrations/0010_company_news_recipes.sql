CREATE TABLE company_news_recipes (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    recipe_key text NOT NULL,
    company_id uuid NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    source_id uuid NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    version integer NOT NULL,
    status text NOT NULL DEFAULT 'draft',
    schema_version text NOT NULL,
    spec jsonb NOT NULL,
    content_hash text NOT NULL,
    generated_by_run_id uuid REFERENCES company_news_extraction_runs(id) ON DELETE SET NULL,
    verified_at timestamptz,
    stale_at timestamptz,
    stale_reason text,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT company_news_recipes_key_not_blank CHECK (btrim(recipe_key) <> ''),
    CONSTRAINT company_news_recipes_version_positive CHECK (version > 0),
    CONSTRAINT company_news_recipes_status_valid CHECK (
        status IN ('draft', 'active', 'stale', 'superseded', 'disabled')
    ),
    CONSTRAINT company_news_recipes_schema_not_blank CHECK (btrim(schema_version) <> ''),
    CONSTRAINT company_news_recipes_hash_not_blank CHECK (btrim(content_hash) <> ''),
    CONSTRAINT company_news_recipes_stale_state CHECK (
        (status = 'stale' AND stale_at IS NOT NULL AND stale_reason IS NOT NULL)
        OR status <> 'stale'
    ),
    UNIQUE (recipe_key, version)
);

CREATE UNIQUE INDEX company_news_recipes_one_active_source_idx
    ON company_news_recipes (source_id)
    WHERE status = 'active';
CREATE INDEX company_news_recipes_company_status_idx
    ON company_news_recipes (company_id, status, updated_at DESC);
CREATE INDEX company_news_recipes_source_version_idx
    ON company_news_recipes (source_id, version DESC);

CREATE TRIGGER company_news_recipes_set_updated_at
BEFORE UPDATE ON company_news_recipes
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE company_news_recipe_state (
    recipe_id uuid PRIMARY KEY REFERENCES company_news_recipes(id) ON DELETE CASCADE,
    last_attempt_at timestamptz,
    last_success_at timestamptz,
    last_correct_at timestamptz,
    last_nonempty_at timestamptz,
    last_item_published_at timestamptz,
    consecutive_failures integer NOT NULL DEFAULT 0,
    consecutive_empty_runs integer NOT NULL DEFAULT 0,
    consecutive_correctness_failures integer NOT NULL DEFAULT 0,
    last_structure_fingerprint text,
    freshness_status text NOT NULL DEFAULT 'unknown',
    correctness_status text NOT NULL DEFAULT 'unknown',
    rebuild_required boolean NOT NULL DEFAULT false,
    reason text,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT company_news_recipe_state_counts_nonnegative CHECK (
        consecutive_failures >= 0
        AND consecutive_empty_runs >= 0
        AND consecutive_correctness_failures >= 0
    ),
    CONSTRAINT company_news_recipe_state_freshness_valid CHECK (
        freshness_status IN ('unknown', 'fresh', 'overdue', 'content_stale')
    ),
    CONSTRAINT company_news_recipe_state_correctness_valid CHECK (
        correctness_status IN ('unknown', 'passing', 'failing')
    )
);

CREATE TRIGGER company_news_recipe_state_set_updated_at
BEFORE UPDATE ON company_news_recipe_state
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE company_news_recipe_runs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    recipe_id uuid NOT NULL REFERENCES company_news_recipes(id) ON DELETE CASCADE,
    source_id uuid NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    job_id uuid REFERENCES jobs(id) ON DELETE SET NULL,
    crawl_run_id uuid REFERENCES crawl_runs(id) ON DELETE SET NULL,
    status text NOT NULL DEFAULT 'running',
    started_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at timestamptz,
    discovered_url_count integer NOT NULL DEFAULT 0,
    accepted_item_count integer NOT NULL DEFAULT 0,
    rejected_url_count integer NOT NULL DEFAULT 0,
    normalized_item_count integer NOT NULL DEFAULT 0,
    new_item_count integer NOT NULL DEFAULT 0,
    latest_published_at timestamptz,
    acceptance_ratio_bps integer NOT NULL DEFAULT 0,
    structure_fingerprint text,
    reasons jsonb NOT NULL DEFAULT '[]'::jsonb,
    error text,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT company_news_recipe_runs_status_valid CHECK (
        status IN ('running', 'passed', 'failed', 'stale', 'cancelled')
    ),
    CONSTRAINT company_news_recipe_runs_finished_state CHECK (
        (status = 'running' AND finished_at IS NULL)
        OR (status <> 'running' AND finished_at IS NOT NULL)
    ),
    CONSTRAINT company_news_recipe_runs_counts_nonnegative CHECK (
        discovered_url_count >= 0
        AND accepted_item_count >= 0
        AND rejected_url_count >= 0
        AND normalized_item_count >= 0
        AND new_item_count >= 0
    ),
    CONSTRAINT company_news_recipe_runs_counts_bounded CHECK (
        accepted_item_count + rejected_url_count <= discovered_url_count
        AND normalized_item_count <= accepted_item_count
        AND new_item_count <= normalized_item_count
    ),
    CONSTRAINT company_news_recipe_runs_ratio_valid CHECK (
        acceptance_ratio_bps BETWEEN 0 AND 10000
    ),
    CONSTRAINT company_news_recipe_runs_reasons_array CHECK (
        jsonb_typeof(reasons) = 'array'
    )
);

CREATE UNIQUE INDEX company_news_recipe_runs_one_running_job_idx
    ON company_news_recipe_runs (job_id)
    WHERE status = 'running' AND job_id IS NOT NULL;
CREATE INDEX company_news_recipe_runs_recipe_started_idx
    ON company_news_recipe_runs (recipe_id, started_at DESC);
CREATE INDEX company_news_recipe_runs_status_started_idx
    ON company_news_recipe_runs (status, started_at DESC);
