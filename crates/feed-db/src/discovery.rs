use std::str::FromStr;

use chrono::{DateTime, Utc};
use feed_core::{
    CandidateStatus, Company, CompanyListing, DiscoveredSource, LifecycleStatus, OwnershipStatus,
    SourceCandidate, SourceKind,
};
use serde_json::{Value, json};
use sqlx::FromRow;
use url::Url;
use uuid::Uuid;

use crate::{Database, DatabaseError};

const FAILED_DISCOVERY_CADENCE_SECONDS: i64 = 86_400;

#[derive(Debug, FromRow)]
struct CompanyRow {
    id: Uuid,
    company_key: String,
    name: String,
    aliases: Value,
    ownership_status: String,
    lifecycle_status: String,
    listings: Value,
    homepage_url: Option<String>,
    investor_relations_url: Option<String>,
    newsroom_url: Option<String>,
    blog_url: Option<String>,
    hints: Value,
    discovery_enabled: bool,
    discovery_not_before: DateTime<Utc>,
    discovery_cadence_seconds: i32,
    metadata: Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl CompanyRow {
    fn into_domain(self) -> Result<Company, DatabaseError> {
        Ok(Company {
            id: self.id,
            company_key: self.company_key,
            name: self.name,
            aliases: serde_json::from_value(self.aliases)?,
            ownership_status: OwnershipStatus::from_str(&self.ownership_status)?,
            lifecycle_status: LifecycleStatus::from_str(&self.lifecycle_status)?,
            listings: serde_json::from_value::<Vec<CompanyListing>>(self.listings)?,
            homepage_url: parse_optional_url(self.homepage_url)?,
            investor_relations_url: parse_optional_url(self.investor_relations_url)?,
            newsroom_url: parse_optional_url(self.newsroom_url)?,
            blog_url: parse_optional_url(self.blog_url)?,
            hints: serde_json::from_value(self.hints)?,
            discovery_enabled: self.discovery_enabled,
            discovery_not_before: self.discovery_not_before,
            discovery_cadence_seconds: self.discovery_cadence_seconds,
            metadata: self.metadata,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(Debug, FromRow)]
struct CandidateRow {
    id: Uuid,
    company_id: Uuid,
    discovery_run_id: Option<Uuid>,
    candidate_url: String,
    candidate_kind: String,
    confidence: f64,
    evidence: Value,
    status: String,
    accepted_source_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl CandidateRow {
    fn into_domain(self) -> Result<SourceCandidate, DatabaseError> {
        Ok(SourceCandidate {
            id: self.id,
            company_id: self.company_id,
            discovery_run_id: self.discovery_run_id,
            candidate_url: Url::parse(&self.candidate_url)?,
            candidate_kind: SourceKind::from_str(&self.candidate_kind)?,
            confidence: self.confidence,
            evidence: self.evidence,
            status: CandidateStatus::from_str(&self.status)?,
            accepted_source_id: self.accepted_source_id,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

impl Database {
    pub async fn get_source_candidate(
        &self,
        candidate_id: Uuid,
    ) -> Result<Option<SourceCandidate>, DatabaseError> {
        let row = sqlx::query_as::<_, CandidateRow>(
            r#"
            SELECT
                id,
                company_id,
                discovery_run_id,
                candidate_url,
                candidate_kind,
                confidence,
                evidence,
                status,
                accepted_source_id,
                created_at,
                updated_at
            FROM source_candidates
            WHERE id = $1
            "#,
        )
        .bind(candidate_id)
        .fetch_optional(self.pool())
        .await?;

        row.map(CandidateRow::into_domain).transpose()
    }

    pub async fn get_company(&self, company_id: Uuid) -> Result<Option<Company>, DatabaseError> {
        let row = sqlx::query_as::<_, CompanyRow>(
            r#"
            SELECT
                id,
                company_key,
                name,
                aliases,
                ownership_status,
                lifecycle_status,
                (
                    SELECT COALESCE(
                        jsonb_agg(
                            jsonb_build_object(
                                'ticker', listing.ticker,
                                'exchange', NULLIF(listing.exchange, ''),
                                'is_primary', listing.is_primary,
                                'metadata', listing.metadata
                            )
                            ORDER BY listing.is_primary DESC, listing.ticker, listing.exchange
                        ),
                        '[]'::jsonb
                    )
                    FROM company_listings AS listing
                    WHERE listing.company_id = companies.id
                ) AS listings,
                homepage_url,
                investor_relations_url,
                newsroom_url,
                blog_url,
                hints,
                discovery_enabled,
                discovery_not_before,
                discovery_cadence_seconds,
                metadata,
                created_at,
                updated_at
            FROM companies
            WHERE id = $1
            "#,
        )
        .bind(company_id)
        .fetch_optional(self.pool())
        .await?;

        row.map(CompanyRow::into_domain).transpose()
    }

    pub async fn get_company_by_key(
        &self,
        company_key: &str,
    ) -> Result<Option<Company>, DatabaseError> {
        let row = sqlx::query_as::<_, CompanyRow>(
            r#"
            SELECT
                id,
                company_key,
                name,
                aliases,
                ownership_status,
                lifecycle_status,
                (
                    SELECT COALESCE(
                        jsonb_agg(
                            jsonb_build_object(
                                'ticker', listing.ticker,
                                'exchange', NULLIF(listing.exchange, ''),
                                'is_primary', listing.is_primary,
                                'metadata', listing.metadata
                            )
                            ORDER BY listing.is_primary DESC, listing.ticker, listing.exchange
                        ),
                        '[]'::jsonb
                    )
                    FROM company_listings AS listing
                    WHERE listing.company_id = companies.id
                ) AS listings,
                homepage_url,
                investor_relations_url,
                newsroom_url,
                blog_url,
                hints,
                discovery_enabled,
                discovery_not_before,
                discovery_cadence_seconds,
                metadata,
                created_at,
                updated_at
            FROM companies
            WHERE company_key = lower($1)
            "#,
        )
        .bind(company_key)
        .fetch_optional(self.pool())
        .await?;

        row.map(CompanyRow::into_domain).transpose()
    }

    pub async fn find_companies_by_name(
        &self,
        name: &str,
        limit: i64,
    ) -> Result<Vec<Company>, DatabaseError> {
        let rows = sqlx::query_as::<_, CompanyRow>(
            r#"
            SELECT
                id,
                company_key,
                name,
                aliases,
                ownership_status,
                lifecycle_status,
                (
                    SELECT COALESCE(
                        jsonb_agg(
                            jsonb_build_object(
                                'ticker', listing.ticker,
                                'exchange', NULLIF(listing.exchange, ''),
                                'is_primary', listing.is_primary,
                                'metadata', listing.metadata
                            )
                            ORDER BY listing.is_primary DESC, listing.ticker, listing.exchange
                        ),
                        '[]'::jsonb
                    )
                    FROM company_listings AS listing
                    WHERE listing.company_id = companies.id
                ) AS listings,
                homepage_url,
                investor_relations_url,
                newsroom_url,
                blog_url,
                hints,
                discovery_enabled,
                discovery_not_before,
                discovery_cadence_seconds,
                metadata,
                created_at,
                updated_at
            FROM companies
            WHERE
                lower(name) = lower($1)
                OR EXISTS (
                    SELECT 1
                    FROM jsonb_array_elements_text(aliases) AS alias(value)
                    WHERE lower(alias.value) = lower($1)
                )
            ORDER BY company_key
            LIMIT $2
            "#,
        )
        .bind(name)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter().map(CompanyRow::into_domain).collect()
    }

    pub async fn list_aliases_colliding_with_company_names(
        &self,
        company_id: Uuid,
        aliases: &[String],
    ) -> Result<Vec<String>, DatabaseError> {
        if aliases.is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_scalar(
            r#"
            SELECT DISTINCT alias.value
            FROM unnest($2::text[]) AS alias(value)
            JOIN companies AS other ON
                other.id <> $1
                AND other.lifecycle_status = 'active'
                AND lower(btrim(other.name)) = lower(btrim(alias.value))
            ORDER BY alias.value
            "#,
        )
        .bind(company_id)
        .bind(aliases)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn list_companies(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Company>, DatabaseError> {
        let rows = sqlx::query_as::<_, CompanyRow>(
            r#"
            SELECT
                id,
                company_key,
                name,
                aliases,
                ownership_status,
                lifecycle_status,
                (
                    SELECT COALESCE(
                        jsonb_agg(
                            jsonb_build_object(
                                'ticker', listing.ticker,
                                'exchange', NULLIF(listing.exchange, ''),
                                'is_primary', listing.is_primary,
                                'metadata', listing.metadata
                            )
                            ORDER BY listing.is_primary DESC, listing.ticker, listing.exchange
                        ),
                        '[]'::jsonb
                    )
                    FROM company_listings AS listing
                    WHERE listing.company_id = companies.id
                ) AS listings,
                homepage_url,
                investor_relations_url,
                newsroom_url,
                blog_url,
                hints,
                discovery_enabled,
                discovery_not_before,
                discovery_cadence_seconds,
                metadata,
                created_at,
                updated_at
            FROM companies
            ORDER BY name, company_key
            LIMIT $1 OFFSET $2
            "#,
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter().map(CompanyRow::into_domain).collect()
    }

    pub async fn count_companies(&self) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar("SELECT count(*) FROM companies")
            .fetch_one(self.pool())
            .await?)
    }

    pub async fn enqueue_due_discovery_jobs(
        &self,
        now: DateTime<Utc>,
        queue_target: u32,
    ) -> Result<u64, DatabaseError> {
        if queue_target == 0 {
            return Err(DatabaseError::Invariant(
                "discovery queue target must be positive".to_owned(),
            ));
        }
        let mut transaction = self.pool().begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('enqueue_due_discovery_jobs', 0))",
        )
        .execute(&mut *transaction)
        .await?;
        let active_jobs: i64 = sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM jobs
            WHERE
                job_type = 'discover_company'
                AND status IN ('pending', 'running')
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
            INSERT INTO jobs (
                job_type,
                job_key,
                status,
                run_after,
                company_id,
                payload
            )
            SELECT
                'discover_company',
                'company:' || company.id::text,
                'pending',
                $1,
                company.id,
                jsonb_build_object('company_id', company.id)
            FROM companies AS company
            LEFT JOIN LATERAL (
                SELECT status, finished_at
                FROM discovery_runs
                WHERE
                    company_id = company.id
                    AND finished_at IS NOT NULL
                ORDER BY started_at DESC
                LIMIT 1
            ) AS last_run ON true
            WHERE
                company.discovery_enabled
                AND company.discovery_not_before <= $1
                AND
                (
                    last_run.finished_at IS NULL
                    OR last_run.finished_at
                        + (
                            CASE
                                WHEN last_run.status = 'failed'
                                THEN $2::bigint
                                ELSE company.discovery_cadence_seconds::bigint
                            END
                            * INTERVAL '1 second'
                        ) <= $1
                )
                AND NOT EXISTS (
                    SELECT 1
                    FROM jobs AS active_job
                    WHERE
                        active_job.job_type = 'discover_company'
                        AND active_job.job_key = 'company:' || company.id::text
                        AND active_job.status IN ('pending', 'running')
                )
            ORDER BY company.discovery_not_before, company.company_key
            LIMIT $3
            ON CONFLICT (job_type, job_key)
                WHERE status IN ('pending', 'running')
            DO NOTHING
            "#,
        )
        .bind(now)
        .bind(FAILED_DISCOVERY_CADENCE_SECONDS)
        .bind(available_slots)
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;
        Ok(result.rows_affected())
    }

    pub async fn begin_discovery_run(
        &self,
        company_id: Uuid,
        job_id: Uuid,
    ) -> Result<Uuid, DatabaseError> {
        let run_id = sqlx::query_scalar(
            r#"
            INSERT INTO discovery_runs (company_id, job_id, status)
            VALUES ($1, $2, 'running')
            RETURNING id
            "#,
        )
        .bind(company_id)
        .bind(job_id)
        .fetch_one(self.pool())
        .await?;
        Ok(run_id)
    }

    pub async fn complete_discovery_run(
        &self,
        run_id: Uuid,
        candidates: &[DiscoveredSource],
        metadata: Value,
    ) -> Result<(), DatabaseError> {
        let mut transaction = self.pool().begin().await?;
        let company_id: Uuid =
            sqlx::query_scalar("SELECT company_id FROM discovery_runs WHERE id = $1 FOR UPDATE")
                .bind(run_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or(DatabaseError::NotFound {
                    entity: "discovery run",
                    id: run_id,
                })?;

        for candidate in candidates {
            sqlx::query(
                r#"
                INSERT INTO source_candidates (
                    company_id,
                    discovery_run_id,
                    candidate_url,
                    candidate_kind,
                    confidence,
                    evidence,
                    status
                )
                VALUES ($1, $2, $3, $4, $5, $6, 'new')
                ON CONFLICT (company_id, candidate_url, candidate_kind)
                DO UPDATE SET
                    discovery_run_id = EXCLUDED.discovery_run_id,
                    confidence = GREATEST(source_candidates.confidence, EXCLUDED.confidence),
                    evidence = EXCLUDED.evidence
                "#,
            )
            .bind(company_id)
            .bind(run_id)
            .bind(candidate.candidate_url.as_str())
            .bind(candidate.candidate_kind.as_str())
            .bind(candidate.confidence)
            .bind(&candidate.evidence)
            .execute(&mut *transaction)
            .await?;
        }

        let candidate_count = i32::try_from(candidates.len()).map_err(|_| {
            DatabaseError::Invariant(
                "discovery produced more candidates than an i32 can represent".to_owned(),
            )
        })?;
        sqlx::query(
            r#"
            UPDATE discovery_runs
            SET
                status = 'completed',
                finished_at = CURRENT_TIMESTAMP,
                candidate_count = $2,
                metadata = $3
            WHERE id = $1
            "#,
        )
        .bind(run_id)
        .bind(candidate_count)
        .bind(metadata)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO event_log (event_type, company_id, payload)
            VALUES ('discovery.completed', $1, $2)
            "#,
        )
        .bind(company_id)
        .bind(json!({
            "run_id": run_id,
            "candidate_count": candidate_count,
        }))
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;
        Ok(())
    }

    pub async fn fail_discovery_run(
        &self,
        run_id: Uuid,
        error: &str,
        metadata: Value,
    ) -> Result<(), DatabaseError> {
        let result = sqlx::query(
            r#"
            UPDATE discovery_runs
            SET
                status = 'failed',
                finished_at = CURRENT_TIMESTAMP,
                error = $2,
                metadata = $3
            WHERE id = $1 AND status = 'running'
            "#,
        )
        .bind(run_id)
        .bind(error)
        .bind(metadata)
        .execute(self.pool())
        .await?;
        if result.rows_affected() != 1 {
            return Err(DatabaseError::NotFound {
                entity: "running discovery run",
                id: run_id,
            });
        }
        Ok(())
    }

    pub async fn cancel_running_discovery_runs_for_job(
        &self,
        job_id: Uuid,
        reason: &str,
    ) -> Result<u64, DatabaseError> {
        let result = sqlx::query(
            r#"
            UPDATE discovery_runs
            SET
                status = 'cancelled',
                finished_at = CURRENT_TIMESTAMP,
                error = $2,
                metadata = metadata || jsonb_build_object(
                    'cancellation_reason',
                    $2
                )
            WHERE job_id = $1 AND status = 'running'
            "#,
        )
        .bind(job_id)
        .bind(reason)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn list_source_candidates(
        &self,
        company_id: Option<Uuid>,
        status: Option<CandidateStatus>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SourceCandidate>, DatabaseError> {
        let status = status.map(CandidateStatus::as_str);
        let rows = sqlx::query_as::<_, CandidateRow>(
            r#"
            SELECT
                id,
                company_id,
                discovery_run_id,
                candidate_url,
                candidate_kind,
                confidence,
                evidence,
                status,
                accepted_source_id,
                created_at,
                updated_at
            FROM source_candidates
            WHERE
                ($1::uuid IS NULL OR company_id = $1)
                AND ($2::text IS NULL OR status = $2)
            ORDER BY confidence DESC, created_at DESC, id
            LIMIT $3 OFFSET $4
            "#,
        )
        .bind(company_id)
        .bind(status)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter().map(CandidateRow::into_domain).collect()
    }

    pub async fn count_source_candidates(
        &self,
        company_id: Option<Uuid>,
        status: Option<CandidateStatus>,
    ) -> Result<i64, DatabaseError> {
        let status = status.map(CandidateStatus::as_str);
        Ok(sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM source_candidates
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

fn parse_optional_url(value: Option<String>) -> Result<Option<Url>, DatabaseError> {
    value
        .map(|value| Url::parse(&value).map_err(DatabaseError::from))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_url_parser_preserves_absence() {
        assert_eq!(parse_optional_url(None).expect("valid absence"), None);
    }
}
