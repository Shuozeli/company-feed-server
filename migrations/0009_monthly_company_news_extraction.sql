-- Companies without an approved RSS/Atom source use a separate, low-frequency
-- URL-suggestion and public-page extraction pipeline. This is intentionally
-- not part of the recurring source crawler schedule.

ALTER TABLE jobs
DROP CONSTRAINT jobs_type_valid;

ALTER TABLE jobs
ADD CONSTRAINT jobs_type_valid CHECK (
    job_type IN (
        'discover_company',
        'validate_candidate',
        'crawl_source',
        'extract_company_news',
        'export_target',
        'normalize_backfill'
    )
);

CREATE TABLE company_news_extraction_runs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id uuid NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    job_id uuid REFERENCES jobs(id) ON DELETE SET NULL,
    window_start timestamptz NOT NULL,
    window_end timestamptz NOT NULL,
    started_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at timestamptz,
    status text NOT NULL DEFAULT 'running',
    suggested_url_count integer NOT NULL DEFAULT 0,
    accepted_url_count integer NOT NULL DEFAULT 0,
    rejected_url_count integer NOT NULL DEFAULT 0,
    source_count integer NOT NULL DEFAULT 0,
    normalized_item_count integer NOT NULL DEFAULT 0,
    new_item_count integer NOT NULL DEFAULT 0,
    error text,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT company_news_extraction_runs_window_valid CHECK (
        window_start < window_end
    ),
    CONSTRAINT company_news_extraction_runs_status_valid CHECK (
        status IN ('running', 'completed', 'failed', 'cancelled')
    ),
    CONSTRAINT company_news_extraction_runs_counts_nonnegative CHECK (
        suggested_url_count >= 0
        AND accepted_url_count >= 0
        AND rejected_url_count >= 0
        AND source_count >= 0
        AND normalized_item_count >= 0
        AND new_item_count >= 0
    ),
    CONSTRAINT company_news_extraction_runs_suggestions_bounded CHECK (
        accepted_url_count + rejected_url_count <= suggested_url_count
    ),
    CONSTRAINT company_news_extraction_runs_items_bounded CHECK (
        new_item_count <= normalized_item_count
        AND normalized_item_count <= accepted_url_count
    ),
    CONSTRAINT company_news_extraction_runs_finished_state CHECK (
        (status = 'running' AND finished_at IS NULL)
        OR (status <> 'running' AND finished_at IS NOT NULL)
    )
);

CREATE INDEX company_news_extraction_runs_company_started_idx
ON company_news_extraction_runs (company_id, started_at DESC, id DESC);

CREATE INDEX company_news_extraction_runs_status_started_idx
ON company_news_extraction_runs (status, started_at DESC);

CREATE UNIQUE INDEX company_news_extraction_runs_one_running_job_idx
ON company_news_extraction_runs (job_id)
WHERE status = 'running' AND job_id IS NOT NULL;
