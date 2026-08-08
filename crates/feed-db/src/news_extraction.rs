use std::str::FromStr;

use chrono::{DateTime, Utc};
use feed_core::{CompanyNewsExtractionRun, RunStatus, Source};
use serde_json::{Value, json};
use sqlx::FromRow;
use url::Url;
use uuid::Uuid;

use crate::{Database, DatabaseError};

#[derive(Clone, Debug)]
pub struct CompanyNewsExtractionCompletion {
    pub suggested_url_count: i32,
    pub accepted_url_count: i32,
    pub rejected_url_count: i32,
    pub source_count: i32,
    pub normalized_item_count: i32,
    pub new_item_count: i32,
    pub metadata: Value,
}

#[derive(Debug, FromRow)]
struct CompanyNewsExtractionRunRow {
    id: Uuid,
    company_id: Uuid,
    job_id: Option<Uuid>,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    status: String,
    suggested_url_count: i32,
    accepted_url_count: i32,
    rejected_url_count: i32,
    source_count: i32,
    normalized_item_count: i32,
    new_item_count: i32,
    error: Option<String>,
    metadata: Value,
}

impl CompanyNewsExtractionRunRow {
    fn into_domain(self) -> Result<CompanyNewsExtractionRun, DatabaseError> {
        Ok(CompanyNewsExtractionRun {
            id: self.id,
            company_id: self.company_id,
            job_id: self.job_id,
            window_start: self.window_start,
            window_end: self.window_end,
            started_at: self.started_at,
            finished_at: self.finished_at,
            status: RunStatus::from_str(&self.status)?,
            suggested_url_count: self.suggested_url_count,
            accepted_url_count: self.accepted_url_count,
            rejected_url_count: self.rejected_url_count,
            source_count: self.source_count,
            normalized_item_count: self.normalized_item_count,
            new_item_count: self.new_item_count,
            error: self.error,
            metadata: self.metadata,
        })
    }
}

impl Database {
    pub async fn list_company_ids_needing_transient_news_retry(
        &self,
        retry_after: DateTime<Utc>,
        include_companies_with_feeds: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Uuid>, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            WITH latest_attempt AS (
                SELECT DISTINCT ON (run.company_id)
                    run.company_id,
                    run.status,
                    run.metadata
                FROM company_news_extraction_runs AS run
                WHERE run.started_at >= $1
                ORDER BY run.company_id, run.started_at DESC, run.id DESC
            ),
            transient_attempt AS (
                SELECT latest.company_id
                FROM latest_attempt AS latest
                WHERE
                    (
                        latest.status = 'failed'
                        AND latest.metadata @> '{"retryable": true}'::jsonb
                    )
                    OR latest.metadata @> '{"continued_after_transient_evidence_failure": true}'::jsonb
                    OR EXISTS (
                        SELECT 1
                        FROM jsonb_array_elements(
                            COALESCE(latest.metadata -> 'failures', '[]'::jsonb)
                        ) AS failure
                        WHERE failure -> 'retryable' = 'true'::jsonb
                    )
                    OR EXISTS (
                        SELECT 1
                        FROM jsonb_array_elements(
                            COALESCE(latest.metadata -> 'recipe_builds', '[]'::jsonb)
                        ) AS build
                        WHERE
                            (
                                build ->> 'outcome' = 'crawl_failed'
                                AND build -> 'retryable' = 'true'::jsonb
                            )
                            OR (
                                CASE
                                    WHEN COALESCE(
                                        build #>> '{failure_diagnostics,retryable_failure_count}',
                                        '0'
                                    ) ~ '^[0-9]+$'
                                    THEN (
                                        build
                                            #>> '{failure_diagnostics,retryable_failure_count}'
                                    )::integer
                                    ELSE 0
                                END > 0
                            )
                    )
            )
            SELECT company.id
            FROM companies AS company
            JOIN transient_attempt AS transient ON transient.company_id = company.id
            WHERE
                company.discovery_enabled
                AND company.lifecycle_status <> 'inactive'
                AND (
                    $2
                    OR NOT EXISTS (
                        SELECT 1
                        FROM sources AS feed_source
                        JOIN source_state AS feed_state
                            ON feed_state.source_id = feed_source.id
                        WHERE
                            feed_source.company_id = company.id
                            AND feed_source.status = 'approved'
                            AND feed_source.kind IN ('rss', 'atom')
                            AND feed_state.last_success_at IS NOT NULL
                            AND feed_state.consecutive_failures = 0
                            AND feed_state.consecutive_zero_runs < 3
                    )
                )
                AND NOT EXISTS (
                    SELECT 1
                    FROM jobs AS active_job
                    WHERE
                        active_job.company_id = company.id
                        AND active_job.job_type = 'extract_company_news'
                        AND active_job.status IN ('pending', 'running')
                )
            ORDER BY company.name, company.company_key
            LIMIT $3 OFFSET $4
            "#,
        )
        .bind(retry_after)
        .bind(include_companies_with_feeds)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn count_companies_needing_transient_news_retry(
        &self,
        retry_after: DateTime<Utc>,
        include_companies_with_feeds: bool,
    ) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            WITH latest_attempt AS (
                SELECT DISTINCT ON (run.company_id)
                    run.company_id,
                    run.status,
                    run.metadata
                FROM company_news_extraction_runs AS run
                WHERE run.started_at >= $1
                ORDER BY run.company_id, run.started_at DESC, run.id DESC
            ),
            transient_attempt AS (
                SELECT latest.company_id
                FROM latest_attempt AS latest
                WHERE
                    (
                        latest.status = 'failed'
                        AND latest.metadata @> '{"retryable": true}'::jsonb
                    )
                    OR latest.metadata @> '{"continued_after_transient_evidence_failure": true}'::jsonb
                    OR EXISTS (
                        SELECT 1
                        FROM jsonb_array_elements(
                            COALESCE(latest.metadata -> 'failures', '[]'::jsonb)
                        ) AS failure
                        WHERE failure -> 'retryable' = 'true'::jsonb
                    )
                    OR EXISTS (
                        SELECT 1
                        FROM jsonb_array_elements(
                            COALESCE(latest.metadata -> 'recipe_builds', '[]'::jsonb)
                        ) AS build
                        WHERE
                            (
                                build ->> 'outcome' = 'crawl_failed'
                                AND build -> 'retryable' = 'true'::jsonb
                            )
                            OR (
                                CASE
                                    WHEN COALESCE(
                                        build #>> '{failure_diagnostics,retryable_failure_count}',
                                        '0'
                                    ) ~ '^[0-9]+$'
                                    THEN (
                                        build
                                            #>> '{failure_diagnostics,retryable_failure_count}'
                                    )::integer
                                    ELSE 0
                                END > 0
                            )
                    )
            )
            SELECT count(*)
            FROM companies AS company
            JOIN transient_attempt AS transient ON transient.company_id = company.id
            WHERE
                company.discovery_enabled
                AND company.lifecycle_status <> 'inactive'
                AND (
                    $2
                    OR NOT EXISTS (
                        SELECT 1
                        FROM sources AS feed_source
                        JOIN source_state AS feed_state
                            ON feed_state.source_id = feed_source.id
                        WHERE
                            feed_source.company_id = company.id
                            AND feed_source.status = 'approved'
                            AND feed_source.kind IN ('rss', 'atom')
                            AND feed_state.last_success_at IS NOT NULL
                            AND feed_state.consecutive_failures = 0
                            AND feed_state.consecutive_zero_runs < 3
                    )
                )
                AND NOT EXISTS (
                    SELECT 1
                    FROM jobs AS active_job
                    WHERE
                        active_job.company_id = company.id
                        AND active_job.job_type = 'extract_company_news'
                        AND active_job.status IN ('pending', 'running')
                )
            "#,
        )
        .bind(retry_after)
        .bind(include_companies_with_feeds)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn company_has_approved_feed(&self, company_id: Uuid) -> Result<bool, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM sources
                WHERE
                    company_id = $1
                    AND status = 'approved'
                    AND kind IN ('rss', 'atom')
            )
            "#,
        )
        .bind(company_id)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn company_has_healthy_approved_feed(
        &self,
        company_id: Uuid,
    ) -> Result<bool, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM sources AS source
                JOIN source_state AS state ON state.source_id = source.id
                WHERE
                    source.company_id = $1
                    AND source.status = 'approved'
                    AND source.kind IN ('rss', 'atom')
                    AND state.last_success_at IS NOT NULL
                    AND state.consecutive_failures = 0
                    AND state.consecutive_zero_runs < 3
            )
            "#,
        )
        .bind(company_id)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn begin_company_news_extraction_run(
        &self,
        company_id: Uuid,
        job_id: Uuid,
        window_start: DateTime<Utc>,
        window_end: DateTime<Utc>,
    ) -> Result<Uuid, DatabaseError> {
        if window_start >= window_end {
            return Err(DatabaseError::InvalidState(
                "news extraction window_start must precede window_end".to_owned(),
            ));
        }
        let run_id = sqlx::query_scalar(
            r#"
            INSERT INTO company_news_extraction_runs (
                company_id, job_id, window_start, window_end, status
            )
            VALUES ($1, $2, $3, $4, 'running')
            ON CONFLICT (job_id) WHERE status = 'running' AND job_id IS NOT NULL
            DO UPDATE SET
                window_start = EXCLUDED.window_start,
                window_end = EXCLUDED.window_end
            RETURNING id
            "#,
        )
        .bind(company_id)
        .bind(job_id)
        .bind(window_start)
        .bind(window_end)
        .fetch_one(self.pool())
        .await?;
        Ok(run_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn get_or_create_company_news_source(
        &self,
        company_id: Uuid,
        source_key: &str,
        origin_url: &Url,
        freshness_slo_seconds: i32,
        public_export_allowed: bool,
        extraction_run_id: Uuid,
    ) -> Result<Source, DatabaseError> {
        if source_key.trim().is_empty() || freshness_slo_seconds <= 0 {
            return Err(DatabaseError::InvalidState(
                "company news import source key and freshness must be valid".to_owned(),
            ));
        }
        let mut transaction = self.pool().begin().await?;
        let inserted_id = sqlx::query_scalar::<_, Uuid>(
            r#"
            INSERT INTO sources (
                source_id,
                company_id,
                kind,
                url,
                status,
                freshness_slo_seconds,
                browser_required,
                public_export_allowed,
                discovery_confidence,
                metadata
            )
            VALUES (
                $1, $2, 'html', $3, 'approved', $4, false, $5, NULL,
                jsonb_build_object(
                    'managed_by', 'manual_company_news_import',
                    'first_extraction_run_id', $6,
                    'scope', 'adapter_cited_public_article_origin'
                )
            )
            ON CONFLICT DO NOTHING
            RETURNING id
            "#,
        )
        .bind(source_key)
        .bind(company_id)
        .bind(origin_url.as_str())
        .bind(freshness_slo_seconds)
        .bind(public_export_allowed)
        .bind(extraction_run_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let (source_id, inserted) = if let Some(source_id) = inserted_id {
            (source_id, true)
        } else {
            let source_id = sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM sources WHERE company_id = $1 AND url = $2",
            )
            .bind(company_id)
            .bind(origin_url.as_str())
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or_else(|| {
                DatabaseError::InvalidState(format!(
                    "company news import source key {source_key} conflicts with another source"
                ))
            })?;
            (source_id, false)
        };
        sqlx::query(
            r#"
            INSERT INTO source_state (source_id)
            VALUES ($1)
            ON CONFLICT (source_id) DO NOTHING
            "#,
        )
        .bind(source_id)
        .execute(&mut *transaction)
        .await?;
        if inserted {
            sqlx::query(
                r#"
                INSERT INTO event_log (event_type, company_id, source_id, payload)
                VALUES ('company_news.source_created', $1, $2, $3)
                "#,
            )
            .bind(company_id)
            .bind(source_id)
            .bind(json!({ "extraction_run_id": extraction_run_id }))
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        self.get_source(source_id)
            .await?
            .ok_or(DatabaseError::NotFound {
                entity: "company news import source",
                id: source_id,
            })
    }

    pub async fn complete_company_news_extraction_run(
        &self,
        run_id: Uuid,
        company_id: Uuid,
        completion: &CompanyNewsExtractionCompletion,
    ) -> Result<(), DatabaseError> {
        validate_completion(completion)?;
        let mut transaction = self.pool().begin().await?;
        let result = sqlx::query(
            r#"
            UPDATE company_news_extraction_runs
            SET
                status = 'completed',
                finished_at = CURRENT_TIMESTAMP,
                suggested_url_count = $2,
                accepted_url_count = $3,
                rejected_url_count = $4,
                source_count = $5,
                normalized_item_count = $6,
                new_item_count = $7,
                error = NULL,
                metadata = $8
            WHERE id = $1 AND company_id = $9 AND status = 'running'
            "#,
        )
        .bind(run_id)
        .bind(completion.suggested_url_count)
        .bind(completion.accepted_url_count)
        .bind(completion.rejected_url_count)
        .bind(completion.source_count)
        .bind(completion.normalized_item_count)
        .bind(completion.new_item_count)
        .bind(&completion.metadata)
        .bind(company_id)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            return Err(DatabaseError::NotFound {
                entity: "running company news extraction run",
                id: run_id,
            });
        }
        sqlx::query(
            r#"
            INSERT INTO event_log (event_type, company_id, payload)
            VALUES ('company_news.extraction_completed', $1, $2)
            "#,
        )
        .bind(company_id)
        .bind(json!({
            "run_id": run_id,
            "suggested_url_count": completion.suggested_url_count,
            "accepted_url_count": completion.accepted_url_count,
            "rejected_url_count": completion.rejected_url_count,
            "source_count": completion.source_count,
            "normalized_item_count": completion.normalized_item_count,
            "new_item_count": completion.new_item_count,
        }))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn fail_company_news_extraction_run(
        &self,
        run_id: Uuid,
        company_id: Uuid,
        error: &str,
        metadata: Value,
    ) -> Result<(), DatabaseError> {
        let mut transaction = self.pool().begin().await?;
        let result = sqlx::query(
            r#"
            UPDATE company_news_extraction_runs
            SET
                status = 'failed',
                finished_at = CURRENT_TIMESTAMP,
                error = $2,
                metadata = $3
            WHERE id = $1 AND company_id = $4 AND status = 'running'
            "#,
        )
        .bind(run_id)
        .bind(error)
        .bind(metadata)
        .bind(company_id)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            return Err(DatabaseError::NotFound {
                entity: "running company news extraction run",
                id: run_id,
            });
        }
        sqlx::query(
            r#"
            INSERT INTO event_log (event_type, company_id, payload)
            VALUES ('company_news.extraction_failed', $1, $2)
            "#,
        )
        .bind(company_id)
        .bind(json!({ "run_id": run_id, "error": error }))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn cancel_running_company_news_extractions_for_job(
        &self,
        job_id: Uuid,
        lease_token: Uuid,
        reason: &str,
    ) -> Result<u64, DatabaseError> {
        let result = sqlx::query(
            r#"
            UPDATE company_news_extraction_runs
            SET
                status = 'cancelled',
                finished_at = CURRENT_TIMESTAMP,
                error = $3
            WHERE
                job_id = $1
                AND status = 'running'
                AND EXISTS (
                    SELECT 1
                    FROM jobs
                    WHERE
                        id = $1
                        AND lease_token = $2
                        AND status = 'running'
                )
            "#,
        )
        .bind(job_id)
        .bind(lease_token)
        .bind(reason)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn list_company_news_extraction_runs(
        &self,
        company_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<CompanyNewsExtractionRun>, DatabaseError> {
        let rows = sqlx::query_as::<_, CompanyNewsExtractionRunRow>(
            r#"
            SELECT
                id,
                company_id,
                job_id,
                window_start,
                window_end,
                started_at,
                finished_at,
                status,
                suggested_url_count,
                accepted_url_count,
                rejected_url_count,
                source_count,
                normalized_item_count,
                new_item_count,
                error,
                metadata
            FROM company_news_extraction_runs
            WHERE $1::uuid IS NULL OR company_id = $1
            ORDER BY started_at DESC, id DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(company_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(CompanyNewsExtractionRunRow::into_domain)
            .collect()
    }

    pub async fn count_company_news_extraction_runs(
        &self,
        company_id: Option<Uuid>,
    ) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar(
            "SELECT count(*) FROM company_news_extraction_runs WHERE $1::uuid IS NULL OR company_id = $1",
        )
        .bind(company_id)
        .fetch_one(self.pool())
        .await?)
    }
}

fn validate_completion(completion: &CompanyNewsExtractionCompletion) -> Result<(), DatabaseError> {
    if completion.suggested_url_count < 0
        || completion.accepted_url_count < 0
        || completion.rejected_url_count < 0
        || completion.source_count < 0
        || completion.normalized_item_count < 0
        || completion.new_item_count < 0
        || completion.accepted_url_count + completion.rejected_url_count
            > completion.suggested_url_count
        || completion.new_item_count > completion.normalized_item_count
        || completion.normalized_item_count > completion.accepted_url_count
    {
        return Err(DatabaseError::InvalidState(
            "company news extraction completion counts are inconsistent".to_owned(),
        ));
    }
    Ok(())
}
