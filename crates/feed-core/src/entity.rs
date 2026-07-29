use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;
use uuid::Uuid;

use crate::{
    CandidateDecision, CandidateDecisionMode, CandidateStatus, CandidateValidationStatus,
    ExportFormat, ExportLayout, LifecycleStatus, OwnershipStatus, RunStatus, SourceKind,
    SourceStatus,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Company {
    pub id: Uuid,
    pub company_key: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub ownership_status: OwnershipStatus,
    pub lifecycle_status: LifecycleStatus,
    pub listings: Vec<CompanyListing>,
    pub homepage_url: Option<Url>,
    pub investor_relations_url: Option<Url>,
    pub newsroom_url: Option<Url>,
    pub blog_url: Option<Url>,
    pub hints: Vec<Url>,
    pub discovery_enabled: bool,
    pub discovery_not_before: DateTime<Utc>,
    pub discovery_cadence_seconds: i32,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompanyListing {
    pub ticker: String,
    pub exchange: Option<String>,
    pub is_primary: bool,
    pub metadata: Value,
}

impl Company {
    pub fn discovery_entry_points(&self) -> Vec<(&'static str, &Url)> {
        let mut entries = Vec::new();
        if let Some(url) = &self.homepage_url {
            entries.push(("homepage", url));
        }
        if let Some(url) = &self.investor_relations_url {
            entries.push(("investor_relations", url));
        }
        if let Some(url) = &self.newsroom_url {
            entries.push(("newsroom", url));
        }
        if let Some(url) = &self.blog_url {
            entries.push(("blog", url));
        }
        entries.extend(self.hints.iter().map(|url| ("hint", url)));
        entries
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredSource {
    pub candidate_url: Url,
    pub candidate_kind: SourceKind,
    pub confidence: f64,
    pub evidence: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceCandidate {
    pub id: Uuid,
    pub company_id: Uuid,
    pub discovery_run_id: Option<Uuid>,
    pub candidate_url: Url,
    pub candidate_kind: SourceKind,
    pub confidence: f64,
    pub evidence: Value,
    pub status: CandidateStatus,
    pub accepted_source_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateValidationRun {
    pub id: Uuid,
    pub candidate_id: Uuid,
    pub job_id: Option<Uuid>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateDecisionRecord {
    pub id: Uuid,
    pub candidate_id: Uuid,
    pub source_id: Option<Uuid>,
    pub decision: CandidateDecision,
    pub decision_mode: CandidateDecisionMode,
    pub actor: String,
    pub reason: String,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateReviewItem {
    pub candidate: SourceCandidate,
    pub company_key: String,
    pub company_name: String,
    pub latest_validation: Option<CandidateValidationRun>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReviewDashboard {
    pub total_companies: i64,
    pub companies_with_candidates: i64,
    pub companies_with_feed_candidates: i64,
    pub companies_with_activated_sources: i64,
    pub companies_with_healthy_sources: i64,
    pub new_feed_candidates: i64,
    pub unvalidated_feed_candidates: i64,
    pub validation_pending: i64,
    pub validation_running: i64,
    pub valid_candidates: i64,
    pub needs_review_candidates: i64,
    pub invalid_candidates: i64,
    pub failed_validations: i64,
    pub activated_sources: i64,
    pub healthy_sources: i64,
    pub failing_sources: i64,
    pub sources_awaiting_first_crawl: i64,
    pub normalized_items: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub id: Uuid,
    pub source_id: String,
    pub company_id: Uuid,
    pub kind: SourceKind,
    pub url: Url,
    pub status: SourceStatus,
    pub freshness_slo_seconds: i32,
    pub browser_required: bool,
    pub public_export_allowed: bool,
    pub discovery_confidence: Option<f64>,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedItem {
    pub id: Uuid,
    pub company_id: Uuid,
    pub source_id: Uuid,
    pub external_id: String,
    pub url: Url,
    pub canonical_url: Url,
    pub title: String,
    pub summary: String,
    pub body_text: String,
    pub body_html: String,
    pub body_markdown: String,
    pub published_at: Option<DateTime<Utc>>,
    pub fetched_at: DateTime<Utc>,
    pub content_hash: String,
    pub source_kind: SourceKind,
    pub content_processing: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedItemSummary {
    #[serde(flatten)]
    pub item: FeedItem,
    pub company_key: String,
    pub company_name: String,
    pub source_key: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RawCrawlItem {
    pub source_item_key: String,
    pub external_id: Option<String>,
    pub url: Url,
    pub canonical_url: Option<Url>,
    pub title: Option<String>,
    pub summary_html: Option<String>,
    pub body_html: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CrawlBatch {
    pub fetched_at: DateTime<Utc>,
    pub detected_source_kind: SourceKind,
    pub items: Vec<RawCrawlItem>,
    pub metadata: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NormalizedFeedItem {
    pub external_id: String,
    pub url: Url,
    pub canonical_url: Url,
    pub title: String,
    pub summary: String,
    pub body_text: String,
    pub body_html: String,
    pub body_markdown: String,
    pub published_at: Option<DateTime<Utc>>,
    pub fetched_at: DateTime<Utc>,
    pub content_hash: String,
    pub source_kind: SourceKind,
    pub raw: Value,
    pub normalized: Value,
    pub content_processing: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcessedCrawlItem {
    pub raw: RawCrawlItem,
    pub normalized: Result<NormalizedFeedItem, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceHealth {
    pub source_id: Uuid,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub consecutive_failures: i32,
    pub backoff_until: Option<DateTime<Utc>>,
    pub consecutive_zero_runs: i32,
    pub total_successful_runs: i64,
    pub total_items: i64,
    pub last_nonzero_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceHealthSummary {
    pub source: Source,
    pub company_key: String,
    pub company_name: String,
    pub health: Option<SourceHealth>,
    pub stored_item_count: i64,
    pub latest_item_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CrawlRun {
    pub id: Uuid,
    pub source_id: Uuid,
    pub job_id: Option<Uuid>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: crate::RunStatus,
    pub item_count: i32,
    pub new_item_count: i32,
    pub error: Option<String>,
    pub metadata: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompanyNewsExtractionRun {
    pub id: Uuid,
    pub company_id: Uuid,
    pub job_id: Option<Uuid>,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: crate::RunStatus,
    pub suggested_url_count: i32,
    pub accepted_url_count: i32,
    pub rejected_url_count: i32,
    pub source_count: i32,
    pub normalized_item_count: i32,
    pub new_item_count: i32,
    pub error: Option<String>,
    pub metadata: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryRun {
    pub id: Uuid,
    pub company_id: Uuid,
    pub job_id: Option<Uuid>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: RunStatus,
    pub candidate_count: i32,
    pub error: Option<String>,
    pub metadata: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportTarget {
    pub id: Uuid,
    pub target_id: String,
    pub repo_url: String,
    pub local_path: PathBuf,
    pub branch: String,
    pub format: ExportFormat,
    pub layout: ExportLayout,
    pub cadence_seconds: i32,
    pub enabled: bool,
    pub push_enabled: bool,
    pub metadata: Value,
    pub last_scheduled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportRun {
    pub id: Uuid,
    pub export_target_id: Uuid,
    pub job_id: Option<Uuid>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: RunStatus,
    pub item_count: i32,
    pub commit_sha: Option<String>,
    pub pushed: bool,
    pub error: Option<String>,
    pub metadata: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportableFeedItem {
    pub item: FeedItem,
    pub company_key: String,
    pub company_name: String,
    pub company_category_key: String,
    pub company_category_name: String,
    pub source_key: String,
    pub previous_exported_path: Option<PathBuf>,
    pub previous_content_hash: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExportedItem {
    pub feed_item_id: Uuid,
    pub exported_path: PathBuf,
    pub exported_content_hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn company_entry_points_preserve_provenance() {
        let now = Utc::now();
        let company = Company {
            id: Uuid::new_v4(),
            company_key: "acme".to_owned(),
            name: "Acme".to_owned(),
            aliases: Vec::new(),
            ownership_status: OwnershipStatus::Private,
            lifecycle_status: LifecycleStatus::Active,
            listings: Vec::new(),
            homepage_url: Some(Url::parse("https://example.com/").expect("valid URL")),
            investor_relations_url: None,
            newsroom_url: None,
            blog_url: None,
            hints: vec![Url::parse("https://example.com/news/").expect("valid URL")],
            discovery_enabled: true,
            discovery_not_before: now,
            discovery_cadence_seconds: 3600,
            metadata: Value::Object(Default::default()),
            created_at: now,
            updated_at: now,
        };

        let entries = company.discovery_entry_points();
        assert_eq!(entries[0].0, "homepage");
        assert_eq!(entries[1].0, "hint");
    }
}
