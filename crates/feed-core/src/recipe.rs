use std::{collections::HashSet, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;
use uuid::Uuid;

use crate::EnumParseError;

pub const COMPANY_NEWS_RECIPE_SCHEMA_VERSION: &str = "company-news-recipe.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipeStatus {
    Draft,
    Active,
    Stale,
    Superseded,
    Disabled,
}

impl RecipeStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Stale => "stale",
            Self::Superseded => "superseded",
            Self::Disabled => "disabled",
        }
    }
}

impl FromStr for RecipeStatus {
    type Err = EnumParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "draft" => Ok(Self::Draft),
            "active" => Ok(Self::Active),
            "stale" => Ok(Self::Stale),
            "superseded" => Ok(Self::Superseded),
            "disabled" => Ok(Self::Disabled),
            _ => Err(EnumParseError::new("recipe status", value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipeRunStatus {
    Running,
    Passed,
    Failed,
    Stale,
    Cancelled,
}

impl RecipeRunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Stale => "stale",
            Self::Cancelled => "cancelled",
        }
    }
}

impl FromStr for RecipeRunStatus {
    type Err = EnumParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "running" => Ok(Self::Running),
            "passed" => Ok(Self::Passed),
            "failed" => Ok(Self::Failed),
            "stale" => Ok(Self::Stale),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(EnumParseError::new("recipe run status", value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipeRenderMode {
    Http,
    Browser,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipeItemScope {
    #[default]
    CompanyIdentity,
    PublicationBoundary,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeFreshnessPolicy {
    pub crawl_interval_seconds: u32,
    pub content_stale_after_seconds: u32,
    pub max_consecutive_empty_runs: u32,
    pub max_consecutive_failures: u32,
}

impl Default for RecipeFreshnessPolicy {
    fn default() -> Self {
        Self {
            crawl_interval_seconds: 12 * 60 * 60,
            content_stale_after_seconds: 180 * 24 * 60 * 60,
            max_consecutive_empty_runs: 3,
            max_consecutive_failures: 3,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeCorrectnessPolicy {
    pub baseline_discovered_items: u32,
    pub baseline_accepted_items: u32,
    pub min_discovered_items: u32,
    pub min_accepted_items: u32,
    pub min_acceptance_ratio_bps: u16,
    pub min_title_chars: u16,
    pub max_title_chars: u16,
    pub max_future_skew_seconds: u32,
}

impl Default for RecipeCorrectnessPolicy {
    fn default() -> Self {
        Self {
            baseline_discovered_items: 1,
            baseline_accepted_items: 1,
            min_discovered_items: 1,
            min_accepted_items: 1,
            min_acceptance_ratio_bps: 2_500,
            min_title_chars: 8,
            max_title_chars: 240,
            max_future_skew_seconds: 24 * 60 * 60,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanyNewsRecipeSpec {
    pub schema_version: String,
    pub publication_url: Url,
    pub render_mode: RecipeRenderMode,
    pub article_link_selector: String,
    pub allowed_hosts: Vec<String>,
    pub include_path_prefixes: Vec<String>,
    pub exclude_path_prefixes: Vec<String>,
    pub max_links: u32,
    pub freshness: RecipeFreshnessPolicy,
    pub correctness: RecipeCorrectnessPolicy,
    #[serde(default)]
    pub item_scope: RecipeItemScope,
    #[serde(default)]
    pub evidence_article_urls: Vec<Url>,
}

impl CompanyNewsRecipeSpec {
    pub fn validate(&self) -> Result<(), RecipeSpecError> {
        if self.schema_version != COMPANY_NEWS_RECIPE_SCHEMA_VERSION {
            return Err(RecipeSpecError::UnsupportedSchema(
                self.schema_version.clone(),
            ));
        }
        validate_public_url(&self.publication_url, "publication_url")?;
        if self.article_link_selector.trim().is_empty()
            || self.article_link_selector.len() > 512
            || self.max_links == 0
            || self.max_links > 500
        {
            return Err(RecipeSpecError::Invalid(
                "link selector and max_links must be bounded".to_owned(),
            ));
        }
        if self.allowed_hosts.is_empty() || self.allowed_hosts.len() > 20 {
            return Err(RecipeSpecError::Invalid(
                "allowed_hosts must contain between 1 and 20 hosts".to_owned(),
            ));
        }
        let mut hosts = HashSet::new();
        for host in &self.allowed_hosts {
            let normalized = host.trim().trim_start_matches("www.").to_ascii_lowercase();
            if normalized.is_empty()
                || normalized.len() > 253
                || !normalized.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '.' | '-')
                })
                || !hosts.insert(normalized)
            {
                return Err(RecipeSpecError::Invalid(
                    "allowed_hosts contains an invalid or duplicate host".to_owned(),
                ));
            }
        }
        validate_prefixes(&self.include_path_prefixes, "include_path_prefixes")?;
        validate_prefixes(&self.exclude_path_prefixes, "exclude_path_prefixes")?;
        if self.freshness.crawl_interval_seconds == 0
            || self.freshness.content_stale_after_seconds < self.freshness.crawl_interval_seconds
            || self.freshness.max_consecutive_empty_runs == 0
            || self.freshness.max_consecutive_failures == 0
        {
            return Err(RecipeSpecError::Invalid(
                "freshness thresholds must be positive and internally ordered".to_owned(),
            ));
        }
        let correctness = &self.correctness;
        if correctness.baseline_discovered_items == 0
            || correctness.baseline_accepted_items == 0
            || correctness.min_discovered_items == 0
            || correctness.min_accepted_items == 0
            || correctness.min_discovered_items > correctness.baseline_discovered_items
            || correctness.min_accepted_items > correctness.baseline_accepted_items
            || correctness.min_acceptance_ratio_bps == 0
            || correctness.min_acceptance_ratio_bps > 10_000
            || correctness.min_title_chars == 0
            || correctness.max_title_chars < correctness.min_title_chars
            || correctness.max_future_skew_seconds == 0
        {
            return Err(RecipeSpecError::Invalid(
                "correctness thresholds are inconsistent".to_owned(),
            ));
        }
        if self.evidence_article_urls.len() > 50 {
            return Err(RecipeSpecError::Invalid(
                "evidence_article_urls exceeds 50".to_owned(),
            ));
        }
        for url in &self.evidence_article_urls {
            validate_public_url(url, "evidence_article_urls")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompanyNewsRecipe {
    pub id: Uuid,
    pub recipe_key: String,
    pub company_id: Uuid,
    pub source_id: Uuid,
    pub version: i32,
    pub status: RecipeStatus,
    pub spec: CompanyNewsRecipeSpec,
    pub content_hash: String,
    pub generated_by_run_id: Option<Uuid>,
    pub verified_at: Option<DateTime<Utc>>,
    pub stale_at: Option<DateTime<Utc>>,
    pub stale_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub health: RecipeHealth,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecipeHealth {
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_correct_at: Option<DateTime<Utc>>,
    pub last_nonempty_at: Option<DateTime<Utc>>,
    pub last_item_published_at: Option<DateTime<Utc>>,
    pub consecutive_failures: i32,
    pub consecutive_empty_runs: i32,
    pub consecutive_correctness_failures: i32,
    pub last_structure_fingerprint: Option<String>,
    pub freshness_status: String,
    pub correctness_status: String,
    pub rebuild_required: bool,
    pub reason: Option<String>,
    pub metadata: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompanyNewsRecipeRun {
    pub id: Uuid,
    pub recipe_id: Uuid,
    pub source_id: Uuid,
    pub job_id: Option<Uuid>,
    pub crawl_run_id: Option<Uuid>,
    pub status: RecipeRunStatus,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub discovered_url_count: i32,
    pub accepted_item_count: i32,
    pub rejected_url_count: i32,
    pub normalized_item_count: i32,
    pub new_item_count: i32,
    pub latest_published_at: Option<DateTime<Utc>>,
    pub acceptance_ratio_bps: i32,
    pub structure_fingerprint: Option<String>,
    pub reasons: Vec<String>,
    pub error: Option<String>,
    pub metadata: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompanyNewsRecipeCoverage {
    pub eligible_companies: i64,
    pub companies_with_approved_feed: i64,
    pub companies_with_healthy_feed: i64,
    pub companies_with_active_recipe: i64,
    pub companies_with_approved_feed_and_active_recipe: i64,
    pub companies_covered_by_feed_or_recipe: i64,
    pub companies_missing_feed_or_recipe: i64,
    pub companies_with_completed_build: i64,
    pub companies_without_completed_build: i64,
    pub companies_uncovered_after_completed_build: i64,
    pub companies_uncovered_awaiting_completed_build: i64,
    pub companies_fresh_and_correct: i64,
    pub companies_content_stale: i64,
    pub companies_with_stale_recipe: i64,
    pub companies_missing_recipe: i64,
    pub active_recipes: i64,
    pub public_recipe_items: i64,
    pub quality_quarantined_recipe_items: i64,
    pub pending_build_jobs: i64,
    pub running_build_jobs: i64,
    pub retrying_build_jobs: i64,
    pub completed_build_jobs: i64,
    pub terminal_failed_build_jobs: i64,
    pub completed_build_runs: i64,
    pub failed_build_runs: i64,
}

fn validate_public_url(url: &Url, field: &'static str) -> Result<(), RecipeSpecError> {
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(RecipeSpecError::Invalid(format!(
            "{field} must be a public HTTP(S) URL without credentials"
        )));
    }
    if url.host_str().is_some_and(is_reserved_example_host) {
        return Err(RecipeSpecError::Invalid(format!(
            "{field} must not use a reserved example host"
        )));
    }
    Ok(())
}

fn is_reserved_example_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    [
        "example",
        "invalid",
        "localhost",
        "test",
        "example.com",
        "example.net",
        "example.org",
    ]
    .iter()
    .any(|reserved| {
        host == *reserved
            || host
                .strip_suffix(reserved)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

fn validate_prefixes(prefixes: &[String], field: &'static str) -> Result<(), RecipeSpecError> {
    if prefixes.len() > 50 {
        return Err(RecipeSpecError::Invalid(format!(
            "{field} exceeds 50 entries"
        )));
    }
    let mut seen = HashSet::new();
    for prefix in prefixes {
        if !prefix.starts_with('/')
            || prefix.len() > 512
            || prefix.contains(['?', '#'])
            || !seen.insert(prefix)
        {
            return Err(RecipeSpecError::Invalid(format!(
                "{field} contains an invalid or duplicate path prefix"
            )));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RecipeSpecError {
    #[error("unsupported recipe schema {0}")]
    UnsupportedSchema(String),
    #[error("invalid recipe: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipe_urls_reject_reserved_example_hosts() {
        for value in [
            "https://public.example/news",
            "https://example.com/news",
            "https://company.invalid/news",
        ] {
            let url = Url::parse(value).expect("URL");
            assert!(validate_public_url(&url, "test_url").is_err(), "{value}");
        }
    }
}
