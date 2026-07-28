CREATE UNIQUE INDEX jobs_one_running_company_news_idx
    ON jobs ((1))
    WHERE status = 'running'
      AND job_type = 'extract_company_news';
