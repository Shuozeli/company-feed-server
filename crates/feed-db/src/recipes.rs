use std::str::FromStr;

use chrono::{DateTime, Duration, Utc};
use feed_core::{
    CompanyNewsRecipe, CompanyNewsRecipeCoverage, CompanyNewsRecipeRun, CompanyNewsRecipeSpec,
    RecipeHealth, RecipeRunStatus, RecipeStatus,
};
use serde_json::{Value, json};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{Database, DatabaseError};

#[derive(Clone, Debug)]
pub struct CompanyNewsRecipeRunCompletion {
    pub discovered_url_count: i32,
    pub accepted_item_count: i32,
    pub rejected_url_count: i32,
    pub normalized_item_count: i32,
    pub new_item_count: i32,
    pub latest_published_at: Option<DateTime<Utc>>,
    pub publication_date_coverage_complete: bool,
    pub acceptance_ratio_bps: i32,
    pub structure_fingerprint: Option<String>,
    pub reasons: Vec<String>,
    pub error: Option<String>,
    pub transient_error: bool,
    pub metadata: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompanyNewsRecipeRunOutcome {
    pub status: RecipeRunStatus,
    pub rebuild_required: bool,
}

/// A company whose recipe needs rebuilding (status='stale' or rebuild_required).
/// One row per company; the approval-gated rebuild candidate queue.
#[derive(Clone, Debug, FromRow)]
pub struct RecipeRebuildCandidate {
    pub company_id: Uuid,
    pub company_key: String,
    pub company_name: String,
    pub reason: String,
    pub consecutive_failures: i32,
    pub stale_at: Option<DateTime<Utc>>,
    pub last_attempt_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Eq, PartialEq, FromRow)]
pub struct ActiveCompanyNewsPublicationClaim {
    pub recipe_id: Uuid,
    pub company_id: Uuid,
    pub company_name: String,
}

#[derive(Debug, FromRow)]
struct RecipeRow {
    id: Uuid,
    recipe_key: String,
    company_id: Uuid,
    source_id: Uuid,
    version: i32,
    status: String,
    spec: Value,
    content_hash: String,
    generated_by_run_id: Option<Uuid>,
    verified_at: Option<DateTime<Utc>>,
    stale_at: Option<DateTime<Utc>>,
    stale_reason: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    last_attempt_at: Option<DateTime<Utc>>,
    last_success_at: Option<DateTime<Utc>>,
    last_correct_at: Option<DateTime<Utc>>,
    last_nonempty_at: Option<DateTime<Utc>>,
    last_item_published_at: Option<DateTime<Utc>>,
    consecutive_failures: i32,
    consecutive_empty_runs: i32,
    consecutive_correctness_failures: i32,
    last_structure_fingerprint: Option<String>,
    freshness_status: String,
    correctness_status: String,
    rebuild_required: bool,
    health_reason: Option<String>,
    health_metadata: Value,
}

impl RecipeRow {
    fn into_domain(self) -> Result<CompanyNewsRecipe, DatabaseError> {
        Ok(CompanyNewsRecipe {
            id: self.id,
            recipe_key: self.recipe_key,
            company_id: self.company_id,
            source_id: self.source_id,
            version: self.version,
            status: RecipeStatus::from_str(&self.status)?,
            spec: serde_json::from_value(self.spec)?,
            content_hash: self.content_hash,
            generated_by_run_id: self.generated_by_run_id,
            verified_at: self.verified_at,
            stale_at: self.stale_at,
            stale_reason: self.stale_reason,
            created_at: self.created_at,
            updated_at: self.updated_at,
            health: RecipeHealth {
                last_attempt_at: self.last_attempt_at,
                last_success_at: self.last_success_at,
                last_correct_at: self.last_correct_at,
                last_nonempty_at: self.last_nonempty_at,
                last_item_published_at: self.last_item_published_at,
                consecutive_failures: self.consecutive_failures,
                consecutive_empty_runs: self.consecutive_empty_runs,
                consecutive_correctness_failures: self.consecutive_correctness_failures,
                last_structure_fingerprint: self.last_structure_fingerprint,
                freshness_status: self.freshness_status,
                correctness_status: self.correctness_status,
                rebuild_required: self.rebuild_required,
                reason: self.health_reason,
                metadata: self.health_metadata,
            },
        })
    }
}

#[derive(Debug, FromRow)]
struct RecipeRunRow {
    id: Uuid,
    recipe_id: Uuid,
    source_id: Uuid,
    job_id: Option<Uuid>,
    crawl_run_id: Option<Uuid>,
    status: String,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    discovered_url_count: i32,
    accepted_item_count: i32,
    rejected_url_count: i32,
    normalized_item_count: i32,
    new_item_count: i32,
    latest_published_at: Option<DateTime<Utc>>,
    acceptance_ratio_bps: i32,
    structure_fingerprint: Option<String>,
    reasons: Value,
    error: Option<String>,
    metadata: Value,
}

#[derive(Debug, FromRow)]
struct RecipeCoverageRow {
    eligible_companies: i64,
    companies_with_approved_feed: i64,
    companies_with_healthy_feed: i64,
    companies_with_active_recipe: i64,
    companies_with_approved_feed_and_active_recipe: i64,
    companies_covered_by_feed_or_recipe: i64,
    companies_missing_feed_or_recipe: i64,
    companies_with_completed_build: i64,
    companies_without_completed_build: i64,
    companies_uncovered_after_completed_build: i64,
    companies_uncovered_awaiting_completed_build: i64,
    companies_fresh_and_correct: i64,
    companies_content_stale: i64,
    companies_with_stale_recipe: i64,
    companies_missing_recipe: i64,
    active_recipes: i64,
    public_recipe_items: i64,
    quality_quarantined_recipe_items: i64,
    pending_build_jobs: i64,
    running_build_jobs: i64,
    retrying_build_jobs: i64,
    completed_build_jobs: i64,
    terminal_failed_build_jobs: i64,
    completed_build_runs: i64,
    failed_build_runs: i64,
}

impl From<RecipeCoverageRow> for CompanyNewsRecipeCoverage {
    fn from(row: RecipeCoverageRow) -> Self {
        Self {
            eligible_companies: row.eligible_companies,
            companies_with_approved_feed: row.companies_with_approved_feed,
            companies_with_healthy_feed: row.companies_with_healthy_feed,
            companies_with_active_recipe: row.companies_with_active_recipe,
            companies_with_approved_feed_and_active_recipe: row
                .companies_with_approved_feed_and_active_recipe,
            companies_covered_by_feed_or_recipe: row.companies_covered_by_feed_or_recipe,
            companies_missing_feed_or_recipe: row.companies_missing_feed_or_recipe,
            companies_with_completed_build: row.companies_with_completed_build,
            companies_without_completed_build: row.companies_without_completed_build,
            companies_uncovered_after_completed_build: row
                .companies_uncovered_after_completed_build,
            companies_uncovered_awaiting_completed_build: row
                .companies_uncovered_awaiting_completed_build,
            companies_fresh_and_correct: row.companies_fresh_and_correct,
            companies_content_stale: row.companies_content_stale,
            companies_with_stale_recipe: row.companies_with_stale_recipe,
            companies_missing_recipe: row.companies_missing_recipe,
            active_recipes: row.active_recipes,
            public_recipe_items: row.public_recipe_items,
            quality_quarantined_recipe_items: row.quality_quarantined_recipe_items,
            pending_build_jobs: row.pending_build_jobs,
            running_build_jobs: row.running_build_jobs,
            retrying_build_jobs: row.retrying_build_jobs,
            completed_build_jobs: row.completed_build_jobs,
            terminal_failed_build_jobs: row.terminal_failed_build_jobs,
            completed_build_runs: row.completed_build_runs,
            failed_build_runs: row.failed_build_runs,
        }
    }
}

impl RecipeRunRow {
    fn into_domain(self) -> Result<CompanyNewsRecipeRun, DatabaseError> {
        Ok(CompanyNewsRecipeRun {
            id: self.id,
            recipe_id: self.recipe_id,
            source_id: self.source_id,
            job_id: self.job_id,
            crawl_run_id: self.crawl_run_id,
            status: RecipeRunStatus::from_str(&self.status)?,
            started_at: self.started_at,
            finished_at: self.finished_at,
            discovered_url_count: self.discovered_url_count,
            accepted_item_count: self.accepted_item_count,
            rejected_url_count: self.rejected_url_count,
            normalized_item_count: self.normalized_item_count,
            new_item_count: self.new_item_count,
            latest_published_at: self.latest_published_at,
            acceptance_ratio_bps: self.acceptance_ratio_bps,
            structure_fingerprint: self.structure_fingerprint,
            reasons: serde_json::from_value(self.reasons)?,
            error: self.error,
            metadata: self.metadata,
        })
    }
}

impl Database {
    pub async fn list_active_company_news_publication_claims(
        &self,
        publication_url: &str,
    ) -> Result<Vec<ActiveCompanyNewsPublicationClaim>, DatabaseError> {
        Ok(sqlx::query_as::<_, ActiveCompanyNewsPublicationClaim>(
            r#"
            SELECT
                recipe.id AS recipe_id,
                recipe.company_id,
                company.name AS company_name
            FROM company_news_recipes AS recipe
            JOIN companies AS company ON company.id = recipe.company_id
            LEFT JOIN company_news_recipe_state AS state ON state.recipe_id = recipe.id
            WHERE
                recipe.status = 'active'
                AND COALESCE(state.rebuild_required, false) = false
                AND regexp_replace(
                    split_part(
                        public_url_identity_key(recipe.spec->>'publication_url'),
                        '?',
                        1
                    ),
                    '^([^/]+)/([[:alpha:]]{2}|[[:alpha:]]{2}-[[:alpha:]]{2})/(.+)$',
                    '\1/\3',
                    'i'
                ) = regexp_replace(
                    split_part(public_url_identity_key($1), '?', 1),
                    '^([^/]+)/([[:alpha:]]{2}|[[:alpha:]]{2}-[[:alpha:]]{2})/(.+)$',
                    '\1/\3',
                    'i'
                )
            ORDER BY recipe.created_at, recipe.id
            "#,
        )
        .bind(publication_url)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn get_company_news_recipe_coverage(
        &self,
    ) -> Result<CompanyNewsRecipeCoverage, DatabaseError> {
        let row = sqlx::query_as::<_, RecipeCoverageRow>(
            r#"
            WITH eligible AS (
                SELECT id
                FROM companies
                WHERE discovery_enabled AND lifecycle_status <> 'inactive'
            ),
            approved_feed AS (
                SELECT DISTINCT source.company_id
                FROM sources AS source
                JOIN eligible ON eligible.id = source.company_id
                WHERE source.status = 'approved'
                  AND source.kind IN ('rss', 'atom')
            ),
            healthy_feed AS (
                SELECT DISTINCT source.company_id
                FROM sources AS source
                JOIN source_state AS state ON state.source_id = source.id
                JOIN eligible ON eligible.id = source.company_id
                WHERE source.status = 'approved'
                  AND source.kind IN ('rss', 'atom')
                  AND state.last_success_at IS NOT NULL
                  AND state.consecutive_failures = 0
                  AND state.consecutive_zero_runs < 3
            ),
            active AS (
                SELECT recipe.company_id, recipe.id, recipe.source_id, state.freshness_status,
                       state.correctness_status,
                       COALESCE(state.rebuild_required, false) AS rebuild_required
                FROM company_news_recipes AS recipe
                LEFT JOIN company_news_recipe_state AS state ON state.recipe_id = recipe.id
                JOIN eligible ON eligible.id = recipe.company_id
                WHERE recipe.status = 'active'
            ),
            recipe_source AS (
                SELECT DISTINCT source_id
                FROM active
                WHERE NOT rebuild_required
            ),
            active_company AS (
                SELECT DISTINCT company_id
                FROM active
                WHERE NOT rebuild_required
                  AND COALESCE(freshness_status, 'unknown') <> 'content_stale'
            ),
            covered AS (
                SELECT company_id FROM healthy_feed
                UNION
                SELECT company_id FROM active_company
            ),
            completed_build_company AS (
                SELECT DISTINCT run.company_id
                FROM company_news_extraction_runs AS run
                JOIN eligible ON eligible.id = run.company_id
                WHERE run.status = 'completed'
            ),
            uncovered AS (
                SELECT eligible.id AS company_id
                FROM eligible
                LEFT JOIN covered ON covered.company_id = eligible.id
                WHERE covered.company_id IS NULL
            ),
            latest_build_job AS (
                SELECT DISTINCT ON (job.company_id)
                    job.company_id,
                    job.status
                FROM jobs AS job
                JOIN eligible ON eligible.id = job.company_id
                WHERE job.job_type = 'extract_company_news'
                ORDER BY job.company_id, job.created_at DESC, job.id DESC
            )
            SELECT
                (SELECT count(*) FROM eligible) AS eligible_companies,
                (SELECT count(*) FROM approved_feed) AS companies_with_approved_feed,
                (SELECT count(*) FROM healthy_feed) AS companies_with_healthy_feed,
                (SELECT count(*) FROM active_company) AS companies_with_active_recipe,
                (SELECT count(*) FROM approved_feed
                 JOIN active_company USING (company_id))
                    AS companies_with_approved_feed_and_active_recipe,
                (SELECT count(*) FROM covered) AS companies_covered_by_feed_or_recipe,
                (SELECT count(*) FROM eligible) - (SELECT count(*) FROM covered)
                    AS companies_missing_feed_or_recipe,
                (SELECT count(*) FROM completed_build_company)
                    AS companies_with_completed_build,
                (SELECT count(*) FROM eligible) - (SELECT count(*) FROM completed_build_company)
                    AS companies_without_completed_build,
                (SELECT count(*) FROM uncovered
                 JOIN completed_build_company USING (company_id))
                    AS companies_uncovered_after_completed_build,
                (SELECT count(*) FROM uncovered
                 LEFT JOIN completed_build_company USING (company_id)
                 WHERE completed_build_company.company_id IS NULL)
                    AS companies_uncovered_awaiting_completed_build,
                (SELECT count(DISTINCT company_id) FROM active
                 WHERE NOT rebuild_required AND freshness_status = 'fresh'
                   AND correctness_status = 'passing') AS companies_fresh_and_correct,
                (SELECT count(DISTINCT company_id) FROM active
                 WHERE NOT rebuild_required AND freshness_status = 'content_stale')
                    AS companies_content_stale,
                (SELECT count(DISTINCT recipe.company_id)
                 FROM company_news_recipes AS recipe
                 JOIN eligible ON eligible.id = recipe.company_id
                 WHERE recipe.status = 'stale') AS companies_with_stale_recipe,
                (SELECT count(*) FROM eligible)
                    - (SELECT count(*) FROM active_company)
                    AS companies_missing_recipe,
                (SELECT count(*) FROM active
                 WHERE NOT rebuild_required
                   AND COALESCE(freshness_status, 'unknown') <> 'content_stale')
                    AS active_recipes,
                (SELECT count(*)
                 FROM feed_items AS item
                 JOIN recipe_source ON recipe_source.source_id = item.source_id
                 JOIN sources AS source ON source.id = item.source_id
                 WHERE NOT item.is_private AND source.status = 'approved')
                    AS public_recipe_items,
                (SELECT count(*)
                 FROM feed_items AS item
                 JOIN recipe_source ON recipe_source.source_id = item.source_id
                 WHERE item.is_private
                   AND item.content_processing #>> '{quality_quarantine,state}'
                       = 'quarantined'
                   AND item.content_processing #> '{quality_quarantine,reversible}'
                       = 'true'::jsonb)
                    AS quality_quarantined_recipe_items,
                (SELECT count(*) FROM jobs
                 WHERE job_type = 'extract_company_news' AND status = 'pending')
                    AS pending_build_jobs,
                (SELECT count(*) FROM jobs
                 WHERE job_type = 'extract_company_news' AND status = 'running')
                    AS running_build_jobs,
                (SELECT count(*) FROM jobs
                 WHERE job_type = 'extract_company_news' AND status = 'pending'
                   AND attempt_count > 0) AS retrying_build_jobs,
                (SELECT count(*) FROM jobs
                 WHERE job_type = 'extract_company_news' AND status = 'completed')
                    AS completed_build_jobs,
                (SELECT count(*) FROM latest_build_job WHERE status = 'failed')
                    AS terminal_failed_build_jobs,
                (SELECT count(*) FROM company_news_extraction_runs WHERE status = 'completed')
                    AS completed_build_runs,
                (SELECT count(*) FROM company_news_extraction_runs WHERE status = 'failed')
                    AS failed_build_runs
            "#,
        )
        .fetch_one(self.pool())
        .await?;
        Ok(row.into())
    }

    /// The approval-gated rebuild candidate queue: companies that own a recipe
    /// needing rebuild (status='stale' OR rebuild_required), worst first. One row
    /// per company.
    pub async fn list_recipe_rebuild_candidates(
        &self,
        limit: i64,
    ) -> Result<Vec<RecipeRebuildCandidate>, DatabaseError> {
        Ok(sqlx::query_as::<_, RecipeRebuildCandidate>(
            r#"
            SELECT company_id, company_key, company_name, reason,
                   consecutive_failures, stale_at, last_attempt_at
            FROM (
                SELECT DISTINCT ON (c.id)
                    c.id AS company_id,
                    c.company_key AS company_key,
                    c.name AS company_name,
                    CASE WHEN r.status = 'stale' THEN 'stale' ELSE 'rebuild_required' END AS reason,
                    COALESCE(st.consecutive_failures, 0) AS consecutive_failures,
                    r.stale_at AS stale_at,
                    st.last_attempt_at AS last_attempt_at
                FROM company_news_recipes AS r
                JOIN companies AS c ON c.id = r.company_id
                LEFT JOIN company_news_recipe_state AS st ON st.recipe_id = r.id
                WHERE r.status = 'stale' OR COALESCE(st.rebuild_required, false) = true
                ORDER BY c.id, COALESCE(st.consecutive_failures, 0) DESC
            ) AS q
            ORDER BY consecutive_failures DESC, stale_at ASC NULLS LAST
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn list_company_ids_needing_news_recipes(
        &self,
        include_companies_with_feeds: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Uuid>, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            SELECT company.id
            FROM companies AS company
            WHERE
                company.discovery_enabled
                AND company.lifecycle_status <> 'inactive'
                AND (
                    $1
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
                    FROM company_news_recipes AS recipe
                    LEFT JOIN company_news_recipe_state AS recipe_state
                        ON recipe_state.recipe_id = recipe.id
                    WHERE
                        recipe.company_id = company.id
                        AND recipe.status = 'active'
                        AND COALESCE(recipe_state.rebuild_required, false) = false
                        AND COALESCE(
                            recipe_state.freshness_status,
                            'unknown'
                        ) <> 'content_stale'
                )
            ORDER BY company.name, company.company_key
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(include_companies_with_feeds)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn count_companies_needing_news_recipes(
        &self,
        include_companies_with_feeds: bool,
    ) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM companies AS company
            WHERE
                company.discovery_enabled
                AND company.lifecycle_status <> 'inactive'
                AND (
                    $1
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
                    FROM company_news_recipes AS recipe
                    LEFT JOIN company_news_recipe_state AS recipe_state
                        ON recipe_state.recipe_id = recipe.id
                    WHERE
                        recipe.company_id = company.id
                        AND recipe.status = 'active'
                        AND COALESCE(recipe_state.rebuild_required, false) = false
                        AND COALESCE(
                            recipe_state.freshness_status,
                            'unknown'
                        ) <> 'content_stale'
                )
            "#,
        )
        .bind(include_companies_with_feeds)
        .fetch_one(self.pool())
        .await?)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn activate_company_news_recipe(
        &self,
        recipe_key: &str,
        company_id: Uuid,
        source_id: Uuid,
        spec: &CompanyNewsRecipeSpec,
        content_hash: &str,
        generated_by_run_id: Option<Uuid>,
        verified_at: DateTime<Utc>,
        initial_latest_published_at: Option<DateTime<Utc>>,
        initial_publication_date_coverage_complete: bool,
        initial_structure_fingerprint: Option<&str>,
        initial_health_metadata: Value,
    ) -> Result<CompanyNewsRecipe, DatabaseError> {
        spec.validate()
            .map_err(|error| DatabaseError::InvalidState(error.to_string()))?;
        if recipe_key.trim().is_empty() || content_hash.trim().is_empty() {
            return Err(DatabaseError::InvalidState(
                "recipe key and content hash cannot be blank".to_owned(),
            ));
        }
        let spec_json = serde_json::to_value(spec)?;
        let mut transaction = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("company-news-recipe:{source_id}"))
            .execute(&mut *transaction)
            .await?;

        let existing_id = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT id
            FROM company_news_recipes
            WHERE source_id = $1 AND status = 'active' AND content_hash = $2
            "#,
        )
        .bind(source_id)
        .bind(content_hash)
        .fetch_optional(&mut *transaction)
        .await?;
        let recipe_id = if let Some(existing_id) = existing_id {
            sqlx::query(
                r#"
                UPDATE company_news_recipes
                SET verified_at = $2, generated_by_run_id = COALESCE($3, generated_by_run_id)
                WHERE id = $1
                "#,
            )
            .bind(existing_id)
            .bind(verified_at)
            .bind(generated_by_run_id)
            .execute(&mut *transaction)
            .await?;
            existing_id
        } else {
            let version: i32 = sqlx::query_scalar(
                "SELECT COALESCE(max(version), 0) + 1 FROM company_news_recipes WHERE recipe_key = $1",
            )
            .bind(recipe_key)
            .fetch_one(&mut *transaction)
            .await?;
            sqlx::query(
                "UPDATE company_news_recipes SET status = 'superseded' WHERE source_id = $1 AND status = 'active'",
            )
            .bind(source_id)
            .execute(&mut *transaction)
            .await?;
            sqlx::query_scalar::<_, Uuid>(
                r#"
                INSERT INTO company_news_recipes (
                    recipe_key, company_id, source_id, version, status,
                    schema_version, spec, content_hash, generated_by_run_id, verified_at
                )
                VALUES ($1, $2, $3, $4, 'active', $5, $6, $7, $8, $9)
                RETURNING id
                "#,
            )
            .bind(recipe_key)
            .bind(company_id)
            .bind(source_id)
            .bind(version)
            .bind(&spec.schema_version)
            .bind(&spec_json)
            .bind(content_hash)
            .bind(generated_by_run_id)
            .bind(verified_at)
            .fetch_one(&mut *transaction)
            .await?
        };
        let previous_latest_published_at = sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
            r#"
                SELECT last_item_published_at
                FROM company_news_recipe_state
                WHERE recipe_id = $1
                "#,
        )
        .bind(recipe_id)
        .fetch_optional(&mut *transaction)
        .await?
        .flatten();
        let effective_latest_published_at =
            initial_latest_published_at.or(previous_latest_published_at);
        let initial_freshness_status = recipe_freshness_status(
            effective_latest_published_at,
            verified_at,
            spec.freshness.content_stale_after_seconds,
            initial_publication_date_coverage_complete,
            true,
        );
        sqlx::query(
            r#"
            INSERT INTO company_news_recipe_state (
                recipe_id, last_success_at, last_correct_at, last_nonempty_at,
                last_item_published_at, last_structure_fingerprint,
                freshness_status, correctness_status, rebuild_required, metadata
            )
            VALUES ($1, $2, $2, $2, $3, $4, $6, 'passing', false, $5)
            ON CONFLICT (recipe_id) DO UPDATE
            SET
                last_success_at = $2,
                last_correct_at = $2,
                last_nonempty_at = COALESCE(company_news_recipe_state.last_nonempty_at, $2),
                consecutive_failures = 0,
                consecutive_empty_runs = 0,
                consecutive_correctness_failures = 0,
                last_item_published_at = COALESCE($3, company_news_recipe_state.last_item_published_at),
                last_structure_fingerprint = COALESCE($4, company_news_recipe_state.last_structure_fingerprint),
                freshness_status = $6,
                correctness_status = 'passing',
                rebuild_required = false,
                reason = NULL,
                metadata = $5
            "#,
        )
        .bind(recipe_id)
        .bind(verified_at)
        .bind(effective_latest_published_at)
        .bind(initial_structure_fingerprint)
        .bind(initial_health_metadata)
        .bind(initial_freshness_status)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            UPDATE sources
            SET
                kind = CASE WHEN $2 = 'browser' THEN 'browser' ELSE 'html' END,
                browser_required = $2 = 'browser',
                freshness_slo_seconds = $3,
                metadata = metadata || jsonb_build_object(
                    'managed_by', 'company_news_recipe',
                    'active_recipe_id', $4,
                    'recipe_schema_version', $5
                )
            WHERE id = $1
            "#,
        )
        .bind(source_id)
        .bind(match spec.render_mode {
            feed_core::RecipeRenderMode::Http => "http",
            feed_core::RecipeRenderMode::Browser => "browser",
        })
        .bind(
            i32::try_from(spec.freshness.crawl_interval_seconds).map_err(|_| {
                DatabaseError::InvalidState("recipe crawl interval exceeds i32".to_owned())
            })?,
        )
        .bind(recipe_id)
        .bind(&spec.schema_version)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO event_log (event_type, company_id, source_id, payload)
            VALUES ('company_news.recipe_activated', $1, $2, $3)
            "#,
        )
        .bind(company_id)
        .bind(source_id)
        .bind(json!({
            "recipe_id": recipe_id,
            "recipe_key": recipe_key,
            "content_hash": content_hash,
            "generated_by_run_id": generated_by_run_id,
        }))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        self.get_company_news_recipe(recipe_id)
            .await?
            .ok_or(DatabaseError::NotFound {
                entity: "company news recipe",
                id: recipe_id,
            })
    }

    pub async fn supersede_active_company_news_recipe(
        &self,
        recipe_id: Uuid,
        reason: &str,
        metadata: Value,
    ) -> Result<bool, DatabaseError> {
        if reason.trim().is_empty() {
            return Err(DatabaseError::InvalidState(
                "recipe supersession reason cannot be blank".to_owned(),
            ));
        }
        let mut transaction = self.pool().begin().await?;
        let recipe = sqlx::query_as::<_, (Uuid, Uuid)>(
            r#"
            SELECT company_id, source_id
            FROM company_news_recipes
            WHERE id = $1 AND status = 'active'
            FOR UPDATE
            "#,
        )
        .bind(recipe_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((company_id, source_id)) = recipe else {
            transaction.rollback().await?;
            return Ok(false);
        };
        sqlx::query(
            r#"
            UPDATE company_news_recipes
            SET status = 'superseded', stale_at = NULL, stale_reason = NULL
            WHERE id = $1 AND status = 'active'
            "#,
        )
        .bind(recipe_id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            UPDATE sources
            SET metadata = metadata - 'active_recipe_id' - 'recipe_schema_version'
            WHERE id = $1 AND metadata ->> 'active_recipe_id' = $2
            "#,
        )
        .bind(source_id)
        .bind(recipe_id.to_string())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            UPDATE jobs
            SET
                status = 'cancelled',
                completed_at = CURRENT_TIMESTAMP,
                last_error = 'recipe superseded before its activation crawl ran'
            WHERE
                job_type = 'crawl_source'
                AND status = 'pending'
                AND source_id = $1
                AND (
                    payload ->> 'recipe_id' = $2
                    OR NOT EXISTS (
                        SELECT 1
                        FROM company_news_recipes AS active_recipe
                        WHERE
                            active_recipe.source_id = $1
                            AND active_recipe.status = 'active'
                    )
                )
            "#,
        )
        .bind(source_id)
        .bind(recipe_id.to_string())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO event_log (event_type, company_id, source_id, payload)
            VALUES (
                'company_news.recipe_superseded',
                $1,
                $2,
                jsonb_build_object(
                    'recipe_id', $3::uuid,
                    'reason', $4::text,
                    'policy', 'company-news-redundant-rebuild.v1',
                    'metadata', $5::jsonb
                )
            )
            "#,
        )
        .bind(company_id)
        .bind(source_id)
        .bind(recipe_id)
        .bind(reason)
        .bind(metadata)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn retire_active_company_news_recipe_for_ownership(
        &self,
        recipe_id: Uuid,
        reason: &str,
        metadata: Value,
    ) -> Result<bool, DatabaseError> {
        if reason.trim().is_empty() {
            return Err(DatabaseError::InvalidState(
                "recipe ownership rejection reason cannot be blank".to_owned(),
            ));
        }
        let mut transaction = self.pool().begin().await?;
        let recipe = sqlx::query_as::<_, (Uuid, Uuid, String)>(
            r#"
            SELECT
                recipe.company_id,
                recipe.source_id,
                COALESCE(recipe.spec->>'publication_url', source.url)
            FROM company_news_recipes AS recipe
            JOIN sources AS source ON source.id = recipe.source_id
            WHERE recipe.id = $1 AND recipe.status = 'active'
            FOR UPDATE
            "#,
        )
        .bind(recipe_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((company_id, source_id, publication_url)) = recipe else {
            transaction.rollback().await?;
            return Ok(false);
        };

        sqlx::query(
            r#"
            UPDATE company_news_recipes
            SET
                status = 'stale',
                stale_at = CURRENT_TIMESTAMP,
                stale_reason = $2
            WHERE id = $1 AND status = 'active'
            "#,
        )
        .bind(recipe_id)
        .bind(reason)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            UPDATE company_news_recipe_state
            SET
                correctness_status = 'failing',
                rebuild_required = true,
                consecutive_correctness_failures =
                    GREATEST(consecutive_correctness_failures, 3),
                reason = $2,
                metadata = metadata || jsonb_build_object(
                    'ownership_rejection',
                    jsonb_build_object(
                        'policy', 'cross-company-feed-ownership.v1',
                        'rejected_at', CURRENT_TIMESTAMP,
                        'metadata', $3::jsonb
                    )
                )
            WHERE recipe_id = $1
            "#,
        )
        .bind(recipe_id)
        .bind(reason)
        .bind(metadata.clone())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            UPDATE sources
            SET
                status = 'disabled',
                public_export_allowed = false,
                metadata =
                    metadata - 'active_recipe_id' - 'recipe_schema_version'
                    || jsonb_build_object(
                        'quality_disable',
                        jsonb_build_object(
                            'reason', $2,
                            'reversible', false,
                            'policy', 'cross-company-feed-ownership.v1',
                            'disabled_at', CURRENT_TIMESTAMP,
                            'metadata', $3::jsonb
                        )
                    )
            WHERE id = $1
            "#,
        )
        .bind(source_id)
        .bind(reason)
        .bind(metadata.clone())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            UPDATE jobs
            SET
                status = 'cancelled',
                completed_at = CURRENT_TIMESTAMP,
                last_error =
                    'cancelled because approved feed evidence belongs to a different company',
                locked_by = NULL,
                locked_at = NULL,
                heartbeat_at = NULL,
                lease_until = NULL,
                lease_token = NULL
            WHERE
                job_type = 'crawl_source'
                AND status = 'pending'
                AND source_id = $1
            "#,
        )
        .bind(source_id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO event_log (event_type, company_id, source_id, payload)
            SELECT
                'feed_item.quality_quarantined',
                item.company_id,
                item.source_id,
                jsonb_build_object(
                    'feed_item_id', item.id,
                    'canonical_url', item.canonical_url,
                    'title', item.title,
                    'published_at', item.published_at,
                    'reason', $2,
                    'reversible', false,
                    'policy', 'cross-company-feed-ownership.v1',
                    'recipe_id', $3::uuid,
                    'metadata', $4::jsonb
                )
            FROM feed_items AS item
            WHERE item.source_id = $1 AND NOT item.is_private
            "#,
        )
        .bind(source_id)
        .bind(reason)
        .bind(recipe_id)
        .bind(metadata.clone())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            UPDATE raw_crawl_items AS raw
            SET
                processing_status = 'skipped',
                normalization_error = 'quality quarantine: ' || $2,
                normalized_feed_item_id = NULL
            FROM feed_items AS item
            WHERE
                item.source_id = $1
                AND NOT item.is_private
                AND raw.id = item.raw_crawl_item_id
            "#,
        )
        .bind(source_id)
        .bind(reason)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            UPDATE feed_items
            SET
                is_private = true,
                content_processing = content_processing || jsonb_build_object(
                    'quality_quarantine',
                    jsonb_build_object(
                        'state', 'quarantined',
                        'reason', $2,
                        'reversible', false,
                        'policy', 'cross-company-feed-ownership.v1',
                        'recipe_id', $3::uuid,
                        'quarantined_at', CURRENT_TIMESTAMP,
                        'metadata', $4::jsonb
                    )
                )
            WHERE source_id = $1 AND NOT is_private
            "#,
        )
        .bind(source_id)
        .bind(reason)
        .bind(recipe_id)
        .bind(metadata.clone())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO event_log (event_type, company_id, source_id, payload)
            VALUES (
                'company_news.recipe_stale',
                $1,
                $2,
                jsonb_build_object(
                    'recipe_id', $3::uuid,
                    'publication_url', $4,
                    'reason', $5,
                    'rebuild_required', true,
                    'policy', 'cross-company-feed-ownership.v1',
                    'metadata', $6::jsonb
                )
            )
            "#,
        )
        .bind(company_id)
        .bind(source_id)
        .bind(recipe_id)
        .bind(publication_url)
        .bind(reason)
        .bind(metadata)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn get_company_news_recipe(
        &self,
        recipe_id: Uuid,
    ) -> Result<Option<CompanyNewsRecipe>, DatabaseError> {
        self.fetch_recipe("recipe.id = $1", Some(recipe_id), None)
            .await
    }

    pub async fn get_active_company_news_recipe_for_source(
        &self,
        source_id: Uuid,
    ) -> Result<Option<CompanyNewsRecipe>, DatabaseError> {
        self.fetch_recipe(
            "recipe.source_id = $1 AND recipe.status = 'active' AND COALESCE(state.rebuild_required, false) = false",
            Some(source_id),
            None,
        )
        .await
    }

    async fn fetch_recipe(
        &self,
        predicate: &str,
        id: Option<Uuid>,
        _unused: Option<&str>,
    ) -> Result<Option<CompanyNewsRecipe>, DatabaseError> {
        let sql = format!(
            r#"
            SELECT
                recipe.id, recipe.recipe_key, recipe.company_id, recipe.source_id,
                recipe.version, recipe.status, recipe.spec, recipe.content_hash,
                recipe.generated_by_run_id, recipe.verified_at, recipe.stale_at,
                recipe.stale_reason, recipe.created_at, recipe.updated_at,
                state.last_attempt_at, state.last_success_at, state.last_correct_at,
                state.last_nonempty_at, state.last_item_published_at,
                COALESCE(state.consecutive_failures, 0) AS consecutive_failures,
                COALESCE(state.consecutive_empty_runs, 0) AS consecutive_empty_runs,
                COALESCE(state.consecutive_correctness_failures, 0) AS consecutive_correctness_failures,
                state.last_structure_fingerprint,
                COALESCE(state.freshness_status, 'unknown') AS freshness_status,
                COALESCE(state.correctness_status, 'unknown') AS correctness_status,
                COALESCE(state.rebuild_required, false) AS rebuild_required,
                state.reason AS health_reason,
                COALESCE(state.metadata, '{{}}'::jsonb) AS health_metadata
            FROM company_news_recipes AS recipe
            LEFT JOIN company_news_recipe_state AS state ON state.recipe_id = recipe.id
            WHERE {predicate}
            ORDER BY recipe.version DESC
            LIMIT 1
            "#
        );
        let row = sqlx::query_as::<_, RecipeRow>(&sql)
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        row.map(RecipeRow::into_domain).transpose()
    }

    pub async fn list_company_news_recipes(
        &self,
        company_id: Option<Uuid>,
        status: Option<RecipeStatus>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<CompanyNewsRecipe>, DatabaseError> {
        let rows = sqlx::query_as::<_, RecipeRow>(
            r#"
            SELECT
                recipe.id, recipe.recipe_key, recipe.company_id, recipe.source_id,
                recipe.version, recipe.status, recipe.spec, recipe.content_hash,
                recipe.generated_by_run_id, recipe.verified_at, recipe.stale_at,
                recipe.stale_reason, recipe.created_at, recipe.updated_at,
                state.last_attempt_at, state.last_success_at, state.last_correct_at,
                state.last_nonempty_at, state.last_item_published_at,
                COALESCE(state.consecutive_failures, 0) AS consecutive_failures,
                COALESCE(state.consecutive_empty_runs, 0) AS consecutive_empty_runs,
                COALESCE(state.consecutive_correctness_failures, 0) AS consecutive_correctness_failures,
                state.last_structure_fingerprint,
                COALESCE(state.freshness_status, 'unknown') AS freshness_status,
                COALESCE(state.correctness_status, 'unknown') AS correctness_status,
                COALESCE(state.rebuild_required, false) AS rebuild_required,
                state.reason AS health_reason,
                COALESCE(state.metadata, '{}'::jsonb) AS health_metadata
            FROM company_news_recipes AS recipe
            LEFT JOIN company_news_recipe_state AS state ON state.recipe_id = recipe.id
            WHERE
                ($1::uuid IS NULL OR recipe.company_id = $1)
                AND ($2::text IS NULL OR recipe.status = $2)
            ORDER BY recipe.updated_at DESC, recipe.id DESC
            LIMIT $3 OFFSET $4
            "#,
        )
        .bind(company_id)
        .bind(status.map(RecipeStatus::as_str))
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(RecipeRow::into_domain).collect()
    }

    pub async fn count_company_news_recipes(
        &self,
        company_id: Option<Uuid>,
        status: Option<RecipeStatus>,
    ) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM company_news_recipes
            WHERE ($1::uuid IS NULL OR company_id = $1)
              AND ($2::text IS NULL OR status = $2)
            "#,
        )
        .bind(company_id)
        .bind(status.map(RecipeStatus::as_str))
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn begin_company_news_recipe_run(
        &self,
        recipe_id: Uuid,
        source_id: Uuid,
        job_id: Uuid,
        crawl_run_id: Uuid,
    ) -> Result<Uuid, DatabaseError> {
        let mut transaction = self.pool().begin().await?;
        let run_id = sqlx::query_scalar(
            r#"
            INSERT INTO company_news_recipe_runs (
                recipe_id, source_id, job_id, crawl_run_id, status
            )
            VALUES ($1, $2, $3, $4, 'running')
            ON CONFLICT (job_id) WHERE status = 'running' AND job_id IS NOT NULL
            DO UPDATE SET crawl_run_id = EXCLUDED.crawl_run_id
            RETURNING id
            "#,
        )
        .bind(recipe_id)
        .bind(source_id)
        .bind(job_id)
        .bind(crawl_run_id)
        .fetch_one(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO company_news_recipe_state (recipe_id, last_attempt_at)
            VALUES ($1, CURRENT_TIMESTAMP)
            ON CONFLICT (recipe_id) DO UPDATE SET last_attempt_at = CURRENT_TIMESTAMP
            "#,
        )
        .bind(recipe_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(run_id)
    }

    pub async fn complete_company_news_recipe_run(
        &self,
        run_id: Uuid,
        recipe: &CompanyNewsRecipe,
        completion: &CompanyNewsRecipeRunCompletion,
    ) -> Result<CompanyNewsRecipeRunOutcome, DatabaseError> {
        validate_run_completion(completion)?;
        let mut transaction = self.pool().begin().await?;
        let current = sqlx::query_as::<
            _,
            (
                i32,
                i32,
                i32,
                Option<String>,
                Option<DateTime<Utc>>,
                String,
                String,
                bool,
            ),
        >(
            r#"
            SELECT consecutive_failures, consecutive_empty_runs,
                   consecutive_correctness_failures, last_structure_fingerprint,
                   last_item_published_at, freshness_status, correctness_status,
                   rebuild_required
            FROM company_news_recipe_state
            WHERE recipe_id = $1
            FOR UPDATE
            "#,
        )
        .bind(recipe.id)
        .fetch_optional(&mut *transaction)
        .await?
        .unwrap_or((
            0,
            0,
            0,
            None,
            None,
            "unknown".to_owned(),
            "unknown".to_owned(),
            false,
        ));
        let correct = completion.reasons.is_empty() && completion.error.is_none();
        let empty = completion.accepted_item_count == 0;
        let (next_failures, next_empty, next_correctness) = next_recipe_health_counts(
            (current.0, current.1, current.2),
            correct,
            empty,
            completion.transient_error,
        );
        let freshness = &recipe.spec.freshness;
        let rebuild_required = if completion.transient_error {
            current.7
        } else {
            next_failures >= i32::try_from(freshness.max_consecutive_failures).unwrap_or(i32::MAX)
                || next_empty
                    >= i32::try_from(freshness.max_consecutive_empty_runs).unwrap_or(i32::MAX)
                || next_correctness
                    >= i32::try_from(freshness.max_consecutive_failures).unwrap_or(i32::MAX)
        };
        let run_status = if rebuild_required {
            RecipeRunStatus::Stale
        } else if correct {
            RecipeRunStatus::Passed
        } else {
            RecipeRunStatus::Failed
        };
        let effective_latest_published_at = completion.latest_published_at.or(current.4);
        let freshness_status = next_recipe_freshness_status(
            &current.5,
            effective_latest_published_at,
            Utc::now(),
            freshness.content_stale_after_seconds,
            completion.publication_date_coverage_complete,
            correct,
            completion.transient_error,
        );
        let correctness_status = if correct {
            "passing"
        } else if completion.transient_error {
            current.6.as_str()
        } else {
            "failing"
        };
        let reason = completion
            .error
            .clone()
            .or_else(|| completion.reasons.first().cloned());
        let structure_changed = current.3.as_deref().is_some_and(|previous| {
            completion
                .structure_fingerprint
                .as_deref()
                .is_some_and(|current| current != previous)
        });
        let mut health_metadata = completion.metadata.clone();
        if let Some(metadata) = health_metadata.as_object_mut() {
            metadata.insert("structure_changed".to_owned(), json!(structure_changed));
            metadata.insert(
                "transient_error".to_owned(),
                json!(completion.transient_error),
            );
            metadata.insert(
                "previous_structure_fingerprint".to_owned(),
                json!(current.3),
            );
            metadata.insert(
                "current_structure_fingerprint".to_owned(),
                json!(completion.structure_fingerprint),
            );
        }

        let updated = sqlx::query(
            r#"
            UPDATE company_news_recipe_runs
            SET
                status = $2,
                finished_at = CURRENT_TIMESTAMP,
                discovered_url_count = $3,
                accepted_item_count = $4,
                rejected_url_count = $5,
                normalized_item_count = $6,
                new_item_count = $7,
                latest_published_at = $8,
                acceptance_ratio_bps = $9,
                structure_fingerprint = $10,
                reasons = $11,
                error = $12,
                metadata = $13
            WHERE id = $1 AND status = 'running'
            "#,
        )
        .bind(run_id)
        .bind(run_status.as_str())
        .bind(completion.discovered_url_count)
        .bind(completion.accepted_item_count)
        .bind(completion.rejected_url_count)
        .bind(completion.normalized_item_count)
        .bind(completion.new_item_count)
        .bind(completion.latest_published_at)
        .bind(completion.acceptance_ratio_bps)
        .bind(&completion.structure_fingerprint)
        .bind(json!(completion.reasons))
        .bind(&completion.error)
        .bind(&health_metadata)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(DatabaseError::NotFound {
                entity: "running company news recipe run",
                id: run_id,
            });
        }
        sqlx::query(
            r#"
            INSERT INTO company_news_recipe_state (
                recipe_id, last_attempt_at, last_success_at, last_correct_at,
                last_nonempty_at, last_item_published_at,
                consecutive_failures, consecutive_empty_runs,
                consecutive_correctness_failures, last_structure_fingerprint,
                freshness_status, correctness_status, rebuild_required, reason, metadata
            )
            VALUES (
                $1, CURRENT_TIMESTAMP,
                CASE WHEN $2 THEN CURRENT_TIMESTAMP ELSE NULL END,
                CASE WHEN $2 THEN CURRENT_TIMESTAMP ELSE NULL END,
                CASE WHEN $3 > 0 THEN CURRENT_TIMESTAMP ELSE NULL END,
                $4, $5, $6, $7, $8, $9,
                $13,
                $10, $11, $12
            )
            ON CONFLICT (recipe_id) DO UPDATE
            SET
                last_attempt_at = CURRENT_TIMESTAMP,
                last_success_at = CASE WHEN $2 THEN CURRENT_TIMESTAMP ELSE company_news_recipe_state.last_success_at END,
                last_correct_at = CASE WHEN $2 THEN CURRENT_TIMESTAMP ELSE company_news_recipe_state.last_correct_at END,
                last_nonempty_at = CASE WHEN $3 > 0 THEN CURRENT_TIMESTAMP ELSE company_news_recipe_state.last_nonempty_at END,
                last_item_published_at = COALESCE($4, company_news_recipe_state.last_item_published_at),
                consecutive_failures = $5,
                consecutive_empty_runs = $6,
                consecutive_correctness_failures = $7,
                last_structure_fingerprint = COALESCE($8, company_news_recipe_state.last_structure_fingerprint),
                freshness_status = $9,
                correctness_status = $13,
                rebuild_required = $10,
                reason = $11,
                metadata = $12
            "#,
        )
        .bind(recipe.id)
        .bind(correct)
        .bind(completion.accepted_item_count)
        .bind(effective_latest_published_at)
        .bind(next_failures)
        .bind(next_empty)
        .bind(next_correctness)
        .bind(&completion.structure_fingerprint)
        .bind(&freshness_status)
        .bind(rebuild_required)
        .bind(&reason)
        .bind(&health_metadata)
        .bind(correctness_status)
        .execute(&mut *transaction)
        .await?;
        if rebuild_required {
            sqlx::query(
                r#"
                UPDATE company_news_recipes
                SET status = 'stale', stale_at = CURRENT_TIMESTAMP, stale_reason = $2
                WHERE id = $1 AND status = 'active'
                "#,
            )
            .bind(recipe.id)
            .bind(
                reason
                    .as_deref()
                    .unwrap_or("recipe correctness threshold exceeded"),
            )
            .execute(&mut *transaction)
            .await?;
        }
        sqlx::query(
            r#"
            INSERT INTO event_log (event_type, company_id, source_id, payload)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(if rebuild_required {
            "company_news.recipe_stale"
        } else {
            "company_news.recipe_checked"
        })
        .bind(recipe.company_id)
        .bind(recipe.source_id)
        .bind(json!({
            "recipe_id": recipe.id,
            "recipe_run_id": run_id,
            "status": run_status,
            "rebuild_required": rebuild_required,
            "freshness_status": freshness_status,
            "structure_changed": structure_changed,
            "transient_error": completion.transient_error,
            "correctness_reasons": completion.reasons,
        }))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(CompanyNewsRecipeRunOutcome {
            status: run_status,
            rebuild_required,
        })
    }

    pub async fn list_company_news_recipe_runs(
        &self,
        recipe_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<CompanyNewsRecipeRun>, DatabaseError> {
        let rows = sqlx::query_as::<_, RecipeRunRow>(
            r#"
            SELECT
                id, recipe_id, source_id, job_id, crawl_run_id, status,
                started_at, finished_at, discovered_url_count, accepted_item_count,
                rejected_url_count, normalized_item_count, new_item_count,
                latest_published_at, acceptance_ratio_bps, structure_fingerprint,
                reasons, error, metadata
            FROM company_news_recipe_runs
            WHERE $1::uuid IS NULL OR recipe_id = $1
            ORDER BY started_at DESC, id DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(recipe_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(RecipeRunRow::into_domain).collect()
    }

    pub async fn count_company_news_recipe_runs(
        &self,
        recipe_id: Option<Uuid>,
    ) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar(
            "SELECT count(*) FROM company_news_recipe_runs WHERE $1::uuid IS NULL OR recipe_id = $1",
        )
        .bind(recipe_id)
        .fetch_one(self.pool())
        .await?)
    }
}

fn validate_run_completion(
    completion: &CompanyNewsRecipeRunCompletion,
) -> Result<(), DatabaseError> {
    if completion.discovered_url_count < 0
        || completion.accepted_item_count < 0
        || completion.rejected_url_count < 0
        || completion.normalized_item_count < 0
        || completion.new_item_count < 0
        || completion.accepted_item_count + completion.rejected_url_count
            > completion.discovered_url_count
        || completion.normalized_item_count > completion.accepted_item_count
        || completion.new_item_count > completion.normalized_item_count
        || !(0..=10_000).contains(&completion.acceptance_ratio_bps)
        || (completion.transient_error && completion.error.is_none())
    {
        return Err(DatabaseError::InvalidState(
            "company news recipe run counts are inconsistent".to_owned(),
        ));
    }
    Ok(())
}

fn next_recipe_health_counts(
    current: (i32, i32, i32),
    correct: bool,
    empty: bool,
    transient_error: bool,
) -> (i32, i32, i32) {
    if transient_error {
        return current;
    }
    (
        if correct { 0 } else { current.0 + 1 },
        if empty { current.1 + 1 } else { 0 },
        if correct { 0 } else { current.2 + 1 },
    )
}

fn recipe_freshness_status(
    latest_published_at: Option<DateTime<Utc>>,
    reference_at: DateTime<Utc>,
    content_stale_after_seconds: u32,
    publication_date_coverage_complete: bool,
    correct: bool,
) -> &'static str {
    if !publication_date_coverage_complete {
        return if correct { "unknown" } else { "overdue" };
    }
    match latest_published_at {
        Some(published_at)
            if published_at
                < reference_at - Duration::seconds(i64::from(content_stale_after_seconds)) =>
        {
            "content_stale"
        }
        Some(_) if correct => "fresh",
        None if correct => "unknown",
        _ => "overdue",
    }
}

fn next_recipe_freshness_status(
    current_status: &str,
    latest_published_at: Option<DateTime<Utc>>,
    reference_at: DateTime<Utc>,
    content_stale_after_seconds: u32,
    publication_date_coverage_complete: bool,
    correct: bool,
    transient_error: bool,
) -> String {
    if transient_error {
        current_status.to_owned()
    } else {
        recipe_freshness_status(
            latest_published_at,
            reference_at,
            content_stale_after_seconds,
            publication_date_coverage_complete,
            correct,
        )
        .to_owned()
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};

    use super::{next_recipe_freshness_status, next_recipe_health_counts, recipe_freshness_status};

    #[test]
    fn freshness_requires_publication_date_evidence() {
        let now = Utc
            .with_ymd_and_hms(2026, 7, 23, 12, 0, 0)
            .single()
            .expect("valid fixture time");
        let stale_after_seconds = 180 * 24 * 60 * 60;

        assert_eq!(
            recipe_freshness_status(None, now, stale_after_seconds, false, true),
            "unknown"
        );
        assert_eq!(
            recipe_freshness_status(
                Some(now - Duration::days(1)),
                now,
                stale_after_seconds,
                true,
                true
            ),
            "fresh"
        );
        assert_eq!(
            recipe_freshness_status(
                Some(now - Duration::days(181)),
                now,
                stale_after_seconds,
                true,
                true
            ),
            "content_stale"
        );
        assert_eq!(
            recipe_freshness_status(
                Some(now - Duration::days(181)),
                now,
                stale_after_seconds,
                false,
                true
            ),
            "unknown",
            "an old maximum cannot prove staleness when accepted items are undated"
        );
        assert_eq!(
            recipe_freshness_status(
                Some(now - Duration::days(1)),
                now,
                stale_after_seconds,
                true,
                false
            ),
            "overdue"
        );
    }

    #[test]
    fn transient_errors_do_not_advance_recipe_drift_streaks() {
        assert_eq!(
            next_recipe_health_counts((2, 2, 2), false, true, true),
            (2, 2, 2)
        );
        assert_eq!(
            next_recipe_health_counts((2, 2, 2), true, false, false),
            (0, 0, 0)
        );
        assert_eq!(
            next_recipe_health_counts((2, 2, 2), false, true, false),
            (3, 3, 3)
        );
        let now = Utc
            .with_ymd_and_hms(2026, 7, 23, 12, 0, 0)
            .single()
            .expect("valid fixture time");
        assert_eq!(
            next_recipe_freshness_status(
                "fresh",
                Some(now - Duration::days(1)),
                now,
                180 * 24 * 60 * 60,
                true,
                false,
                true,
            ),
            "fresh"
        );
    }
}
