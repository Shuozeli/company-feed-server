use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use feed_core::{AppSettings, Job, JobType};
use feed_db::{Database, DatabaseError, JobFailureOutcome};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

#[async_trait]
pub trait JobHandler: Send + Sync + 'static {
    fn supported_job_types(&self) -> &[JobType];

    async fn handle(&self, job: &Job) -> Result<(), JobHandlerError>;
}

#[derive(Clone, Default)]
pub struct JobHandlerRegistry {
    handlers: HashMap<JobType, Arc<dyn JobHandler>>,
    supported_job_types: Vec<JobType>,
}

impl JobHandlerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<H>(mut self, handler: Arc<H>) -> Result<Self, SchedulerError>
    where
        H: JobHandler,
    {
        let handler: Arc<dyn JobHandler> = handler;
        for job_type in handler.supported_job_types() {
            if self.handlers.contains_key(job_type) {
                return Err(SchedulerError::DuplicateHandler(*job_type));
            }
            self.handlers.insert(*job_type, handler.clone());
        }
        self.supported_job_types = JobType::ALL
            .into_iter()
            .filter(|job_type| self.handlers.contains_key(job_type))
            .collect();
        Ok(self)
    }
}

#[async_trait]
impl JobHandler for JobHandlerRegistry {
    fn supported_job_types(&self) -> &[JobType] {
        &self.supported_job_types
    }

    async fn handle(&self, job: &Job) -> Result<(), JobHandlerError> {
        let Some(handler) = self.handlers.get(&job.job_type) else {
            return Err(JobHandlerError::permanent(format!(
                "no handler is registered for {}",
                job.job_type
            )));
        };
        handler.handle(job).await
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct JobHandlerError {
    message: String,
    retryable: bool,
    worker_cooldown: Option<Duration>,
}

impl JobHandlerError {
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            worker_cooldown: None,
        }
    }

    pub fn retryable_with_worker_cooldown(
        message: impl Into<String>,
        worker_cooldown: Duration,
    ) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            worker_cooldown: Some(worker_cooldown),
        }
    }

    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            worker_cooldown: None,
        }
    }

    pub fn is_retryable(&self) -> bool {
        self.retryable
    }

    pub fn worker_cooldown(&self) -> Option<Duration> {
        self.worker_cooldown
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobRunnerConfig {
    pub lease_duration: Duration,
    pub heartbeat_interval: Duration,
    pub poll_interval: Duration,
    pub retry_base: Duration,
    pub retry_max: Duration,
    pub max_company_news_in_flight: u32,
}

impl JobRunnerConfig {
    pub fn from_settings(settings: &AppSettings) -> Self {
        Self {
            lease_duration: settings.job_lease,
            heartbeat_interval: settings.job_heartbeat,
            poll_interval: settings.job_poll_interval,
            retry_base: settings.job_retry_base,
            retry_max: settings.job_retry_max,
            max_company_news_in_flight: settings.news_extraction_job_concurrency,
        }
    }

    pub fn validate(self) -> Result<Self, SchedulerError> {
        if self.lease_duration.is_zero()
            || self.heartbeat_interval.is_zero()
            || self.poll_interval.is_zero()
            || self.retry_base.is_zero()
            || self.retry_max.is_zero()
            || self.max_company_news_in_flight == 0
        {
            return Err(SchedulerError::InvalidConfig(
                "scheduler durations must be positive".to_owned(),
            ));
        }
        if self.heartbeat_interval >= self.lease_duration {
            return Err(SchedulerError::InvalidConfig(
                "heartbeat interval must be shorter than the lease".to_owned(),
            ));
        }
        if self.retry_base > self.retry_max {
            return Err(SchedulerError::InvalidConfig(
                "retry base must not exceed retry maximum".to_owned(),
            ));
        }
        Ok(self)
    }
}

#[derive(Clone)]
pub struct JobRunner<H> {
    database: Database,
    worker_id: String,
    handler: Arc<H>,
    config: JobRunnerConfig,
}

impl<H: JobHandler> JobRunner<H> {
    pub fn new(
        database: Database,
        worker_id: impl Into<String>,
        handler: Arc<H>,
        config: JobRunnerConfig,
    ) -> Result<Self, SchedulerError> {
        Ok(Self {
            database,
            worker_id: worker_id.into(),
            handler,
            config: config.validate()?,
        })
    }

    pub async fn run_until_cancelled(
        &self,
        shutdown: CancellationToken,
    ) -> Result<(), SchedulerError> {
        let supported = self.handler.supported_job_types();
        info!(
            worker_id = %self.worker_id,
            supported_job_types = ?supported,
            "job runner started"
        );

        loop {
            if shutdown.is_cancelled() {
                break;
            }

            if supported.is_empty() {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = tokio::time::sleep(self.config.poll_interval) => {}
                }
                continue;
            }

            match self.run_once(shutdown.clone()).await {
                Ok(JobRunOutcome::Idle) => {
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = tokio::time::sleep(self.config.poll_interval) => {}
                    }
                }
                Ok(outcome) => {
                    debug!(?outcome, "job runner iteration finished");
                }
                Err(error) => {
                    error!(%error, "job runner iteration failed");
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = tokio::time::sleep(self.config.poll_interval) => {}
                    }
                }
            }
        }

        info!(worker_id = %self.worker_id, "job runner stopped");
        Ok(())
    }

    pub async fn run_once(
        &self,
        shutdown: CancellationToken,
    ) -> Result<JobRunOutcome, SchedulerError> {
        let Some(claimed) = self
            .database
            .claim_job(
                &self.worker_id,
                self.config.lease_duration,
                self.handler.supported_job_types(),
                self.config.max_company_news_in_flight,
            )
            .await?
        else {
            return Ok(JobRunOutcome::Idle);
        };

        let job_id = claimed.job.id;
        info!(
            %job_id,
            job_type = %claimed.job.job_type,
            job_key = %claimed.job.job_key,
            attempt = claimed.job.attempt_count,
            "claimed job"
        );

        let handler_future = self.handler.handle(&claimed.job);
        tokio::pin!(handler_future);

        let first_heartbeat = Instant::now() + self.config.heartbeat_interval;
        let mut heartbeat =
            tokio::time::interval_at(first_heartbeat, self.config.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);

        let handler_result = loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    if claimed.job.job_type == JobType::DiscoverCompany {
                        self.database
                            .cancel_running_discovery_runs_for_job(
                                job_id,
                                claimed.lease_token,
                                "worker shut down during discovery",
                            )
                            .await?;
                    } else if claimed.job.job_type == JobType::ValidateCandidate {
                        self.database
                            .cancel_running_candidate_validations_for_job(
                                job_id,
                                claimed.lease_token,
                                "worker shut down during candidate validation",
                            )
                            .await?;
                    } else if claimed.job.job_type == JobType::CrawlSource {
                        self.database
                            .cancel_running_crawl_runs_for_job(
                                job_id,
                                claimed.lease_token,
                                "worker shut down during source crawl",
                            )
                            .await?;
                    } else if claimed.job.job_type == JobType::CrawlContent {
                        self.database
                            .cancel_running_content_crawl_attempts_for_job(
                                job_id,
                                claimed.lease_token,
                                "worker shut down during content crawl",
                            )
                            .await?;
                    } else if claimed.job.job_type == JobType::ExtractCompanyNews {
                        self.database
                            .cancel_running_company_news_extractions_for_job(
                                job_id,
                                claimed.lease_token,
                                "worker shut down during company news extraction",
                            )
                            .await?;
                    }
                    let retry_at = Utc::now();
                    let outcome = self.database.record_job_failure(
                        job_id,
                        claimed.lease_token,
                        "worker shutting down",
                        Some(retry_at),
                    ).await?;
                    return Ok(match outcome {
                        JobFailureOutcome::Retrying { run_after } => {
                            JobRunOutcome::ShutdownRequeued { job_id, run_after }
                        }
                        JobFailureOutcome::Failed => JobRunOutcome::Failed { job_id },
                    });
                }
                result = &mut handler_future => break result,
                _ = heartbeat.tick() => {
                    let renewed = self.database.heartbeat_job(
                        job_id,
                        claimed.lease_token,
                        self.config.lease_duration,
                    ).await?;
                    if !renewed {
                        warn!(%job_id, "job lease was lost while handler was running");
                        if claimed.job.job_type == JobType::DiscoverCompany {
                            self.database
                                .cancel_running_discovery_runs_for_job(
                                    job_id,
                                    claimed.lease_token,
                                    "worker lost its lease during discovery",
                                )
                                .await?;
                        } else if claimed.job.job_type == JobType::ValidateCandidate {
                            self.database
                                .cancel_running_candidate_validations_for_job(
                                    job_id,
                                    claimed.lease_token,
                                    "worker lost its lease during candidate validation",
                                )
                                .await?;
                        } else if claimed.job.job_type == JobType::CrawlSource {
                            self.database
                                .cancel_running_crawl_runs_for_job(
                                    job_id,
                                    claimed.lease_token,
                                    "worker lost its lease during source crawl",
                                )
                                .await?;
                        } else if claimed.job.job_type == JobType::CrawlContent {
                            self.database
                                .cancel_running_content_crawl_attempts_for_job(
                                    job_id,
                                    claimed.lease_token,
                                    "worker lost its lease during content crawl",
                                )
                                .await?;
                        } else if claimed.job.job_type == JobType::ExtractCompanyNews {
                            self.database
                                .cancel_running_company_news_extractions_for_job(
                                    job_id,
                                    claimed.lease_token,
                                    "worker lost its lease during company news extraction",
                                )
                                .await?;
                        }
                        return Ok(JobRunOutcome::LostLease { job_id });
                    }
                }
            }
        };

        match handler_result {
            Ok(()) => {
                if self
                    .database
                    .complete_job(job_id, claimed.lease_token)
                    .await?
                {
                    info!(%job_id, "completed job");
                    Ok(JobRunOutcome::Completed { job_id })
                } else {
                    warn!(%job_id, "job completed after its lease was lost");
                    Ok(JobRunOutcome::LostLease { job_id })
                }
            }
            Err(handler_error) => {
                if claimed.job.job_type == JobType::DiscoverCompany {
                    self.database
                        .cancel_running_discovery_runs_for_job(
                            job_id,
                            claimed.lease_token,
                            &format!("discovery handler returned an error: {handler_error}"),
                        )
                        .await?;
                } else if claimed.job.job_type == JobType::ValidateCandidate {
                    self.database
                        .cancel_running_candidate_validations_for_job(
                            job_id,
                            claimed.lease_token,
                            &format!(
                                "candidate validation handler returned an error: {handler_error}"
                            ),
                        )
                        .await?;
                } else if claimed.job.job_type == JobType::CrawlSource {
                    self.database
                        .cancel_running_crawl_runs_for_job(
                            job_id,
                            claimed.lease_token,
                            &format!("source crawl handler returned an error: {handler_error}"),
                        )
                        .await?;
                } else if claimed.job.job_type == JobType::CrawlContent {
                    self.database
                        .cancel_running_content_crawl_attempts_for_job(
                            job_id,
                            claimed.lease_token,
                            &format!("content crawl handler returned an error: {handler_error}"),
                        )
                        .await?;
                } else if claimed.job.job_type == JobType::ExtractCompanyNews {
                    self.database
                        .cancel_running_company_news_extractions_for_job(
                            job_id,
                            claimed.lease_token,
                            &format!(
                                "company news extraction handler returned an error: {handler_error}"
                            ),
                        )
                        .await?;
                }
                let retry_at = if handler_error.is_retryable() {
                    let delay = retry_delay(
                        self.config.retry_base,
                        self.config.retry_max,
                        claimed.job.attempt_count,
                    );
                    let delay = chrono::Duration::from_std(delay)
                        .map_err(|_| SchedulerError::RetryDelayOutOfRange(delay))?;
                    Some(Utc::now() + delay)
                } else {
                    None
                };
                let outcome = self
                    .database
                    .record_job_failure(
                        job_id,
                        claimed.lease_token,
                        &handler_error.to_string(),
                        retry_at,
                    )
                    .await?;
                if let Some(cooldown) = handler_error.worker_cooldown() {
                    warn!(
                        %job_id,
                        cooldown_seconds = cooldown.as_secs_f64(),
                        "cooling down worker after shared dependency failure"
                    );
                    tokio::select! {
                        _ = shutdown.cancelled() => {}
                        _ = tokio::time::sleep(cooldown) => {}
                    }
                }
                match outcome {
                    JobFailureOutcome::Retrying { run_after } => {
                        warn!(%job_id, %run_after, error = %handler_error, "job scheduled for retry");
                        Ok(JobRunOutcome::RetryScheduled { job_id, run_after })
                    }
                    JobFailureOutcome::Failed => {
                        error!(%job_id, error = %handler_error, "job failed permanently");
                        Ok(JobRunOutcome::Failed { job_id })
                    }
                }
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JobRunOutcome {
    Idle,
    Completed {
        job_id: Uuid,
    },
    RetryScheduled {
        job_id: Uuid,
        run_after: DateTime<Utc>,
    },
    Failed {
        job_id: Uuid,
    },
    LostLease {
        job_id: Uuid,
    },
    ShutdownRequeued {
        job_id: Uuid,
        run_after: DateTime<Utc>,
    },
}

#[derive(Default)]
pub struct NoopJobHandler;

#[async_trait]
impl JobHandler for NoopJobHandler {
    fn supported_job_types(&self) -> &[JobType] {
        &[]
    }

    async fn handle(&self, job: &Job) -> Result<(), JobHandlerError> {
        Err(JobHandlerError::permanent(format!(
            "no handler is registered for {}",
            job.job_type
        )))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("invalid scheduler configuration: {0}")]
    InvalidConfig(String),
    #[error("retry delay {0:?} cannot be represented by chrono")]
    RetryDelayOutOfRange(Duration),
    #[error("more than one handler was registered for {0}")]
    DuplicateHandler(JobType),
}

fn retry_delay(base: Duration, maximum: Duration, attempt_count: i32) -> Duration {
    let exponent = u32::try_from(attempt_count.saturating_sub(1))
        .unwrap_or_default()
        .min(31);
    base.saturating_mul(2_u32.saturating_pow(exponent))
        .min(maximum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_is_exponential_and_capped() {
        let base = Duration::from_secs(30);
        let maximum = Duration::from_secs(300);

        assert_eq!(retry_delay(base, maximum, 1), Duration::from_secs(30));
        assert_eq!(retry_delay(base, maximum, 2), Duration::from_secs(60));
        assert_eq!(retry_delay(base, maximum, 4), Duration::from_secs(240));
        assert_eq!(retry_delay(base, maximum, 5), maximum);
        assert_eq!(retry_delay(base, maximum, i32::MAX), maximum);
    }

    #[test]
    fn config_rejects_heartbeat_that_cannot_renew_in_time() {
        let config = JobRunnerConfig {
            lease_duration: Duration::from_secs(30),
            heartbeat_interval: Duration::from_secs(30),
            poll_interval: Duration::from_secs(1),
            retry_base: Duration::from_secs(1),
            retry_max: Duration::from_secs(60),
            max_company_news_in_flight: 1,
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn registry_rejects_duplicate_job_type_handlers() {
        let registry = JobHandlerRegistry::new()
            .register(Arc::new(TestHandler))
            .expect("register first handler");
        assert!(registry.register(Arc::new(TestHandler)).is_err());
    }

    struct TestHandler;

    #[async_trait]
    impl JobHandler for TestHandler {
        fn supported_job_types(&self) -> &[JobType] {
            &[JobType::DiscoverCompany]
        }

        async fn handle(&self, _job: &Job) -> Result<(), JobHandlerError> {
            Ok(())
        }
    }
}
