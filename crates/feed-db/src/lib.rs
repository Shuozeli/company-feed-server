mod config_sync;
mod content_crawling;
mod crawling;
mod discovery;
mod exports;
mod feeds;
mod jobs;
mod news_extraction;
mod recipes;
mod sources;
mod universe;
mod validation;

use std::time::Duration;

use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

pub use config_sync::ConfigSyncSummary;
pub use content_crawling::{
    CONTENT_EXTRACTION_VERSION, ContentCrawlCandidate, ContentCrawlCoverage, ContentCrawlFailure,
    ContentCrawlSuccess,
};
pub use crawling::{CrawlPersistSummary, FeedItemQualityQuarantine, RecipeArtifactFailure};
pub use feeds::{
    ApprovedFeedItemCompanyClaim, FeedItemSignatureCandidate, FeedItemSummaryFilter,
    PublicFeedItemCompanyClaim,
};
pub use jobs::{JobFailureOutcome, JobLeaseError};
pub use news_extraction::CompanyNewsExtractionCompletion;
pub use recipes::{
    ActiveCompanyNewsPublicationClaim, CompanyNewsRecipeRunCompletion, CompanyNewsRecipeRunOutcome,
};
pub use sources::ApprovedSourceCompanyClaim;
pub use universe::{
    ActivatedCompany, CompanyImportAction, CompanyImportSummary, CompanyWaveSummary,
    UniverseImportOptions, UniverseWaveOptions,
};
pub use validation::CandidateValidationCompletion;

const POSTGRES_SCHEMA: &str = include_str!("../../../schema/postgres.sql");
const SCHEMA_LOCK_KEY: i64 = 0x4346_5343_4845_4D41;

#[derive(Clone, Debug)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self, DatabaseError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(Duration::from_secs(10))
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Initializes a brand-new database from the declarative schema and
    /// verifies that an existing database has already been reconciled.
    ///
    /// Existing databases are changed only by `pg-schema-diff`; application
    /// startup never executes a hand-written or embedded migration sequence.
    pub async fn ensure_schema(&self) -> Result<(), DatabaseError> {
        let mut connection = self.pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(SCHEMA_LOCK_KEY)
            .execute(&mut *connection)
            .await?;

        let result = async {
            let schema_exists: bool =
                sqlx::query_scalar("SELECT to_regclass('public.jobs') IS NOT NULL")
                    .fetch_one(&mut *connection)
                    .await?;
            if !schema_exists {
                sqlx::raw_sql(POSTGRES_SCHEMA)
                    .execute(&mut *connection)
                    .await?;
            }

            let schema_ready: bool = sqlx::query_scalar(
                r#"
                SELECT
                    to_regclass('public.jobs') IS NOT NULL
                    AND to_regclass('public.content_crawl_state') IS NOT NULL
                    AND to_regclass('public.content_crawl_attempts') IS NOT NULL
                "#,
            )
            .fetch_one(&mut *connection)
            .await?;
            if !schema_ready {
                return Err(DatabaseError::SchemaDrift(
                    "database does not match schema/postgres.sql; run scripts/schema-apply.sh"
                        .to_owned(),
                ));
            }
            Ok(())
        }
        .await;

        let unlock_result = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(SCHEMA_LOCK_KEY)
            .execute(&mut *connection)
            .await;
        result?;
        unlock_result?;
        Ok(())
    }

    pub async fn ping(&self) -> Result<(), DatabaseError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    pub async fn close(self) {
        self.pool.close().await;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DatabaseError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error("database schema drift: {0}")]
    SchemaDrift(String),
    #[error(transparent)]
    InvalidEnum(#[from] feed_core::EnumParseError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("duration {0:?} cannot be represented by Postgres")]
    InvalidDuration(Duration),
    #[error("value {value} for {field} exceeds the supported database range")]
    NumericRange { field: &'static str, value: u64 },
    #[error("path for export target {target_id} is not valid UTF-8")]
    NonUtf8Path { target_id: String },
    #[error("database returned an invalid job lease for job {0}")]
    InvalidJobLease(Uuid),
    #[error("invalid URL stored in the database: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("{entity} {id} was not found")]
    NotFound { entity: &'static str, id: Uuid },
    #[error("database invariant violated: {0}")]
    Invariant(String),
    #[error("invalid state transition: {0}")]
    InvalidState(String),
    #[error(transparent)]
    LostJobLease(#[from] JobLeaseError),
}
