use std::str::FromStr;

use chrono::{DateTime, Utc};
use feed_core::{
    CandidateDecision, CandidateDecisionMode, CandidateDecisionRecord, CandidateReviewItem,
    CandidateStatus, CandidateValidationRun, CandidateValidationStatus, JobSpec, JobType,
    ReviewDashboard, Source, SourceCandidate, SourceHealth, SourceHealthSummary, SourceKind,
    SourceStatus,
};
use serde_json::Value;
use sqlx::FromRow;
use url::Url;
use uuid::Uuid;

use crate::{Database, DatabaseError};

#[derive(Clone, Debug)]
pub struct CandidateValidationCompletion {
    pub status: CandidateValidationStatus,
    pub detected_kind: Option<SourceKind>,
    pub final_url: Option<Url>,
    pub http_status: Option<i32>,
    pub item_count: i32,
    pub titled_item_count: i32,
    pub latest_item_at: Option<DateTime<Utc>>,
    pub policy_reasons: Vec<String>,
    pub error: Option<String>,
    pub metadata: Value,
}

#[derive(Debug, FromRow)]
struct CandidateValidationRunRow {
    id: Uuid,
    candidate_id: Uuid,
    job_id: Option<Uuid>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    status: String,
    detected_kind: Option<String>,
    final_url: Option<String>,
    http_status: Option<i32>,
    item_count: i32,
    titled_item_count: i32,
    latest_item_at: Option<DateTime<Utc>>,
    policy_reasons: Value,
    error: Option<String>,
    metadata: Value,
}

impl CandidateValidationRunRow {
    fn into_domain(self) -> Result<CandidateValidationRun, DatabaseError> {
        Ok(CandidateValidationRun {
            id: self.id,
            candidate_id: self.candidate_id,
            job_id: self.job_id,
            started_at: self.started_at,
            finished_at: self.finished_at,
            status: CandidateValidationStatus::from_str(&self.status)?,
            detected_kind: self
                .detected_kind
                .map(|kind| SourceKind::from_str(&kind))
                .transpose()?,
            final_url: self.final_url.map(|url| Url::parse(&url)).transpose()?,
            http_status: self.http_status,
            item_count: self.item_count,
            titled_item_count: self.titled_item_count,
            latest_item_at: self.latest_item_at,
            policy_reasons: serde_json::from_value(self.policy_reasons)?,
            error: self.error,
            metadata: self.metadata,
        })
    }
}

#[derive(Debug, FromRow)]
struct CandidateReviewRow {
    candidate_id: Uuid,
    company_id: Uuid,
    discovery_run_id: Option<Uuid>,
    candidate_url: String,
    candidate_kind: String,
    confidence: f64,
    evidence: Value,
    candidate_status: String,
    accepted_source_id: Option<Uuid>,
    candidate_created_at: DateTime<Utc>,
    candidate_updated_at: DateTime<Utc>,
    company_key: String,
    company_name: String,
    validation_id: Option<Uuid>,
    validation_job_id: Option<Uuid>,
    validation_started_at: Option<DateTime<Utc>>,
    validation_finished_at: Option<DateTime<Utc>>,
    validation_status: Option<String>,
    validation_detected_kind: Option<String>,
    validation_final_url: Option<String>,
    validation_http_status: Option<i32>,
    validation_item_count: Option<i32>,
    validation_titled_item_count: Option<i32>,
    validation_latest_item_at: Option<DateTime<Utc>>,
    validation_policy_reasons: Option<Value>,
    validation_error: Option<String>,
    validation_metadata: Option<Value>,
}

impl CandidateReviewRow {
    fn into_domain(self) -> Result<CandidateReviewItem, DatabaseError> {
        let latest_validation = match (
            self.validation_id,
            self.validation_started_at,
            self.validation_status,
        ) {
            (Some(id), Some(started_at), Some(status)) => Some(
                CandidateValidationRunRow {
                    id,
                    candidate_id: self.candidate_id,
                    job_id: self.validation_job_id,
                    started_at,
                    finished_at: self.validation_finished_at,
                    status,
                    detected_kind: self.validation_detected_kind,
                    final_url: self.validation_final_url,
                    http_status: self.validation_http_status,
                    item_count: self.validation_item_count.unwrap_or_default(),
                    titled_item_count: self.validation_titled_item_count.unwrap_or_default(),
                    latest_item_at: self.validation_latest_item_at,
                    policy_reasons: self
                        .validation_policy_reasons
                        .unwrap_or_else(|| Value::Array(Vec::new())),
                    error: self.validation_error,
                    metadata: self
                        .validation_metadata
                        .unwrap_or_else(|| Value::Object(Default::default())),
                }
                .into_domain()?,
            ),
            (None, None, None) => None,
            _ => {
                return Err(DatabaseError::Invariant(format!(
                    "candidate {} has a partial latest validation row",
                    self.candidate_id
                )));
            }
        };

        Ok(CandidateReviewItem {
            candidate: SourceCandidate {
                id: self.candidate_id,
                company_id: self.company_id,
                discovery_run_id: self.discovery_run_id,
                candidate_url: Url::parse(&self.candidate_url)?,
                candidate_kind: SourceKind::from_str(&self.candidate_kind)?,
                confidence: self.confidence,
                evidence: self.evidence,
                status: CandidateStatus::from_str(&self.candidate_status)?,
                accepted_source_id: self.accepted_source_id,
                created_at: self.candidate_created_at,
                updated_at: self.candidate_updated_at,
            },
            company_key: self.company_key,
            company_name: self.company_name,
            latest_validation,
        })
    }
}

#[derive(Debug, FromRow)]
struct CandidateDecisionRow {
    id: Uuid,
    candidate_id: Uuid,
    source_id: Option<Uuid>,
    decision: String,
    decision_mode: String,
    actor: String,
    reason: String,
    metadata: Value,
    created_at: DateTime<Utc>,
}

impl CandidateDecisionRow {
    fn into_domain(self) -> Result<CandidateDecisionRecord, DatabaseError> {
        Ok(CandidateDecisionRecord {
            id: self.id,
            candidate_id: self.candidate_id,
            source_id: self.source_id,
            decision: CandidateDecision::from_str(&self.decision)?,
            decision_mode: CandidateDecisionMode::from_str(&self.decision_mode)?,
            actor: self.actor,
            reason: self.reason,
            metadata: self.metadata,
            created_at: self.created_at,
        })
    }
}

#[derive(Debug, FromRow)]
struct ReviewDashboardRow {
    total_companies: i64,
    companies_with_candidates: i64,
    companies_with_feed_candidates: i64,
    companies_with_activated_sources: i64,
    companies_with_healthy_sources: i64,
    new_feed_candidates: i64,
    unvalidated_feed_candidates: i64,
    validation_pending: i64,
    validation_running: i64,
    valid_candidates: i64,
    needs_review_candidates: i64,
    invalid_candidates: i64,
    failed_validations: i64,
    activated_sources: i64,
    healthy_sources: i64,
    failing_sources: i64,
    sources_awaiting_first_crawl: i64,
    normalized_items: i64,
}

impl From<ReviewDashboardRow> for ReviewDashboard {
    fn from(row: ReviewDashboardRow) -> Self {
        Self {
            total_companies: row.total_companies,
            companies_with_candidates: row.companies_with_candidates,
            companies_with_feed_candidates: row.companies_with_feed_candidates,
            companies_with_activated_sources: row.companies_with_activated_sources,
            companies_with_healthy_sources: row.companies_with_healthy_sources,
            new_feed_candidates: row.new_feed_candidates,
            unvalidated_feed_candidates: row.unvalidated_feed_candidates,
            validation_pending: row.validation_pending,
            validation_running: row.validation_running,
            valid_candidates: row.valid_candidates,
            needs_review_candidates: row.needs_review_candidates,
            invalid_candidates: row.invalid_candidates,
            failed_validations: row.failed_validations,
            activated_sources: row.activated_sources,
            healthy_sources: row.healthy_sources,
            failing_sources: row.failing_sources,
            sources_awaiting_first_crawl: row.sources_awaiting_first_crawl,
            normalized_items: row.normalized_items,
        }
    }
}

#[derive(Debug, FromRow)]
struct SourceHealthSummaryRow {
    id: Uuid,
    source_id: String,
    company_id: Uuid,
    kind: String,
    url: String,
    source_status: String,
    freshness_slo_seconds: i32,
    browser_required: bool,
    public_export_allowed: bool,
    discovery_confidence: Option<f64>,
    source_metadata: Value,
    source_created_at: DateTime<Utc>,
    source_updated_at: DateTime<Utc>,
    company_key: String,
    company_name: String,
    health_source_id: Option<Uuid>,
    last_attempt_at: Option<DateTime<Utc>>,
    last_success_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    consecutive_failures: Option<i32>,
    backoff_until: Option<DateTime<Utc>>,
    consecutive_zero_runs: Option<i32>,
    total_successful_runs: Option<i64>,
    total_items: Option<i64>,
    last_nonzero_at: Option<DateTime<Utc>>,
    health_updated_at: Option<DateTime<Utc>>,
    stored_item_count: i64,
    latest_item_at: Option<DateTime<Utc>>,
}

impl SourceHealthSummaryRow {
    fn into_domain(self) -> Result<SourceHealthSummary, DatabaseError> {
        let health = self.health_source_id.map(|source_id| SourceHealth {
            source_id,
            last_attempt_at: self.last_attempt_at,
            last_success_at: self.last_success_at,
            last_error: self.last_error,
            consecutive_failures: self.consecutive_failures.unwrap_or_default(),
            backoff_until: self.backoff_until,
            consecutive_zero_runs: self.consecutive_zero_runs.unwrap_or_default(),
            total_successful_runs: self.total_successful_runs.unwrap_or_default(),
            total_items: self.total_items.unwrap_or_default(),
            last_nonzero_at: self.last_nonzero_at,
            updated_at: self.health_updated_at.unwrap_or(self.source_updated_at),
        });
        Ok(SourceHealthSummary {
            source: Source {
                id: self.id,
                source_id: self.source_id,
                company_id: self.company_id,
                kind: SourceKind::from_str(&self.kind)?,
                url: Url::parse(&self.url)?,
                status: SourceStatus::from_str(&self.source_status)?,
                freshness_slo_seconds: self.freshness_slo_seconds,
                browser_required: self.browser_required,
                public_export_allowed: self.public_export_allowed,
                discovery_confidence: self.discovery_confidence,
                metadata: self.source_metadata,
                created_at: self.source_created_at,
                updated_at: self.source_updated_at,
            },
            company_key: self.company_key,
            company_name: self.company_name,
            health,
            stored_item_count: self.stored_item_count,
            latest_item_at: self.latest_item_at,
        })
    }
}

impl Database {
    pub async fn enqueue_unvalidated_candidate_jobs(
        &self,
        now: DateTime<Utc>,
        queue_target: u32,
    ) -> Result<u64, DatabaseError> {
        self.enqueue_unvalidated_candidate_jobs_with_coverage(now, queue_target, false)
            .await
    }

    pub async fn enqueue_unvalidated_candidate_jobs_including_covered(
        &self,
        now: DateTime<Utc>,
        queue_target: u32,
    ) -> Result<u64, DatabaseError> {
        self.enqueue_unvalidated_candidate_jobs_with_coverage(now, queue_target, true)
            .await
    }

    async fn enqueue_unvalidated_candidate_jobs_with_coverage(
        &self,
        now: DateTime<Utc>,
        queue_target: u32,
        include_covered_companies: bool,
    ) -> Result<u64, DatabaseError> {
        if queue_target == 0 {
            return Err(DatabaseError::Invariant(
                "validation queue target must be positive".to_owned(),
            ));
        }
        let mut transaction = self.pool().begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('enqueue_unvalidated_candidate_jobs', 0))",
        )
        .execute(&mut *transaction)
        .await?;
        let active_jobs: i64 = sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM jobs
            WHERE job_type = 'validate_candidate' AND status IN ('pending', 'running')
            "#,
        )
        .fetch_one(&mut *transaction)
        .await?;
        let available_slots = i64::from(queue_target).saturating_sub(active_jobs).max(0);
        if available_slots == 0 {
            transaction.commit().await?;
            return Ok(0);
        }

        let result = sqlx::query(
            r#"
            WITH eligible_candidates AS (
                SELECT
                    candidate.*,
                    row_number() OVER (
                        PARTITION BY candidate.company_id
                        ORDER BY
                            candidate.confidence DESC,
                            candidate.created_at,
                            candidate.id
                    ) AS company_rank
                FROM source_candidates AS candidate
                WHERE
                    candidate.status = 'new'
                    AND candidate.candidate_kind IN ('rss', 'atom')
                    AND (
                        $3::boolean
                        OR NOT EXISTS (
                            SELECT 1
                            FROM sources AS source
                            WHERE
                                source.company_id = candidate.company_id
                                AND source.status = 'approved'
                        )
                        OR (
                            EXISTS (
                                SELECT 1
                                FROM discovery_runs AS discovery
                                JOIN jobs AS discovery_job
                                  ON discovery_job.id = discovery.job_id
                                WHERE
                                    discovery.id = candidate.discovery_run_id
                                    AND discovery_job.payload->>'seed_origin'
                                        = 'company_news_recipe_builder'
                            )
                            AND NOT EXISTS (
                                SELECT 1
                                FROM sources AS feed_source
                                JOIN source_state AS feed_state
                                  ON feed_state.source_id = feed_source.id
                                WHERE
                                    feed_source.company_id = candidate.company_id
                                    AND feed_source.status = 'approved'
                                    AND feed_source.kind IN ('rss', 'atom')
                                    AND feed_state.last_success_at IS NOT NULL
                                    AND feed_state.consecutive_failures = 0
                                    AND feed_state.consecutive_zero_runs < 3
                            )
                        )
                    )
                    AND NOT EXISTS (
                        SELECT 1
                        FROM candidate_validation_runs AS validation
                        WHERE validation.candidate_id = candidate.id
                    )
                    AND NOT EXISTS (
                        SELECT 1
                        FROM jobs AS active_job
                        WHERE
                            active_job.job_type = 'validate_candidate'
                            AND active_job.job_key = 'candidate:' || candidate.id::text
                            AND active_job.status IN ('pending', 'running')
                    )
            )
            INSERT INTO jobs (
                job_type,
                job_key,
                status,
                priority,
                run_after,
                company_id,
                candidate_id,
                payload
            )
            SELECT
                'validate_candidate',
                'candidate:' || candidate.id::text,
                'pending',
                (candidate.confidence * 1000)::smallint,
                $1,
                candidate.company_id,
                candidate.id,
                jsonb_build_object('candidate_id', candidate.id)
            FROM eligible_candidates AS candidate
            WHERE
                candidate.company_rank = 1
            ORDER BY candidate.confidence DESC, candidate.created_at, candidate.id
            LIMIT $2
            ON CONFLICT (job_type, job_key)
                WHERE status IN ('pending', 'running')
            DO NOTHING
            "#,
        )
        .bind(now)
        .bind(available_slots)
        .bind(include_covered_companies)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(result.rows_affected())
    }

    pub async fn reconsider_automatically_rejected_scope_candidates(
        &self,
        now: DateTime<Utc>,
        limit: u32,
    ) -> Result<u64, DatabaseError> {
        if limit == 0 {
            return Err(DatabaseError::Invariant(
                "automatic scope-reconsideration limit must be positive".to_owned(),
            ));
        }
        let mut transaction = self.pool().begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('enqueue_unvalidated_candidate_jobs', 0))",
        )
        .execute(&mut *transaction)
        .await?;
        let result = sqlx::query(
            r#"
            WITH eligible_candidates AS (
                SELECT
                    candidate.id,
                    candidate.company_id,
                    candidate.confidence,
                    candidate.created_at,
                    row_number() OVER (
                        PARTITION BY candidate.company_id
                        ORDER BY
                            candidate.confidence DESC,
                            candidate.created_at,
                            candidate.id
                    ) AS company_rank
                FROM source_candidates AS candidate
                JOIN LATERAL (
                    SELECT validation.status, validation.metadata
                    FROM candidate_validation_runs AS validation
                    WHERE validation.candidate_id = candidate.id
                    ORDER BY validation.started_at DESC, validation.id DESC
                    LIMIT 1
                ) AS latest_validation ON true
                JOIN LATERAL (
                    SELECT decision.decision, decision.decision_mode
                    FROM candidate_decisions AS decision
                    WHERE decision.candidate_id = candidate.id
                    ORDER BY decision.created_at DESC, decision.id DESC
                    LIMIT 1
                ) AS latest_decision ON true
                WHERE
                    candidate.status = 'rejected'
                    AND candidate.candidate_kind IN ('rss', 'atom')
                    AND latest_validation.status = 'invalid'
                    AND latest_validation.metadata->'policy'->>'company_scope_passed' = 'false'
                    AND latest_validation.metadata->'policy'->>'adapter_recommended' = 'true'
                    AND latest_validation.metadata->'policy'->>'has_usable_items' = 'true'
                    AND latest_validation.metadata->'policy'->>'sitemap_source' = 'false'
                    AND latest_validation.metadata->'policy'->>'non_editorial_item_scope' = 'false'
                    AND latest_validation.metadata->'policy'->>'publication_host_excluded' = 'false'
                    AND latest_validation.metadata->'policy'->>'redundant_with_approved_feed' = 'false'
                    AND NULLIF(btrim(latest_validation.metadata->'feed'->>'feed_title'), '') IS NOT NULL
                    AND latest_decision.decision = 'rejected'
                    AND latest_decision.decision_mode = 'automatic'
                    AND NOT EXISTS (
                        SELECT 1
                        FROM candidate_decisions AS operator_decision
                        WHERE
                            operator_decision.candidate_id = candidate.id
                            AND operator_decision.decision = 'rejected'
                            AND operator_decision.decision_mode = 'operator'
                    )
                    AND NOT EXISTS (
                        SELECT 1
                        FROM jobs AS active_job
                        WHERE
                            active_job.job_type = 'validate_candidate'
                            AND active_job.job_key = 'candidate:' || candidate.id::text
                            AND active_job.status IN ('pending', 'running')
                    )
            ),
            selected AS (
                SELECT id
                FROM eligible_candidates
                WHERE company_rank = 1
                ORDER BY confidence DESC, created_at, id
                LIMIT $2
            ),
            reopened AS (
                UPDATE source_candidates AS candidate
                SET status = 'new', accepted_source_id = NULL
                FROM selected
                WHERE candidate.id = selected.id
                RETURNING candidate.id, candidate.company_id, candidate.confidence
            ),
            logged AS (
                INSERT INTO event_log (event_type, company_id, payload)
                SELECT
                    'source_candidate.reopened_for_validation',
                    reopened.company_id,
                    jsonb_build_object(
                        'candidate_id', reopened.id,
                        'reason', 'feed_title_company_scope_policy_v1'
                    )
                FROM reopened
                RETURNING id
            )
            INSERT INTO jobs (
                job_type,
                job_key,
                status,
                priority,
                run_after,
                company_id,
                candidate_id,
                payload
            )
            SELECT
                'validate_candidate',
                'candidate:' || reopened.id::text,
                'pending',
                (reopened.confidence * 1000)::smallint,
                $1,
                reopened.company_id,
                reopened.id,
                jsonb_build_object(
                    'candidate_id', reopened.id,
                    'revalidation_reason', 'feed_title_company_scope_policy_v1'
                )
            FROM reopened
            ON CONFLICT (job_type, job_key)
                WHERE status IN ('pending', 'running')
            DO NOTHING
            "#,
        )
        .bind(now)
        .bind(i64::from(limit))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(result.rows_affected())
    }

    pub async fn enqueue_candidate_validation(
        &self,
        candidate_id: Uuid,
        now: DateTime<Utc>,
        priority: i16,
    ) -> Result<feed_core::Job, DatabaseError> {
        let candidate =
            self.get_source_candidate(candidate_id)
                .await?
                .ok_or(DatabaseError::NotFound {
                    entity: "source candidate",
                    id: candidate_id,
                })?;
        if candidate.status != CandidateStatus::New {
            return Err(DatabaseError::InvalidState(format!(
                "source candidate {candidate_id} is not reviewable"
            )));
        }
        if !matches!(candidate.candidate_kind, SourceKind::Rss | SourceKind::Atom) {
            return Err(DatabaseError::InvalidState(format!(
                "source candidate {candidate_id} is not RSS/Atom"
            )));
        }
        let mut spec = JobSpec::new(
            JobType::ValidateCandidate,
            format!("candidate:{candidate_id}"),
            now,
        );
        spec.priority = priority;
        spec.max_attempts = 3;
        spec.company_id = Some(candidate.company_id);
        spec.candidate_id = Some(candidate_id);
        spec.payload = serde_json::json!({ "candidate_id": candidate_id });
        self.enqueue_job(&spec).await
    }

    pub async fn begin_candidate_validation_run(
        &self,
        candidate_id: Uuid,
        job_id: Uuid,
    ) -> Result<Uuid, DatabaseError> {
        let run_id = sqlx::query_scalar(
            r#"
            INSERT INTO candidate_validation_runs (candidate_id, job_id, status)
            SELECT id, $2, 'running'
            FROM source_candidates
            WHERE
                id = $1
                AND status = 'new'
                AND candidate_kind IN ('rss', 'atom')
            RETURNING id
            "#,
        )
        .bind(candidate_id)
        .bind(job_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| {
            DatabaseError::InvalidState(format!(
                "source candidate {candidate_id} is missing or not validation-eligible"
            ))
        })?;
        Ok(run_id)
    }

    pub async fn complete_candidate_validation_run(
        &self,
        run_id: Uuid,
        completion: &CandidateValidationCompletion,
    ) -> Result<(), DatabaseError> {
        if completion.status == CandidateValidationStatus::Running {
            return Err(DatabaseError::InvalidState(
                "validation completion cannot remain running".to_owned(),
            ));
        }
        if completion.item_count < 0
            || completion.titled_item_count < 0
            || completion.titled_item_count > completion.item_count
        {
            return Err(DatabaseError::InvalidState(
                "validation item counts are inconsistent".to_owned(),
            ));
        }
        let result = sqlx::query(
            r#"
            UPDATE candidate_validation_runs
            SET
                status = $2,
                finished_at = CURRENT_TIMESTAMP,
                detected_kind = $3,
                final_url = $4,
                http_status = $5,
                item_count = $6,
                titled_item_count = $7,
                latest_item_at = $8,
                policy_reasons = $9,
                error = $10,
                metadata = $11
            WHERE id = $1 AND status = 'running'
            "#,
        )
        .bind(run_id)
        .bind(completion.status.as_str())
        .bind(completion.detected_kind.map(SourceKind::as_str))
        .bind(completion.final_url.as_ref().map(Url::as_str))
        .bind(completion.http_status)
        .bind(completion.item_count)
        .bind(completion.titled_item_count)
        .bind(completion.latest_item_at)
        .bind(serde_json::to_value(&completion.policy_reasons)?)
        .bind(&completion.error)
        .bind(&completion.metadata)
        .execute(self.pool())
        .await?;
        if result.rows_affected() != 1 {
            return Err(DatabaseError::NotFound {
                entity: "running candidate validation",
                id: run_id,
            });
        }
        Ok(())
    }

    pub async fn cancel_running_candidate_validations_for_job(
        &self,
        job_id: Uuid,
        reason: &str,
    ) -> Result<u64, DatabaseError> {
        let result = sqlx::query(
            r#"
            UPDATE candidate_validation_runs
            SET
                status = 'cancelled',
                finished_at = CURRENT_TIMESTAMP,
                error = $2
            WHERE job_id = $1 AND status = 'running'
            "#,
        )
        .bind(job_id)
        .bind(reason)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn list_candidate_validation_runs(
        &self,
        candidate_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<CandidateValidationRun>, DatabaseError> {
        let rows = sqlx::query_as::<_, CandidateValidationRunRow>(
            r#"
            SELECT
                id,
                candidate_id,
                job_id,
                started_at,
                finished_at,
                status,
                detected_kind,
                final_url,
                http_status,
                item_count,
                titled_item_count,
                latest_item_at,
                policy_reasons,
                error,
                metadata
            FROM candidate_validation_runs
            WHERE $1::uuid IS NULL OR candidate_id = $1
            ORDER BY started_at DESC, id DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(candidate_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(CandidateValidationRunRow::into_domain)
            .collect()
    }

    pub async fn count_candidate_validation_runs(
        &self,
        candidate_id: Option<Uuid>,
    ) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM candidate_validation_runs
            WHERE $1::uuid IS NULL OR candidate_id = $1
            "#,
        )
        .bind(candidate_id)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn list_review_candidates(
        &self,
        candidate_status: Option<CandidateStatus>,
        validation_status: Option<CandidateValidationStatus>,
        candidate_kind: Option<SourceKind>,
        feed_only: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<CandidateReviewItem>, DatabaseError> {
        let rows = sqlx::query_as::<_, CandidateReviewRow>(
            r#"
            SELECT
                candidate.id AS candidate_id,
                candidate.company_id,
                candidate.discovery_run_id,
                candidate.candidate_url,
                candidate.candidate_kind,
                candidate.confidence,
                candidate.evidence,
                candidate.status AS candidate_status,
                candidate.accepted_source_id,
                candidate.created_at AS candidate_created_at,
                candidate.updated_at AS candidate_updated_at,
                company.company_key,
                company.name AS company_name,
                validation.id AS validation_id,
                validation.job_id AS validation_job_id,
                validation.started_at AS validation_started_at,
                validation.finished_at AS validation_finished_at,
                validation.status AS validation_status,
                validation.detected_kind AS validation_detected_kind,
                validation.final_url AS validation_final_url,
                validation.http_status AS validation_http_status,
                validation.item_count AS validation_item_count,
                validation.titled_item_count AS validation_titled_item_count,
                validation.latest_item_at AS validation_latest_item_at,
                validation.policy_reasons AS validation_policy_reasons,
                validation.error AS validation_error,
                validation.metadata AS validation_metadata
            FROM source_candidates AS candidate
            JOIN companies AS company ON company.id = candidate.company_id
            LEFT JOIN LATERAL (
                SELECT *
                FROM candidate_validation_runs
                WHERE candidate_id = candidate.id
                ORDER BY started_at DESC, id DESC
                LIMIT 1
            ) AS validation ON true
            WHERE
                ($1::text IS NULL OR candidate.status = $1)
                AND ($2::text IS NULL OR validation.status = $2)
                AND ($3::text IS NULL OR candidate.candidate_kind = $3)
                AND (NOT $4 OR candidate.candidate_kind IN ('rss', 'atom'))
            ORDER BY
                CASE validation.status
                    WHEN 'needs_review' THEN 0
                    WHEN 'invalid' THEN 1
                    WHEN 'failed' THEN 2
                    WHEN 'valid' THEN 3
                    WHEN 'running' THEN 4
                    ELSE 5
                END,
                candidate.confidence DESC,
                candidate.created_at,
                candidate.id
            LIMIT $5 OFFSET $6
            "#,
        )
        .bind(candidate_status.map(CandidateStatus::as_str))
        .bind(validation_status.map(CandidateValidationStatus::as_str))
        .bind(candidate_kind.map(SourceKind::as_str))
        .bind(feed_only)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(CandidateReviewRow::into_domain)
            .collect()
    }

    pub async fn count_review_candidates(
        &self,
        candidate_status: Option<CandidateStatus>,
        validation_status: Option<CandidateValidationStatus>,
        candidate_kind: Option<SourceKind>,
        feed_only: bool,
    ) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM source_candidates AS candidate
            LEFT JOIN LATERAL (
                SELECT status
                FROM candidate_validation_runs
                WHERE candidate_id = candidate.id
                ORDER BY started_at DESC, id DESC
                LIMIT 1
            ) AS validation ON true
            WHERE
                ($1::text IS NULL OR candidate.status = $1)
                AND ($2::text IS NULL OR validation.status = $2)
                AND ($3::text IS NULL OR candidate.candidate_kind = $3)
                AND (NOT $4 OR candidate.candidate_kind IN ('rss', 'atom'))
            "#,
        )
        .bind(candidate_status.map(CandidateStatus::as_str))
        .bind(validation_status.map(CandidateValidationStatus::as_str))
        .bind(candidate_kind.map(SourceKind::as_str))
        .bind(feed_only)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn get_review_dashboard(&self) -> Result<ReviewDashboard, DatabaseError> {
        let row = sqlx::query_as::<_, ReviewDashboardRow>(
            r#"
            SELECT
                (SELECT count(*) FROM companies) AS total_companies,
                (
                    SELECT count(DISTINCT company_id)
                    FROM source_candidates
                    WHERE status IN ('new', 'accepted')
                ) AS companies_with_candidates,
                (
                    SELECT count(DISTINCT company_id)
                    FROM source_candidates
                    WHERE
                        status IN ('new', 'accepted')
                        AND candidate_kind IN ('rss', 'atom')
                ) AS companies_with_feed_candidates,
                (
                    SELECT count(DISTINCT company_id)
                    FROM sources
                    WHERE status = 'approved'
                ) AS companies_with_activated_sources,
                (
                    SELECT count(DISTINCT source.company_id)
                    FROM sources AS source
                    JOIN source_state AS state ON state.source_id = source.id
                    WHERE
                        source.status = 'approved'
                        AND state.last_success_at IS NOT NULL
                        AND state.consecutive_failures = 0
                ) AS companies_with_healthy_sources,
                (
                    SELECT count(*)
                    FROM source_candidates
                    WHERE status = 'new' AND candidate_kind IN ('rss', 'atom')
                ) AS new_feed_candidates,
                (
                    SELECT count(*)
                    FROM source_candidates AS candidate
                    WHERE
                        candidate.status = 'new'
                        AND candidate.candidate_kind IN ('rss', 'atom')
                        AND NOT EXISTS (
                            SELECT 1
                            FROM candidate_validation_runs
                            WHERE candidate_id = candidate.id
                        )
                ) AS unvalidated_feed_candidates,
                (
                    SELECT count(*)
                    FROM jobs
                    WHERE job_type = 'validate_candidate' AND status = 'pending'
                ) AS validation_pending,
                (
                    SELECT count(*)
                    FROM jobs
                    WHERE job_type = 'validate_candidate' AND status = 'running'
                ) AS validation_running,
                (
                    SELECT count(*)
                    FROM candidate_validation_runs AS validation
                    JOIN source_candidates AS candidate
                        ON candidate.id = validation.candidate_id
                    WHERE
                        validation.status = 'valid'
                        AND candidate.status <> 'rejected'
                        AND NOT EXISTS (
                            SELECT 1
                            FROM candidate_validation_runs AS newer
                            WHERE
                                newer.candidate_id = validation.candidate_id
                                AND (newer.started_at, newer.id) >
                                    (validation.started_at, validation.id)
                        )
                ) AS valid_candidates,
                (
                    SELECT count(*)
                    FROM candidate_validation_runs AS validation
                    JOIN source_candidates AS candidate
                        ON candidate.id = validation.candidate_id
                    WHERE
                        validation.status = 'needs_review'
                        AND candidate.status <> 'rejected'
                        AND NOT EXISTS (
                            SELECT 1
                            FROM candidate_validation_runs AS newer
                            WHERE
                                newer.candidate_id = validation.candidate_id
                                AND (newer.started_at, newer.id) >
                                    (validation.started_at, validation.id)
                        )
                ) AS needs_review_candidates,
                (
                    SELECT count(*)
                    FROM candidate_validation_runs AS validation
                    JOIN source_candidates AS candidate
                        ON candidate.id = validation.candidate_id
                    WHERE
                        validation.status = 'invalid'
                        AND candidate.status <> 'rejected'
                        AND NOT EXISTS (
                            SELECT 1
                            FROM candidate_validation_runs AS newer
                            WHERE
                                newer.candidate_id = validation.candidate_id
                                AND (newer.started_at, newer.id) >
                                    (validation.started_at, validation.id)
                        )
                ) AS invalid_candidates,
                (
                    SELECT count(*)
                    FROM candidate_validation_runs AS validation
                    JOIN source_candidates AS candidate
                        ON candidate.id = validation.candidate_id
                    WHERE
                        validation.status = 'failed'
                        AND candidate.status <> 'rejected'
                        AND NOT EXISTS (
                            SELECT 1
                            FROM candidate_validation_runs AS newer
                            WHERE
                                newer.candidate_id = validation.candidate_id
                                AND (newer.started_at, newer.id) >
                                    (validation.started_at, validation.id)
                        )
                ) AS failed_validations,
                (
                    SELECT count(*) FROM sources WHERE status = 'approved'
                ) AS activated_sources,
                (
                    SELECT count(*)
                    FROM sources AS source
                    JOIN source_state AS state ON state.source_id = source.id
                    WHERE
                        source.status = 'approved'
                        AND state.last_success_at IS NOT NULL
                        AND state.consecutive_failures = 0
                ) AS healthy_sources,
                (
                    SELECT count(*)
                    FROM sources AS source
                    JOIN source_state AS state ON state.source_id = source.id
                    WHERE source.status = 'approved' AND state.consecutive_failures > 0
                ) AS failing_sources,
                (
                    SELECT count(*)
                    FROM sources AS source
                    LEFT JOIN source_state AS state ON state.source_id = source.id
                    WHERE
                        source.status = 'approved'
                        AND (state.source_id IS NULL OR state.last_attempt_at IS NULL)
                ) AS sources_awaiting_first_crawl,
                (
                    SELECT count(*)
                    FROM feed_items AS item
                    JOIN sources AS source ON source.id = item.source_id
                    WHERE NOT item.is_private AND source.status = 'approved'
                ) AS normalized_items
            "#,
        )
        .fetch_one(self.pool())
        .await?;
        Ok(row.into())
    }

    pub async fn list_review_source_health(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SourceHealthSummary>, DatabaseError> {
        let rows = sqlx::query_as::<_, SourceHealthSummaryRow>(
            r#"
            SELECT
                source.id,
                source.source_id,
                source.company_id,
                source.kind,
                source.url,
                source.status AS source_status,
                source.freshness_slo_seconds,
                source.browser_required,
                source.public_export_allowed,
                source.discovery_confidence,
                source.metadata AS source_metadata,
                source.created_at AS source_created_at,
                source.updated_at AS source_updated_at,
                company.company_key,
                company.name AS company_name,
                state.source_id AS health_source_id,
                state.last_attempt_at,
                state.last_success_at,
                state.last_error,
                state.consecutive_failures,
                state.backoff_until,
                state.consecutive_zero_runs,
                state.total_successful_runs,
                state.total_items,
                state.last_nonzero_at,
                state.updated_at AS health_updated_at,
                item_summary.stored_item_count,
                item_summary.latest_item_at
            FROM sources AS source
            JOIN companies AS company ON company.id = source.company_id
            LEFT JOIN source_state AS state ON state.source_id = source.id
            LEFT JOIN LATERAL (
                SELECT
                    count(*) AS stored_item_count,
                    max(published_at) AS latest_item_at
                FROM feed_items
                WHERE source_id = source.id
            ) AS item_summary ON true
            WHERE source.status = 'approved'
            ORDER BY
                CASE
                    WHEN state.consecutive_failures > 0 THEN 0
                    WHEN state.last_attempt_at IS NULL THEN 1
                    WHEN state.consecutive_zero_runs > 0 THEN 2
                    ELSE 3
                END,
                state.updated_at DESC NULLS FIRST,
                company.name,
                source.source_id
            LIMIT $1 OFFSET $2
            "#,
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(SourceHealthSummaryRow::into_domain)
            .collect()
    }

    pub async fn list_candidate_decisions(
        &self,
        candidate_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<CandidateDecisionRecord>, DatabaseError> {
        let rows = sqlx::query_as::<_, CandidateDecisionRow>(
            r#"
            SELECT
                id,
                candidate_id,
                source_id,
                decision,
                decision_mode,
                actor,
                reason,
                metadata,
                created_at
            FROM candidate_decisions
            WHERE $1::uuid IS NULL OR candidate_id = $1
            ORDER BY created_at DESC, id DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(candidate_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(CandidateDecisionRow::into_domain)
            .collect()
    }

    pub async fn count_candidate_decisions(
        &self,
        candidate_id: Option<Uuid>,
    ) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM candidate_decisions
            WHERE $1::uuid IS NULL OR candidate_id = $1
            "#,
        )
        .bind(candidate_id)
        .fetch_one(self.pool())
        .await?)
    }
}
