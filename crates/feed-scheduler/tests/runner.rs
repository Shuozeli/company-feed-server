#![cfg(feature = "postgres-tests")]

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::Utc;
use feed_core::{Job, JobSpec, JobStatus, JobType};
use feed_db::Database;
use feed_scheduler::{JobHandler, JobHandlerError, JobRunOutcome, JobRunner, JobRunnerConfig};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct SuccessfulHandler;

#[async_trait]
impl JobHandler for SuccessfulHandler {
    fn supported_job_types(&self) -> &[JobType] {
        &[JobType::DiscoverCompany]
    }

    async fn handle(&self, _job: &Job) -> Result<(), JobHandlerError> {
        Ok(())
    }
}

struct RetryableHandler;

#[async_trait]
impl JobHandler for RetryableHandler {
    fn supported_job_types(&self) -> &[JobType] {
        &[JobType::DiscoverCompany]
    }

    async fn handle(&self, _job: &Job) -> Result<(), JobHandlerError> {
        Err(JobHandlerError::retryable("temporary integration failure"))
    }
}

struct CooldownHandler;

#[async_trait]
impl JobHandler for CooldownHandler {
    fn supported_job_types(&self) -> &[JobType] {
        &[JobType::DiscoverCompany]
    }

    async fn handle(&self, _job: &Job) -> Result<(), JobHandlerError> {
        Err(JobHandlerError::retryable_with_worker_cooldown(
            "shared dependency unavailable",
            Duration::from_millis(50),
        ))
    }
}

#[tokio::test]
async fn runner_completes_successful_jobs_and_schedules_retryable_failures() {
    let Some(database_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL is not set; skipping Postgres integration test");
        return;
    };
    let database = Database::connect(&database_url, 5)
        .await
        .expect("connect to test Postgres");
    database.ensure_schema().await.expect("ensure schema");
    let config = JobRunnerConfig {
        lease_duration: Duration::from_secs(30),
        heartbeat_interval: Duration::from_secs(10),
        poll_interval: Duration::from_millis(10),
        retry_base: Duration::from_secs(30),
        retry_max: Duration::from_secs(300),
        max_company_news_in_flight: 1,
    };

    let successful_key = format!("runner-success:{}", Uuid::new_v4());
    let successful = database
        .enqueue_job(&JobSpec::new(
            JobType::DiscoverCompany,
            &successful_key,
            Utc::now(),
        ))
        .await
        .expect("enqueue successful job");
    let runner = JobRunner::new(
        database.clone(),
        "successful-runner",
        Arc::new(SuccessfulHandler),
        config,
    )
    .expect("configure runner");
    assert_eq!(
        runner
            .run_once(CancellationToken::new())
            .await
            .expect("run successful job"),
        JobRunOutcome::Completed {
            job_id: successful.id
        }
    );
    assert_eq!(
        database
            .get_job(successful.id)
            .await
            .expect("load completed job")
            .expect("completed job exists")
            .status,
        JobStatus::Completed
    );

    let retry_key = format!("runner-retry:{}", Uuid::new_v4());
    let retry = database
        .enqueue_job(&JobSpec::new(
            JobType::DiscoverCompany,
            &retry_key,
            Utc::now(),
        ))
        .await
        .expect("enqueue retryable job");
    let runner = JobRunner::new(
        database.clone(),
        "retry-runner",
        Arc::new(RetryableHandler),
        config,
    )
    .expect("configure runner");
    match runner
        .run_once(CancellationToken::new())
        .await
        .expect("run retryable job")
    {
        JobRunOutcome::RetryScheduled { job_id, run_after } => {
            assert_eq!(job_id, retry.id);
            assert!(run_after > Utc::now());
        }
        outcome => panic!("expected retry outcome, got {outcome:?}"),
    }
    assert_eq!(
        database
            .get_job(retry.id)
            .await
            .expect("load retryable job")
            .expect("retryable job exists")
            .status,
        JobStatus::Pending
    );

    let cooldown_key = format!("runner-cooldown:{}", Uuid::new_v4());
    let cooldown_job = database
        .enqueue_job(&JobSpec::new(
            JobType::DiscoverCompany,
            &cooldown_key,
            Utc::now(),
        ))
        .await
        .expect("enqueue cooldown job");
    let runner = JobRunner::new(
        database.clone(),
        "cooldown-runner",
        Arc::new(CooldownHandler),
        config,
    )
    .expect("configure runner");
    let started_at = tokio::time::Instant::now();
    match runner
        .run_once(CancellationToken::new())
        .await
        .expect("run cooldown job")
    {
        JobRunOutcome::RetryScheduled { job_id, run_after } => {
            assert_eq!(job_id, cooldown_job.id);
            assert!(run_after > Utc::now());
        }
        outcome => panic!("expected retry outcome, got {outcome:?}"),
    }
    assert!(
        started_at.elapsed() >= Duration::from_millis(45),
        "worker returned before its shared-dependency cooldown elapsed"
    );

    sqlx::query("DELETE FROM jobs WHERE job_key = ANY($1::text[])")
        .bind(vec![successful_key, retry_key, cooldown_key])
        .execute(database.pool())
        .await
        .expect("clean up integration jobs");
}
