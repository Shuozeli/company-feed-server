DROP INDEX IF EXISTS jobs_one_running_company_news_idx;

CREATE INDEX IF NOT EXISTS jobs_running_company_news_idx
    ON jobs (job_type, status)
    WHERE status = 'running'
      AND job_type = 'extract_company_news';
