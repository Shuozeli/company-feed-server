pub mod config;
pub mod content_policy;
pub mod domain;
pub mod entity;
pub mod job;
pub mod recipe;
pub mod url_policy;

/// The transparent default identity used for public web fetches.
///
/// Operators should override this with `PUBLIC_FETCH_USER_AGENT` and include
/// deployment-specific contact information.
pub const DEFAULT_PUBLIC_FETCH_USER_AGENT: &str =
    "CompanyFeedServer/0.1 (+https://github.com/Shuozeli/company-feed-server)";

pub use config::{
    AppSettings, CompaniesConfig, CompanyListingSeed, CompanySeed, ConfigError, ExportTargetSeed,
    ExportTargetsConfig, ValidationActivationPolicy, WebDiscoveryAdapterMode,
};
pub use content_policy::{is_cms_placeholder_article, is_non_editorial_utility_article};
pub use domain::{
    CandidateDecision, CandidateDecisionMode, CandidateStatus, CandidateValidationStatus,
    EnumParseError, ExportFormat, ExportLayout, LifecycleStatus, OwnershipStatus, RawItemStatus,
    RunStatus, SourceKind, SourceStatus,
};
pub use entity::{
    CandidateDecisionRecord, CandidateReviewItem, CandidateValidationRun, Company, CompanyListing,
    CompanyNewsExtractionRun, CrawlBatch, CrawlRun, DiscoveredSource, DiscoveryRun, ExportRun,
    ExportTarget, ExportableFeedItem, ExportedItem, FeedItem, FeedItemSummary, NormalizedFeedItem,
    ProcessedCrawlItem, RawCrawlItem, ReviewDashboard, Source, SourceCandidate, SourceHealth,
    SourceHealthSummary,
};
pub use job::{ClaimedJob, Job, JobSpec, JobStatus, JobType};
pub use recipe::{
    COMPANY_NEWS_RECIPE_SCHEMA_VERSION, CompanyNewsRecipe, CompanyNewsRecipeCoverage,
    CompanyNewsRecipeRun, CompanyNewsRecipeSpec, RecipeCorrectnessPolicy, RecipeFetchProfile,
    RecipeFreshnessPolicy, RecipeHealth, RecipeItemScope, RecipeRenderMode, RecipeRunStatus,
    RecipeSpecError, RecipeStatus,
};
pub use url_policy::{has_invalid_resource_query, is_sitemap_url, resource_query_pairs};
