use std::str::FromStr;

use chrono::{DateTime, Utc};
use feed_core::{
    CandidateDecision, CandidateDecisionMode, CandidateStatus, Source, SourceKind, SourceStatus,
};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::FromRow;
use url::Url;
use uuid::Uuid;

use crate::{Database, DatabaseError};

#[derive(Debug, FromRow)]
pub(crate) struct SourceRow {
    id: Uuid,
    source_id: String,
    company_id: Uuid,
    kind: String,
    url: String,
    status: String,
    freshness_slo_seconds: i32,
    browser_required: bool,
    public_export_allowed: bool,
    discovery_confidence: Option<f64>,
    metadata: Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq, FromRow, Serialize)]
pub struct ApprovedSourceCompanyClaim {
    pub source_id: Uuid,
    pub company_id: Uuid,
    pub company_name: String,
}

impl SourceRow {
    pub(crate) fn into_domain(self) -> Result<Source, DatabaseError> {
        Ok(Source {
            id: self.id,
            source_id: self.source_id,
            company_id: self.company_id,
            kind: SourceKind::from_str(&self.kind)?,
            url: Url::parse(&self.url)?,
            status: SourceStatus::from_str(&self.status)?,
            freshness_slo_seconds: self.freshness_slo_seconds,
            browser_required: self.browser_required,
            public_export_allowed: self.public_export_allowed,
            discovery_confidence: self.discovery_confidence,
            metadata: self.metadata,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(Debug, FromRow)]
struct AcceptanceCandidateRow {
    id: Uuid,
    company_id: Uuid,
    candidate_url: String,
    candidate_kind: String,
    confidence: f64,
    evidence: Value,
    status: String,
    accepted_source_id: Option<Uuid>,
}

impl Database {
    pub async fn get_source(&self, source_id: Uuid) -> Result<Option<Source>, DatabaseError> {
        load_source_row_from_pool(self.pool(), source_id)
            .await?
            .map(SourceRow::into_domain)
            .transpose()
    }

    pub async fn list_approved_feed_source_company_claims(
        &self,
        source_url: &Url,
    ) -> Result<Vec<ApprovedSourceCompanyClaim>, DatabaseError> {
        Ok(sqlx::query_as::<_, ApprovedSourceCompanyClaim>(
            r#"
            SELECT
                source.id AS source_id,
                source.company_id,
                company.name AS company_name
            FROM sources AS source
            JOIN companies AS company ON company.id = source.company_id
            WHERE
                source.status = 'approved'
                AND source.kind IN ('rss', 'atom')
                AND public_url_identity_key(source.url)
                    = public_url_identity_key($1)
            ORDER BY source.created_at, source.id
            "#,
        )
        .bind(source_url.as_str())
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn accept_source_candidate(
        &self,
        candidate_id: Uuid,
        source_id: &str,
        freshness_slo_seconds: i32,
        public_export_allowed: bool,
    ) -> Result<Source, DatabaseError> {
        self.accept_source_candidate_with_decision(
            candidate_id,
            source_id,
            freshness_slo_seconds,
            public_export_allowed,
            CandidateDecisionMode::Operator,
            "feed-admin",
            "operator accepted source candidate",
            Value::Object(Default::default()),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn accept_source_candidate_with_decision(
        &self,
        candidate_id: Uuid,
        source_id: &str,
        freshness_slo_seconds: i32,
        public_export_allowed: bool,
        decision_mode: CandidateDecisionMode,
        actor: &str,
        reason: &str,
        decision_metadata: Value,
    ) -> Result<Source, DatabaseError> {
        if source_id.trim().is_empty() {
            return Err(DatabaseError::InvalidState(
                "source_id cannot be empty".to_owned(),
            ));
        }
        if freshness_slo_seconds <= 0 {
            return Err(DatabaseError::InvalidState(
                "freshness_slo_seconds must be positive".to_owned(),
            ));
        }
        if actor.trim().is_empty() || reason.trim().is_empty() {
            return Err(DatabaseError::InvalidState(
                "decision actor and reason cannot be empty".to_owned(),
            ));
        }

        let mut transaction = self.pool().begin().await?;
        let candidate = sqlx::query_as::<_, AcceptanceCandidateRow>(
            r#"
            SELECT
                id,
                company_id,
                candidate_url,
                candidate_kind,
                confidence,
                evidence,
                status,
                accepted_source_id
            FROM source_candidates
            WHERE id = $1
            FOR UPDATE
            "#,
        )
        .bind(candidate_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(DatabaseError::NotFound {
            entity: "source candidate",
            id: candidate_id,
        })?;

        let candidate_status = CandidateStatus::from_str(&candidate.status)?;
        if candidate_status == CandidateStatus::Rejected {
            return Err(DatabaseError::InvalidState(format!(
                "source candidate {} has been rejected",
                candidate.id
            )));
        }
        if let Some(accepted_source_id) = candidate.accepted_source_id {
            let source = load_source_row(&mut transaction, accepted_source_id)
                .await?
                .ok_or_else(|| {
                    DatabaseError::Invariant(format!(
                        "accepted candidate {} references missing source {}",
                        candidate.id, accepted_source_id
                    ))
                })?;
            transaction.commit().await?;
            return source.into_domain();
        }

        let kind = SourceKind::from_str(&candidate.candidate_kind)?;
        let browser_required = kind == SourceKind::Browser;
        let metadata = json!({
            "accepted_candidate_id": candidate.id,
            "discovery_evidence": candidate.evidence,
            "activation": {
                "decision_mode": decision_mode.as_str(),
                "actor": actor,
                "reason": reason,
                "metadata": decision_metadata.clone(),
            },
        });
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
            VALUES ($1, $2, $3, $4, 'approved', $5, $6, $7, $8, $9)
            ON CONFLICT DO NOTHING
            RETURNING id
            "#,
        )
        .bind(source_id)
        .bind(candidate.company_id)
        .bind(kind.as_str())
        .bind(&candidate.candidate_url)
        .bind(freshness_slo_seconds)
        .bind(browser_required)
        .bind(public_export_allowed)
        .bind(candidate.confidence)
        .bind(metadata)
        .fetch_optional(&mut *transaction)
        .await?;

        let source_row = if let Some(source_id) = inserted_id {
            load_source_row(&mut transaction, source_id)
                .await?
                .ok_or_else(|| {
                    DatabaseError::Invariant(format!(
                        "newly inserted source {source_id} could not be loaded"
                    ))
                })?
        } else {
            sqlx::query_as::<_, SourceRow>(
                r#"
                SELECT
                    id,
                    source_id,
                    company_id,
                    kind,
                    url,
                    status,
                    freshness_slo_seconds,
                    browser_required,
                    public_export_allowed,
                    discovery_confidence,
                    metadata,
                    created_at,
                    updated_at
                FROM sources
                WHERE company_id = $1 AND url = $2
                "#,
            )
            .bind(candidate.company_id)
            .bind(&candidate.candidate_url)
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or_else(|| {
                DatabaseError::InvalidState(format!(
                    "source_id {source_id} conflicts with an existing source"
                ))
            })?
        };

        sqlx::query(
            r#"
            INSERT INTO source_state (source_id)
            VALUES ($1)
            ON CONFLICT (source_id) DO NOTHING
            "#,
        )
        .bind(source_row.id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            UPDATE source_candidates
            SET status = 'accepted', accepted_source_id = $2
            WHERE id = $1
            "#,
        )
        .bind(candidate_id)
        .bind(source_row.id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO event_log (event_type, company_id, source_id, payload)
            VALUES ('source_candidate.accepted', $1, $2, $3)
            "#,
        )
        .bind(candidate.company_id)
        .bind(source_row.id)
        .bind(json!({ "candidate_id": candidate_id }))
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO candidate_decisions (
                candidate_id,
                source_id,
                decision,
                decision_mode,
                actor,
                reason,
                metadata
            )
            VALUES ($1, $2, 'activated', $3, $4, $5, $6)
            "#,
        )
        .bind(candidate_id)
        .bind(source_row.id)
        .bind(decision_mode.as_str())
        .bind(actor)
        .bind(reason)
        .bind(decision_metadata)
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;
        source_row.into_domain()
    }

    pub async fn reject_source_candidate(&self, candidate_id: Uuid) -> Result<(), DatabaseError> {
        self.reject_source_candidate_with_decision(
            candidate_id,
            CandidateDecisionMode::Operator,
            "feed-admin",
            "operator rejected source candidate",
            Value::Object(Default::default()),
        )
        .await
    }

    pub async fn reject_source_candidate_with_decision(
        &self,
        candidate_id: Uuid,
        decision_mode: CandidateDecisionMode,
        actor: &str,
        reason: &str,
        metadata: Value,
    ) -> Result<(), DatabaseError> {
        if actor.trim().is_empty() || reason.trim().is_empty() {
            return Err(DatabaseError::InvalidState(
                "decision actor and reason cannot be empty".to_owned(),
            ));
        }
        let mut transaction = self.pool().begin().await?;
        let candidate = sqlx::query_as::<_, AcceptanceCandidateRow>(
            r#"
            SELECT
                id,
                company_id,
                candidate_url,
                candidate_kind,
                confidence,
                evidence,
                status,
                accepted_source_id
            FROM source_candidates
            WHERE id = $1
            FOR UPDATE
            "#,
        )
        .bind(candidate_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(DatabaseError::NotFound {
            entity: "source candidate",
            id: candidate_id,
        })?;
        let candidate_status = CandidateStatus::from_str(&candidate.status)?;
        if candidate_status == CandidateStatus::Rejected {
            return Err(DatabaseError::InvalidState(format!(
                "source candidate {candidate_id} has already been rejected"
            )));
        }
        if let Some(source_id) = candidate.accepted_source_id {
            sqlx::query(
                r#"
                UPDATE sources
                SET status = 'disabled', public_export_allowed = false
                WHERE id = $1
                "#,
            )
            .bind(source_id)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                r#"
                UPDATE jobs
                SET
                    status = 'cancelled',
                    completed_at = CURRENT_TIMESTAMP,
                    last_error = 'source disabled by operator decision'
                WHERE
                    source_id = $1
                    AND job_type = 'crawl_source'
                    AND status = 'pending'
                "#,
            )
            .bind(source_id)
            .execute(&mut *transaction)
            .await?;
        }
        sqlx::query(
            r#"
            UPDATE source_candidates
            SET status = 'rejected', accepted_source_id = NULL
            WHERE id = $1
            "#,
        )
        .bind(candidate_id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO candidate_decisions (
                candidate_id,
                source_id,
                decision,
                decision_mode,
                actor,
                reason,
                metadata
            )
            VALUES ($1, $2, 'rejected', $3, $4, $5, $6)
            "#,
        )
        .bind(candidate_id)
        .bind(candidate.accepted_source_id)
        .bind(decision_mode.as_str())
        .bind(actor)
        .bind(reason)
        .bind(metadata)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO event_log (event_type, company_id, source_id, payload)
            VALUES ('source_candidate.rejected', $1, $2, $3)
            "#,
        )
        .bind(candidate.company_id)
        .bind(candidate.accepted_source_id)
        .bind(json!({
            "candidate_id": candidate_id,
            "disabled_accepted_source": candidate.accepted_source_id.is_some(),
        }))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn keep_source_candidate_for_review(
        &self,
        candidate_id: Uuid,
        decision_mode: CandidateDecisionMode,
        actor: &str,
        reason: &str,
        metadata: Value,
    ) -> Result<(), DatabaseError> {
        if actor.trim().is_empty() || reason.trim().is_empty() {
            return Err(DatabaseError::InvalidState(
                "decision actor and reason cannot be empty".to_owned(),
            ));
        }
        let candidate_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM source_candidates WHERE id = $1 AND status = 'new')",
        )
        .bind(candidate_id)
        .fetch_one(self.pool())
        .await?;
        if !candidate_exists {
            return Err(DatabaseError::InvalidState(format!(
                "source candidate {candidate_id} is missing or is no longer new"
            )));
        }
        sqlx::query(
            r#"
            INSERT INTO candidate_decisions (
                candidate_id,
                decision,
                decision_mode,
                actor,
                reason,
                metadata
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(candidate_id)
        .bind(CandidateDecision::KeptForReview.as_str())
        .bind(decision_mode.as_str())
        .bind(actor)
        .bind(reason)
        .bind(metadata)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn list_sources(
        &self,
        company_id: Option<Uuid>,
        status: Option<SourceStatus>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Source>, DatabaseError> {
        let status = status.map(SourceStatus::as_str);
        let rows = sqlx::query_as::<_, SourceRow>(
            r#"
            SELECT
                id,
                source_id,
                company_id,
                kind,
                url,
                status,
                freshness_slo_seconds,
                browser_required,
                public_export_allowed,
                discovery_confidence,
                metadata,
                created_at,
                updated_at
            FROM sources
            WHERE
                ($1::uuid IS NULL OR company_id = $1)
                AND ($2::text IS NULL OR status = $2)
            ORDER BY source_id
            LIMIT $3 OFFSET $4
            "#,
        )
        .bind(company_id)
        .bind(status)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(SourceRow::into_domain).collect()
    }

    pub async fn count_sources(
        &self,
        company_id: Option<Uuid>,
        status: Option<SourceStatus>,
    ) -> Result<i64, DatabaseError> {
        let status = status.map(SourceStatus::as_str);
        Ok(sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM sources
            WHERE
                ($1::uuid IS NULL OR company_id = $1)
                AND ($2::text IS NULL OR status = $2)
            "#,
        )
        .bind(company_id)
        .bind(status)
        .fetch_one(self.pool())
        .await?)
    }
}

async fn load_source_row_from_pool(
    pool: &sqlx::PgPool,
    source_id: Uuid,
) -> Result<Option<SourceRow>, sqlx::Error> {
    sqlx::query_as::<_, SourceRow>(
        r#"
        SELECT
            id,
            source_id,
            company_id,
            kind,
            url,
            status,
            freshness_slo_seconds,
            browser_required,
            public_export_allowed,
            discovery_confidence,
            metadata,
            created_at,
            updated_at
        FROM sources
        WHERE id = $1
        "#,
    )
    .bind(source_id)
    .fetch_optional(pool)
    .await
}

async fn load_source_row(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source_id: Uuid,
) -> Result<Option<SourceRow>, sqlx::Error> {
    sqlx::query_as::<_, SourceRow>(
        r#"
        SELECT
            id,
            source_id,
            company_id,
            kind,
            url,
            status,
            freshness_slo_seconds,
            browser_required,
            public_export_allowed,
            discovery_confidence,
            metadata,
            created_at,
            updated_at
        FROM sources
        WHERE id = $1
        "#,
    )
    .bind(source_id)
    .fetch_optional(&mut **transaction)
    .await
}
