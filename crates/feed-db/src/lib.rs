mod config_sync;
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

use sqlx::{
    PgPool,
    migrate::{MigrateError, Migrator},
    postgres::PgPoolOptions,
};
use uuid::Uuid;

pub use config_sync::ConfigSyncSummary;
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

static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

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

    pub async fn migrate(&self) -> Result<(), DatabaseError> {
        MIGRATOR.run(&self.pool).await?;
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
    #[error(transparent)]
    Migration(#[from] MigrateError),
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
