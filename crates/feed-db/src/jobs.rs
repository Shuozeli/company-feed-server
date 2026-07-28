use std::{str::FromStr, time::Duration};

use chrono::{DateTime, Utc};
use feed_core::{ClaimedJob, Job, JobSpec, JobStatus, JobType};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use crate::{Database, DatabaseError};

const MAX_ERROR_LENGTH: usize = 16_384;
const COMPANY_NEWS_CLAIM_LOCK: i64 = 0x434f_4d50_4e45_5753;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JobFailureOutcome {
    Retrying { run_after: DateTime<Utc> },
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("job {job_id} is no longer owned by lease {lease_token}")]
pub struct JobLeaseError {
    pub job_id: Uuid,
    pub lease_token: Uuid,
}

#[derive(Debug, FromRow)]
struct JobRow {
    id: Uuid,
    job_type: String,
    job_key: String,
    status: String,
    priority: i16,
    run_after: DateTime<Utc>,
    attempt_count: i32,
    max_attempts: i32,
    company_id: Option<Uuid>,
    candidate_id: Option<Uuid>,
    source_id: Option<Uuid>,
    export_target_id: Option<Uuid>,
    payload: Value,
    last_error: Option<String>,
    locked_by: Option<String>,
    lease_token: Option<Uuid>,
    lease_until: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl JobRow {
    fn into_job(self) -> Result<Job, DatabaseError> {
        Ok(Job {
            id: self.id,
            job_type: JobType::from_str(&self.job_type)?,
            job_key: self.job_key,
            status: JobStatus::from_str(&self.status)?,
            priority: self.priority,
            run_after: self.run_after,
            attempt_count: self.attempt_count,
            max_attempts: self.max_attempts,
            company_id: self.company_id,
            candidate_id: self.candidate_id,
            source_id: self.source_id,
            export_target_id: self.export_target_id,
            payload: self.payload,
            last_error: self.last_error,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }

    fn into_claimed(self) -> Result<ClaimedJob, DatabaseError> {
        let job_id = self.id;
        let worker_id = self
            .locked_by
            .clone()
            .ok_or(DatabaseError::InvalidJobLease(job_id))?;
        let lease_token = self
            .lease_token
            .ok_or(DatabaseError::InvalidJobLease(job_id))?;
        let lease_until = self
            .lease_until
            .ok_or(DatabaseError::InvalidJobLease(job_id))?;
        Ok(ClaimedJob {
            job: self.into_job()?,
            worker_id,
            lease_token,
            lease_until,
        })
    }
}

impl Database {
    pub async fn enqueue_job(&self, spec: &JobSpec) -> Result<Job, DatabaseError> {
        let row = sqlx::query_as::<_, JobRow>(
            r#"
            INSERT INTO jobs (
                job_type,
                job_key,
                status,
                priority,
                run_after,
                max_attempts,
                company_id,
                candidate_id,
                source_id,
                export_target_id,
                payload
            )
            VALUES ($1, $2, 'pending', $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT (job_type, job_key)
                WHERE status IN ('pending', 'running')
            DO UPDATE SET
                run_after = CASE
                    WHEN jobs.status = 'pending'
                    THEN LEAST(jobs.run_after, EXCLUDED.run_after)
                    ELSE jobs.run_after
                END,
                priority = CASE
                    WHEN jobs.status = 'pending'
                    THEN GREATEST(jobs.priority, EXCLUDED.priority)
                    ELSE jobs.priority
                END
            RETURNING
                id,
                job_type,
                job_key,
                status,
                priority,
                run_after,
                attempt_count,
                max_attempts,
                company_id,
                candidate_id,
                source_id,
                export_target_id,
                payload,
                last_error,
                locked_by,
                lease_token,
                lease_until,
                created_at,
                updated_at
            "#,
        )
        .bind(spec.job_type.as_str())
        .bind(&spec.job_key)
        .bind(spec.priority)
        .bind(spec.run_after)
        .bind(spec.max_attempts)
        .bind(spec.company_id)
        .bind(spec.candidate_id)
        .bind(spec.source_id)
        .bind(spec.export_target_id)
        .bind(&spec.payload)
        .fetch_one(self.pool())
        .await?;

        row.into_job()
    }

    pub async fn get_job(&self, job_id: Uuid) -> Result<Option<Job>, DatabaseError> {
        let row = sqlx::query_as::<_, JobRow>(
            r#"
            SELECT
                id,
                job_type,
                job_key,
                status,
                priority,
                run_after,
                attempt_count,
                max_attempts,
                company_id,
                candidate_id,
                source_id,
                export_target_id,
                payload,
                last_error,
                locked_by,
                lease_token,
                lease_until,
                created_at,
                updated_at
            FROM jobs
            WHERE id = $1
            "#,
        )
        .bind(job_id)
        .fetch_optional(self.pool())
        .await?;

        row.map(JobRow::into_job).transpose()
    }

    pub async fn claim_job(
        &self,
        worker_id: &str,
        lease_duration: Duration,
        supported_types: &[JobType],
        max_company_news_in_flight: u32,
    ) -> Result<Option<ClaimedJob>, DatabaseError> {
        if supported_types.is_empty() || max_company_news_in_flight == 0 {
            return Ok(None);
        }
        let lease_seconds = duration_seconds(lease_duration)?;
        self.fail_exhausted_expired_jobs().await?;
        let serialize_company_news_claim = supported_types.contains(&JobType::ExtractCompanyNews);
        let supported_types = supported_types
            .iter()
            .map(|job_type| job_type.as_str())
            .collect::<Vec<_>>();
        let lease_token = Uuid::new_v4();
        let mut transaction = self.pool().begin().await?;
        if serialize_company_news_claim {
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(COMPANY_NEWS_CLAIM_LOCK)
                .execute(&mut *transaction)
                .await?;
        }

        let row = sqlx::query_as::<_, JobRow>(
            r#"
            WITH candidate AS (
                SELECT id
                FROM jobs
                WHERE
                    job_type = ANY($1::text[])
                    AND attempt_count < max_attempts
                    AND (
                        (status = 'pending' AND run_after <= CURRENT_TIMESTAMP)
                        OR (
                            status = 'running'
                            AND lease_until <= CURRENT_TIMESTAMP
                        )
                    )
                    AND (
                        job_type <> 'extract_company_news'
                        OR (
                            SELECT COUNT(*)
                            FROM jobs AS active_company_news
                            WHERE
                                active_company_news.job_type = 'extract_company_news'
                                AND active_company_news.status = 'running'
                                AND active_company_news.id <> jobs.id
                        ) < $5
                    )
                ORDER BY priority DESC, run_after, created_at
                FOR UPDATE SKIP LOCKED
                LIMIT 1
            )
            UPDATE jobs AS job
            SET
                status = 'running',
                locked_by = $2,
                locked_at = CURRENT_TIMESTAMP,
                heartbeat_at = CURRENT_TIMESTAMP,
                lease_until = CURRENT_TIMESTAMP + ($3::bigint * INTERVAL '1 second'),
                lease_token = $4,
                attempt_count = job.attempt_count + 1,
                last_error = CASE
                    WHEN job.status = 'running'
                    THEN 'previous worker lease expired'
                    ELSE job.last_error
                END,
                completed_at = NULL
            FROM candidate
            WHERE job.id = candidate.id
            RETURNING
                job.id,
                job.job_type,
                job.job_key,
                job.status,
                job.priority,
                job.run_after,
                job.attempt_count,
                job.max_attempts,
                job.company_id,
                job.candidate_id,
                job.source_id,
                job.export_target_id,
                job.payload,
                job.last_error,
                job.locked_by,
                job.lease_token,
                job.lease_until,
                job.created_at,
                job.updated_at
            "#,
        )
        .bind(&supported_types)
        .bind(worker_id)
        .bind(lease_seconds)
        .bind(lease_token)
        .bind(i64::from(max_company_news_in_flight))
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;

        row.map(JobRow::into_claimed).transpose()
    }

    pub async fn heartbeat_job(
        &self,
        job_id: Uuid,
        lease_token: Uuid,
        lease_duration: Duration,
    ) -> Result<bool, DatabaseError> {
        let lease_seconds = duration_seconds(lease_duration)?;
        let result = sqlx::query(
            r#"
            UPDATE jobs
            SET
                heartbeat_at = CURRENT_TIMESTAMP,
                lease_until = CURRENT_TIMESTAMP + ($3::bigint * INTERVAL '1 second')
            WHERE
                id = $1
                AND lease_token = $2
                AND status = 'running'
                AND lease_until > CURRENT_TIMESTAMP
            "#,
        )
        .bind(job_id)
        .bind(lease_token)
        .bind(lease_seconds)
        .execute(self.pool())
        .await?;

        Ok(result.rows_affected() == 1)
    }

    pub async fn complete_job(
        &self,
        job_id: Uuid,
        lease_token: Uuid,
    ) -> Result<bool, DatabaseError> {
        let result = sqlx::query(
            r#"
            UPDATE jobs
            SET
                status = 'completed',
                completed_at = CURRENT_TIMESTAMP,
                locked_by = NULL,
                locked_at = NULL,
                heartbeat_at = NULL,
                lease_until = NULL,
                lease_token = NULL,
                last_error = NULL
            WHERE
                id = $1
                AND lease_token = $2
                AND status = 'running'
                AND lease_until > CURRENT_TIMESTAMP
            "#,
        )
        .bind(job_id)
        .bind(lease_token)
        .execute(self.pool())
        .await?;

        Ok(result.rows_affected() == 1)
    }

    pub async fn record_job_failure(
        &self,
        job_id: Uuid,
        lease_token: Uuid,
        error: &str,
        retry_at: Option<DateTime<Utc>>,
    ) -> Result<JobFailureOutcome, DatabaseError> {
        let mut transaction = self.pool().begin().await?;
        let attempt = sqlx::query_as::<_, (i32, i32)>(
            r#"
            SELECT attempt_count, max_attempts
            FROM jobs
            WHERE
                id = $1
                AND lease_token = $2
                AND status = 'running'
                AND lease_until > CURRENT_TIMESTAMP
            FOR UPDATE
            "#,
        )
        .bind(job_id)
        .bind(lease_token)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(JobLeaseError {
            job_id,
            lease_token,
        })?;

        let error = truncate_error(error);
        let outcome = if let Some(run_after) = retry_at.filter(|_| attempt.0 < attempt.1) {
            let persisted_run_after = sqlx::query_scalar::<_, DateTime<Utc>>(
                r#"
                UPDATE jobs
                SET
                    status = 'pending',
                    run_after = GREATEST($3, CURRENT_TIMESTAMP),
                    locked_by = NULL,
                    locked_at = NULL,
                    heartbeat_at = NULL,
                    lease_until = NULL,
                    lease_token = NULL,
                    last_error = $4,
                    completed_at = NULL
                WHERE id = $1 AND lease_token = $2
                RETURNING run_after
                "#,
            )
            .bind(job_id)
            .bind(lease_token)
            .bind(run_after)
            .bind(&error)
            .fetch_one(&mut *transaction)
            .await?;
            JobFailureOutcome::Retrying {
                run_after: persisted_run_after,
            }
        } else {
            sqlx::query(
                r#"
                UPDATE jobs
                SET
                    status = 'failed',
                    completed_at = CURRENT_TIMESTAMP,
                    locked_by = NULL,
                    locked_at = NULL,
                    heartbeat_at = NULL,
                    lease_until = NULL,
                    lease_token = NULL,
                    last_error = $3
                WHERE id = $1 AND lease_token = $2
                "#,
            )
            .bind(job_id)
            .bind(lease_token)
            .bind(&error)
            .execute(&mut *transaction)
            .await?;
            JobFailureOutcome::Failed
        };

        transaction.commit().await?;
        Ok(outcome)
    }

    async fn fail_exhausted_expired_jobs(&self) -> Result<u64, DatabaseError> {
        let result = sqlx::query(
            r#"
            UPDATE jobs
            SET
                status = 'failed',
                completed_at = CURRENT_TIMESTAMP,
                locked_by = NULL,
                locked_at = NULL,
                heartbeat_at = NULL,
                lease_until = NULL,
                lease_token = NULL,
                last_error = CASE
                    WHEN last_error IS NULL OR last_error = ''
                    THEN 'worker lease expired after final attempt'
                    ELSE last_error || E'\nworker lease expired after final attempt'
                END
            WHERE
                status = 'running'
                AND lease_until <= CURRENT_TIMESTAMP
                AND attempt_count >= max_attempts
            "#,
        )
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }
}

fn duration_seconds(duration: Duration) -> Result<i64, DatabaseError> {
    if duration.as_secs() == 0 || duration.subsec_nanos() != 0 {
        return Err(DatabaseError::InvalidDuration(duration));
    }
    i64::try_from(duration.as_secs()).map_err(|_| DatabaseError::InvalidDuration(duration))
}

fn truncate_error(error: &str) -> String {
    if error.len() <= MAX_ERROR_LENGTH {
        return error.to_owned();
    }

    let mut end = MAX_ERROR_LENGTH;
    while !error.is_char_boundary(end) {
        end -= 1;
    }
    error[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_errors_on_utf8_boundary() {
        let error = "好".repeat(MAX_ERROR_LENGTH);
        let truncated = truncate_error(&error);
        assert!(truncated.len() <= MAX_ERROR_LENGTH);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[test]
    fn rejects_subsecond_and_zero_leases() {
        assert!(duration_seconds(Duration::ZERO).is_err());
        assert!(duration_seconds(Duration::from_millis(999)).is_err());
        assert!(duration_seconds(Duration::from_millis(1_500)).is_err());
    }
}
