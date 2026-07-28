use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use chrono::{Duration, NaiveDate, Utc};
use feed_core::{
    AppSettings, COMPANY_NEWS_RECIPE_SCHEMA_VERSION, CandidateDecisionMode,
    CandidateValidationStatus, Company, CompanyNewsRecipe, CompanyNewsRecipeSpec, CrawlBatch,
    FeedItem, Job, JobSpec, JobType, ProcessedCrawlItem, RawCrawlItem, RecipeCorrectnessPolicy,
    RecipeFreshnessPolicy, RecipeItemScope, RecipeRenderMode, RecipeStatus, Source,
    SourceCandidate, SourceKind, SourceStatus, ValidationActivationPolicy, WebDiscoveryAdapterMode,
    is_cms_placeholder_article, is_non_editorial_utility_article, is_sitemap_url,
};
use feed_crawler::{
    ArticleFetchFailure, ArticlePageError, CrawlError, HtmlArticleCrawlReport, HtmlArticleCrawler,
    HtmlArticleCrawlerConfig, HtmlRecipeCrawlCache, HtmlRecipeCrawlReport, HtmlRecipeCrawler,
    HtmlRecipeCrawlerConfig, RecipeCrawlError, RssAtomCrawler, RssAtomCrawlerConfig,
    distinct_sanitized_content_count, repeated_sanitized_content_urls,
};
use feed_db::{
    ActiveCompanyNewsPublicationClaim, ApprovedFeedItemCompanyClaim, ApprovedSourceCompanyClaim,
    CandidateValidationCompletion, CompanyNewsExtractionCompletion, CompanyNewsRecipeRunCompletion,
    Database, FeedItemQualityQuarantine, FeedItemSignatureCandidate, PublicFeedItemCompanyClaim,
    RecipeArtifactFailure,
};
use feed_discovery::{DiscoveryClient, DiscoveryConfig, DiscoveryError, DiscoverySeed};
use feed_exporter::{ExportError, export_archive};
use feed_normalizer::normalize_item;
use feed_scheduler::{JobHandler, JobHandlerError, JobHandlerRegistry, SchedulerError};
use feed_web_adapter::{
    CompanyNewsExtractionAdapterClient, CompanyNewsExtractionAdapterConfig,
    CompanyNewsExtractionRequest, CompanyNewsExtractionResponse, WebAdapterError,
    WebDiscoveryAdapterClient, WebDiscoveryAdapterConfig, WebDiscoveryCompany, WebDiscoveryRequest,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use url::Url;

pub fn build_discovery_job_registry(
    database: Database,
    settings: &AppSettings,
) -> Result<JobHandlerRegistry, JobBootstrapError> {
    let discovery = DiscoveryClient::new(DiscoveryConfig {
        request_timeout: settings.discovery_request_timeout,
        max_response_bytes: settings.discovery_max_response_bytes,
        max_concurrency: settings.discovery_max_concurrency,
        max_candidates: settings.discovery_max_candidates,
        probe_common_paths: settings.discovery_probe_common_paths,
        allow_private_networks: settings.discovery_allow_private_networks,
        user_agent: settings.public_fetch_user_agent.clone(),
    })?;
    let web_adapter = match settings.web_discovery_adapter_mode {
        WebDiscoveryAdapterMode::Disabled => None,
        WebDiscoveryAdapterMode::Fallback | WebDiscoveryAdapterMode::Augment => {
            let base_url = settings.web_discovery_adapter_url.clone().ok_or_else(|| {
                JobBootstrapError::WebAdapter(WebAdapterError::InvalidConfig(
                    "adapter URL is missing".to_owned(),
                ))
            })?;
            Some(WebAdapterIntegration::new(
                WebDiscoveryAdapterClient::new(WebDiscoveryAdapterConfig {
                    base_url,
                    bearer_token: settings.web_discovery_adapter_token.clone(),
                    request_timeout: settings.web_discovery_adapter_timeout,
                    max_response_bytes: settings.web_discovery_adapter_max_response_bytes,
                    max_candidates: settings.web_discovery_adapter_max_candidates,
                })?,
                settings.web_discovery_adapter_mode,
                settings.web_discovery_adapter_max_candidates,
            )?)
        }
    };
    let registry = JobHandlerRegistry::new().register(Arc::new(
        DiscoveryJobHandler::new(database, discovery).with_web_adapter(web_adapter),
    ))?;
    Ok(registry)
}

pub fn build_validation_job_registry(
    database: Database,
    settings: &AppSettings,
) -> Result<JobHandlerRegistry, JobBootstrapError> {
    let crawler = build_crawler(settings)?;
    let policy = CandidateValidationPolicy {
        auto_activate: settings.validation_auto_activate,
        public_export_allowed: settings.validation_public_export,
        activation_policy: settings.validation_activation_policy,
        max_item_age_days: settings.validation_max_item_age_days,
        freshness_slo_seconds: settings.auto_activation_freshness_seconds,
    };
    Ok(
        JobHandlerRegistry::new().register(Arc::new(CandidateValidationJobHandler::new(
            database, crawler, policy,
        )))?,
    )
}

pub fn build_crawl_export_job_registry(
    database: Database,
    settings: &AppSettings,
) -> Result<JobHandlerRegistry, JobBootstrapError> {
    let crawler = build_crawler(settings)?;
    let recipe_crawler = build_recipe_crawler(settings)?;
    Ok(JobHandlerRegistry::new()
        .register(Arc::new(CrawlJobHandler::new(
            database.clone(),
            crawler,
            recipe_crawler,
        )))?
        .register(Arc::new(ExportJobHandler::new(database)))?)
}

pub fn build_news_extraction_job_registry(
    database: Database,
    settings: &AppSettings,
) -> Result<JobHandlerRegistry, JobBootstrapError> {
    if !settings.news_extraction_enabled {
        return Err(JobBootstrapError::NewsExtractionDisabled);
    }
    let base_url = settings
        .news_extraction_adapter_url
        .clone()
        .ok_or_else(|| {
            JobBootstrapError::WebAdapter(WebAdapterError::InvalidConfig(
                "manual news import adapter URL is missing".to_owned(),
            ))
        })?;
    let adapter = CompanyNewsExtractionAdapterClient::new(CompanyNewsExtractionAdapterConfig {
        base_url,
        bearer_token: settings.news_extraction_adapter_token.clone(),
        request_timeout: settings.news_extraction_adapter_timeout,
        max_response_bytes: settings.news_extraction_adapter_max_response_bytes,
        max_articles: settings.news_extraction_max_articles,
    })?;
    let crawler = HtmlArticleCrawler::new(HtmlArticleCrawlerConfig {
        request_timeout: settings.news_extraction_fetch_timeout,
        max_response_bytes: settings.news_extraction_max_response_bytes,
        max_articles: settings.news_extraction_max_articles,
        max_concurrency: settings.news_extraction_max_concurrency,
        max_per_host_concurrency: settings.news_extraction_max_per_host_concurrency,
        min_content_chars: settings.news_extraction_min_content_chars,
        allow_private_networks: settings.news_extraction_allow_private_networks,
        user_agent: settings.public_fetch_user_agent.clone(),
    })?;
    let recipe_crawler = HtmlRecipeCrawler::new(HtmlRecipeCrawlerConfig {
        request_timeout: settings.news_extraction_fetch_timeout,
        max_response_bytes: settings.news_extraction_max_response_bytes,
        max_articles: settings.news_extraction_max_articles,
        max_concurrency: settings.news_extraction_max_concurrency,
        max_per_host_concurrency: settings.news_extraction_max_per_host_concurrency,
        min_content_chars: settings.news_extraction_min_content_chars,
        allow_private_networks: settings.news_extraction_allow_private_networks,
        user_agent: settings.public_fetch_user_agent.clone(),
    })?;
    Ok(
        JobHandlerRegistry::new().register(Arc::new(CompanyNewsExtractionJobHandler::new(
            database,
            adapter,
            crawler,
            recipe_crawler,
            settings.news_extraction_max_articles,
            settings.news_extraction_source_freshness_seconds,
            settings.news_extraction_public_export,
        )))?,
    )
}

fn build_crawler(settings: &AppSettings) -> Result<RssAtomCrawler, CrawlError> {
    RssAtomCrawler::new(RssAtomCrawlerConfig {
        request_timeout: settings.crawler_request_timeout,
        max_response_bytes: settings.crawler_max_response_bytes,
        max_items: settings.crawler_max_items,
        user_agent: settings.public_fetch_user_agent.clone(),
    })
}

fn build_recipe_crawler(settings: &AppSettings) -> Result<HtmlRecipeCrawler, RecipeCrawlError> {
    HtmlRecipeCrawler::new(HtmlRecipeCrawlerConfig {
        request_timeout: settings.crawler_request_timeout,
        max_response_bytes: settings.crawler_max_response_bytes,
        max_articles: settings.crawler_max_items.clamp(1, 100),
        max_concurrency: 4,
        min_content_chars: 200,
        allow_private_networks: false,
        user_agent: settings.public_fetch_user_agent.clone(),
        ..HtmlRecipeCrawlerConfig::default()
    })
}

#[derive(Clone)]
pub struct DiscoveryJobHandler {
    database: Database,
    discovery: DiscoveryClient,
    web_adapter: Option<WebAdapterIntegration>,
}

impl DiscoveryJobHandler {
    pub fn new(database: Database, discovery: DiscoveryClient) -> Self {
        Self {
            database,
            discovery,
            web_adapter: None,
        }
    }

    pub fn with_web_adapter(mut self, web_adapter: Option<WebAdapterIntegration>) -> Self {
        self.web_adapter = web_adapter;
        self
    }
}

#[derive(Clone)]
pub struct WebAdapterIntegration {
    client: WebDiscoveryAdapterClient,
    mode: WebDiscoveryAdapterMode,
    max_candidates: usize,
}

impl WebAdapterIntegration {
    pub fn new(
        client: WebDiscoveryAdapterClient,
        mode: WebDiscoveryAdapterMode,
        max_candidates: usize,
    ) -> Result<Self, WebAdapterError> {
        if mode == WebDiscoveryAdapterMode::Disabled {
            return Err(WebAdapterError::InvalidConfig(
                "a configured adapter must use fallback or augment mode".to_owned(),
            ));
        }
        if max_candidates == 0 || u32::try_from(max_candidates).is_err() {
            return Err(WebAdapterError::InvalidConfig(
                "adapter max_candidates must fit in a positive u32".to_owned(),
            ));
        }
        Ok(Self {
            client,
            mode,
            max_candidates,
        })
    }
}

struct WebAdapterOutcome {
    seeds: Vec<DiscoverySeed>,
    metadata: Value,
    called: bool,
}

const MAX_DISCOVERY_JOB_SEEDS: usize = 100;

#[async_trait]
impl JobHandler for DiscoveryJobHandler {
    fn supported_job_types(&self) -> &[JobType] {
        &[JobType::DiscoverCompany]
    }

    async fn handle(&self, job: &Job) -> Result<(), JobHandlerError> {
        let company_id = job.company_id.ok_or_else(|| {
            JobHandlerError::permanent("discover_company job is missing company_id")
        })?;
        let company = self
            .database
            .get_company(company_id)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?
            .ok_or_else(|| {
                JobHandlerError::permanent(format!("company {company_id} does not exist"))
            })?;
        let provided_seeds = discovery_job_seeds(&job.payload)?;
        let run_id = self
            .database
            .begin_discovery_run(company_id, job.id)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?;

        let adapter_outcome = if provided_seeds.is_empty() {
            match self.request_web_adapter(job.id, &company).await {
                Ok(outcome) => outcome,
                Err(error) if !company.discovery_entry_points().is_empty() => WebAdapterOutcome {
                    seeds: Vec::new(),
                    metadata: json!({
                        "status": "failed",
                        "retryable": error.is_retryable(),
                        "error": error.safe_summary(),
                    }),
                    called: true,
                },
                Err(error) => {
                    let safe_error = error.safe_summary();
                    self.database
                    .fail_discovery_run(
                        run_id,
                        &safe_error,
                        json!({
                            "web_adapter": {
                                "status": "failed",
                                "retryable": error.is_retryable(),
                                "error": safe_error,
                            }
                        }),
                    )
                    .await
                    .map_err(|database_error| {
                        JobHandlerError::retryable(format!(
                            "{safe_error}; additionally failed to close discovery run: {database_error}"
                        ))
                })?;
                    return Err(classify_web_adapter_error(error));
                }
            }
        } else {
            WebAdapterOutcome {
                metadata: json!({
                    "status": "provided_by_job",
                    "mode": "seeded_public_discovery",
                    "candidate_count": provided_seeds.len(),
                    "seed_origin": job.payload.get("seed_origin"),
                    "origin_run_id": job.payload.get("origin_run_id"),
                }),
                seeds: provided_seeds,
                called: false,
            }
        };

        if adapter_outcome.called
            && adapter_outcome.seeds.is_empty()
            && company.discovery_entry_points().is_empty()
        {
            return self
                .database
                .complete_discovery_run(
                    run_id,
                    &[],
                    json!({ "web_adapter": adapter_outcome.metadata }),
                )
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()));
        }

        match self
            .discovery
            .discover_company_with_seeds(&company, &adapter_outcome.seeds)
            .await
        {
            Ok(report) => self
                .database
                .complete_discovery_run(
                    run_id,
                    &report.candidates,
                    merge_web_adapter_metadata(report.metadata(), adapter_outcome.metadata),
                )
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string())),
            Err(error) => {
                let metadata =
                    merge_web_adapter_metadata(error.metadata(), adapter_outcome.metadata);
                self.database
                    .fail_discovery_run(run_id, &error.to_string(), metadata)
                    .await
                    .map_err(|database_error| {
                        JobHandlerError::retryable(format!(
                            "{error}; additionally failed to close discovery run: {database_error}"
                        ))
                    })?;
                Err(classify_discovery_error(error))
            }
        }
    }
}

impl DiscoveryJobHandler {
    async fn request_web_adapter(
        &self,
        request_id: uuid::Uuid,
        company: &Company,
    ) -> Result<WebAdapterOutcome, WebAdapterError> {
        let Some(adapter) = &self.web_adapter else {
            return Ok(WebAdapterOutcome {
                seeds: Vec::new(),
                metadata: json!({ "status": "disabled" }),
                called: false,
            });
        };
        let has_entry_points = !company.discovery_entry_points().is_empty();
        if adapter.mode == WebDiscoveryAdapterMode::Fallback && has_entry_points {
            return Ok(WebAdapterOutcome {
                seeds: Vec::new(),
                metadata: json!({ "status": "not_requested", "mode": "fallback" }),
                called: false,
            });
        }

        let known_urls = company
            .discovery_entry_points()
            .into_iter()
            .map(|(_, url)| url.clone())
            .collect::<Vec<_>>();
        let request = WebDiscoveryRequest::new(
            request_id,
            WebDiscoveryCompany {
                company_id: company.id,
                name: company.name.clone(),
                aliases: company.aliases.clone(),
                known_urls,
                sector: metadata_string(&company.metadata, "sector"),
                industry: metadata_string(&company.metadata, "industry"),
            },
            u32::try_from(adapter.max_candidates).map_err(|_| {
                WebAdapterError::InvalidConfig("adapter max candidate limit exceeds u32".to_owned())
            })?,
        );
        let response = adapter.client.discover(&request).await?;
        let candidate_count = response.candidates.len();
        let seeds = response
            .candidates
            .into_iter()
            .map(|candidate| DiscoverySeed {
                url: candidate.url,
                role: candidate.role.as_str().to_owned(),
                rank_score: candidate.rank_score,
            })
            .collect();
        Ok(WebAdapterOutcome {
            seeds,
            metadata: json!({
                "status": "completed",
                "mode": adapter.mode.as_str(),
                "request_id": response.request_id,
                "adapter_trace_id": response.adapter_trace_id,
                "candidate_count": candidate_count,
            }),
            called: true,
        })
    }
}

fn discovery_job_seeds(payload: &Value) -> Result<Vec<DiscoverySeed>, JobHandlerError> {
    let Some(value) = payload.get("seeds") else {
        return Ok(Vec::new());
    };
    let seeds = serde_json::from_value::<Vec<DiscoverySeed>>(value.clone()).map_err(|error| {
        JobHandlerError::permanent(format!("discover_company job has invalid seeds: {error}"))
    })?;
    if seeds.len() > MAX_DISCOVERY_JOB_SEEDS {
        return Err(JobHandlerError::permanent(format!(
            "discover_company job has {} seeds; maximum is {MAX_DISCOVERY_JOB_SEEDS}",
            seeds.len()
        )));
    }
    Ok(seeds)
}

fn metadata_string(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .or_else(|| {
            metadata
                .get("universe")
                .and_then(|universe| universe.get(key))
        })
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn merge_web_adapter_metadata(mut discovery: Value, web_adapter: Value) -> Value {
    if let Some(object) = discovery.as_object_mut() {
        object.insert("web_adapter".to_owned(), web_adapter);
    }
    discovery
}

fn classify_discovery_error(error: DiscoveryError) -> JobHandlerError {
    match error {
        DiscoveryError::NoEntryPoints(_)
        | DiscoveryError::InvalidEntryPoint { .. }
        | DiscoveryError::InvalidSeed { .. } => JobHandlerError::permanent(error.to_string()),
        DiscoveryError::InvalidConfig(_)
        | DiscoveryError::Client(_)
        | DiscoveryError::AllEntryPointsFailed { .. } => {
            JobHandlerError::retryable(error.to_string())
        }
    }
}

fn classify_web_adapter_error(error: WebAdapterError) -> JobHandlerError {
    let summary = error.safe_summary();
    if error.is_retryable() {
        let cooldown = std::time::Duration::from_secs(
            error
                .retry_after_seconds()
                .unwrap_or(WEB_ADAPTER_OUTAGE_COOLDOWN.as_secs())
                .max(WEB_ADAPTER_OUTAGE_COOLDOWN.as_secs()),
        );
        JobHandlerError::retryable_with_worker_cooldown(summary, cooldown)
    } else {
        JobHandlerError::permanent(summary)
    }
}

const WEB_ADAPTER_OUTAGE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);
const FEED_TITLE_DIVERSITY_MIN_SAMPLE: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FeedTitleDiversity {
    titled_item_count: usize,
    usable_titled_item_count: usize,
    distinct_titled_item_count: usize,
    passed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FeedContentDiversity {
    item_count: usize,
    repeated_content_item_count: usize,
    passed: bool,
}

const PUBLICATION_TOPIC_COMPROMISE_MIN_SAMPLE: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PublicationTopicCompromise {
    sample_item_count: usize,
    suspicious_item_count: usize,
    detected: bool,
}

fn publication_topic_compromise(
    company: &Company,
    items: &[RawCrawlItem],
) -> PublicationTopicCompromise {
    let sample = items.iter().take(20).collect::<Vec<_>>();
    let suspicious_item_count = sample
        .iter()
        .filter(|item| raw_item_looks_like_gambling_spam(item))
        .count();
    let sample_item_count = sample.len();
    let company_terms = company_scope_identity_terms(company);
    let company_named_title_count = sample
        .iter()
        .filter(|item| {
            item.title
                .as_deref()
                .is_some_and(|title| text_mentions_company_scope_term(title, &company_terms))
        })
        .count();
    let detected = !company_profile_allows_gambling_publication(company)
        && sample_item_count >= PUBLICATION_TOPIC_COMPROMISE_MIN_SAMPLE
        && suspicious_item_count.saturating_mul(5) >= sample_item_count.saturating_mul(4)
        && company_named_title_count.saturating_mul(2) < sample_item_count;
    PublicationTopicCompromise {
        sample_item_count,
        suspicious_item_count,
        detected,
    }
}

fn company_profile_allows_gambling_publication(company: &Company) -> bool {
    let mut profile = company.name.to_ascii_lowercase();
    for alias in &company.aliases {
        profile.push(' ');
        profile.push_str(&alias.to_ascii_lowercase());
    }
    profile.push(' ');
    profile.push_str(&company.metadata.to_string().to_ascii_lowercase());

    [
        "amusement",
        "casino",
        "entertainment",
        "gambling",
        "gaming",
        "hotel",
        "igaming",
        "lottery",
        "payment",
        "poker",
        "prediction market",
        "resort",
        "sports betting",
        "sportsbook",
        "wagering",
    ]
    .iter()
    .any(|marker| profile.contains(marker))
}

fn raw_item_looks_like_gambling_spam(item: &RawCrawlItem) -> bool {
    let mut text = item.url.as_str().to_ascii_lowercase();
    if let Some(canonical_url) = &item.canonical_url {
        text.push(' ');
        text.push_str(&canonical_url.as_str().to_ascii_lowercase());
    }
    if let Some(title) = &item.title {
        text.push(' ');
        text.push_str(&title.to_ascii_lowercase());
    }
    if let Some(summary) = &item.summary_html {
        text.push(' ');
        text.push_str(&summary.to_ascii_lowercase());
    }
    if let Some(body) = &item.body_html {
        text.push(' ');
        text.push_str(&body.to_ascii_lowercase());
    }

    let casino_signal = [
        "casino",
        "cazino",
        "cazinou",
        "gambling establishment",
        "gambling",
        "igaming",
    ]
    .iter()
    .any(|marker| text.contains(marker));
    let promotion_or_wager_signal = [
        "betting",
        "bonus",
        "depunere",
        "free spin",
        "no deposit",
        "no-deposit",
        "pariere",
        "promo code",
        "rotiri gratuite",
        "sportsbook",
        "wager",
    ]
    .iter()
    .any(|marker| text.contains(marker));
    let casino_game_signal = ["blackjack", "poker", "roulette", "slot"]
        .iter()
        .any(|marker| text.contains(marker));
    let known_spam_campaign_signal = [
        "1xbet",
        "1win",
        "20bet",
        "22bet",
        "bc game",
        "bigclash",
        "bovada",
        "chicken road",
        "mostbet",
        "pinco",
        "sweet bonanza",
    ]
    .iter()
    .any(|marker| text.contains(marker));

    known_spam_campaign_signal
        || (casino_signal && (promotion_or_wager_signal || casino_game_signal))
}

fn apply_direct_publication_topic_compromise_filter(
    company: &Company,
    report: &mut HtmlArticleCrawlReport,
) -> usize {
    let assessment = publication_topic_compromise(company, &report.items);
    if !assessment.detected {
        return 0;
    }

    let rejected_count = report.items.len();
    for item in report.items.drain(..) {
        report.failures.push(ArticleFetchFailure {
            url: item.url,
            reason: "publication_topic_compromise_detected".to_owned(),
            retryable: false,
            error: format!(
                "publication sample is dominated by gambling SEO content unrelated to {} \
                 ({} of {} sampled items)",
                company.name, assessment.suspicious_item_count, assessment.sample_item_count
            ),
        });
    }
    report.failures.sort_by(|left, right| {
        left.url
            .as_str()
            .cmp(right.url.as_str())
            .then_with(|| left.reason.cmp(&right.reason))
    });
    rejected_count
}

fn apply_recipe_publication_topic_compromise_filter(
    company: &Company,
    spec: &CompanyNewsRecipeSpec,
    report: &mut HtmlRecipeCrawlReport,
) -> Option<PublicationTopicCompromise> {
    let assessment = publication_topic_compromise(company, &report.items);
    if !assessment.detected {
        return None;
    }

    for item in report.items.drain(..) {
        report.failures.push(ArticleFetchFailure {
            url: item.url,
            reason: "publication_topic_compromise_detected".to_owned(),
            retryable: false,
            error: format!(
                "publication sample is dominated by gambling SEO content unrelated to {} \
                 ({} of {} sampled items)",
                company.name, assessment.suspicious_item_count, assessment.sample_item_count
            ),
        });
    }
    recompute_recipe_report_correctness(spec, report);
    if !report
        .correctness_reasons
        .iter()
        .any(|reason| reason == "publication_topic_compromise_detected")
    {
        report
            .correctness_reasons
            .push("publication_topic_compromise_detected".to_owned());
    }
    Some(assessment)
}

fn feed_content_diversity(items: &[feed_core::RawCrawlItem]) -> FeedContentDiversity {
    let repeated_content_item_count = repeated_sanitized_content_urls(items).len();
    FeedContentDiversity {
        item_count: items.len(),
        repeated_content_item_count,
        passed: items.len() < FEED_TITLE_DIVERSITY_MIN_SAMPLE
            || repeated_content_item_count.saturating_mul(2) < items.len(),
    }
}

fn feed_title_diversity(items: &[feed_core::RawCrawlItem]) -> FeedTitleDiversity {
    let titled_item_count = items
        .iter()
        .filter_map(|item| item.title.as_deref())
        .filter(|title| !title.trim().is_empty())
        .count();
    let titles = items
        .iter()
        .filter_map(|item| {
            let title = item.title.as_deref()?.trim();
            if title.is_empty() {
                return None;
            }
            let content = item
                .body_html
                .as_deref()
                .or(item.summary_html.as_deref())
                .unwrap_or_default();
            let identity_url = item.canonical_url.as_ref().unwrap_or(&item.url);
            (!is_cms_placeholder_article(title, content)
                && !is_non_editorial_utility_article(title, identity_url))
            .then_some(title)
        })
        .map(|title| {
            title
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        })
        .filter(|title| !title.is_empty())
        .collect::<Vec<_>>();
    let usable_titled_item_count = titles.len();
    let distinct_titled_item_count = titles.into_iter().collect::<HashSet<_>>().len();
    FeedTitleDiversity {
        titled_item_count,
        usable_titled_item_count,
        distinct_titled_item_count,
        passed: usable_titled_item_count < FEED_TITLE_DIVERSITY_MIN_SAMPLE
            || distinct_titled_item_count >= 2,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CandidateValidationPolicy {
    pub auto_activate: bool,
    pub public_export_allowed: bool,
    pub activation_policy: ValidationActivationPolicy,
    pub max_item_age_days: u32,
    pub freshness_slo_seconds: i32,
}

#[derive(Clone)]
pub struct CandidateValidationJobHandler {
    database: Database,
    crawler: RssAtomCrawler,
    policy: CandidateValidationPolicy,
}

impl CandidateValidationJobHandler {
    pub fn new(
        database: Database,
        crawler: RssAtomCrawler,
        policy: CandidateValidationPolicy,
    ) -> Self {
        Self {
            database,
            crawler,
            policy,
        }
    }
}

#[async_trait]
impl JobHandler for CandidateValidationJobHandler {
    fn supported_job_types(&self) -> &[JobType] {
        &[JobType::ValidateCandidate]
    }

    async fn handle(&self, job: &Job) -> Result<(), JobHandlerError> {
        let candidate_id = job.candidate_id.ok_or_else(|| {
            JobHandlerError::permanent("validate_candidate job is missing candidate_id")
        })?;
        let candidate = self
            .database
            .get_source_candidate(candidate_id)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?
            .ok_or_else(|| {
                JobHandlerError::permanent(format!("candidate {candidate_id} does not exist"))
            })?;
        if candidate.status != feed_core::CandidateStatus::New {
            return Ok(());
        }
        let company = self
            .database
            .get_company(candidate.company_id)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?
            .ok_or_else(|| {
                JobHandlerError::permanent(format!(
                    "company {} does not exist",
                    candidate.company_id
                ))
            })?;
        let run_id = self
            .database
            .begin_candidate_validation_run(candidate.id, job.id)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
        let source = validation_source(&candidate);

        match self.crawler.crawl(&source).await {
            Ok(batch) => {
                let item_count = i32::try_from(batch.items.len())
                    .map_err(|_| JobHandlerError::permanent("validation item count exceeds i32"))?;
                let title_diversity = feed_title_diversity(&batch.items);
                let content_diversity = feed_content_diversity(&batch.items);
                let titled_item_count = i32::try_from(title_diversity.titled_item_count)
                    .map_err(|_| JobHandlerError::permanent("titled item count exceeds i32"))?;
                let usable_titled_item_count =
                    i32::try_from(title_diversity.usable_titled_item_count).map_err(|_| {
                        JobHandlerError::permanent("usable titled item count exceeds i32")
                    })?;
                let distinct_titled_item_count =
                    i32::try_from(title_diversity.distinct_titled_item_count).map_err(|_| {
                        JobHandlerError::permanent("distinct titled item count exceeds i32")
                    })?;
                let latest_item_at = batch
                    .items
                    .iter()
                    .filter_map(|item| item.published_at)
                    .max();
                let final_url = batch
                    .metadata
                    .get("feed_url")
                    .and_then(Value::as_str)
                    .and_then(|value| Url::parse(value).ok())
                    .or_else(|| Some(candidate.candidate_url.clone()));
                let publication_host_excluded =
                    publication_host_is_excluded(&company, &candidate.candidate_url)
                        || final_url
                            .as_ref()
                            .is_some_and(|url| publication_host_is_excluded(&company, url));
                let approved_source_company_claims = self
                    .database
                    .list_approved_feed_source_company_claims(
                        final_url.as_ref().unwrap_or(&candidate.candidate_url),
                    )
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                let distinct_company_source_claim = distinct_company_approved_source_claim(
                    company.id,
                    &company.name,
                    &approved_source_company_claims,
                )
                .cloned();
                let ownership_reason =
                    official_ownership_reason(&company, &candidate, final_url.as_ref());
                let adapter_recommended =
                    evidence_has_web_adapter_recommendation(&candidate.evidence);
                let sitemap_source = is_sitemap_url(&candidate.candidate_url)
                    || final_url.as_ref().is_some_and(is_sitemap_url);
                let non_editorial_item_scope =
                    feed_scope_is_non_editorial(&candidate.candidate_url, &batch);
                let topic_compromise = publication_topic_compromise(&company, &batch.items);
                let company_scope_relevance = feed_company_scope_relevance(
                    &company,
                    final_url.as_ref().unwrap_or(&candidate.candidate_url),
                    batch.metadata.get("feed_title").and_then(Value::as_str),
                    &batch.items,
                );
                let candidate_item_urls = batch
                    .items
                    .iter()
                    .map(raw_crawl_item_identity_url)
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                let approved_feed_company_claims = self
                    .database
                    .list_approved_feed_item_company_claims(&candidate_item_urls)
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                let distinct_company_feed_item_claim = distinct_company_approved_feed_claim(
                    company.id,
                    &company.name,
                    candidate_item_urls.len(),
                    &approved_feed_company_claims,
                )
                .cloned();
                let candidate_signature_candidates = batch
                    .items
                    .iter()
                    .filter_map(raw_crawl_item_signature_candidate)
                    .collect::<Vec<_>>();
                let approved_feed_signature_matches = self
                    .database
                    .list_approved_feed_item_signature_matches(
                        company.id,
                        &candidate_signature_candidates,
                    )
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                    .into_iter()
                    .collect::<HashSet<_>>();
                let mut approved_feed_matches = self
                    .database
                    .list_approved_feed_item_url_matches(company.id, &candidate_item_urls)
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                    .into_iter()
                    .collect::<HashSet<_>>();
                approved_feed_matches.extend(approved_feed_signature_matches.iter().cloned());
                let approved_feed_overlap_count = approved_feed_matches.len();
                let redundant_with_approved_feed = feed_candidate_fully_covered_by_approved_feed(
                    candidate_item_urls.len(),
                    approved_feed_overlap_count,
                );
                let editorial = !sitemap_source && has_editorial_evidence(&candidate, &batch);
                let risky_scope = has_risky_feed_scope(&candidate, &batch);
                let preferred_locale = has_preferred_locale(&candidate.candidate_url);
                let freshness_cutoff =
                    Utc::now() - Duration::days(i64::from(self.policy.max_item_age_days));
                let fresh_enough = latest_item_at
                    .map(|published_at| published_at >= freshness_cutoff)
                    .unwrap_or(true);
                let mut policy_reasons = Vec::new();
                if let Some(reason) = ownership_reason {
                    policy_reasons.push(reason.to_owned());
                } else {
                    policy_reasons.push("official_ownership_needs_review".to_owned());
                }
                if publication_host_excluded {
                    policy_reasons.push("publication_host_excluded_by_company_policy".to_owned());
                }
                if adapter_recommended {
                    policy_reasons.push("trusted_web_adapter_recommendation".to_owned());
                }
                if distinct_company_source_claim.is_some() {
                    policy_reasons.push("feed_claimed_by_distinct_company".to_owned());
                }
                if distinct_company_feed_item_claim.is_some() {
                    policy_reasons.push("feed_items_claimed_by_distinct_company_feed".to_owned());
                }
                if sitemap_source {
                    policy_reasons.push("sitemap_is_not_editorial_feed".to_owned());
                }
                if non_editorial_item_scope {
                    policy_reasons.push("feed_item_scope_is_non_editorial".to_owned());
                } else {
                    policy_reasons.push("feed_item_scope_is_editorial_or_unspecified".to_owned());
                }
                if topic_compromise.detected {
                    policy_reasons.push("publication_topic_compromise_detected".to_owned());
                } else {
                    policy_reasons.push("publication_topic_compromise_not_detected".to_owned());
                }
                if company_scope_relevance.required {
                    if company_scope_relevance.passed {
                        policy_reasons.push("feed_is_company_scoped".to_owned());
                    } else {
                        policy_reasons.push("feed_company_scope_below_minimum".to_owned());
                    }
                }
                if company_scope_relevance.feed_title_corroborated {
                    policy_reasons.push("feed_title_identifies_company".to_owned());
                }
                if redundant_with_approved_feed {
                    policy_reasons.push("feed_items_fully_covered_by_approved_sources".to_owned());
                } else {
                    policy_reasons.push("feed_adds_distinct_items".to_owned());
                }
                if editorial {
                    policy_reasons.push("editorial_feed_evidence".to_owned());
                } else {
                    policy_reasons.push("editorial_scope_needs_review".to_owned());
                }
                if risky_scope {
                    policy_reasons.push("user_or_operational_feed_scope_needs_review".to_owned());
                }
                if preferred_locale {
                    policy_reasons.push("preferred_or_unspecified_locale".to_owned());
                } else {
                    policy_reasons.push("non_english_locale_needs_review".to_owned());
                }
                if fresh_enough {
                    policy_reasons.push("feed_is_current_or_undated".to_owned());
                } else {
                    policy_reasons.push("feed_is_stale".to_owned());
                }
                if item_count > 0 {
                    policy_reasons.push("feed_has_items".to_owned());
                } else {
                    policy_reasons.push("feed_is_empty".to_owned());
                }
                if titled_item_count > 0 {
                    policy_reasons.push("feed_has_titled_items".to_owned());
                } else {
                    policy_reasons.push("feed_has_no_titled_items".to_owned());
                }
                if usable_titled_item_count > 0 {
                    policy_reasons.push("feed_has_usable_article_titles".to_owned());
                } else {
                    policy_reasons.push("feed_has_no_usable_article_titles".to_owned());
                }
                if title_diversity.passed {
                    policy_reasons.push("feed_title_diversity_not_degenerate".to_owned());
                } else {
                    policy_reasons.push("feed_title_diversity_below_minimum".to_owned());
                }
                if content_diversity.passed {
                    policy_reasons.push("feed_content_diversity_not_degenerate".to_owned());
                } else {
                    policy_reasons.push("feed_content_diversity_below_minimum".to_owned());
                }

                let has_usable_items = item_count > 0
                    && usable_titled_item_count > 0
                    && title_diversity.passed
                    && content_diversity.passed;
                let strict_policy_passed = ownership_reason.is_some()
                    && !publication_host_excluded
                    && editorial
                    && !sitemap_source
                    && !non_editorial_item_scope
                    && !topic_compromise.detected
                    && company_scope_relevance.passed
                    && !redundant_with_approved_feed
                    && distinct_company_source_claim.is_none()
                    && distinct_company_feed_item_claim.is_none()
                    && !risky_scope
                    && preferred_locale
                    && fresh_enough
                    && has_usable_items;
                let trusted_adapter_passed = self.policy.activation_policy
                    == ValidationActivationPolicy::TrustedAdapter
                    && adapter_recommended
                    && !publication_host_excluded
                    && !sitemap_source
                    && !non_editorial_item_scope
                    && !topic_compromise.detected
                    && company_scope_relevance.passed
                    && !redundant_with_approved_feed
                    && distinct_company_source_claim.is_none()
                    && distinct_company_feed_item_claim.is_none()
                    && has_usable_items;
                let policy_passed = strict_policy_passed || trusted_adapter_passed;
                let provisional = trusted_adapter_passed && !strict_policy_passed;
                let validation_status = if publication_host_excluded
                    || sitemap_source
                    || non_editorial_item_scope
                    || topic_compromise.detected
                    || !company_scope_relevance.passed
                    || redundant_with_approved_feed
                    || distinct_company_source_claim.is_some()
                    || distinct_company_feed_item_claim.is_some()
                {
                    CandidateValidationStatus::Invalid
                } else if policy_passed {
                    CandidateValidationStatus::Valid
                } else if !has_usable_items {
                    CandidateValidationStatus::Invalid
                } else {
                    CandidateValidationStatus::NeedsReview
                };
                let sample_items = batch
                    .items
                    .iter()
                    .take(5)
                    .map(|item| {
                        json!({
                            "title": item.title,
                            "url": item.url,
                            "published_at": item.published_at,
                        })
                    })
                    .collect::<Vec<_>>();
                let topic_compromise_metadata = json!({
                    "detected": topic_compromise.detected,
                    "sample_item_count": topic_compromise.sample_item_count,
                    "suspicious_item_count": topic_compromise.suspicious_item_count,
                    "policy": "publication-topic-compromise.v1",
                });
                self.database
                    .complete_candidate_validation_run(
                        run_id,
                        &CandidateValidationCompletion {
                            status: validation_status,
                            detected_kind: Some(batch.detected_source_kind),
                            final_url,
                            http_status: Some(200),
                            item_count,
                            titled_item_count,
                            latest_item_at,
                            policy_reasons: policy_reasons.clone(),
                            error: None,
                            metadata: json!({
                                "feed": batch.metadata,
                                "sample_items": sample_items,
                                "auto_activation_enabled": self.policy.auto_activate,
                                "policy": {
                                    "activation_policy": self.policy.activation_policy.as_str(),
                                    "ownership_reason": ownership_reason,
                                    "publication_host_excluded":
                                        publication_host_excluded,
                                    "adapter_recommended": adapter_recommended,
                                    "sitemap_source": sitemap_source,
                                    "non_editorial_item_scope": non_editorial_item_scope,
                                    "publication_topic_compromise":
                                        topic_compromise_metadata,
                                    "company_scope_required": company_scope_relevance.required,
                                    "company_scope_relevant_item_count":
                                        company_scope_relevance.relevant_item_count,
                                    "company_scope_total_item_count":
                                        company_scope_relevance.total_item_count,
                                    "company_scope_relevance_ratio_bps":
                                        company_scope_relevance.relevance_ratio_bps,
                                    "company_scope_passed": company_scope_relevance.passed,
                                    "company_scope_feed_title_corroborated":
                                        company_scope_relevance.feed_title_corroborated,
                                    "company_scope_off_company_host_item_count":
                                        company_scope_relevance.off_company_host_item_count,
                                    "company_scope_off_company_host_ratio_bps":
                                        company_scope_relevance.off_company_host_ratio_bps,
                                    "approved_feed_overlap_count":
                                        approved_feed_overlap_count,
                                    "approved_feed_signature_overlap_count":
                                        approved_feed_signature_matches.len(),
                                    "candidate_identity_url_count":
                                        candidate_item_urls.len(),
                                    "approved_feed_overlap_ratio_bps": ratio_bps(
                                        approved_feed_overlap_count,
                                        candidate_item_urls.len(),
                                    ),
                                    "redundant_with_approved_feed":
                                        redundant_with_approved_feed,
                                    "distinct_company_source_claim":
                                        distinct_company_source_claim.as_ref(),
                                    "distinct_company_feed_item_claim":
                                        distinct_company_feed_item_claim.as_ref(),
                                    "editorial_evidence": editorial,
                                    "risky_scope": risky_scope,
                                    "preferred_locale": preferred_locale,
                                    "fresh_enough": fresh_enough,
                                    "has_usable_items": has_usable_items,
                                    "usable_titled_item_count": usable_titled_item_count,
                                    "distinct_titled_item_count": distinct_titled_item_count,
                                    "title_diversity_passed": title_diversity.passed,
                                    "repeated_content_item_count":
                                        content_diversity.repeated_content_item_count,
                                    "content_diversity_passed": content_diversity.passed,
                                    "strict_policy_passed": strict_policy_passed,
                                    "trusted_adapter_passed": trusted_adapter_passed,
                                    "provisional": provisional,
                                    "max_item_age_days": self.policy.max_item_age_days,
                                },
                            }),
                        },
                    )
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?;

                if policy_passed && self.policy.auto_activate {
                    let source_id = default_source_id(
                        &company.company_key,
                        batch.detected_source_kind,
                        candidate.id,
                    );
                    let activated = self
                        .database
                        .accept_source_candidate_with_decision(
                            candidate.id,
                            &source_id,
                            self.policy.freshness_slo_seconds,
                            self.policy.public_export_allowed,
                            CandidateDecisionMode::Automatic,
                            "feed-validation-worker",
                            if provisional {
                                "trusted web-adapter recommendation has usable RSS/Atom content"
                            } else {
                                "deterministic official RSS/Atom validation policy passed"
                            },
                            json!({
                                "validation_run_id": run_id,
                                "policy_reasons": policy_reasons,
                                "activation_policy": self.policy.activation_policy.as_str(),
                                "adapter_recommended": adapter_recommended,
                                "publication_host_excluded":
                                    publication_host_excluded,
                                "publication_topic_compromise":
                                    topic_compromise_metadata,
                                "strict_policy_passed": strict_policy_passed,
                                "provisional": provisional,
                                "company_scope_required": company_scope_relevance.required,
                                "company_scope_relevant_item_count":
                                    company_scope_relevance.relevant_item_count,
                                "company_scope_total_item_count":
                                    company_scope_relevance.total_item_count,
                                "company_scope_relevance_ratio_bps":
                                    company_scope_relevance.relevance_ratio_bps,
                                "company_scope_feed_title_corroborated":
                                    company_scope_relevance.feed_title_corroborated,
                                "company_scope_off_company_host_item_count":
                                    company_scope_relevance.off_company_host_item_count,
                                "company_scope_off_company_host_ratio_bps":
                                    company_scope_relevance.off_company_host_ratio_bps,
                                "approved_feed_overlap_count":
                                    approved_feed_overlap_count,
                                "approved_feed_signature_overlap_count":
                                    approved_feed_signature_matches.len(),
                                "candidate_identity_url_count":
                                    candidate_item_urls.len(),
                                "approved_feed_overlap_ratio_bps": ratio_bps(
                                    approved_feed_overlap_count,
                                    candidate_item_urls.len(),
                                ),
                                "redundant_with_approved_feed":
                                    redundant_with_approved_feed,
                                "distinct_company_source_claim":
                                    distinct_company_source_claim.as_ref(),
                                "distinct_company_feed_item_claim":
                                    distinct_company_feed_item_claim.as_ref(),
                                "repeated_content_item_count":
                                    content_diversity.repeated_content_item_count,
                                "content_diversity_passed": content_diversity.passed,
                            }),
                        )
                        .await
                        .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                    let mut crawl_job = JobSpec::new(
                        JobType::CrawlSource,
                        format!("source:{}", activated.id),
                        Utc::now(),
                    );
                    crawl_job.company_id = Some(activated.company_id);
                    crawl_job.source_id = Some(activated.id);
                    crawl_job.priority = i16::MAX / 2;
                    crawl_job.payload = json!({ "source_id": activated.id });
                    self.database
                        .enqueue_job(&crawl_job)
                        .await
                        .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                } else if validation_status == CandidateValidationStatus::Invalid {
                    let reason = if publication_host_excluded {
                        "feed host is explicitly excluded by the company publication policy"
                    } else if sitemap_source {
                        "sitemap resources are URL inventories, not editorial RSS/Atom feeds"
                    } else if non_editorial_item_scope {
                        "feed is dominated by documentation, forum, comment, or operational items"
                    } else if topic_compromise.detected {
                        "feed is dominated by gambling SEO content unrelated to the company profile"
                    } else if !company_scope_relevance.passed {
                        "unrelated or shared feed is not scoped to the candidate company"
                    } else if redundant_with_approved_feed {
                        "feed items are fully covered by existing approved RSS/Atom sources"
                    } else if distinct_company_source_claim.is_some() {
                        "feed URL is already owned by an approved source for a distinct company"
                    } else if distinct_company_feed_item_claim.is_some() {
                        "feed items are already owned by an approved feed for a distinct company"
                    } else if !title_diversity.passed {
                        "feed sample repeats one title across at least five items"
                    } else if !content_diversity.passed {
                        "feed sample reuses one sanitized body across different article titles"
                    } else if usable_titled_item_count == 0 && titled_item_count > 0 {
                        "feed contains only unedited CMS placeholder articles"
                    } else {
                        "feed has no usable titled items"
                    };
                    self.database
                        .reject_source_candidate_with_decision(
                            candidate.id,
                            CandidateDecisionMode::Automatic,
                            "feed-validation-worker",
                            reason,
                            json!({
                                "validation_run_id": run_id,
                                "policy_reasons": policy_reasons,
                                "activation_policy": self.policy.activation_policy.as_str(),
                                "publication_host_excluded":
                                    publication_host_excluded,
                                "sitemap_source": sitemap_source,
                                "non_editorial_item_scope": non_editorial_item_scope,
                                "publication_topic_compromise":
                                    topic_compromise_metadata,
                                "company_scope_required": company_scope_relevance.required,
                                "company_scope_relevant_item_count":
                                    company_scope_relevance.relevant_item_count,
                                "company_scope_total_item_count":
                                    company_scope_relevance.total_item_count,
                                "company_scope_relevance_ratio_bps":
                                    company_scope_relevance.relevance_ratio_bps,
                                "company_scope_passed": company_scope_relevance.passed,
                                "company_scope_off_company_host_item_count":
                                    company_scope_relevance.off_company_host_item_count,
                                "company_scope_off_company_host_ratio_bps":
                                    company_scope_relevance.off_company_host_ratio_bps,
                                "approved_feed_overlap_count":
                                    approved_feed_overlap_count,
                                "approved_feed_signature_overlap_count":
                                    approved_feed_signature_matches.len(),
                                "candidate_identity_url_count":
                                    candidate_item_urls.len(),
                                "approved_feed_overlap_ratio_bps": ratio_bps(
                                    approved_feed_overlap_count,
                                    candidate_item_urls.len(),
                                ),
                                "redundant_with_approved_feed":
                                    redundant_with_approved_feed,
                                "distinct_company_source_claim":
                                    distinct_company_source_claim.as_ref(),
                                "distinct_company_feed_item_claim":
                                    distinct_company_feed_item_claim.as_ref(),
                                "usable_titled_item_count": usable_titled_item_count,
                                "distinct_titled_item_count": distinct_titled_item_count,
                                "title_diversity_passed": title_diversity.passed,
                                "repeated_content_item_count":
                                    content_diversity.repeated_content_item_count,
                                "content_diversity_passed": content_diversity.passed,
                            }),
                        )
                        .await
                        .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                } else {
                    self.database
                        .keep_source_candidate_for_review(
                            candidate.id,
                            CandidateDecisionMode::Automatic,
                            "feed-validation-worker",
                            if policy_passed {
                                "automatic activation is disabled"
                            } else {
                                "candidate requires operator review"
                            },
                            json!({
                                "validation_run_id": run_id,
                                "policy_reasons": policy_reasons,
                            }),
                        )
                        .await
                        .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                }
                Ok(())
            }
            Err(error) => {
                let retryable = validation_error_is_retryable(&error);
                let (http_status, final_url) = validation_error_http(&error);
                let status = if retryable {
                    CandidateValidationStatus::Failed
                } else {
                    CandidateValidationStatus::Invalid
                };
                self.database
                    .complete_candidate_validation_run(
                        run_id,
                        &CandidateValidationCompletion {
                            status,
                            detected_kind: None,
                            final_url,
                            http_status,
                            item_count: 0,
                            titled_item_count: 0,
                            latest_item_at: None,
                            policy_reasons: vec![if retryable {
                                "transient_fetch_or_upstream_failure".to_owned()
                            } else {
                                "not_a_supported_public_feed".to_owned()
                            }],
                            error: Some(error.to_string()),
                            metadata: Value::Object(Default::default()),
                        },
                    )
                    .await
                    .map_err(|database_error| {
                        JobHandlerError::retryable(format!(
                            "{error}; additionally failed to close validation run: {database_error}"
                        ))
                    })?;
                if retryable {
                    Err(JobHandlerError::retryable(error.to_string()))
                } else {
                    self.database
                        .reject_source_candidate_with_decision(
                            candidate.id,
                            CandidateDecisionMode::Automatic,
                            "feed-validation-worker",
                            "candidate is not a supported public RSS/Atom feed",
                            json!({
                                "validation_run_id": run_id,
                                "error": error.to_string(),
                            }),
                        )
                        .await
                        .map_err(|database_error| {
                            JobHandlerError::retryable(database_error.to_string())
                        })?;
                    Ok(())
                }
            }
        }
    }
}

fn validation_source(candidate: &SourceCandidate) -> Source {
    let now = Utc::now();
    Source {
        id: uuid::Uuid::new_v4(),
        source_id: format!("validation-{}", candidate.id),
        company_id: candidate.company_id,
        kind: candidate.candidate_kind,
        url: candidate.candidate_url.clone(),
        status: SourceStatus::Approved,
        freshness_slo_seconds: 3_600,
        browser_required: false,
        public_export_allowed: false,
        discovery_confidence: Some(candidate.confidence),
        metadata: Value::Object(Default::default()),
        created_at: now,
        updated_at: now,
    }
}

fn official_ownership_reason(
    company: &Company,
    candidate: &SourceCandidate,
    final_url: Option<&Url>,
) -> Option<&'static str> {
    let candidate_host = candidate.candidate_url.host_str();
    let final_host = final_url
        .and_then(Url::host_str)
        .or(candidate.candidate_url.host_str());
    if company_publication_host_policy_matches(company, "excluded_hosts", candidate_host)
        || company_publication_host_policy_matches(company, "excluded_hosts", final_host)
    {
        return None;
    }
    let candidate_is_verified =
        company_publication_host_policy_matches(company, "verified_hosts", candidate_host);
    let final_is_verified =
        company_publication_host_policy_matches(company, "verified_hosts", final_host);
    if candidate_is_verified && (final_is_verified || hosts_related(candidate_host, final_host)) {
        return Some("verified_publication_host");
    }
    let candidate_matches_profile = company
        .discovery_entry_points()
        .iter()
        .any(|(_, entry)| hosts_related(candidate_host, entry.host_str()));
    let final_matches_profile = company
        .discovery_entry_points()
        .iter()
        .any(|(_, entry)| hosts_related(final_host, entry.host_str()));
    if candidate_matches_profile
        && (final_matches_profile || hosts_related(candidate_host, final_host))
    {
        return Some("official_profile_host");
    }
    if evidence_links_from_official_host(company, &candidate.evidence) {
        return Some("official_profile_link_evidence");
    }
    if evidence_links_from_company_named_host(company, &candidate.evidence) {
        return Some("company_name_link_evidence");
    }
    if company_identity_matches_host(company, candidate_host)
        && company_identity_matches_host(company, final_host)
    {
        return Some("company_name_matches_feed_host");
    }
    None
}

fn evidence_has_web_adapter_recommendation(evidence: &Value) -> bool {
    evidence
        .get("observations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|observation| {
            observation.get("external_web_adapter").is_some()
                || observation.get("method").and_then(Value::as_str) == Some("external_web_adapter")
        })
}

fn evidence_links_from_official_host(company: &Company, evidence: &Value) -> bool {
    evidence
        .get("observations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|observation| observation.get("found_on").and_then(Value::as_str))
        .filter_map(|value| Url::parse(value).ok())
        .any(|found_on| {
            company
                .discovery_entry_points()
                .iter()
                .any(|(_, entry)| hosts_related(found_on.host_str(), entry.host_str()))
        })
}

fn evidence_links_from_company_named_host(company: &Company, evidence: &Value) -> bool {
    evidence
        .get("observations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|observation| {
            observation.get("method").and_then(Value::as_str) == Some("html_alternate")
        })
        .filter_map(|observation| observation.get("found_on").and_then(Value::as_str))
        .filter_map(|value| Url::parse(value).ok())
        .any(|found_on| company_identity_matches_host(company, found_on.host_str()))
}

fn company_identity_matches_host(company: &Company, host: Option<&str>) -> bool {
    let Some(host) = host else {
        return false;
    };
    std::iter::once(company.name.as_str())
        .chain(company.aliases.iter().map(String::as_str))
        .any(|name| company_identity_name_matches_host(name, host))
}

fn company_identity_name_matches_host(name: &str, host: &str) -> bool {
    let mut host_labels = host
        .split('.')
        .map(compact_ascii_alphanumeric)
        .filter(|label| !label.is_empty())
        .collect::<Vec<_>>();
    // A company name that includes a branding suffix such as ".com" must not
    // match every host under that public suffix. Identity evidence comes from
    // the registrable/subdomain labels, never the terminal DNS label.
    if host_labels.len() > 1 {
        host_labels.pop();
    }
    let words = company_identity_words(name);
    let compact = words.join("");
    let host_normalized_compact = words
        .iter()
        .map(|word| match word.as_str() {
            // "Bancorp" is the legal issuer form while bank brands commonly
            // use "bank" in their owned host (for example U.S. Bancorp on
            // usbank.com). Keep this normalization limited to the combined
            // identity so a generic bank host does not match every bancorp.
            "bancorp" => "bank",
            word => word,
        })
        .collect::<String>();
    let searched_name_compact = compact_ascii_alphanumeric(&company_search_name(name));
    let bancorp_brand_compact = searched_name_compact
        .strip_suffix("bancorp")
        .map(|prefix| format!("{prefix}bank"))
        .filter(|value| value.len() >= 5);
    let brand_tokens = company_brand_tokens(name);
    let brand_acronym = company_brand_acronym(&brand_tokens);
    let brand_legal_acronyms = company_brand_legal_acronyms(name, &brand_tokens);
    let first_word_initials_brand = (brand_tokens.len() >= 3
        && brand_tokens.first().is_some_and(|word| word.len() >= 4))
    .then(|| {
        let mut value = brand_tokens[0].clone();
        value.extend(
            brand_tokens[1..]
                .iter()
                .filter_map(|word| word.chars().next()),
        );
        value
    });
    let short_brand_matches_bounded_host_affix = |brand: &str| {
        (3..=4).contains(&brand.len())
            && host_labels.iter().any(|label| {
                label
                    .strip_prefix(brand)
                    .is_some_and(is_bounded_company_host_suffix)
                    || label
                        .strip_suffix(brand)
                        .is_some_and(is_bounded_company_host_prefix)
            })
    };
    let compact_matches = compact.len() >= 3
        && host_labels.iter().any(|label| {
            label == &compact
                || (compact.len() >= 5
                    && (label.starts_with(&compact) || label.ends_with(&compact)))
        })
        || short_brand_matches_bounded_host_affix(&compact)
        || (host_normalized_compact != compact
            && host_normalized_compact.len() >= 5
            && host_labels.iter().any(|label| {
                label == &host_normalized_compact
                    || label.starts_with(&host_normalized_compact)
                    || label.ends_with(&host_normalized_compact)
            }))
        || bancorp_brand_compact.as_ref().is_some_and(|brand| {
            host_labels
                .iter()
                .any(|label| label == brand || label.starts_with(brand) || label.ends_with(brand))
        })
        || brand_acronym.as_ref().is_some_and(|acronym| {
            host_labels.iter().any(|label| label == acronym)
                || short_brand_matches_bounded_host_affix(acronym)
        })
        || brand_tokens
            .first()
            .is_some_and(|brand| short_brand_matches_bounded_host_affix(brand))
        || brand_legal_acronyms
            .iter()
            .any(|acronym| host_labels.iter().any(|label| label == acronym))
        || first_word_initials_brand
            .as_ref()
            .is_some_and(|brand| host_labels.iter().any(|label| label == brand));
    compact_matches
        || words
            .iter()
            .filter(|word| word.len() >= 3)
            .any(|word| host_labels.iter().any(|label| label == word))
}

fn is_bounded_company_host_suffix(suffix: &str) -> bool {
    matches!(
        suffix,
        "bank"
            | "bio"
            | "capital"
            | "corp"
            | "group"
            | "health"
            | "holdings"
            | "labs"
            | "online"
            | "properties"
            | "systems"
            | "tech"
            | "trucks"
    )
}

fn is_bounded_company_host_prefix(prefix: &str) -> bool {
    matches!(prefix, "get" | "go" | "join" | "my" | "try" | "use")
}

fn company_identity_words(name: &str) -> Vec<String> {
    name.split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|word| word.len() >= 2 && !is_company_name_stop_word(word))
        .collect()
}

fn company_brand_tokens(name: &str) -> Vec<String> {
    let mut tokens = company_search_name(name)
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|word| !word.is_empty() && !is_company_brand_suffix_or_connector_word(word))
        .collect::<Vec<_>>();
    for suffix in [
        ["p", "l", "c"].as_slice(),
        ["l", "l", "c"].as_slice(),
        ["s", "a"].as_slice(),
        ["n", "v"].as_slice(),
    ] {
        if tokens.len() >= suffix.len()
            && tokens[tokens.len() - suffix.len()..]
                .iter()
                .map(String::as_str)
                .eq(suffix.iter().copied())
        {
            tokens.truncate(tokens.len() - suffix.len());
            break;
        }
    }
    tokens
}

fn company_brand_acronym(tokens: &[String]) -> Option<String> {
    (tokens.len() >= 3)
        .then(|| {
            tokens
                .iter()
                .filter_map(|word| word.chars().next())
                .collect::<String>()
        })
        .filter(|value| value.len() >= 3)
}

fn company_brand_legal_acronyms(name: &str, tokens: &[String]) -> Vec<String> {
    if tokens.len() < 2 {
        return Vec::new();
    }
    let Some((legal_initial, legal_suffix)) = name
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .find_map(|word| match word.as_str() {
            "co" | "company" => Some(('c', "co")),
            "corp" | "corporation" => Some(('c', "corp")),
            "inc" | "incorporated" => Some(('i', "inc")),
            "limited" | "ltd" => Some(('l', "ltd")),
            "plc" => Some(('p', "plc")),
            _ => None,
        })
    else {
        return Vec::new();
    };
    let base = tokens
        .iter()
        .filter_map(|word| word.chars().next())
        .collect::<String>();
    let initial_form = format!("{base}{legal_initial}");
    let full_form = format!("{base}{legal_suffix}");
    [initial_form, full_form]
        .into_iter()
        .filter(|value| value.len() >= 3)
        .collect()
}

fn is_company_brand_suffix_or_connector_word(word: &str) -> bool {
    matches!(
        word,
        "ag" | "and"
            | "co"
            | "company"
            | "corp"
            | "corporation"
            | "inc"
            | "incorporated"
            | "limited"
            | "llc"
            | "lp"
            | "ltd"
            | "nv"
            | "plc"
            | "railroad"
            | "railway"
            | "reit"
            | "sa"
            | "se"
            | "the"
    )
}

fn compact_ascii_alphanumeric(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_company_name_stop_word(word: &str) -> bool {
    matches!(
        word,
        "ag" | "and"
            | "class"
            | "co"
            | "common"
            | "company"
            | "corp"
            | "corporation"
            | "group"
            | "holding"
            | "holdings"
            | "inc"
            | "incorporated"
            | "international"
            | "limited"
            | "llc"
            | "lp"
            | "ltd"
            | "motor"
            | "nv"
            | "ordinary"
            | "of"
            | "plc"
            | "reit"
            | "sa"
            | "se"
            | "share"
            | "shares"
            | "stock"
            | "systems"
            | "technologies"
            | "technology"
            | "the"
    )
}

fn has_editorial_evidence(candidate: &SourceCandidate, batch: &CrawlBatch) -> bool {
    evidence_has_editorial_role(&candidate.evidence)
        || text_has_editorial_marker(candidate.candidate_url.as_str())
        || batch
            .metadata
            .get("feed_title")
            .and_then(Value::as_str)
            .is_some_and(text_has_editorial_marker)
        || batch
            .items
            .iter()
            .take(20)
            .any(|item| text_has_editorial_marker(item.url.as_str()))
}

fn evidence_has_editorial_role(evidence: &Value) -> bool {
    evidence
        .get("observations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|observation| observation.get("external_web_adapter"))
        .filter_map(|adapter| adapter.get("roles"))
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_str)
        .any(|role| {
            matches!(
                role,
                "corporate_blog"
                    | "engineering_blog"
                    | "investor_relations"
                    | "newsroom"
                    | "press_releases"
            )
        })
}

fn text_has_editorial_marker(value: &str) -> bool {
    let tokens = value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "blog"
                | "blogs"
                | "developer"
                | "developers"
                | "devblog"
                | "devblogs"
                | "engineering"
                | "insight"
                | "insights"
                | "media"
                | "news"
                | "newsletter"
                | "newsroom"
                | "press"
                | "pressroom"
                | "research"
                | "stories"
                | "updates"
        ) || token.ends_with("blog")
            || token.ends_with("news")
    }) || (tokens.iter().any(|token| token == "security")
        && tokens.iter().any(|token| token == "lab" || token == "labs"))
}

fn has_risky_feed_scope(candidate: &SourceCandidate, batch: &CrawlBatch) -> bool {
    text_has_risky_scope_marker(candidate.candidate_url.as_str())
        || batch
            .metadata
            .get("feed_title")
            .and_then(Value::as_str)
            .is_some_and(text_has_risky_scope_marker)
}

fn deterministic_feed_quality_failure(error: impl Into<String>) -> JobHandlerError {
    JobHandlerError::permanent(error)
}

fn feed_scope_is_non_editorial(feed_url: &Url, batch: &CrawlBatch) -> bool {
    let feed_url = feed_url.as_str().to_ascii_lowercase();
    if feed_url.contains("/boardmessages")
        || feed_url.contains("/discuss/")
        || feed_url.contains("/feed/topics")
        || feed_url.contains("/trust/alerts/feed")
    {
        return true;
    }

    let sample = batch.items.iter().take(20).collect::<Vec<_>>();
    if sample.len() < 5 {
        return false;
    }
    let non_editorial = sample
        .iter()
        .filter(|item| feed_item_is_non_editorial(item))
        .count();
    non_editorial.saturating_mul(5) >= sample.len().saturating_mul(4)
}

fn feed_item_is_non_editorial(item: &feed_core::RawCrawlItem) -> bool {
    let title = item
        .title
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if title.starts_with("forum post:") {
        return true;
    }

    let identity_url = item.canonical_url.as_ref().unwrap_or(&item.url);
    if is_non_editorial_utility_article(&title, identity_url) {
        return true;
    }
    let url = identity_url.as_str().to_ascii_lowercase();
    if url.contains("/cadence_technology_forums/")
        || url.contains("/discuss/t/")
        || url.contains("/forum/")
        || url.contains("/forums/")
        || url.contains("/bc-p/")
    {
        return true;
    }
    let documentation_path =
        url.contains("/docs/") || url.contains("/documentation/") || url.contains("/reference/");
    documentation_path
        && !url.contains("pressrelease")
        && !url.contains("press-release")
        && !url.contains("/news/")
        && !url.contains("/blog/")
}

fn text_has_risky_scope_marker(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    normalized.starts_with("life at ")
        || normalized
            .split(|character: char| !character.is_ascii_alphanumeric())
            .map(str::to_ascii_lowercase)
            .any(|token| {
                matches!(
                    token.as_str(),
                    "alert"
                        | "alerts"
                        | "comment"
                        | "comments"
                        | "community"
                        | "career"
                        | "careers"
                        | "forum"
                        | "forums"
                        | "job"
                        | "jobs"
                        | "notification"
                        | "notifications"
                        | "oembed"
                        | "resource"
                        | "resources"
                        | "status"
                        | "support"
                )
            })
}

fn has_preferred_locale(url: &Url) -> bool {
    let locale = url
        .path_segments()
        .into_iter()
        .flatten()
        .take(3)
        .find_map(|segment| {
            let normalized = segment.to_ascii_lowercase().replace('-', "_");
            is_locale_path_segment(&normalized).then_some(normalized)
        });
    locale.is_none_or(|locale| matches!(locale.as_str(), "en" | "en_gb" | "en_us"))
}

fn is_locale_path_segment(segment: &str) -> bool {
    matches!(
        segment,
        "ar" | "bg"
            | "cs"
            | "da"
            | "de"
            | "el"
            | "en"
            | "en_gb"
            | "en_us"
            | "es"
            | "es_es"
            | "fi"
            | "fr"
            | "he"
            | "hi"
            | "hu"
            | "id"
            | "it"
            | "ja"
            | "ko"
            | "nl"
            | "no"
            | "pl"
            | "pt"
            | "pt_br"
            | "pt_pt"
            | "ro"
            | "ru"
            | "sk"
            | "sv"
            | "th"
            | "tr"
            | "uk"
            | "vi"
            | "zh"
            | "zh_cn"
            | "zh_tw"
    )
}

fn hosts_related(left: Option<&str>, right: Option<&str>) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    let left = left.trim_start_matches("www.").to_ascii_lowercase();
    let right = right.trim_start_matches("www.").to_ascii_lowercase();
    left == right || left.ends_with(&format!(".{right}")) || right.ends_with(&format!(".{left}"))
}

fn default_source_id(company_key: &str, kind: SourceKind, candidate_id: uuid::Uuid) -> String {
    let suffix = candidate_id.simple().to_string();
    format!("{company_key}-{}-{}", kind.as_str(), &suffix[..12])
}

fn validation_error_is_retryable(error: &CrawlError) -> bool {
    match error {
        CrawlError::Client(_) | CrawlError::Request { .. } => true,
        CrawlError::HttpStatus { status, .. } => *status == 408 || *status == 429 || *status >= 500,
        CrawlError::ResponseTooLarge { .. }
        | CrawlError::InvalidConfig(_)
        | CrawlError::UnsupportedSourceKind(_)
        | CrawlError::UnsupportedUrl(_)
        | CrawlError::InvalidFeed(_)
        | CrawlError::ItemMissingUrl
        | CrawlError::Serialize(_) => false,
    }
}

fn validation_error_http(error: &CrawlError) -> (Option<i32>, Option<Url>) {
    match error {
        CrawlError::HttpStatus { url, status } => (Some(i32::from(*status)), Some(url.clone())),
        CrawlError::Request { url, .. }
        | CrawlError::ResponseTooLarge { url, .. }
        | CrawlError::UnsupportedUrl(url) => (None, Some(url.clone())),
        CrawlError::InvalidConfig(_)
        | CrawlError::Client(_)
        | CrawlError::UnsupportedSourceKind(_)
        | CrawlError::InvalidFeed(_)
        | CrawlError::ItemMissingUrl
        | CrawlError::Serialize(_) => (None, None),
    }
}

const COMPANY_NEWS_JOB_SCHEMA_VERSION: &str = "company-news-extraction-job.v1";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompanyNewsJobPayload {
    schema_version: String,
    window_start: chrono::DateTime<Utc>,
    window_end: chrono::DateTime<Utc>,
    max_articles: u32,
    #[serde(default)]
    include_covered: bool,
}

fn recipe_activation_crawl_job(
    company_id: uuid::Uuid,
    source_id: uuid::Uuid,
    recipe_id: uuid::Uuid,
) -> JobSpec {
    let mut job = JobSpec::new(
        JobType::CrawlSource,
        format!("source:{source_id}"),
        Utc::now(),
    );
    job.company_id = Some(company_id);
    job.source_id = Some(source_id);
    job.priority = i16::MAX / 2;
    job.payload = json!({
        "source_id": source_id,
        "recipe_id": recipe_id,
        "trigger": "recipe_activation",
    });
    job
}

#[derive(Clone)]
pub struct CompanyNewsExtractionJobHandler {
    database: Database,
    adapter: CompanyNewsExtractionAdapterClient,
    crawler: HtmlArticleCrawler,
    recipe_crawler: HtmlRecipeCrawler,
    max_articles: usize,
    freshness_slo_seconds: i32,
    public_export_allowed: bool,
}

impl CompanyNewsExtractionJobHandler {
    pub fn new(
        database: Database,
        adapter: CompanyNewsExtractionAdapterClient,
        crawler: HtmlArticleCrawler,
        recipe_crawler: HtmlRecipeCrawler,
        max_articles: usize,
        freshness_slo_seconds: i32,
        public_export_allowed: bool,
    ) -> Self {
        Self {
            database,
            adapter,
            crawler,
            recipe_crawler,
            max_articles,
            freshness_slo_seconds,
            public_export_allowed,
        }
    }

    async fn supersede_redundant_rebuilds(
        &self,
        recipe_ids: &[uuid::Uuid],
        reason: &str,
        metadata: Value,
    ) -> Result<usize, JobHandlerError> {
        let mut superseded = 0_usize;
        for recipe_id in recipe_ids {
            superseded += usize::from(
                self.database
                    .supersede_active_company_news_recipe(*recipe_id, reason, metadata.clone())
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?,
            );
        }
        Ok(superseded)
    }
}

#[async_trait]
impl JobHandler for CompanyNewsExtractionJobHandler {
    fn supported_job_types(&self) -> &[JobType] {
        &[JobType::ExtractCompanyNews]
    }

    async fn handle(&self, job: &Job) -> Result<(), JobHandlerError> {
        let company_id = job.company_id.ok_or_else(|| {
            JobHandlerError::permanent("extract_company_news job is missing company_id")
        })?;
        let payload: CompanyNewsJobPayload =
            serde_json::from_value(job.payload.clone()).map_err(|error| {
                JobHandlerError::permanent(format!(
                    "extract_company_news job has invalid payload: {error}"
                ))
            })?;
        validate_company_news_job_payload(&payload, self.max_articles)?;
        let company = self
            .database
            .get_company(company_id)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?
            .ok_or_else(|| {
                JobHandlerError::permanent(format!("company {company_id} does not exist"))
            })?;
        let run_id = self
            .database
            .begin_company_news_extraction_run(
                company_id,
                job.id,
                payload.window_start,
                payload.window_end,
            )
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?;

        if !payload.include_covered
            && self
                .database
                .company_has_healthy_approved_feed(company_id)
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()))?
        {
            self.database
                .complete_company_news_extraction_run(
                    run_id,
                    company_id,
                    &CompanyNewsExtractionCompletion {
                        suggested_url_count: 0,
                        accepted_url_count: 0,
                        rejected_url_count: 0,
                        source_count: 0,
                        normalized_item_count: 0,
                        new_item_count: 0,
                        metadata: json!({
                            "outcome": "skipped_healthy_approved_feed_available",
                            "content_contract": "generic-public-article.v1",
                        }),
                    },
                )
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
            return Ok(());
        }

        let source_candidates = self
            .database
            .list_source_candidates(Some(company_id), None, 100, 0)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
        let mut known_urls = company
            .discovery_entry_points()
            .into_iter()
            .map(|(_, url)| url.clone())
            .chain(
                source_candidates
                    .iter()
                    .filter(|candidate| {
                        matches!(
                            candidate.candidate_kind,
                            SourceKind::Html | SourceKind::Browser
                        )
                    })
                    .map(|candidate| candidate.candidate_url.clone()),
            )
            .collect::<Vec<_>>();
        let mut seen_known_urls = HashSet::new();
        known_urls.retain(|url| seen_known_urls.insert(url.as_str().to_owned()));
        known_urls.truncate(100);
        let adapter_company_name = company_search_name(&company.name);
        let mut adapter_aliases = company.aliases.clone();
        let ambiguous_aliases = self
            .database
            .list_aliases_colliding_with_company_names(company_id, &adapter_aliases)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
        if !ambiguous_aliases.is_empty() {
            let ambiguous_keys = ambiguous_aliases
                .iter()
                .map(|alias| alias.trim().to_ascii_lowercase())
                .collect::<HashSet<_>>();
            adapter_aliases
                .retain(|alias| !ambiguous_keys.contains(&alias.trim().to_ascii_lowercase()));
            info!(
                %company_id,
                aliases = ?ambiguous_aliases,
                "excluded aliases that collide with another active company name"
            );
        }
        if adapter_company_name != company.name
            && !adapter_aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(&company.name))
        {
            adapter_aliases.push(company.name.clone());
        }
        let request = CompanyNewsExtractionRequest::new(
            // The adapter request is idempotent within one extraction attempt,
            // not across every retry of the durable job. Inputs such as
            // `known_urls` may legitimately change between attempts as other
            // discovery work lands. Reusing the job UUID in that case makes a
            // conforming adapter reject the changed payload with HTTP 409.
            run_id,
            WebDiscoveryCompany {
                company_id,
                name: adapter_company_name,
                aliases: adapter_aliases,
                known_urls,
                sector: metadata_string(&company.metadata, "sector"),
                industry: metadata_string(&company.metadata, "industry"),
            },
            payload.window_start,
            payload.window_end,
            payload.max_articles,
        );
        let adapter_response = match self.adapter.extract_news(&request).await {
            Ok(response) => response,
            Err(error) => {
                let safe_error = error.safe_summary();
                self.database
                    .fail_company_news_extraction_run(
                        run_id,
                        company_id,
                        &safe_error,
                        json!({
                            "stage": "url_suggestion",
                            "retryable": error.is_retryable(),
                        }),
                    )
                    .await
                    .map_err(|database_error| {
                        JobHandlerError::retryable(format!(
                            "{safe_error}; additionally failed to close extraction run: {database_error}"
                        ))
                    })?;
                return Err(classify_web_adapter_error(error));
            }
        };
        let public_discovery_seeds = company_news_publication_discovery_seeds(&adapter_response);
        let suggested_url_count = i32::try_from(adapter_response.articles.len())
            .map_err(|_| JobHandlerError::permanent("suggested URL count exceeds i32"))?;
        let existing_recipes = self
            .database
            .list_company_news_recipes(Some(company_id), None, 1_000, 0)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
        let publication_fallback_available = adapter_response
            .publications
            .iter()
            .any(|publication| likely_company_news_publication(&publication.url))
            || source_candidates.iter().any(|candidate| {
                matches!(
                    candidate.candidate_kind,
                    SourceKind::Html | SourceKind::Browser
                ) && likely_editorial_listing(&candidate.candidate_url)
            })
            || existing_recipes
                .iter()
                .any(|recipe| existing_recipe_is_build_input(recipe, payload.include_covered))
            || adapter_response
                .articles
                .iter()
                .filter_map(|article| infer_publication_url(&article.url))
                .any(|publication| likely_company_news_publication(&publication));
        if adapter_response.articles.is_empty() {
            warn!(
                %company_id,
                company_name = %company.name,
                "manual company news import adapter returned no URLs"
            );
        }
        let urls = adapter_response
            .articles
            .iter()
            .map(|article| article.url.clone())
            .collect::<Vec<_>>();
        let mut report = match self.crawler.crawl_urls(&urls).await {
            Ok(report) => report,
            Err(error) => {
                let retryable = classify_article_crawler_error(&error);
                let message = error.to_string();
                self.database
                    .fail_company_news_extraction_run(
                        run_id,
                        company_id,
                        &message,
                        json!({
                            "stage": "public_page_fetch",
                            "retryable": retryable,
                        }),
                    )
                    .await
                    .map_err(|database_error| {
                        JobHandlerError::retryable(format!(
                            "{message}; additionally failed to close extraction run: {database_error}"
                        ))
                    })?;
                return Err(if retryable {
                    JobHandlerError::retryable(message)
                } else {
                    JobHandlerError::permanent(message)
                });
            }
        };

        let mut in_window_items = Vec::new();
        for item in report.items.drain(..) {
            if item.published_at.is_some_and(|published_at| {
                published_at < payload.window_start || published_at > payload.window_end
            }) {
                report.failures.push(ArticleFetchFailure {
                    url: item.url,
                    reason: "outside_requested_window".to_owned(),
                    retryable: false,
                    error: "page publication timestamp is outside the requested window".to_owned(),
                });
            } else {
                in_window_items.push(item);
            }
        }
        report.items = in_window_items;
        let direct_company_scope_rejected_count =
            apply_direct_article_company_scope_filter(&company, &mut report);
        let direct_topic_compromise_rejected_count =
            apply_direct_publication_topic_compromise_filter(&company, &mut report);
        let continued_after_transient_evidence_failure = report.items.is_empty()
            && report.failures.iter().any(|failure| failure.retryable)
            && publication_fallback_available;
        if report.items.is_empty()
            && report.failures.iter().any(|failure| failure.retryable)
            && !publication_fallback_available
        {
            let error = "all suggested article pages failed with at least one transient error";
            self.database
                .fail_company_news_extraction_run(
                    run_id,
                    company_id,
                    error,
                    json!({
                        "stage": "public_page_fetch",
                        "suggested_url_count": suggested_url_count,
                        "failures": report.failures,
                    }),
                )
                .await
                .map_err(|database_error| JobHandlerError::retryable(database_error.to_string()))?;
            return Err(JobHandlerError::retryable(error));
        }
        if continued_after_transient_evidence_failure {
            warn!(
                %company_id,
                company_name = %company.name,
                "all suggested article pages were transiently unavailable; continuing with independent publication validation"
            );
        }

        let evidence_item_urls = report
            .items
            .iter()
            .map(raw_crawl_item_identity_url)
            .collect::<Vec<_>>();
        let evidence_signature_candidates = report
            .items
            .iter()
            .filter_map(raw_crawl_item_signature_candidate)
            .collect::<Vec<_>>();
        let approved_feed_signature_overlap_urls = self
            .database
            .list_approved_feed_item_signature_matches(company_id, &evidence_signature_candidates)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?
            .into_iter()
            .collect::<HashSet<_>>();
        let mut approved_feed_overlap_urls = self
            .database
            .list_approved_feed_item_url_matches(company_id, &evidence_item_urls)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?
            .into_iter()
            .collect::<HashSet<_>>();
        approved_feed_overlap_urls.extend(approved_feed_signature_overlap_urls.iter().cloned());
        report.items.retain(|item| {
            !approved_feed_overlap_urls.contains(&raw_crawl_item_identity_url(item))
        });

        let mut grouped = BTreeMap::<String, (Url, Vec<feed_core::RawCrawlItem>)>::new();
        for item in report.items {
            let origin = article_origin(&item.url).map_err(JobHandlerError::permanent)?;
            grouped
                .entry(origin.as_str().to_owned())
                .or_insert_with(|| (origin, Vec::new()))
                .1
                .push(item);
        }
        let accepted_url_count = grouped
            .values()
            .map(|(_, items)| items.len())
            .sum::<usize>();
        let rejected_url_count = report.failures.len();
        let content_metrics =
            company_news_content_metrics(grouped.values().flat_map(|(_, items)| items.iter()));
        let mut source_count = 0_i32;
        let mut normalized_item_count = 0_i32;
        let mut new_item_count = 0_i32;
        let mut skipped_origins = Vec::new();

        for (_, (origin, raw_items)) in grouped {
            let source_key = company_news_import_source_key(&company.company_key, &origin);
            let source = self
                .database
                .get_or_create_company_news_source(
                    company_id,
                    &source_key,
                    &origin,
                    self.freshness_slo_seconds,
                    self.public_export_allowed,
                    run_id,
                )
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
            if source.status != SourceStatus::Approved
                || !matches!(source.kind, SourceKind::Html | SourceKind::Browser)
            {
                skipped_origins.push(json!({
                    "origin": origin,
                    "source_id": source.id,
                    "status": source.status,
                    "kind": source.kind,
                    "item_count": raw_items.len(),
                    "reason": "source_disabled_or_incompatible",
                }));
                continue;
            }
            source_count += 1;
            let crawl_run_id = self
                .database
                .begin_crawl_run(source.id, job.id)
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
            let mut detected_source = source.clone();
            detected_source.kind = SourceKind::Html;
            let processed = raw_items
                .into_iter()
                .map(|raw| ProcessedCrawlItem {
                    normalized: normalize_item(&detected_source, &raw, report.fetched_at)
                        .map_err(|error| error.to_string()),
                    raw,
                })
                .collect::<Vec<_>>();
            match self
                .database
                .complete_crawl_run(
                    crawl_run_id,
                    &detected_source,
                    report.fetched_at,
                    &processed,
                    json!({
                        "ingestion_mode": "manual_company_news_import",
                        "company_news_extraction_run_id": run_id,
                        "adapter_request_id": request.request_id,
                        "window_start": payload.window_start,
                        "window_end": payload.window_end,
                        "content_contract": "generic-public-article.v1",
                    }),
                )
                .await
            {
                Ok(summary) => {
                    normalized_item_count += summary.normalized_item_count;
                    new_item_count += summary.new_item_count;
                }
                Err(error) => {
                    let message = error.to_string();
                    self.database
                        .fail_crawl_run(crawl_run_id, &detected_source, &message)
                        .await
                        .map_err(|database_error| {
                            JobHandlerError::retryable(format!(
                                "{message}; additionally failed to close crawl run: {database_error}"
                            ))
                        })?;
                    self.database
                        .fail_company_news_extraction_run(
                            run_id,
                            company_id,
                            &message,
                            json!({ "stage": "persistence", "origin": origin }),
                        )
                        .await
                        .map_err(|database_error| {
                            JobHandlerError::retryable(database_error.to_string())
                        })?;
                    return Err(JobHandlerError::retryable(message));
                }
            }
        }

        let evidence_urls = adapter_response
            .articles
            .iter()
            .map(|article| article.url.clone())
            .collect::<Vec<_>>();
        let adapter_publication_ranks = adapter_response
            .publications
            .iter()
            .map(|publication| {
                (
                    publication_identity_key(&stable_publication_url(&publication.url)),
                    publication.rank_score.to_bits(),
                )
            })
            .fold(
                BTreeMap::<String, u64>::new(),
                |mut ranks, (identity, rank)| {
                    ranks
                        .entry(identity)
                        .and_modify(|current| *current = (*current).max(rank))
                        .or_insert(rank);
                    ranks
                },
            );
        let mut publication_urls = Vec::new();
        for publication in &adapter_response.publications {
            let stable_publication = stable_publication_url(&publication.url);
            if let Some(parent) = infer_publication_url(&stable_publication)
                && publication_identity_key(&parent)
                    != publication_identity_key(&stable_publication)
            {
                publication_urls.push(parent);
            }
            publication_urls.push(stable_publication);
        }
        publication_urls.extend(
            existing_recipes
                .iter()
                .filter(|recipe| existing_recipe_is_build_input(recipe, payload.include_covered))
                .map(|recipe| stable_publication_url(&recipe.spec.publication_url)),
        );
        publication_urls.extend(
            source_candidates
                .iter()
                .filter(|candidate| {
                    matches!(
                        candidate.candidate_kind,
                        SourceKind::Html | SourceKind::Browser
                    ) && likely_editorial_listing(&candidate.candidate_url)
                })
                .map(|candidate| stable_publication_url(&candidate.candidate_url)),
        );
        publication_urls.extend(evidence_urls.iter().filter_map(infer_publication_url));
        let mut seen_publications = HashSet::new();
        publication_urls.retain(|url| seen_publications.insert(publication_identity_key(url)));
        publication_urls.sort_by_key(|url| {
            let (path_depth, path_length) = publication_listing_specificity(url);
            Reverse((
                path_depth,
                publication_evidence_support(url, &evidence_urls),
                adapter_publication_ranks
                    .get(&publication_identity_key(url))
                    .copied()
                    .unwrap_or_default(),
                path_length,
            ))
        });
        publication_urls.truncate(self.max_articles.min(12));
        let evidence_identities = evidence_urls
            .iter()
            .map(publication_identity_key)
            .collect::<HashSet<_>>();
        let publication_boundary_identities = adapter_response
            .publications
            .iter()
            .map(|publication| publication_identity_key(&stable_publication_url(&publication.url)))
            .chain(
                source_candidates
                    .iter()
                    .filter(|candidate| {
                        evidence_has_web_adapter_recommendation(&candidate.evidence)
                    })
                    .map(|candidate| {
                        publication_identity_key(&stable_publication_url(&candidate.candidate_url))
                    }),
            )
            .chain(
                existing_recipes
                    .iter()
                    .filter(|recipe| {
                        existing_recipe_is_build_input(recipe, payload.include_covered)
                            && (recipe.spec.item_scope == RecipeItemScope::PublicationBoundary
                                || (recipe.generated_by_run_id.is_some()
                                    && adapter_cited_publication_item_scope(
                                        &company,
                                        &recipe.spec.publication_url,
                                    ) == RecipeItemScope::PublicationBoundary))
                    })
                    .map(|recipe| {
                        publication_identity_key(&stable_publication_url(
                            &recipe.spec.publication_url,
                        ))
                    }),
            )
            .collect::<HashSet<_>>();
        let mut active_publication_identities = existing_recipes
            .iter()
            .filter(|recipe| recipe_is_healthy_active(recipe))
            .map(|recipe| publication_identity_key(&recipe.spec.publication_url))
            .collect::<HashSet<_>>();

        let mut recipe_builds = Vec::new();
        let mut activated_recipe_count = 0_i32;
        let mut selected_recipes = Vec::<SelectedRecipeSample>::new();
        for publication_url in publication_urls {
            let publication_identity = publication_identity_key(&publication_url);
            let matching_active_recipe_ids = existing_recipes
                .iter()
                .filter(|recipe| {
                    recipe.status == RecipeStatus::Active
                        && publication_identity_key(&recipe.spec.publication_url)
                            == publication_identity
                })
                .map(|recipe| recipe.id)
                .collect::<Vec<_>>();
            let healthy_active_publication = existing_recipes.iter().any(|recipe| {
                recipe_is_healthy_active(recipe)
                    && publication_identity_key(&recipe.spec.publication_url)
                        == publication_identity
            });
            let revalidating_existing_publication = existing_recipes.iter().any(|recipe| {
                existing_recipe_is_build_input(recipe, payload.include_covered)
                    && publication_identity_key(&recipe.spec.publication_url)
                        == publication_identity
            });
            let rebuilding_active_publication = existing_recipes.iter().any(|recipe| {
                recipe.status == RecipeStatus::Active
                    && existing_recipe_is_build_input(recipe, payload.include_covered)
                    && publication_identity_key(&recipe.spec.publication_url)
                        == publication_identity
            });
            if publication_host_is_excluded(&company, &publication_url) {
                recipe_builds.push(json!({
                    "publication_url": publication_url,
                    "outcome": "publication_host_excluded",
                    "reasons": ["publication_host_excluded_by_company_policy"],
                }));
                continue;
            }
            if evidence_identities.contains(&publication_identity)
                && looks_like_article_detail_url(&publication_url)
            {
                recipe_builds.push(json!({
                    "publication_url": publication_url,
                    "outcome": "article_url_not_listing",
                    "reasons": ["publication_url_matches_evidence_article"],
                }));
                continue;
            }
            if healthy_active_publication && !payload.include_covered {
                recipe_builds.push(json!({
                    "publication_url": publication_url,
                    "outcome": "duplicates_active_publication",
                    "reasons": ["publication_identity_already_active"],
                }));
                continue;
            }
            let editorial_evidence_support =
                publication_evidence_support(&publication_url, &evidence_urls);
            if !likely_company_news_publication(&publication_url)
                && editorial_evidence_support < 3
                && !revalidating_existing_publication
            {
                recipe_builds.push(json!({
                    "publication_url": publication_url,
                    "outcome": "not_editorial_listing",
                    "reasons": [
                        "publication_url_lacks_editorial_scope_and_collection_evidence"
                    ],
                    "editorial_evidence_support": editorial_evidence_support,
                }));
                continue;
            }
            let publication_claims = self
                .database
                .list_active_company_news_publication_claims(publication_url.as_str())
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
            if let Some(conflict) = publication_claims
                .iter()
                .find(|claim| publication_claim_conflicts(company_id, &company.name, claim))
            {
                recipe_builds.push(json!({
                    "publication_url": publication_url,
                    "outcome": "claimed_by_other_company",
                    "reasons": ["publication_claimed_by_distinct_company"],
                    "claiming_company_id": conflict.company_id,
                    "claiming_company_name": conflict.company_name,
                    "claiming_recipe_id": conflict.recipe_id,
                }));
                continue;
            }
            let matching_evidence = evidence_urls
                .iter()
                .filter(|url| hosts_related(publication_url.host_str(), url.host_str()))
                .take(50)
                .cloned()
                .collect::<Vec<_>>();
            let mut initial_spec = build_company_news_recipe_spec(
                &company,
                publication_url.clone(),
                matching_evidence,
                self.max_articles,
            );
            if publication_boundary_identities.contains(&publication_identity) {
                initial_spec.item_scope =
                    adapter_cited_publication_item_scope(&company, &publication_url);
            }
            let mut recipe_crawl_cache = HtmlRecipeCrawlCache::default();
            let validation = match crawl_recipe_candidate(
                &self.recipe_crawler,
                &company,
                &initial_spec,
                &mut recipe_crawl_cache,
            )
            .await
            {
                Ok(first_report) if first_report.correctness_passed() => {
                    Ok((initial_spec, first_report))
                }
                Ok(first_report) => {
                    let initial_scope = if initial_spec.include_path_prefixes.is_empty() {
                        "broad"
                    } else {
                        "listing_path_prefix"
                    };
                    let mut attempts = vec![recipe_validation_attempt(
                        initial_scope,
                        &initial_spec,
                        &first_report,
                    )];
                    let mut last_report = first_report;
                    let mut alternatives = Vec::new();
                    if let Some(prefix) = evidence_article_path_prefix(
                        &initial_spec.publication_url,
                        &initial_spec.evidence_article_urls,
                    ) && initial_spec.include_path_prefixes != [prefix.clone()]
                    {
                        let mut evidence_spec = initial_spec.clone();
                        evidence_spec.include_path_prefixes = vec![prefix];
                        alternatives.push(("evidence_path_prefix", evidence_spec));
                    }
                    if recipe_should_try_broad_scope(
                        &initial_spec,
                        last_report.discovered_url_count,
                    ) {
                        let mut broad_spec = initial_spec.clone();
                        broad_spec.include_path_prefixes.clear();
                        alternatives.push(("broad", broad_spec));
                    }

                    let mut successful = None;
                    let mut crawl_failure = None;
                    for (scope, candidate_spec) in alternatives {
                        match crawl_recipe_candidate(
                            &self.recipe_crawler,
                            &company,
                            &candidate_spec,
                            &mut recipe_crawl_cache,
                        )
                        .await
                        {
                            Ok(report) if report.correctness_passed() => {
                                let mut candidate_spec = candidate_spec;
                                record_broad_scope_validation_evidence(
                                    &mut candidate_spec,
                                    &report.items,
                                );
                                successful = Some((candidate_spec, report));
                                break;
                            }
                            Ok(report) => {
                                attempts.push(recipe_validation_attempt(
                                    scope,
                                    &candidate_spec,
                                    &report,
                                ));
                                last_report = report;
                            }
                            Err(error) => {
                                crawl_failure = Some(json!({
                                    "publication_url": publication_url,
                                    "outcome": "crawl_failed",
                                    "scope": scope,
                                    "include_path_prefixes": candidate_spec.include_path_prefixes,
                                    "error": error.to_string(),
                                    "retryable": error.is_retryable(),
                                    "attempts": attempts,
                                }));
                                break;
                            }
                        }
                    }
                    if let Some(successful) = successful {
                        Ok(successful)
                    } else if let Some(crawl_failure) = crawl_failure {
                        Err(crawl_failure)
                    } else {
                        Err(json!({
                            "publication_url": publication_url,
                            "outcome": "correctness_failed",
                            "reasons": last_report.correctness_reasons,
                            "discovered_url_count": last_report.discovered_url_count,
                            "accepted_item_count": last_report.accepted_item_count,
                            "distinct_title_count": last_report.distinct_title_count,
                            "distinct_content_count": last_report.distinct_content_count,
                            "failure_diagnostics": recipe_failure_diagnostics(&last_report),
                            "attempts": attempts,
                        }))
                    }
                }
                Err(error) => Err(json!({
                    "publication_url": publication_url,
                    "outcome": "crawl_failed",
                    "error": error.to_string(),
                    "retryable": error.is_retryable(),
                })),
            };
            let (mut spec, validation_report) = match validation {
                Ok(value) => value,
                Err(outcome) => {
                    recipe_builds.push(outcome);
                    continue;
                }
            };
            if !publication_scope_has_editorial_evidence(
                &publication_url,
                &spec.evidence_article_urls,
                &validation_report,
            ) {
                recipe_builds.push(json!({
                    "publication_url": publication_url,
                    "outcome": "not_editorial_listing",
                    "reasons": [
                        "publication_url_lacks_editorial_scope_and_collection_evidence"
                    ],
                    "accepted_item_count": validation_report.accepted_item_count,
                    "distinct_title_count": validation_report.distinct_title_count,
                    "distinct_content_count": validation_report.distinct_content_count,
                }));
                continue;
            }
            if !organizational_scope_has_editorial_evidence(&publication_url, &validation_report) {
                recipe_builds.push(json!({
                    "publication_url": publication_url,
                    "outcome": "not_editorial_listing",
                    "reasons": [
                        "organizational_page_lacks_editorial_collection_evidence"
                    ],
                    "accepted_item_count": validation_report.accepted_item_count,
                    "distinct_title_count": validation_report.distinct_title_count,
                    "distinct_content_count": validation_report.distinct_content_count,
                    "latest_published_at": validation_report.latest_published_at,
                }));
                continue;
            }
            if spec.include_path_prefixes.is_empty()
                && looks_like_article_detail_url(&publication_url)
            {
                recipe_builds.push(json!({
                    "publication_url": publication_url,
                    "outcome": "article_url_not_listing",
                    "reasons": ["broad_listing_fallback_started_from_article_url"],
                }));
                continue;
            }
            let validation_item_urls = validation_report
                .items
                .iter()
                .map(raw_crawl_item_identity_url)
                .collect::<Vec<_>>();
            let validation_signature_candidates = validation_report
                .items
                .iter()
                .filter_map(raw_crawl_item_signature_candidate)
                .collect::<Vec<_>>();
            let validation_item_signatures = validation_report
                .items
                .iter()
                .filter_map(raw_crawl_item_signature)
                .collect::<HashSet<_>>();
            let approved_feed_company_claims = self
                .database
                .list_approved_feed_item_company_claims(&validation_item_urls)
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
            if let Some(conflict) = distinct_company_approved_feed_claim(
                company_id,
                &company.name,
                validation_report.accepted_item_count,
                &approved_feed_company_claims,
            ) {
                let matched_item_count =
                    usize::try_from(conflict.matched_item_count).unwrap_or(usize::MAX);
                recipe_builds.push(json!({
                    "publication_url": publication_url,
                    "outcome": "claimed_by_other_company_feed",
                    "reasons": ["publication_items_claimed_by_distinct_company_feed"],
                    "claiming_company_id": conflict.company_id,
                    "claiming_company_name": conflict.company_name,
                    "accepted_item_count": validation_report.accepted_item_count,
                    "approved_feed_overlap_count": conflict.matched_item_count,
                    "approved_feed_overlap_ratio_bps": ratio_bps(
                        matched_item_count,
                        validation_report.accepted_item_count,
                    ),
                }));
                continue;
            }
            let selected_recipe_urls = selected_recipes
                .iter()
                .flat_map(|selected| selected.item_urls.iter().cloned())
                .collect::<HashSet<_>>();
            let selected_recipe_signatures = selected_recipes
                .iter()
                .flat_map(|selected| selected.item_signatures.iter().cloned())
                .collect::<HashSet<_>>();
            if recipe_items_are_fully_covered(
                &validation_report.items,
                &selected_recipe_urls,
                &selected_recipe_signatures,
            ) {
                let superseded_redundant_recipe_count = self
                    .supersede_redundant_rebuilds(
                        &matching_active_recipe_ids,
                        "overlaps_selected_recipe_items",
                        json!({
                            "publication_url": publication_url,
                            "accepted_item_count": validation_report.accepted_item_count,
                            "active_recipe_overlap_count": validation_report.accepted_item_count,
                        }),
                    )
                    .await?;
                if superseded_redundant_recipe_count > 0 {
                    active_publication_identities.remove(&publication_identity);
                }
                recipe_builds.push(json!({
                    "publication_url": publication_url,
                    "outcome": "overlaps_selected_recipe_items",
                    "superseded_redundant_recipe":
                        superseded_redundant_recipe_count > 0,
                    "superseded_redundant_recipe_count":
                        superseded_redundant_recipe_count,
                    "accepted_item_count": validation_report.accepted_item_count,
                    "active_recipe_overlap_count": validation_report.accepted_item_count,
                    "active_recipe_overlap_ratio_bps": 10_000,
                }));
                continue;
            }
            let validation_item_url_set =
                validation_item_urls.iter().cloned().collect::<HashSet<_>>();
            let covered_selected_recipes = selected_recipes_covered_by_candidate(
                &selected_recipes,
                &validation_item_url_set,
                &validation_item_signatures,
            );
            let fully_covered_active_recipe_ids = self
                .database
                .list_active_recipe_ids_fully_covered_by_item_urls(
                    company_id,
                    &validation_item_urls,
                )
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                .into_iter()
                .collect::<HashSet<_>>();
            let covered_active_detail_recipe_ids = existing_recipes
                .iter()
                .filter(|recipe| {
                    recipe.status == RecipeStatus::Active
                        && fully_covered_active_recipe_ids.contains(&recipe.id)
                        && is_stable_editorial_parent(
                            &publication_url,
                            &recipe.spec.publication_url,
                        )
                })
                .map(|recipe| recipe.id)
                .collect::<Vec<_>>();
            let approved_feed_signature_matches = self
                .database
                .list_approved_feed_item_signature_matches(
                    company_id,
                    &validation_signature_candidates,
                )
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                .into_iter()
                .collect::<HashSet<_>>();
            let mut approved_feed_matches = self
                .database
                .list_approved_feed_item_url_matches(company_id, &validation_item_urls)
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                .into_iter()
                .collect::<HashSet<_>>();
            approved_feed_matches.extend(approved_feed_signature_matches.iter().cloned());
            let approved_feed_overlap_count = approved_feed_matches.len();
            if recipe_overlaps_approved_feed(
                validation_report.accepted_item_count,
                approved_feed_overlap_count,
            ) {
                let superseded_redundant_recipe_count = self
                    .supersede_redundant_rebuilds(
                        &matching_active_recipe_ids,
                        "overlaps_approved_feed",
                        json!({
                            "publication_url": publication_url,
                            "accepted_item_count": validation_report.accepted_item_count,
                            "approved_feed_overlap_count": approved_feed_overlap_count,
                            "approved_feed_signature_overlap_count":
                                approved_feed_signature_matches.len(),
                        }),
                    )
                    .await?;
                if superseded_redundant_recipe_count > 0 {
                    active_publication_identities.remove(&publication_identity);
                }
                recipe_builds.push(json!({
                    "publication_url": publication_url,
                    "outcome": "overlaps_approved_feed",
                    "superseded_redundant_recipe":
                        superseded_redundant_recipe_count > 0,
                    "superseded_redundant_recipe_count":
                        superseded_redundant_recipe_count,
                    "accepted_item_count": validation_report.accepted_item_count,
                    "approved_feed_overlap_count": approved_feed_overlap_count,
                    "approved_feed_signature_overlap_count":
                        approved_feed_signature_matches.len(),
                    "approved_feed_overlap_ratio_bps": ratio_bps(
                        approved_feed_overlap_count,
                        validation_report.accepted_item_count,
                    ),
                }));
                continue;
            }
            let (active_recipe_matches, active_recipe_signature_matches) =
                if rebuilding_active_publication {
                    (HashSet::new(), HashSet::new())
                } else {
                    let signature_matches = self
                        .database
                        .list_active_recipe_item_signature_matches(
                            company_id,
                            &validation_signature_candidates,
                        )
                        .await
                        .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                        .into_iter()
                        .collect::<HashSet<_>>();
                    let mut matches = self
                        .database
                        .list_active_recipe_item_url_matches(company_id, &validation_item_urls)
                        .await
                        .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                        .into_iter()
                        .collect::<HashSet<_>>();
                    matches.extend(signature_matches.iter().cloned());
                    (matches, signature_matches)
                };
            let active_recipe_overlap_count = validation_report
                .items
                .iter()
                .filter(|item| {
                    let identity_url = raw_crawl_item_identity_url(item);
                    active_recipe_matches.contains(&identity_url)
                        || selected_recipe_urls.contains(&identity_url)
                        || raw_crawl_item_signature(item).is_some_and(|signature| {
                            selected_recipe_signatures.contains(&signature)
                        })
                })
                .count();
            if covered_selected_recipes.is_empty()
                && covered_active_detail_recipe_ids.is_empty()
                && recipe_overlaps_active_recipe(
                    validation_report.accepted_item_count,
                    active_recipe_overlap_count,
                )
            {
                recipe_builds.push(json!({
                    "publication_url": publication_url,
                    "outcome": "overlaps_active_recipe",
                    "accepted_item_count": validation_report.accepted_item_count,
                    "active_recipe_overlap_count": active_recipe_overlap_count,
                    "active_recipe_signature_overlap_count":
                        active_recipe_signature_matches.len(),
                    "active_recipe_overlap_ratio_bps": ratio_bps(
                        active_recipe_overlap_count,
                        validation_report.accepted_item_count,
                    ),
                }));
                continue;
            }
            calibrate_recipe_correctness(&mut spec, &validation_report)?;
            let source_key = company_news_import_source_key(&company.company_key, &publication_url);
            let source = self
                .database
                .get_or_create_company_news_source(
                    company_id,
                    &source_key,
                    &publication_url,
                    i32::try_from(spec.freshness.crawl_interval_seconds).map_err(|_| {
                        JobHandlerError::permanent("recipe crawl interval exceeds i32")
                    })?,
                    self.public_export_allowed,
                    run_id,
                )
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
            if source.status != SourceStatus::Approved
                || !matches!(source.kind, SourceKind::Html | SourceKind::Browser)
            {
                recipe_builds.push(json!({
                    "publication_url": publication_url,
                    "outcome": "source_incompatible",
                    "source_id": source.id,
                    "source_status": source.status,
                    "source_kind": source.kind,
                }));
                continue;
            }
            let encoded_spec = serde_json::to_vec(&spec).map_err(|error| {
                JobHandlerError::permanent(format!("serialize crawl recipe: {error}"))
            })?;
            let content_hash = hex::encode(Sha256::digest(&encoded_spec));
            let recipe_key = format!("{source_key}:recipe");
            let recipe = self
                .database
                .activate_company_news_recipe(
                    &recipe_key,
                    company_id,
                    source.id,
                    &spec,
                    &content_hash,
                    Some(run_id),
                    validation_report.fetched_at,
                    validation_report.latest_published_at,
                    validation_report.publication_date_coverage_complete,
                    Some(&validation_report.structure_fingerprint),
                    json!({
                        "activation": "independent_public_crawl",
                        "item_scope": spec.item_scope,
                        "discovered_url_count": validation_report.discovered_url_count,
                        "accepted_item_count": validation_report.accepted_item_count,
                        "distinct_title_count": validation_report.distinct_title_count,
                        "distinct_content_count": validation_report.distinct_content_count,
                        "acceptance_ratio_bps": validation_report.acceptance_ratio_bps,
                        "dated_item_count": validation_report.dated_item_count,
                        "publication_date_coverage_complete":
                            validation_report.publication_date_coverage_complete,
                        "content_stale": validation_report.content_stale,
                    }),
                )
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
            let superseded_duplicate_identity_recipe_count = self
                .supersede_redundant_rebuilds(
                    &matching_active_recipe_ids
                        .iter()
                        .copied()
                        .filter(|recipe_id| *recipe_id != recipe.id)
                        .collect::<Vec<_>>(),
                    "duplicate_publication_identity",
                    json!({
                        "publication_url": publication_url,
                        "superseded_by_recipe_id": recipe.id,
                    }),
                )
                .await?;
            let covered_selected_recipe_ids = covered_selected_recipes
                .iter()
                .map(|selected| selected.recipe_id)
                .collect::<Vec<_>>();
            let superseded_selected_recipe_count = self
                .supersede_redundant_rebuilds(
                    &covered_selected_recipe_ids,
                    "covered_by_broader_selected_recipe_items",
                    json!({
                        "publication_url": publication_url,
                        "superseded_by_recipe_id": recipe.id,
                        "accepted_item_count": validation_report.accepted_item_count,
                    }),
                )
                .await?;
            if superseded_selected_recipe_count > 0 {
                for selected in &covered_selected_recipes {
                    active_publication_identities.remove(&selected.publication_identity);
                    let selected_recipe_id = selected.recipe_id.to_string();
                    if let Some(build) = recipe_builds.iter_mut().find(|build| {
                        build.get("recipe_id").and_then(Value::as_str)
                            == Some(selected_recipe_id.as_str())
                    }) {
                        build["outcome"] = json!("superseded_by_broader_selected_recipe_items");
                        build["superseded_by_recipe_id"] = json!(recipe.id);
                    }
                }
                let covered_ids = covered_selected_recipe_ids
                    .iter()
                    .copied()
                    .collect::<HashSet<_>>();
                selected_recipes.retain(|selected| !covered_ids.contains(&selected.recipe_id));
                activated_recipe_count = activated_recipe_count.saturating_sub(
                    i32::try_from(superseded_selected_recipe_count).unwrap_or(i32::MAX),
                );
            }
            let superseded_active_detail_recipe_count = self
                .supersede_redundant_rebuilds(
                    &covered_active_detail_recipe_ids,
                    "covered_by_stable_parent_publication",
                    json!({
                        "publication_url": publication_url,
                        "superseded_by_recipe_id": recipe.id,
                        "accepted_item_count": validation_report.accepted_item_count,
                    }),
                )
                .await?;
            if superseded_active_detail_recipe_count > 0 {
                for existing in existing_recipes
                    .iter()
                    .filter(|existing| covered_active_detail_recipe_ids.contains(&existing.id))
                {
                    active_publication_identities
                        .remove(&publication_identity_key(&existing.spec.publication_url));
                }
            }
            let crawl_job = recipe_activation_crawl_job(company_id, source.id, recipe.id);
            self.database
                .enqueue_job(&crawl_job)
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
            source_count += 1;
            activated_recipe_count += 1;
            active_publication_identities.insert(publication_identity.clone());
            selected_recipes.push(SelectedRecipeSample {
                recipe_id: recipe.id,
                publication_identity,
                item_urls: validation_item_url_set,
                item_signatures: validation_item_signatures,
            });
            recipe_builds.push(json!({
                "publication_url": publication_url,
                "outcome": "activated",
                "rebuilt_active_publication": rebuilding_active_publication,
                "source_id": source.id,
                "recipe_id": recipe.id,
                "recipe_version": recipe.version,
                "content_stale": validation_report.content_stale,
                "dated_item_count": validation_report.dated_item_count,
                "publication_date_coverage_complete":
                    validation_report.publication_date_coverage_complete,
                "discovered_url_count": validation_report.discovered_url_count,
                "accepted_item_count": validation_report.accepted_item_count,
                "distinct_title_count": validation_report.distinct_title_count,
                "distinct_content_count": validation_report.distinct_content_count,
                "acceptance_ratio_bps": validation_report.acceptance_ratio_bps,
                "superseded_duplicate_identity_recipe_count":
                    superseded_duplicate_identity_recipe_count,
                "superseded_selected_recipe_count":
                    superseded_selected_recipe_count,
                "superseded_active_detail_recipe_count":
                    superseded_active_detail_recipe_count,
            }));
        }

        let has_healthy_approved_feed = self
            .database
            .company_has_healthy_approved_feed(company_id)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
        let (seeded_discovery_job_id, seeded_discovery_seed_count) =
            if !should_seed_publication_discovery(
                &public_discovery_seeds,
                has_healthy_approved_feed,
                payload.include_covered,
            ) {
                (None, 0_usize)
            } else {
                let mut discovery_job = JobSpec::new(
                    JobType::DiscoverCompany,
                    format!("company:{company_id}:recipe-seeds:{run_id}"),
                    Utc::now(),
                );
                discovery_job.company_id = Some(company_id);
                discovery_job.priority = i16::MAX / 2;
                discovery_job.max_attempts = 3;
                discovery_job.payload = json!({
                    "schema_version": "recipe-publication-discovery-seeds.v1",
                    "seed_origin": "company_news_recipe_builder",
                    "origin_run_id": run_id,
                    "seeds": public_discovery_seeds,
                });
                let queued = self
                    .database
                    .enqueue_job(&discovery_job)
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                (Some(queued.id), public_discovery_seeds.len())
            };

        let completion = CompanyNewsExtractionCompletion {
            suggested_url_count,
            accepted_url_count: i32::try_from(accepted_url_count)
                .map_err(|_| JobHandlerError::permanent("accepted URL count exceeds i32"))?,
            rejected_url_count: i32::try_from(rejected_url_count)
                .map_err(|_| JobHandlerError::permanent("rejected URL count exceeds i32"))?,
            source_count,
            normalized_item_count,
            new_item_count,
            metadata: json!({
                "outcome": "completed",
                "adapter_request_id": request.request_id,
                "adapter_trace_id": adapter_response.adapter_trace_id,
                "window_start": payload.window_start,
                "window_end": payload.window_end,
                "content_contract": "generic-public-article.v1",
                "content_metrics": content_metrics,
                "failures": report.failures,
                "continued_after_transient_evidence_failure":
                    continued_after_transient_evidence_failure,
                "direct_company_scope_rejected_count":
                    direct_company_scope_rejected_count,
                "direct_topic_compromise_rejected_count":
                    direct_topic_compromise_rejected_count,
                "skipped_approved_feed_overlap_count": approved_feed_overlap_urls.len(),
                "skipped_approved_feed_signature_overlap_count":
                    approved_feed_signature_overlap_urls.len(),
                "skipped_origins": skipped_origins,
                "activated_recipe_count": activated_recipe_count,
                "recipe_builds": recipe_builds,
                "seeded_discovery_job_id": seeded_discovery_job_id,
                "seeded_discovery_seed_count": seeded_discovery_seed_count,
            }),
        };
        self.database
            .complete_company_news_extraction_run(run_id, company_id, &completion)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
        info!(
            %company_id,
            company_name = %company.name,
            suggested_url_count,
            accepted_url_count = completion.accepted_url_count,
            rejected_url_count = completion.rejected_url_count,
            normalized_item_count,
            new_item_count,
            "manual company news import completed"
        );
        Ok(())
    }
}

fn company_news_publication_discovery_seeds(
    response: &CompanyNewsExtractionResponse,
) -> Vec<DiscoverySeed> {
    let mut seen = HashSet::new();
    response
        .publications
        .iter()
        .map(|publication| {
            (
                stable_publication_url(&publication.url),
                publication.rank_score,
            )
        })
        .chain(response.articles.iter().filter_map(|article| {
            infer_publication_url(&article.url)
                .map(|publication| (stable_publication_url(&publication), article.rank_score))
        }))
        .filter(|(url, _)| likely_company_news_publication(url))
        .filter(|(url, _)| seen.insert(publication_identity_key(url)))
        .take(12)
        .map(|(url, rank_score)| DiscoverySeed {
            role: company_news_publication_role(&url).to_owned(),
            url,
            rank_score,
        })
        .collect()
}

fn should_seed_publication_discovery(
    seeds: &[DiscoverySeed],
    has_healthy_approved_feed: bool,
    include_covered: bool,
) -> bool {
    !seeds.is_empty() && (!has_healthy_approved_feed || include_covered)
}

fn company_news_publication_role(url: &Url) -> &'static str {
    let identity =
        format!("{}{}", url.host_str().unwrap_or_default(), url.path()).to_ascii_lowercase();
    if identity.contains("engineering") {
        "engineering_blog"
    } else if identity.contains("blog") {
        "corporate_blog"
    } else if identity.contains("press") {
        "press_releases"
    } else if identity.contains("investor") {
        "investor_relations"
    } else {
        "newsroom"
    }
}

fn validate_company_news_job_payload(
    payload: &CompanyNewsJobPayload,
    max_articles: usize,
) -> Result<(), JobHandlerError> {
    if payload.schema_version != COMPANY_NEWS_JOB_SCHEMA_VERSION {
        return Err(JobHandlerError::permanent(format!(
            "unsupported company news job schema {}",
            payload.schema_version
        )));
    }
    if payload.window_start >= payload.window_end {
        return Err(JobHandlerError::permanent(
            "company news job window_start must precede window_end",
        ));
    }
    if payload.max_articles == 0 || payload.max_articles as usize > max_articles {
        return Err(JobHandlerError::permanent(format!(
            "company news job max_articles must be between 1 and {max_articles}"
        )));
    }
    Ok(())
}

fn classify_article_crawler_error(error: &ArticlePageError) -> bool {
    error.is_retryable()
}

fn article_origin(url: &Url) -> Result<Url, String> {
    let mut origin = url.clone();
    if !matches!(origin.scheme(), "http" | "https") || origin.host_str().is_none() {
        return Err(format!("article URL {url} has no public HTTP origin"));
    }
    origin.set_path("/");
    origin.set_query(None);
    origin.set_fragment(None);
    Ok(origin)
}

fn company_news_import_source_key(company_key: &str, origin: &Url) -> String {
    let digest = hex::encode(Sha256::digest(origin.as_str().as_bytes()));
    format!("{company_key}-news-import-{}", &digest[..12])
}

fn build_company_news_recipe_spec(
    company: &Company,
    publication_url: Url,
    evidence_article_urls: Vec<Url>,
    max_articles: usize,
) -> CompanyNewsRecipeSpec {
    let publication_is_company_related =
        publication_host_is_company_related(company, &publication_url);
    let mut allowed_hosts = publication_url
        .host_str()
        .map(normalized_recipe_host)
        .into_iter()
        .chain(
            evidence_article_urls
                .iter()
                .filter_map(Url::host_str)
                .map(normalized_recipe_host),
        )
        .chain(
            (publication_is_company_related
                && !is_editorial_subdomain(publication_url.host_str())
                && !is_hosted_publication_profile(&publication_url))
            .then(|| {
                company
                    .discovery_entry_points()
                    .into_iter()
                    .filter_map(|(_, url)| url.host_str().map(normalized_recipe_host))
            })
            .into_iter()
            .flatten(),
        )
        .collect::<Vec<_>>();
    allowed_hosts.sort();
    allowed_hosts.dedup();
    let include_path_prefixes =
        preferred_initial_recipe_path_prefix(&publication_url, &evidence_article_urls)
            .into_iter()
            .collect();
    CompanyNewsRecipeSpec {
        schema_version: COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
        publication_url,
        render_mode: RecipeRenderMode::Http,
        article_link_selector: "a[href]".to_owned(),
        allowed_hosts,
        include_path_prefixes,
        exclude_path_prefixes: [
            "/author/",
            "/authors/",
            "/category/",
            "/login/",
            "/page/",
            "/search/",
            "/tag/",
            "/tags/",
            "/topic/",
            "/topics/",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        max_links: u32::try_from(max_articles.clamp(1, 100)).unwrap_or(100),
        freshness: RecipeFreshnessPolicy::default(),
        correctness: RecipeCorrectnessPolicy::default(),
        item_scope: RecipeItemScope::CompanyIdentity,
        evidence_article_urls,
    }
}

async fn crawl_recipe_candidate(
    crawler: &HtmlRecipeCrawler,
    company: &Company,
    spec: &CompanyNewsRecipeSpec,
    cache: &mut HtmlRecipeCrawlCache,
) -> Result<HtmlRecipeCrawlReport, RecipeCrawlError> {
    let mut report = crawler.crawl_with_cache(spec, cache).await?;
    apply_company_scope_filter(company, spec, &mut report);
    apply_dominant_editorial_namespace_filter(spec, &mut report);
    apply_recipe_publication_topic_compromise_filter(company, spec, &mut report);
    Ok(report)
}

fn recipe_validation_attempt(
    scope: &str,
    spec: &CompanyNewsRecipeSpec,
    report: &HtmlRecipeCrawlReport,
) -> Value {
    json!({
        "scope": scope,
        "include_path_prefixes": &spec.include_path_prefixes,
        "reasons": &report.correctness_reasons,
        "discovered_url_count": report.discovered_url_count,
        "accepted_item_count": report.accepted_item_count,
        "distinct_title_count": report.distinct_title_count,
        "distinct_content_count": report.distinct_content_count,
        "failure_diagnostics": recipe_failure_diagnostics(report),
    })
}

fn evidence_article_path_prefix(
    publication_url: &Url,
    evidence_article_urls: &[Url],
) -> Option<String> {
    let publication_path = publication_url.path().trim_end_matches('/');
    let mut paths_by_host = BTreeMap::<String, Vec<String>>::new();
    for evidence in evidence_article_urls
        .iter()
        .filter(|url| hosts_related(publication_url.host_str(), url.host_str()))
    {
        let path = evidence.path();
        if path.is_empty() || path == "/" || path.trim_end_matches('/') == publication_path {
            continue;
        }
        let Some(host) = evidence.host_str().map(normalized_recipe_host) else {
            continue;
        };
        paths_by_host.entry(host).or_default().push(path.to_owned());
    }

    let mut ranked_candidates = Vec::new();
    for paths in paths_by_host.values_mut() {
        paths.sort();
        paths.dedup();
        if paths.len() < 2 {
            continue;
        }
        let mut candidates = HashSet::new();
        for left_index in 0..paths.len() {
            for right in &paths[left_index + 1..] {
                if let Some(prefix) = common_path_prefix_at_boundary(&paths[left_index], right)
                    && evidence_path_prefix_is_specific(&prefix)
                {
                    candidates.insert(prefix);
                }
            }
        }
        ranked_candidates.extend(candidates.into_iter().map(|prefix| {
            let support = paths
                .iter()
                .filter(|path| path.starts_with(&prefix))
                .count();
            (prefix, support)
        }));
    }
    ranked_candidates
        .into_iter()
        .max_by(|(left, left_support), (right, right_support)| {
            (*left_support, left.len()).cmp(&(*right_support, right.len()))
        })
        .map(|(prefix, _)| prefix)
}

fn preferred_initial_recipe_path_prefix(
    publication_url: &Url,
    evidence_article_urls: &[Url],
) -> Option<String> {
    let listing_prefix = recipe_listing_prefix(publication_url);
    let should_prefer_evidence = !has_explicit_editorial_path_segment(publication_url)
        && !is_editorial_subdomain(publication_url.host_str())
        && !is_hosted_publication_profile(publication_url);
    if should_prefer_evidence
        && let Some(evidence_prefix) =
            evidence_article_path_prefix(publication_url, evidence_article_urls)
    {
        return Some(evidence_prefix);
    }
    listing_prefix
}

fn evidence_path_prefix_is_specific(prefix: &str) -> bool {
    let segments = prefix
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() != 1 || !prefix.ends_with('/') {
        return true;
    }
    matches!(
        segments[0].to_ascii_lowercase().as_str(),
        "articles"
            | "blog"
            | "blogs"
            | "engineering"
            | "insights"
            | "journal"
            | "news"
            | "newsroom"
            | "press"
            | "press-release"
            | "press-releases"
            | "research"
            | "stories"
            | "updates"
    )
}

fn publication_evidence_support(publication_url: &Url, evidence_article_urls: &[Url]) -> usize {
    let publication_path = recipe_listing_prefix(publication_url)
        .unwrap_or_else(|| publication_url.path().trim_end_matches('/').to_owned());
    if publication_path.is_empty() || publication_path == "/" {
        return 0;
    }
    evidence_article_urls
        .iter()
        .filter(|url| hosts_related(publication_url.host_str(), url.host_str()))
        .filter(|url| url.path().starts_with(&publication_path))
        .count()
}

fn common_path_prefix_at_boundary(left: &str, right: &str) -> Option<String> {
    let mut prefix = left
        .chars()
        .zip(right.chars())
        .take_while(|(left, right)| left == right)
        .map(|(character, _)| character)
        .collect::<String>();
    while !prefix
        .chars()
        .last()
        .is_some_and(|character| matches!(character, '/' | '-' | '_'))
    {
        prefix.pop();
    }
    (prefix.len() >= 5 && prefix != "/").then_some(prefix)
}

fn calibrate_recipe_correctness(
    spec: &mut CompanyNewsRecipeSpec,
    report: &feed_crawler::HtmlRecipeCrawlReport,
) -> Result<(), JobHandlerError> {
    let discovered = u32::try_from(report.discovered_url_count)
        .map_err(|_| JobHandlerError::permanent("recipe discovered URL count exceeds u32"))?;
    let accepted = u32::try_from(report.accepted_item_count)
        .map_err(|_| JobHandlerError::permanent("recipe accepted item count exceeds u32"))?;
    if discovered == 0 || accepted == 0 {
        return Err(JobHandlerError::permanent(
            "a recipe cannot be calibrated from an empty validation crawl",
        ));
    }
    spec.correctness.baseline_discovered_items = discovered;
    spec.correctness.baseline_accepted_items = accepted;
    spec.correctness.min_discovered_items = discovered.div_ceil(4).max(1);
    spec.correctness.min_accepted_items = accepted.div_ceil(4).max(1);
    spec.correctness.min_acceptance_ratio_bps = (report.acceptance_ratio_bps / 2).max(1_000);
    spec.validate()
        .map_err(|error| JobHandlerError::permanent(error.to_string()))
}

fn recipe_listing_prefix(url: &Url) -> Option<String> {
    let path = url.path().trim();
    if path.is_empty() || path == "/" {
        return None;
    }
    let mut segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if segments.last().is_some_and(|segment| {
        matches!(
            segment.to_ascii_lowercase().as_str(),
            "default"
                | "default.asp"
                | "default.aspx"
                | "default.htm"
                | "default.html"
                | "default.php"
                | "index"
                | "index.asp"
                | "index.aspx"
                | "index.htm"
                | "index.html"
                | "index.php"
        )
    }) {
        segments.pop();
    } else if let Some(listing_stem) = segments
        .last()
        .and_then(|segment| semantic_listing_document_stem(segment))
    {
        let last_index = segments.len() - 1;
        segments[last_index] = listing_stem;
    }
    (!segments.is_empty()).then(|| format!("/{}/", segments.join("/")))
}

fn semantic_listing_document_stem(segment: &str) -> Option<String> {
    let lowercase = segment.to_ascii_lowercase();
    let suffix = [".aspx", ".html", ".asp", ".htm", ".php"]
        .into_iter()
        .find(|suffix| lowercase.ends_with(suffix))?;
    let stem = &segment[..segment.len().saturating_sub(suffix.len())];
    let normalized = stem.to_ascii_lowercase().replace('_', "-");
    let exact_listing = matches!(
        normalized.as_str(),
        "announcements"
            | "articles"
            | "blog"
            | "blogs"
            | "changelog"
            | "changelogs"
            | "company-news"
            | "feature-articles"
            | "insights"
            | "latest-news"
            | "media"
            | "media-center"
            | "media-centre"
            | "news"
            | "news-and-events"
            | "news-center"
            | "news-centre"
            | "news-events"
            | "newsroom"
            | "press"
            | "press-release"
            | "press-releases"
            | "publications"
            | "release-notes"
            | "releases"
            | "research"
            | "resources"
            | "stories"
            | "updates"
            | "what-s-new"
            | "whats-new"
    );
    let named_collection = [
        "-announcements",
        "-articles",
        "-blogs",
        "-insights",
        "-news",
        "-press-releases",
        "-publications",
        "-releases",
        "-resources",
        "-stories",
        "-updates",
    ]
    .iter()
    .any(|suffix| normalized.ends_with(suffix));
    (exact_listing || named_collection).then(|| stem.to_owned())
}

fn publication_listing_specificity(url: &Url) -> (usize, usize) {
    let prefix = recipe_listing_prefix(url).unwrap_or_else(|| url.path().to_owned());
    let depth = prefix
        .split('/')
        .filter(|segment| !segment.is_empty())
        .count();
    (depth, prefix.len())
}

fn recipe_evidence_requires_broad_scope(spec: &CompanyNewsRecipeSpec) -> bool {
    evidence_requires_broad_scope(&spec.publication_url, &spec.evidence_article_urls)
}

fn recipe_should_try_broad_scope(
    spec: &CompanyNewsRecipeSpec,
    discovered_url_count: usize,
) -> bool {
    !spec.include_path_prefixes.is_empty()
        && (discovered_url_count == 0 || recipe_evidence_requires_broad_scope(spec))
}

fn record_broad_scope_validation_evidence(
    spec: &mut CompanyNewsRecipeSpec,
    items: &[feed_core::RawCrawlItem],
) {
    if !spec.include_path_prefixes.is_empty()
        || recipe_listing_prefix(&spec.publication_url).is_none()
        || recipe_evidence_requires_broad_scope(spec)
    {
        return;
    }
    let mut evidence = items
        .iter()
        .map(|item| item.canonical_url.as_ref().unwrap_or(&item.url))
        .filter(|url| hosts_related(spec.publication_url.host_str(), url.host_str()))
        .cloned()
        .chain(spec.evidence_article_urls.iter().cloned())
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    evidence.retain(|url| seen.insert(publication_identity_key(url)));
    evidence.truncate(50);
    spec.evidence_article_urls = evidence;
}

fn evidence_requires_broad_scope(publication_url: &Url, evidence_article_urls: &[Url]) -> bool {
    let Some(listing_prefix) = recipe_listing_prefix(publication_url) else {
        return false;
    };
    evidence_article_urls.iter().any(|evidence| {
        hosts_related(publication_url.host_str(), evidence.host_str())
            && !evidence.path().starts_with(&listing_prefix)
    })
}

fn effective_recipe_include_path_prefixes(
    configured: &[String],
    adapter_generated: bool,
    publication_url: &Url,
    evidence_article_urls: &[Url],
) -> Vec<String> {
    if !adapter_generated {
        return configured.to_vec();
    }
    let Some(prefix) = recipe_listing_prefix(publication_url) else {
        return configured.to_vec();
    };
    let preferred_prefix =
        preferred_initial_recipe_path_prefix(publication_url, evidence_article_urls)
            .unwrap_or_else(|| prefix.clone());
    if !configured.is_empty() {
        if configured == [preferred_prefix.clone()]
            || evidence_article_path_prefix(publication_url, evidence_article_urls)
                .is_some_and(|evidence_prefix| configured == [evidence_prefix])
        {
            return configured.to_vec();
        }
        if configured == [prefix.clone()]
            && preferred_prefix != prefix
            && !evidence_requires_broad_scope(publication_url, evidence_article_urls)
        {
            return vec![preferred_prefix];
        }
        return if evidence_requires_broad_scope(publication_url, evidence_article_urls) {
            Vec::new()
        } else {
            vec![prefix]
        };
    };
    if evidence_requires_broad_scope(publication_url, evidence_article_urls) {
        Vec::new()
    } else {
        vec![prefix]
    }
}

fn effective_recipe_allowed_hosts(
    configured: &[String],
    adapter_generated: bool,
    publication_url: &Url,
    evidence_article_urls: &[Url],
) -> Vec<String> {
    if !adapter_generated
        || (!is_editorial_subdomain(publication_url.host_str())
            && !is_hosted_publication_profile(publication_url))
    {
        return configured.to_vec();
    }
    let mut allowed_hosts = publication_url
        .host_str()
        .map(normalized_recipe_host)
        .into_iter()
        .chain(
            evidence_article_urls
                .iter()
                .filter_map(Url::host_str)
                .map(normalized_recipe_host),
        )
        .collect::<Vec<_>>();
    allowed_hosts.sort();
    allowed_hosts.dedup();
    allowed_hosts
}

fn infer_publication_url(article_url: &Url) -> Option<Url> {
    const EDITORIAL_SEGMENTS: &[&str] = &[
        "articles",
        "blog",
        "blogs",
        "engineering",
        "insights",
        "news",
        "newsroom",
        "press",
        "press-release",
        "press-releases",
        "research",
        "stories",
        "updates",
    ];
    let segments = article_url
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let root = segments
        .iter()
        .position(|segment| EDITORIAL_SEGMENTS.contains(&segment.to_ascii_lowercase().as_str()))?;
    if root + 1 >= segments.len() {
        return None;
    }
    let mut publication = article_url.clone();
    publication.set_path(&format!("/{}/", segments[..=root].join("/")));
    publication.set_query(None);
    publication.set_fragment(None);
    Some(publication)
}

fn is_stable_editorial_parent(parent: &Url, detail: &Url) -> bool {
    infer_publication_url(detail).is_some_and(|inferred| {
        publication_identity_key(&inferred) == publication_identity_key(parent)
    })
}

const EDITORIAL_PATH_MARKERS: &[&str] = &[
    "announcement",
    "article",
    "blog",
    "case-study",
    "changelog",
    "content",
    "customer-stories",
    "developer",
    "engineering",
    "episode",
    "featured",
    "insight",
    "innovation",
    "journal",
    "knowledge",
    "learn",
    "library",
    "latest",
    "media",
    "message",
    "news",
    "notice",
    "paper",
    "perspective",
    "podcast",
    "post",
    "publication",
    "press",
    "release",
    "report",
    "research",
    "resource",
    "review",
    "stories",
    "tech",
    "thought-leadership",
    "updates",
    "what-s-new",
    "whats-new",
];

fn likely_editorial_listing(url: &Url) -> bool {
    let segments = url
        .path_segments()
        .into_iter()
        .flatten()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    segments
        .iter()
        .any(|segment| path_segment_has_editorial_marker(segment))
        && !has_taxonomy_path_segment(url)
        && !has_subscription_utility_path_segment(url)
}

fn path_segment_has_editorial_marker(segment: &str) -> bool {
    EDITORIAL_PATH_MARKERS
        .iter()
        .any(|marker| segment.contains(marker))
}

fn looks_like_article_detail_url(url: &Url) -> bool {
    const LISTING_SEGMENTS: &[&str] = &[
        "announcements",
        "articles",
        "blog",
        "blogs",
        "case-studies",
        "changelog",
        "company-news",
        "content",
        "customer-stories",
        "developer",
        "developers",
        "engineering",
        "episodes",
        "insights",
        "investor-news",
        "journal",
        "latest",
        "latest-news",
        "media",
        "media-center",
        "media-centre",
        "news",
        "news-and-events",
        "news-center",
        "news-centre",
        "news-events",
        "news-media",
        "newsroom",
        "notices",
        "papers",
        "perspectives",
        "podcast",
        "posts",
        "press",
        "press-release",
        "press-releases",
        "publications",
        "releases",
        "reports",
        "research",
        "resources",
        "stories",
        "tech",
        "technology",
        "updates",
        "what-s-new",
        "whats-new",
    ];
    let mut segments = url
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    if segments.len() == 1 && is_hosted_publication_profile(url) && segments[0].starts_with('@') {
        return false;
    }
    if segments.last().is_some_and(|segment| {
        matches!(
            segment.as_str(),
            "default" | "default.aspx" | "index" | "index.html" | "index.htm"
        )
    }) {
        segments.pop();
    }
    let Some(last) = segments.last() else {
        return false;
    };
    let normalized_last = last
        .strip_suffix(".html")
        .or_else(|| last.strip_suffix(".htm"))
        .or_else(|| last.strip_suffix(".aspx"))
        .unwrap_or(last);
    if LISTING_SEGMENTS.contains(&normalized_last) {
        return false;
    }
    if normalized_last.len() == 4
        && normalized_last.bytes().all(|byte| byte.is_ascii_digit())
        && segments
            .iter()
            .rev()
            .nth(1)
            .is_some_and(|segment| LISTING_SEGMENTS.contains(&segment.as_str()))
    {
        return false;
    }
    let leading_segments = &segments[..segments.len().saturating_sub(1)];
    let follows_year = leading_segments.last().is_some_and(|segment| {
        segment.len() == 4 && segment.bytes().all(|byte| byte.is_ascii_digit())
    });
    let follows_detail_marker = leading_segments.iter().any(|segment| {
        matches!(
            segment.as_str(),
            "detail" | "details" | "news-detail" | "news-details"
        )
    });
    let strong_slug = normalized_last.len() >= 32
        && normalized_last.bytes().filter(|byte| *byte == b'-').count() >= 3;
    (follows_year || follows_detail_marker || strong_slug)
        && leading_segments
            .iter()
            .any(|segment| path_segment_has_editorial_marker(segment))
}

fn publication_matches_evidence_article(spec: &CompanyNewsRecipeSpec) -> bool {
    let publication_identity = publication_identity_key(&spec.publication_url);
    looks_like_article_detail_url(&spec.publication_url)
        && spec
            .evidence_article_urls
            .iter()
            .any(|url| publication_identity_key(url) == publication_identity)
}

fn stable_publication_url(url: &Url) -> Url {
    let Some(prefix_len) = temporal_archive_prefix_len(url) else {
        return url.clone();
    };
    let mut stable = url.clone();
    let segments = url
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .take(prefix_len)
        .collect::<Vec<_>>();
    let stable_path = if segments.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}/", segments.join("/"))
    };
    stable.set_path(&stable_path);
    stable.set_query(None);
    stable.set_fragment(None);
    if segments.is_empty()
        && !is_editorial_subdomain(stable.host_str())
        && !is_hosted_publication_profile(&stable)
    {
        return url.clone();
    }
    stable
}

fn temporal_archive_prefix_len(url: &Url) -> Option<usize> {
    let segments = url
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() >= 2 {
        let year = strip_page_document_extension(segments[segments.len() - 2])
            .parse::<u16>()
            .ok();
        let month = strip_page_document_extension(segments[segments.len() - 1])
            .parse::<u8>()
            .ok();
        if year.is_some_and(|year| matches!(year, 1900..=2099))
            && month.is_some_and(|month| matches!(month, 1..=12))
        {
            return Some(segments.len() - 2);
        }
    }
    let year = strip_page_document_extension(segments.last()?)
        .parse::<u16>()
        .ok()?;
    if !matches!(year, 1900..=2099) {
        return None;
    }
    let mut prefix_len = segments.len() - 1;
    if prefix_len > 0
        && matches!(
            segments[prefix_len - 1].to_ascii_lowercase().as_str(),
            "archive" | "archives" | "year" | "years"
        )
    {
        prefix_len -= 1;
    }
    Some(prefix_len)
}

fn strip_page_document_extension(segment: &str) -> &str {
    [".aspx", ".html", ".asp", ".htm", ".php"]
        .into_iter()
        .find_map(|suffix| segment.strip_suffix(suffix))
        .unwrap_or(segment)
}

fn has_temporal_archive_suffix(url: &Url) -> bool {
    temporal_archive_prefix_len(url).is_some()
}

fn likely_company_news_publication(url: &Url) -> bool {
    if publication_url_has_hard_non_editorial_scope(url) {
        return false;
    }
    if is_globenewswire_host(url.host_str()) {
        return true;
    }
    if is_editorial_subdomain(url.host_str()) || is_hosted_publication_profile(url) {
        return true;
    }
    likely_editorial_listing(url)
}

fn publication_url_has_hard_non_editorial_scope(url: &Url) -> bool {
    if is_globenewswire_host(url.host_str()) {
        return !is_scoped_globenewswire_listing(url);
    }
    is_unscoped_access_newsroom(url)
        || is_unscoped_shared_news_publication(url)
        || is_unscoped_fund_manager_publication(url)
        || url_has_any_path_segment(url, &["discuss"])
        || has_taxonomy_path_segment(url)
        || has_subscription_utility_path_segment(url)
        || is_non_editorial_documentation_or_help_publication(url)
        || has_non_editorial_parent_scope(url)
        || has_temporal_archive_suffix(url)
}

fn publication_scope_has_editorial_evidence(
    url: &Url,
    evidence_article_urls: &[Url],
    report: &HtmlRecipeCrawlReport,
) -> bool {
    let repeatable_article_evidence = publication_evidence_support(url, evidence_article_urls) >= 2
        || evidence_article_path_prefix(url, evidence_article_urls).is_some();
    likely_company_news_publication(url)
        || (!publication_url_has_hard_non_editorial_scope(url)
            && report.accepted_item_count >= 3
            && report.distinct_title_count >= 3
            && repeatable_article_evidence)
}

fn has_non_editorial_parent_scope(url: &Url) -> bool {
    const NON_EDITORIAL_PARENT_SEGMENTS: &[&str] = &[
        "capabilities",
        "capability",
        "expertise",
        "industries",
        "industry",
        "our-services",
        "product",
        "products",
        "role",
        "roles",
        "service",
        "services",
        "solutions",
        "use-case",
        "use-cases",
    ];
    url_has_any_path_segment(url, NON_EDITORIAL_PARENT_SEGMENTS)
        && !has_explicit_editorial_path_segment(url)
}

fn has_organizational_parent_scope(url: &Url) -> bool {
    const ORGANIZATIONAL_PARENT_SEGMENTS: &[&str] =
        &["careers", "departments", "jobs", "team", "teams"];
    url_has_any_path_segment(url, ORGANIZATIONAL_PARENT_SEGMENTS)
        && !has_explicit_editorial_path_segment(url)
}

fn url_has_any_path_segment(url: &Url, expected: &[&str]) -> bool {
    url.path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .map(str::to_ascii_lowercase)
        .any(|segment| expected.contains(&segment.as_str()))
}

fn has_explicit_editorial_path_segment(url: &Url) -> bool {
    const EXPLICIT_EDITORIAL_SEGMENTS: &[&str] = &[
        "announcements",
        "articles",
        "blog",
        "blogs",
        "changelog",
        "changelogs",
        "company-news",
        "insights",
        "news",
        "newsroom",
        "press",
        "press-release",
        "press-releases",
        "publications",
        "release-notes",
        "research",
        "stories",
        "updates",
        "what-s-new",
        "whats-new",
    ];
    url.path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .map(str::to_ascii_lowercase)
        .any(|segment| {
            let normalized = segment
                .strip_suffix(".html")
                .or_else(|| segment.strip_suffix(".htm"))
                .or_else(|| segment.strip_suffix(".aspx"))
                .unwrap_or(&segment);
            EXPLICIT_EDITORIAL_SEGMENTS.contains(&normalized)
                || ["-blog", "_blog"]
                    .iter()
                    .any(|suffix| normalized.ends_with(suffix))
        })
}

fn organizational_scope_has_editorial_evidence(url: &Url, report: &HtmlRecipeCrawlReport) -> bool {
    !has_organizational_parent_scope(url)
        || (report.accepted_item_count >= 3
            && report.distinct_title_count >= 3
            && report.latest_published_at.is_some())
}

fn is_non_editorial_documentation_or_help_publication(url: &Url) -> bool {
    let segments = url
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let has_explicit_editorial_listing = segments.iter().any(|segment| {
        matches!(
            segment.as_str(),
            "blog"
                | "blogs"
                | "changelog"
                | "changelogs"
                | "company-news"
                | "engineering"
                | "insights"
                | "news"
                | "newsroom"
                | "press"
                | "press-releases"
                | "product-updates"
                | "release-notes"
                | "releases"
                | "research"
                | "stories"
                | "updates"
                | "what-s-new"
                | "whats-new"
        )
    });
    let host_label = url
        .host_str()
        .map(normalized_recipe_host)
        .and_then(|host| host.split('.').next().map(str::to_owned))
        .unwrap_or_default();
    let is_help_host = matches!(
        host_label.as_str(),
        "help" | "helpcenter" | "knowledgebase" | "support"
    );
    if is_help_host
        && segments
            .iter()
            .any(|segment| matches!(segment.as_str(), "article" | "articles"))
    {
        return true;
    }
    let has_help_release_collection = is_help_host
        && segments
            .iter()
            .any(|segment| matches!(segment.as_str(), "section" | "sections"))
        && segments.iter().any(|segment| {
            ["changelog", "news", "release", "update"]
                .iter()
                .any(|marker| segment.contains(marker))
        });
    if has_explicit_editorial_listing || has_help_release_collection {
        return false;
    }
    let has_documentation_reference_scope = segments.iter().any(|segment| {
        matches!(
            segment.as_str(),
            "api-reference"
                | "docs"
                | "documentation"
                | "knowledge-base"
                | "reference"
                | "references"
        )
    });
    is_help_host || has_documentation_reference_scope
}

fn is_unscoped_fund_manager_publication(url: &Url) -> bool {
    let host = url.host_str().map(normalized_recipe_host);
    let path = url.path().trim_end_matches('/').to_ascii_lowercase();
    matches!(
        (host.as_deref(), path.as_str()),
        (
            Some("blackrock.com"),
            "/us/financial-professionals/investments/products/closed-end-funds/press-releases"
        ) | (
            Some("gabelli.com"),
            "/insights/gabelli-media/press-releases"
        )
    )
}

fn is_unscoped_shared_news_publication(url: &Url) -> bool {
    let host = url.host_str().map(normalized_recipe_host);
    let path = url.path().trim_end_matches('/').to_ascii_lowercase();
    matches!(
        (host.as_deref(), path.as_str()),
        (Some("businesswire.com"), "/news")
            | (Some("investing.com"), "/news")
            | (
                Some("nasdaq.com"),
                "/european-market-activity/news/company-news"
            )
            | (Some("nasdaq.com"), "/market-activity/quotes/press-releases")
            | (Some("nasdaq.com"), "/press-release")
            | (Some("prnewswire.com"), "/news")
            | (Some("prnewswire.com"), "/news-releases")
            | (Some("prnewswire.com"), "/resources/articles")
            | (Some("prnewswire.com"), "/ru/press-releases")
            | (Some("stocktitan.net"), "/news")
    )
}

fn has_taxonomy_path_segment(url: &Url) -> bool {
    const TAXONOMY_SEGMENTS: &[&str] = &[
        "author",
        "authors",
        "categories",
        "category",
        "collection",
        "collections",
        "content-type",
        "page",
        "pillar",
        "search",
        "series",
        "tag",
        "tags",
        "topic",
        "topics",
    ];
    url.path_segments()
        .into_iter()
        .flatten()
        .map(str::to_ascii_lowercase)
        .any(|segment| {
            TAXONOMY_SEGMENTS.contains(&segment.as_str())
                || is_short_taxonomy_alias(&segment, &["category", "categories"])
                || matches!(
                    segment.as_str(),
                    "blog-search" | "news-search" | "search-results" | "site-search"
                )
        })
}

fn has_subscription_utility_path_segment(url: &Url) -> bool {
    url.path_segments()
        .into_iter()
        .flatten()
        .map(str::to_ascii_lowercase)
        .any(|segment| {
            matches!(
                segment.as_str(),
                "email-alert"
                    | "email-alerts"
                    | "investor-email-alert"
                    | "investor-email-alerts"
                    | "news-alert"
                    | "news-alerts"
                    | "press-release-alert"
                    | "press-release-alerts"
                    | "subscribe"
                    | "subscriptions"
                    | "unsubscribe"
            ) || segment.starts_with("subscribe-")
                || segment.starts_with("subscribe_")
        })
}

fn is_short_taxonomy_alias(segment: &str, terms: &[&str]) -> bool {
    let parts = segment.split(['-', '_']).collect::<Vec<_>>();
    parts.len() <= 3 && parts.iter().any(|part| terms.contains(part))
}

fn is_globenewswire_host(host: Option<&str>) -> bool {
    host.map(normalized_recipe_host)
        .is_some_and(|host| host == "globenewswire.com" || host.ends_with(".globenewswire.com"))
}

fn is_scoped_globenewswire_listing(url: &Url) -> bool {
    let segments = url
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    segments
        .windows(3)
        .any(|window| window[0] == "search" && window[1] == "organization")
}

fn is_unscoped_access_newsroom(url: &Url) -> bool {
    let is_access_host = url
        .host_str()
        .map(normalized_recipe_host)
        .is_some_and(|host| host == "accessnewswire.com");
    if !is_access_host {
        return false;
    }
    let segments = url
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    segments.len() == 1 && segments[0].eq_ignore_ascii_case("newsroom")
}

fn is_editorial_subdomain(host: Option<&str>) -> bool {
    const MARKERS: &[&str] = &[
        "blog",
        "builder",
        "developer",
        "engineering",
        "journal",
        "labs",
        "media",
        "news",
        "press",
        "research",
        "stories",
        "tech",
        "updates",
    ];
    let Some(host) = host.map(normalized_recipe_host) else {
        return false;
    };
    let labels = host.split('.').collect::<Vec<_>>();
    let common_country_code_second_level = labels.len() >= 2
        && labels.last().is_some_and(|label| label.len() == 2)
        && labels.get(labels.len() - 2).is_some_and(|label| {
            matches!(*label, "ac" | "co" | "com" | "edu" | "gov" | "net" | "org")
        });
    let minimum_labels = if common_country_code_second_level {
        4
    } else {
        3
    };
    if labels.len() < minimum_labels {
        return false;
    }
    let label = labels.first().copied().unwrap_or_default();
    MARKERS.iter().any(|marker| label.contains(marker))
}

fn is_hosted_publication_profile(url: &Url) -> bool {
    let host = url.host_str().map(normalized_recipe_host);
    if host
        .as_deref()
        .is_some_and(|host| host.ends_with(".substack.com") && host != "substack.com")
    {
        return true;
    }
    host.as_deref() == Some("medium.com")
        && url
            .path_segments()
            .into_iter()
            .flatten()
            .find(|segment| !segment.is_empty())
            .is_some_and(|segment| segment.starts_with('@'))
}

fn publication_identity_key(url: &Url) -> String {
    let mut normalized = url.clone();
    normalized.set_fragment(None);
    normalized.set_query(None);
    if normalized.set_scheme("https").is_err() {
        return normalized.as_str().to_owned();
    }
    if let Some(host) = normalized.host_str().map(normalized_recipe_host)
        && normalized.set_host(Some(&host)).is_err()
    {
        return normalized.as_str().to_owned();
    }
    let mut segments = normalized
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() > 1 && looks_like_locale_segment(segments[0]) {
        segments.remove(0);
    }
    let path = if segments.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", segments.join("/"))
    };
    normalized.set_path(&path);
    normalized.as_str().to_owned()
}

fn looks_like_locale_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    (bytes.len() == 2 && bytes.iter().all(u8::is_ascii_alphabetic))
        || (bytes.len() == 5
            && bytes[2] == b'-'
            && bytes[..2].iter().all(u8::is_ascii_alphabetic)
            && bytes[3..].iter().all(u8::is_ascii_alphabetic))
}

fn normalized_recipe_host(host: &str) -> String {
    host.trim_start_matches("www.").to_ascii_lowercase()
}

fn raw_crawl_item_identity_url(item: &feed_core::RawCrawlItem) -> String {
    item.canonical_url
        .as_ref()
        .unwrap_or(&item.url)
        .as_str()
        .to_owned()
}

fn raw_crawl_item_signature_candidate(
    item: &feed_core::RawCrawlItem,
) -> Option<FeedItemSignatureCandidate> {
    Some(FeedItemSignatureCandidate {
        identity_url: raw_crawl_item_identity_url(item),
        title: item.title.as_ref()?.clone(),
        published_at: item.published_at,
    })
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RecipeItemSignature {
    normalized_title: String,
    published_on: Option<NaiveDate>,
}

fn raw_crawl_item_signature(item: &feed_core::RawCrawlItem) -> Option<RecipeItemSignature> {
    let normalized_title = item
        .title
        .as_ref()?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    (!normalized_title.is_empty()).then(|| RecipeItemSignature {
        normalized_title,
        published_on: item
            .published_at
            .map(|published_at| published_at.date_naive()),
    })
}

#[derive(Clone, Debug)]
struct SelectedRecipeSample {
    recipe_id: uuid::Uuid,
    publication_identity: String,
    item_urls: HashSet<String>,
    item_signatures: HashSet<RecipeItemSignature>,
}

fn recipe_items_are_fully_covered(
    items: &[feed_core::RawCrawlItem],
    selected_recipe_urls: &HashSet<String>,
    selected_recipe_signatures: &HashSet<RecipeItemSignature>,
) -> bool {
    if items.is_empty() {
        return false;
    }
    let fully_url_covered = items
        .iter()
        .all(|item| selected_recipe_urls.contains(&raw_crawl_item_identity_url(item)));
    let fully_signature_covered = items.len() >= 3
        && items.iter().all(|item| {
            selected_recipe_urls.contains(&raw_crawl_item_identity_url(item))
                || raw_crawl_item_signature(item)
                    .is_some_and(|signature| selected_recipe_signatures.contains(&signature))
        });
    fully_url_covered || fully_signature_covered
}

fn selected_recipes_covered_by_candidate(
    selected_recipes: &[SelectedRecipeSample],
    candidate_urls: &HashSet<String>,
    candidate_signatures: &HashSet<RecipeItemSignature>,
) -> Vec<SelectedRecipeSample> {
    selected_recipes
        .iter()
        .filter(|selected| {
            let fully_url_covered = !selected.item_urls.is_empty()
                && selected
                    .item_urls
                    .iter()
                    .all(|url| candidate_urls.contains(url));
            let fully_signature_covered = selected.item_urls.len() >= 3
                && !selected.item_signatures.is_empty()
                && selected
                    .item_signatures
                    .iter()
                    .all(|signature| candidate_signatures.contains(signature));
            fully_url_covered || fully_signature_covered
        })
        .cloned()
        .collect()
}

fn ratio_bps(numerator: usize, denominator: usize) -> usize {
    if denominator == 0 {
        return 0;
    }
    numerator
        .min(denominator)
        .saturating_mul(10_000)
        .checked_div(denominator)
        .unwrap_or(0)
}

fn recipe_failure_diagnostics(report: &feed_crawler::HtmlRecipeCrawlReport) -> Value {
    let mut reason_counts = BTreeMap::new();
    let mut retryable_count = 0_usize;
    for failure in &report.failures {
        *reason_counts
            .entry(failure.reason.as_str())
            .or_insert(0_usize) += 1;
        retryable_count += usize::from(failure.retryable);
    }
    let samples = report
        .failures
        .iter()
        .take(3)
        .map(|failure| {
            json!({
                "url": failure.url,
                "reason": failure.reason,
                "retryable": failure.retryable,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "failure_count": report.failures.len(),
        "retryable_failure_count": retryable_count,
        "distinct_title_count": report.distinct_title_count,
        "distinct_content_count": report.distinct_content_count,
        "reason_counts": reason_counts,
        "samples": samples,
    })
}

fn recipe_correctness_blocked_by_retryable_fetches(
    report: &HtmlRecipeCrawlReport,
    policy: &RecipeCorrectnessPolicy,
) -> bool {
    const RETRYABLE_THRESHOLD_REASONS: &[&str] = &[
        "accepted_items_below_minimum",
        "acceptance_ratio_below_minimum",
    ];
    if report.correctness_reasons.is_empty()
        || report
            .correctness_reasons
            .iter()
            .any(|reason| !RETRYABLE_THRESHOLD_REASONS.contains(&reason.as_str()))
    {
        return false;
    }

    let retryable_failure_count = report
        .failures
        .iter()
        .filter(|failure| failure.retryable)
        .count();
    if retryable_failure_count == 0 {
        return false;
    }
    let possible_accepted = report
        .accepted_item_count
        .saturating_add(retryable_failure_count)
        .min(report.discovered_url_count);
    possible_accepted >= usize::try_from(policy.min_accepted_items).unwrap_or(usize::MAX)
        && ratio_bps(possible_accepted, report.discovered_url_count)
            >= usize::from(policy.min_acceptance_ratio_bps)
}

fn transient_recipe_fetch_error(report: &HtmlRecipeCrawlReport) -> String {
    let retryable_failure_count = report
        .failures
        .iter()
        .filter(|failure| failure.retryable)
        .count();
    format!(
        "retryable article fetch failures prevented recipe correctness validation \
         ({retryable_failure_count} transient failures; {} of {} articles accepted)",
        report.accepted_item_count, report.discovered_url_count
    )
}

fn recipe_overlaps_approved_feed(accepted_item_count: usize, overlap_count: usize) -> bool {
    overlap_count > 0 && ratio_bps(overlap_count, accepted_item_count) >= 5_000
}

fn feed_candidate_fully_covered_by_approved_feed(
    candidate_item_count: usize,
    overlap_count: usize,
) -> bool {
    candidate_item_count >= 3 && overlap_count >= candidate_item_count
}

fn recipe_overlaps_active_recipe(accepted_item_count: usize, overlap_count: usize) -> bool {
    overlap_count >= 3 && ratio_bps(overlap_count, accepted_item_count) >= 8_000
}

fn runtime_recipe_supersession_reason(
    company_scope_below_minimum: bool,
    overlaps_approved_feed: bool,
    overlaps_preferred_active_recipe: bool,
    duplicates_active_publication: bool,
) -> Option<&'static str> {
    if company_scope_below_minimum {
        Some("company_scope_relevance_below_minimum")
    } else if overlaps_approved_feed {
        Some("overlaps_approved_feed")
    } else if overlaps_preferred_active_recipe {
        Some("overlaps_preferred_active_recipe")
    } else if duplicates_active_publication {
        Some("duplicates_active_publication")
    } else {
        None
    }
}

fn company_search_name(name: &str) -> String {
    const SECURITY_MARKERS: &[&str] = &[
        " class a common stock",
        " class b common stock",
        " class c common stock",
        " class a ordinary shares",
        " class b ordinary shares",
        " class c ordinary shares",
        " american depositary shares",
        " american depositary share",
        " american depository shares",
        " american depository share",
        " new york registry shares",
        " ordinary shares",
        " common shares",
        " common share",
        " new common stock",
        " common stock",
        " capital stock",
        " depositary shares",
        " depositary receipts",
        " common units",
        " tangible equity units",
        " tangible equity unit",
        " warrants",
        " warrant",
        " units",
    ];
    let mut cleaned = name.trim().to_owned();
    let lowercase = cleaned.to_ascii_lowercase();
    const TERMINAL_SECURITY_SUFFIXES: &[&str] = &[
        " corporate units",
        " dep shr srs a pfd",
        " preferred shares",
        " preferred share",
        " preferred stock",
        " preference shares",
        " preference share",
        " rights",
        " unit",
    ];
    let marker_index = SECURITY_MARKERS
        .iter()
        .filter_map(|marker| lowercase.find(marker))
        .chain(
            TERMINAL_SECURITY_SUFFIXES
                .iter()
                .filter(|suffix| lowercase.ends_with(**suffix))
                .map(|suffix| lowercase.len().saturating_sub(suffix.len())),
        )
        .min();
    if let Some(marker_index) = marker_index {
        cleaned.truncate(marker_index);
        cleaned = cleaned
            .trim_end_matches(|character: char| {
                character.is_whitespace() || matches!(character, ',' | '-' | ':' | ';')
            })
            .to_owned();

        let words = cleaned.split_whitespace().collect::<Vec<_>>();
        let security_class_index = words
            .iter()
            .rposition(|word| matches!(word.to_ascii_lowercase().as_str(), "class" | "series"));
        if let Some(index) = security_class_index {
            let class_words = &words[index..];
            let class_token = class_words.get(1).map(|word| {
                word.trim_matches(|character: char| !character.is_ascii_alphanumeric())
            });
            let is_short_class_token = class_token.is_some_and(|token| {
                !token.is_empty()
                    && token.len() <= 4
                    && token
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric())
            });
            let is_bounded_security_class = class_words.len() <= 8;
            if is_short_class_token && is_bounded_security_class {
                cleaned = words[..index].join(" ");
            }
        }

        // Exchange datasets sometimes place the issuer jurisdiction before the
        // security description. Remove only the small, known jurisdiction
        // annotations; useful aliases such as "(Acacia Tech)" remain intact.
        const JURISDICTION_SUFFIXES: &[&str] = &[
            " (canada)",
            " (de)",
            " (delaware)",
            " (ireland)",
            " (md)",
            " (tx)",
        ];
        let lowercase = cleaned.to_ascii_lowercase();
        if let Some(suffix) = JURISDICTION_SUFFIXES
            .iter()
            .find(|suffix| lowercase.ends_with(**suffix))
        {
            cleaned.truncate(cleaned.len().saturating_sub(suffix.len()));
            cleaned = cleaned.trim_end().to_owned();
        }
    }
    if cleaned.is_empty() {
        name.trim().to_owned()
    } else {
        cleaned
    }
}

fn company_recipe_identity_key(name: &str) -> String {
    company_search_name(name)
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

pub fn company_names_share_recipe_issuer(left: &str, right: &str) -> bool {
    if company_recipe_identity_key(left) == company_recipe_identity_key(right) {
        return true;
    }

    let issuer_tokens = |name: &str| {
        let mut tokens = company_brand_tokens(name);
        while tokens.last().is_some_and(|token| {
            matches!(
                token.as_str(),
                "adr" | "adrs" | "ads" | "gdr" | "gdrs" | "receipt" | "receipts"
            )
        }) {
            tokens.pop();
        }
        tokens
    };
    let left_tokens = issuer_tokens(left);
    !left_tokens.is_empty() && left_tokens == issuer_tokens(right)
}

fn publication_claim_conflicts(
    company_id: uuid::Uuid,
    company_name: &str,
    claim: &ActiveCompanyNewsPublicationClaim,
) -> bool {
    company_identity_claim_conflicts(
        company_id,
        company_name,
        claim.company_id,
        &claim.company_name,
    )
}

fn company_identity_claim_conflicts(
    company_id: uuid::Uuid,
    company_name: &str,
    claiming_company_id: uuid::Uuid,
    claiming_company_name: &str,
) -> bool {
    claiming_company_id != company_id
        && !company_names_share_recipe_issuer(claiming_company_name, company_name)
}

fn distinct_company_approved_feed_claim<'a>(
    company_id: uuid::Uuid,
    company_name: &str,
    accepted_item_count: usize,
    claims: &'a [ApprovedFeedItemCompanyClaim],
) -> Option<&'a ApprovedFeedItemCompanyClaim> {
    claims.iter().find(|claim| {
        company_identity_claim_conflicts(
            company_id,
            company_name,
            claim.company_id,
            &claim.company_name,
        ) && usize::try_from(claim.matched_item_count).is_ok_and(|matched_item_count| {
            recipe_overlaps_approved_feed(accepted_item_count, matched_item_count)
        })
    })
}

fn distinct_company_approved_source_claim<'a>(
    company_id: uuid::Uuid,
    company_name: &str,
    claims: &'a [ApprovedSourceCompanyClaim],
) -> Option<&'a ApprovedSourceCompanyClaim> {
    claims.iter().find(|claim| {
        company_identity_claim_conflicts(
            company_id,
            company_name,
            claim.company_id,
            &claim.company_name,
        )
    })
}

fn distinct_competing_companies(
    company: &Company,
    candidate_url: &str,
    claims: &[PublicFeedItemCompanyClaim],
    companies: &BTreeMap<uuid::Uuid, Company>,
) -> Vec<Company> {
    let mut seen = HashSet::new();
    let mut competing_companies = claims
        .iter()
        .filter(|claim| claim.candidate_url == candidate_url)
        .filter(|claim| {
            company_identity_claim_conflicts(
                company.id,
                &company.name,
                claim.company_id,
                &claim.company_name,
            )
        })
        .filter(|claim| seen.insert(claim.company_id))
        .filter_map(|claim| companies.get(&claim.company_id).cloned())
        .collect::<Vec<_>>();
    competing_companies.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.id.cmp(&right.id))
    });
    competing_companies
}

fn cross_company_raw_item_scope_rejections(
    company: &Company,
    items: &[RawCrawlItem],
    claims: &[PublicFeedItemCompanyClaim],
    companies: &BTreeMap<uuid::Uuid, Company>,
) -> BTreeMap<String, Vec<String>> {
    items
        .iter()
        .filter_map(|item| {
            let candidate_url = raw_crawl_item_identity_url(item);
            let competing_companies =
                distinct_competing_companies(company, &candidate_url, claims, companies);
            (!competing_companies.is_empty()
                && !raw_item_has_cross_company_scope(company, item, &competing_companies))
            .then(|| {
                (
                    candidate_url,
                    competing_companies
                        .into_iter()
                        .map(|company| company.name)
                        .collect(),
                )
            })
        })
        .collect()
}

fn existing_recipe_is_build_input(recipe: &CompanyNewsRecipe, include_covered: bool) -> bool {
    recipe_state_is_build_input(
        recipe.status,
        recipe.stale_reason.as_deref(),
        recipe.health.rebuild_required,
        &recipe.health.freshness_status,
        include_covered,
    )
}

fn recipe_is_healthy_active(recipe: &CompanyNewsRecipe) -> bool {
    recipe_state_is_healthy_active(
        recipe.status,
        recipe.health.rebuild_required,
        &recipe.health.freshness_status,
    )
}

fn recipe_status_is_rebuild_candidate(status: RecipeStatus, stale_reason: Option<&str>) -> bool {
    matches!(status, RecipeStatus::Active | RecipeStatus::Stale)
        && stale_reason != Some("publication_owned_by_different_company")
}

fn recipe_state_requires_rebuild(
    status: RecipeStatus,
    stale_reason: Option<&str>,
    rebuild_required: bool,
    freshness_status: &str,
) -> bool {
    recipe_status_is_rebuild_candidate(status, stale_reason)
        && (status == RecipeStatus::Stale
            || rebuild_required
            || freshness_status == "content_stale")
}

fn recipe_state_is_build_input(
    status: RecipeStatus,
    stale_reason: Option<&str>,
    rebuild_required: bool,
    freshness_status: &str,
    include_covered: bool,
) -> bool {
    recipe_state_requires_rebuild(status, stale_reason, rebuild_required, freshness_status)
        || (include_covered && recipe_status_is_rebuild_candidate(status, stale_reason))
}

fn recipe_state_is_healthy_active(
    status: RecipeStatus,
    rebuild_required: bool,
    freshness_status: &str,
) -> bool {
    status == RecipeStatus::Active && !rebuild_required && freshness_status != "content_stale"
}

const MIN_COMPANY_SCOPE_RELEVANCE_BPS: usize = 5_000;

fn apply_company_scope_filter(
    company: &Company,
    spec: &CompanyNewsRecipeSpec,
    report: &mut HtmlRecipeCrawlReport,
) {
    let shared_host = shared_news_host_for_company(company, spec.publication_url.host_str());
    let excluded_host = publication_host_is_excluded(company, &spec.publication_url);
    if spec.item_scope == RecipeItemScope::PublicationBoundary
        || (!shared_host
            && !excluded_host
            && publication_host_is_company_related(company, &spec.publication_url))
    {
        return;
    }

    let accepted_before_filter = report.items.len();
    let mut scoped_items = Vec::with_capacity(accepted_before_filter);
    for item in report.items.drain(..) {
        if raw_item_mentions_company(company, &item) {
            scoped_items.push(item);
        } else {
            report.failures.push(ArticleFetchFailure {
                url: item.url,
                reason: "article_not_company_scoped".to_owned(),
                retryable: false,
                error: "article title and URL do not identify the recipe company".to_owned(),
            });
        }
    }
    report.items = scoped_items;

    let relevance_ratio_bps = ratio_bps(report.items.len(), accepted_before_filter);
    recompute_recipe_report_correctness(spec, report);
    if accepted_before_filter > 0
        && relevance_ratio_bps < MIN_COMPANY_SCOPE_RELEVANCE_BPS
        && !report
            .correctness_reasons
            .iter()
            .any(|reason| reason == "company_scope_relevance_below_minimum")
    {
        report
            .correctness_reasons
            .push("company_scope_relevance_below_minimum".to_owned());
    }
}

fn apply_dominant_editorial_namespace_filter(
    spec: &CompanyNewsRecipeSpec,
    report: &mut HtmlRecipeCrawlReport,
) {
    let item_urls = report
        .items
        .iter()
        .map(|item| item.canonical_url.as_ref().unwrap_or(&item.url))
        .collect::<Vec<_>>();
    let Some(namespace) =
        dominant_editorial_descendant_namespace(&spec.publication_url, &item_urls)
    else {
        return;
    };

    let mut scoped_items = Vec::with_capacity(report.items.len());
    for item in report.items.drain(..) {
        let identity_url = item.canonical_url.as_ref().unwrap_or(&item.url);
        if editorial_descendant_namespace(&spec.publication_url, identity_url).as_deref()
            == Some(namespace.as_str())
        {
            scoped_items.push(item);
        } else {
            report.failures.push(ArticleFetchFailure {
                url: item.url,
                reason: "article_outside_dominant_editorial_namespace".to_owned(),
                retryable: false,
                error: format!(
                    "an ambiguous publication root was narrowed to its dominant /{namespace}/ \
                     editorial child"
                ),
            });
        }
    }
    report.items = scoped_items;
    recompute_recipe_report_correctness(spec, report);
}

fn dominant_editorial_descendant_namespace(
    publication_url: &Url,
    item_urls: &[&Url],
) -> Option<String> {
    const MIN_SUPPORT: usize = 5;

    let publication_segments = normalized_url_path_segments(publication_url);
    if !publication_segments.last().is_some_and(|segment| {
        matches!(
            segment.as_str(),
            "engineering" | "innovation" | "technology"
        )
    }) {
        return None;
    }

    let mut support_by_namespace = BTreeMap::<String, usize>::new();
    for item_url in item_urls {
        if let Some(namespace) = editorial_descendant_namespace(publication_url, item_url) {
            *support_by_namespace.entry(namespace).or_default() += 1;
        }
    }
    let (namespace, support) = support_by_namespace.into_iter().max_by(
        |(left_namespace, left_support), (right_namespace, right_support)| {
            left_support
                .cmp(right_support)
                .then_with(|| right_namespace.cmp(left_namespace))
        },
    )?;
    (support >= MIN_SUPPORT && support.saturating_mul(2) > item_urls.len()).then_some(namespace)
}

fn editorial_descendant_namespace(publication_url: &Url, item_url: &Url) -> Option<String> {
    const EXPLICIT_EDITORIAL_CHILDREN: &[&str] = &[
        "article",
        "articles",
        "blog",
        "blogs",
        "insights",
        "news",
        "post",
        "posts",
        "press",
        "press-release",
        "press-releases",
        "research",
        "stories",
    ];

    if !hosts_related(publication_url.host_str(), item_url.host_str()) {
        return None;
    }
    let publication_segments = normalized_url_path_segments(publication_url);
    let item_segments = normalized_url_path_segments(item_url);
    if item_segments.len() <= publication_segments.len()
        || !item_segments.starts_with(&publication_segments)
    {
        return None;
    }
    let child = &item_segments[publication_segments.len()];
    EXPLICIT_EDITORIAL_CHILDREN
        .contains(&child.as_str())
        .then(|| child.clone())
}

fn normalized_url_path_segments(url: &Url) -> Vec<String> {
    url.path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn publication_host_is_company_related(company: &Company, publication_url: &Url) -> bool {
    let host = publication_url.host_str();
    if company_publication_host_policy_matches(company, "excluded_hosts", host) {
        return false;
    }
    if company_publication_host_policy_matches(company, "verified_hosts", host) {
        return true;
    }

    company_identity_matches_host(company, host)
        || company
            .discovery_entry_points()
            .iter()
            .any(|(_, entry)| hosts_related(host, entry.host_str()))
}

fn publication_host_is_excluded(company: &Company, publication_url: &Url) -> bool {
    company_publication_host_policy_matches(company, "excluded_hosts", publication_url.host_str())
}

fn direct_evidence_host_is_excluded(company: &Company, host: Option<&str>) -> bool {
    company_publication_host_policy_matches(company, "direct_evidence_excluded_hosts", host)
}

fn company_publication_host_policy_matches(
    company: &Company,
    field: &str,
    host: Option<&str>,
) -> bool {
    let Some(host) = host else {
        return false;
    };
    company
        .metadata
        .get("publication_host_policy")
        .and_then(|policy| policy.get(field))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(normalized_publication_policy_host)
        .any(|policy_host| hosts_related(Some(host), Some(policy_host.as_str())))
}

fn normalized_publication_policy_host(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(url) = Url::parse(value) {
        return url.host_str().map(normalized_recipe_host);
    }
    let host = value
        .trim_start_matches("//")
        .split('/')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default();
    (!host.is_empty()).then(|| normalized_recipe_host(host))
}

fn adapter_cited_publication_item_scope(
    company: &Company,
    publication_url: &Url,
) -> RecipeItemScope {
    if shared_news_host_for_company(company, publication_url.host_str())
        || !publication_host_is_company_related(company, publication_url)
    {
        RecipeItemScope::CompanyIdentity
    } else {
        RecipeItemScope::PublicationBoundary
    }
}

fn effective_recipe_item_scope(
    company: &Company,
    configured_scope: RecipeItemScope,
    adapter_generated: bool,
    publication_url: &Url,
) -> RecipeItemScope {
    // A shared wire, market-news, or asset-manager host is never a durable
    // publication boundary for one named company/vehicle. This also upgrades
    // legacy adapter recipes that predate the shared-host classification.
    if shared_news_host_for_company(company, publication_url.host_str()) {
        return RecipeItemScope::CompanyIdentity;
    }
    if adapter_generated {
        // A reviewed exclusion always overrides an immutable historical
        // boundary. Otherwise preserve an explicit adapter boundary: many
        // issuers publish under product, brand, acronym, or renamed-company
        // domains that cannot be recovered from name heuristics alone.
        if publication_host_is_excluded(company, publication_url) {
            RecipeItemScope::CompanyIdentity
        } else if configured_scope == RecipeItemScope::PublicationBoundary {
            RecipeItemScope::PublicationBoundary
        } else {
            adapter_cited_publication_item_scope(company, publication_url)
        }
    } else {
        configured_scope
    }
}

fn shared_multi_company_news_host(host: Option<&str>) -> bool {
    const DOMAINS: &[&str] = &[
        "accessnewswire.com",
        "barchart.com",
        "benzinga.com",
        "biospace.com",
        "bloomberg.com",
        "businessinsider.com",
        "businesswire.com",
        "einpresswire.com",
        "eqs-news.com",
        "finance.yahoo.com",
        "fool.com",
        "forbes.com",
        "globenewswire.com",
        "gurufocus.com",
        "investing.com",
        "lasvegassun.com",
        "marketbeat.com",
        "marketscreener.com",
        "marketwatch.com",
        "msn.com",
        "nasdaq.com",
        "natlawreview.com",
        "newmediawire.com",
        "newsfilecorp.com",
        "newswire.ca",
        "newswire.com",
        "pluang.com",
        "prnewswire.com",
        "public.com",
        "quiverquant.com",
        "reuters.com",
        "sahmcapital.com",
        "seekingalpha.com",
        "simplywall.st",
        "stockstotrade.com",
        "stocktitan.net",
        "streetinsider.com",
        "tipranks.com",
        "tradingview.com",
        "zacks.com",
    ];
    host.map(normalized_recipe_host).is_some_and(|host| {
        DOMAINS
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    })
}

fn shared_news_host_for_company(company: &Company, host: Option<&str>) -> bool {
    shared_multi_company_news_host(host) || shared_manager_host_for_vehicle(company, host)
}

fn shared_manager_host_for_vehicle(company: &Company, host: Option<&str>) -> bool {
    const MANAGER_DOMAINS: &[&str] = &[
        "angeloakcapital.com",
        "blackrock.com",
        "blackstone.com",
        "gabelli.com",
        "invesco.com",
        "mfs.com",
        "royceinvest.com",
        "sprott.com",
    ];
    company_requires_composite_vehicle_identity(company)
        && host.map(normalized_recipe_host).is_some_and(|host| {
            MANAGER_DOMAINS
                .iter()
                .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
        })
}

fn company_requires_composite_vehicle_identity(company: &Company) -> bool {
    let name = company_search_name(&company.name).to_ascii_lowercase();
    let name_tokens = name
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<HashSet<_>>();
    name.contains("shares of beneficial interest")
        || name_tokens.contains("trust")
        || name_tokens.contains("fund")
}

fn apply_direct_article_company_scope_filter(
    company: &Company,
    report: &mut HtmlArticleCrawlReport,
) -> usize {
    let accepted_before_filter = report.items.len();
    let mut scoped_items = Vec::with_capacity(accepted_before_filter);
    for item in report.items.drain(..) {
        let shared_host = shared_news_host_for_company(company, item.url.host_str())
            || shared_news_host_for_company(
                company,
                item.canonical_url.as_ref().and_then(|url| url.host_str()),
            );
        let direct_evidence_excluded =
            direct_evidence_host_is_excluded(company, item.url.host_str())
                || direct_evidence_host_is_excluded(
                    company,
                    item.canonical_url.as_ref().and_then(|url| url.host_str()),
                );
        let identity_url = item.canonical_url.as_ref().unwrap_or(&item.url);
        let company_scope_required =
            shared_host || !publication_host_is_company_related(company, identity_url);
        if !direct_evidence_excluded
            && (!company_scope_required || raw_item_mentions_company(company, &item))
        {
            scoped_items.push(item);
        } else {
            let (reason, error) = if direct_evidence_excluded {
                (
                    "article_host_excluded_by_company_policy",
                    "article host is explicitly excluded by the company publication policy",
                )
            } else {
                (
                    "article_not_company_scoped",
                    "article on an unrelated or shared publication does not identify the requested company",
                )
            };
            report.failures.push(ArticleFetchFailure {
                url: item.url,
                reason: reason.to_owned(),
                retryable: false,
                error: error.to_owned(),
            });
        }
    }
    report.items = scoped_items;
    report.failures.sort_by(|left, right| {
        left.url
            .as_str()
            .cmp(right.url.as_str())
            .then_with(|| left.reason.cmp(&right.reason))
    });
    accepted_before_filter.saturating_sub(report.items.len())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FeedCompanyScopeRelevance {
    required: bool,
    feed_title_corroborated: bool,
    off_company_host_item_count: usize,
    off_company_host_ratio_bps: usize,
    total_item_count: usize,
    relevant_item_count: usize,
    relevance_ratio_bps: usize,
    passed: bool,
}

fn feed_company_scope_relevance(
    company: &Company,
    feed_url: &Url,
    feed_title: Option<&str>,
    items: &[feed_core::RawCrawlItem],
) -> FeedCompanyScopeRelevance {
    let shared_host = shared_news_host_for_company(company, feed_url.host_str());
    let feed_title_corroborated = !shared_host
        && feed_title.is_some_and(|title| feed_title_identifies_company(company, title));
    let total_item_count = items.len();
    let off_company_host_item_count = items
        .iter()
        .filter(|item| {
            let identity_url = item.canonical_url.as_ref().unwrap_or(&item.url);
            !publication_host_is_company_related(company, identity_url)
        })
        .count();
    let off_company_host_ratio_bps = ratio_bps(off_company_host_item_count, total_item_count);
    let feed_items_escape_company_host =
        total_item_count > 0 && off_company_host_item_count * 2 > total_item_count;
    let required = shared_host
        || (!feed_title_corroborated
            && (!publication_host_is_company_related(company, feed_url)
                || feed_items_escape_company_host));
    let relevant_item_count = if required {
        items
            .iter()
            .filter(|item| raw_item_mentions_company(company, item))
            .count()
    } else {
        total_item_count
    };
    let relevance_ratio_bps = ratio_bps(relevant_item_count, total_item_count);
    FeedCompanyScopeRelevance {
        required,
        feed_title_corroborated,
        off_company_host_item_count,
        off_company_host_ratio_bps,
        total_item_count,
        relevant_item_count,
        relevance_ratio_bps,
        passed: !required
            || (total_item_count > 0 && relevance_ratio_bps >= MIN_COMPANY_SCOPE_RELEVANCE_BPS),
    }
}

fn feed_title_identifies_company(company: &Company, feed_title: &str) -> bool {
    let title_tokens = feed_title
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if title_tokens.is_empty() {
        return false;
    }

    std::iter::once(company.name.as_str())
        .chain(company.aliases.iter().map(String::as_str))
        .any(|name| {
            let brand_tokens = company_brand_tokens(name)
                .into_iter()
                .filter(|token| !is_company_scope_generic_word(token))
                .collect::<Vec<_>>();
            let exact_brand_phrase = brand_tokens.len() >= 2
                && title_tokens
                    .windows(brand_tokens.len())
                    .any(|window| window == brand_tokens.as_slice());
            let distinctive_single_token = brand_tokens.len() == 1
                && (brand_tokens[0].len() >= 4
                    || brand_tokens[0]
                        .chars()
                        .any(|character| character.is_ascii_digit()))
                && title_tokens.iter().any(|token| token == &brand_tokens[0]);
            let acronym_match = company_brand_acronym(&company_brand_tokens(name))
                .is_some_and(|acronym| title_tokens.iter().any(|token| token == &acronym));
            exact_brand_phrase || distinctive_single_token || acronym_match
        })
}

fn raw_item_mentions_company(company: &Company, item: &feed_core::RawCrawlItem) -> bool {
    let manager_scoped = shared_manager_host_for_vehicle(company, item.url.host_str())
        || shared_manager_host_for_vehicle(
            company,
            item.canonical_url.as_ref().and_then(|url| url.host_str()),
        );
    if manager_scoped {
        return raw_item_mentions_composite_vehicle_identity(company, item);
    }
    let terms = company_scope_identity_terms(company);
    // A fetched page's declared canonical URL is stronger identity evidence
    // than the adapter-supplied request path. Shared release hosts commonly
    // route by a numeric ID and ignore an arbitrary trailing slug; trusting
    // that requested slug would let a fabricated company name pass even when
    // the returned title and canonical URL identify a different issuer.
    let identity_url = item.canonical_url.as_ref().unwrap_or(&item.url);
    let standard_identity_match = !terms.is_empty()
        && (item
            .title
            .as_deref()
            .is_some_and(|title| text_mentions_company_scope_term(title, &terms))
            || url_mentions_company_scope_term(identity_url, &terms));
    standard_identity_match || title_mentions_host_corrob_short_acronym(company, item)
}

fn feed_item_mentions_company(company: &Company, item: &FeedItem) -> bool {
    raw_item_mentions_company(
        company,
        &RawCrawlItem {
            source_item_key: item.external_id.clone(),
            external_id: Some(item.external_id.clone()),
            url: item.url.clone(),
            canonical_url: Some(item.canonical_url.clone()),
            title: Some(item.title.clone()),
            summary_html: None,
            body_html: None,
            published_at: item.published_at,
            payload: Value::Null,
        },
    )
}

pub fn feed_item_has_cross_company_scope(
    company: &Company,
    item: &FeedItem,
    competing_companies: &[Company],
) -> bool {
    raw_item_has_cross_company_scope(
        company,
        &RawCrawlItem {
            source_item_key: item.external_id.clone(),
            external_id: Some(item.external_id.clone()),
            url: item.url.clone(),
            canonical_url: Some(item.canonical_url.clone()),
            title: Some(item.title.clone()),
            summary_html: None,
            body_html: None,
            published_at: item.published_at,
            payload: Value::Null,
        },
        competing_companies,
    )
}

fn raw_item_has_cross_company_scope(
    company: &Company,
    item: &RawCrawlItem,
    competing_companies: &[Company],
) -> bool {
    let identity_url = item.canonical_url.as_ref().unwrap_or(&item.url);
    let host_scoped = !shared_news_host_for_company(company, identity_url.host_str())
        && publication_host_is_company_related(company, identity_url);
    if !host_scoped && !raw_item_mentions_company(company, item) {
        return false;
    }

    let company_has_entry_points = company
        .discovery_entry_points()
        .into_iter()
        .next()
        .is_some();
    let company_entry_host_matches = company
        .discovery_entry_points()
        .into_iter()
        .any(|(_, entry)| hosts_related(identity_url.host_str(), entry.host_str()));
    if !company_has_entry_points || company_entry_host_matches {
        return true;
    }

    let company_tokens = company_collision_identity_tokens(company);
    !competing_companies.iter().any(|competing_company| {
        company_identity_claim_conflicts(
            company.id,
            &company.name,
            competing_company.id,
            &competing_company.name,
        ) && company_collision_identity_is_strict_subset(
            &company_tokens,
            &company_collision_identity_tokens(competing_company),
        ) && !shared_news_host_for_company(competing_company, identity_url.host_str())
            && publication_host_is_company_related(competing_company, identity_url)
            && raw_item_mentions_company(competing_company, item)
    })
}

fn company_collision_identity_tokens(company: &Company) -> HashSet<String> {
    std::iter::once(company.name.as_str())
        .chain(company.aliases.iter().map(String::as_str))
        .flat_map(company_brand_tokens)
        .filter(|token| !is_company_scope_generic_word(token))
        .collect()
}

fn company_collision_identity_is_strict_subset(
    company_tokens: &HashSet<String>,
    competing_tokens: &HashSet<String>,
) -> bool {
    !company_tokens.is_empty()
        && company_tokens.len() < competing_tokens.len()
        && company_tokens.is_subset(competing_tokens)
}

fn raw_item_mentions_composite_vehicle_identity(
    company: &Company,
    item: &feed_core::RawCrawlItem,
) -> bool {
    let identity_url = item.canonical_url.as_ref().unwrap_or(&item.url);
    let evidence_tokens = item
        .title
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(identity_url.path()))
        .flat_map(|value| {
            value
                .split(|character: char| !character.is_ascii_alphanumeric())
                .map(str::to_ascii_lowercase)
                .filter(|token| !token.is_empty())
        })
        .collect::<HashSet<_>>();
    std::iter::once(company.name.as_str())
        .chain(company.aliases.iter().map(String::as_str))
        .any(|name| {
            let mut identity_tokens = company_identity_words(&company_search_name(name));
            identity_tokens.dedup();
            if identity_tokens.len() < 2 {
                return false;
            }
            let required_matches = if identity_tokens.len() >= 4 { 3 } else { 2 };
            let vehicle_specific_match = identity_tokens.iter().any(|token| {
                !is_manager_or_vehicle_class_word(token) && evidence_tokens.contains(token)
            });
            evidence_tokens.contains(&identity_tokens[0])
                && vehicle_specific_match
                && identity_tokens
                    .iter()
                    .filter(|token| evidence_tokens.contains(*token))
                    .count()
                    >= required_matches
        })
}

fn is_manager_or_vehicle_class_word(token: &str) -> bool {
    matches!(
        token,
        "angel"
            | "blackrock"
            | "blackstone"
            | "common"
            | "corp"
            | "corporation"
            | "fund"
            | "funds"
            | "gabelli"
            | "inc"
            | "incorporated"
            | "interest"
            | "invesco"
            | "limited"
            | "ltd"
            | "mfs"
            | "oak"
            | "royce"
            | "shares"
            | "sprott"
            | "stock"
            | "trust"
    )
}

fn title_mentions_host_corrob_short_acronym(
    company: &Company,
    item: &feed_core::RawCrawlItem,
) -> bool {
    let Some(title) = item.title.as_deref() else {
        return false;
    };
    let acronyms = std::iter::once(company.name.as_str())
        .chain(company.aliases.iter().map(String::as_str))
        .filter_map(|name| {
            let tokens = company_brand_tokens(name);
            (tokens.len() == 2)
                .then(|| {
                    tokens
                        .iter()
                        .filter_map(|token| token.chars().next())
                        .collect::<String>()
                })
                .filter(|acronym| acronym.len() == 2)
        })
        .collect::<HashSet<_>>();
    if acronyms.is_empty() {
        return false;
    }
    let corroborated = [&item.url, item.canonical_url.as_ref().unwrap_or(&item.url)]
        .into_iter()
        .filter_map(|url| url.host_str())
        .flat_map(|host| {
            let mut labels = host
                .split('.')
                .map(compact_ascii_alphanumeric)
                .filter(|label| !label.is_empty())
                .collect::<Vec<_>>();
            // Never treat the terminal DNS label as company corroboration
            // (for example a company acronym "AI" on an unrelated `.ai`
            // publication). A matching registrable or subdomain label is
            // still useful independent ownership evidence.
            if labels.len() > 1 {
                labels.pop();
            }
            labels
        })
        .filter(|label| acronyms.contains(label))
        .collect::<HashSet<_>>();
    !corroborated.is_empty()
        && text_mentions_company_scope_term_with_minimum_exact_length(title, &corroborated, 1)
}

fn company_scope_identity_terms(company: &Company) -> HashSet<String> {
    std::iter::once(company.name.as_str())
        .chain(company.aliases.iter().map(String::as_str))
        .flat_map(|name| {
            let cleaned = company_search_name(name);
            let identity_words = company_identity_words(&cleaned)
                .into_iter()
                .filter(|word| company_scope_identity_word_is_distinctive(word));
            let brand_tokens = company_brand_tokens(&cleaned)
                .into_iter()
                .filter(|word| !is_company_scope_annotation_word(word))
                .collect::<Vec<_>>();
            let acronym = company_brand_acronym(&brand_tokens);
            let legal_acronyms = company_brand_legal_acronyms(&cleaned, &brand_tokens);
            identity_words.chain(acronym).chain(legal_acronyms)
        })
        .collect()
}

fn company_scope_identity_word_is_distinctive(word: &str) -> bool {
    !is_company_scope_generic_word(word)
        && !is_company_scope_annotation_word(word)
        && (word.len() >= 3 || word.chars().any(|character| character.is_ascii_digit()))
}

fn is_company_scope_annotation_word(word: &str) -> bool {
    matches!(
        word,
        "aka"
            | "brand"
            | "dba"
            | "due"
            | "fka"
            | "formerly"
            | "in"
            | "incorporating"
            | "informally"
            | "known"
            | "maker"
            | "makers"
            | "name"
            | "named"
            | "prev"
            | "previous"
            | "previously"
            | "process"
            | "product"
            | "pronounced"
            | "renamed"
            | "tbd"
            | "to"
            | "trademark"
            | "was"
    )
}

fn is_company_scope_generic_word(word: &str) -> bool {
    matches!(
        word,
        "advanced"
            | "bio"
            | "biopharma"
            | "capital"
            | "digital"
            | "energy"
            | "financial"
            | "global"
            | "health"
            | "industries"
            | "media"
            | "medical"
            | "new"
            | "pharma"
            | "pharmaceutical"
            | "pharmaceuticals"
            | "resources"
            | "services"
            | "solutions"
            | "therapeutics"
    )
}

fn text_mentions_company_scope_term(text: &str, terms: &HashSet<String>) -> bool {
    text_mentions_company_scope_term_with_minimum_exact_length(text, terms, 1)
}

fn url_mentions_company_scope_term(url: &Url, terms: &HashSet<String>) -> bool {
    // Short path segments are often aggregator identifiers or truncations
    // rather than company identities (for example `/news/Art` for an Artelo
    // article). Keep exact short-name matching in titles, where it has semantic
    // context, but require a less ambiguous token in URLs.
    text_mentions_company_scope_term_with_minimum_exact_length(url.as_str(), terms, 4)
}

fn text_mentions_company_scope_term_with_minimum_exact_length(
    text: &str,
    terms: &HashSet<String>,
    minimum_exact_length: usize,
) -> bool {
    text.split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|token| !token.is_empty())
        .any(|token| {
            terms.iter().any(|term| {
                (term.len() >= minimum_exact_length && token == *term)
                    || (token.len() >= 5
                        && term.len() >= 5
                        && token.len().abs_diff(term.len()) <= 3
                        && (token.starts_with(term) || term.starts_with(&token)))
            })
        })
}

fn recompute_recipe_report_correctness(
    spec: &CompanyNewsRecipeSpec,
    report: &mut HtmlRecipeCrawlReport,
) {
    const RECOMPUTED_REASONS: &[&str] = &[
        "discovered_items_below_minimum",
        "accepted_items_below_minimum",
        "acceptance_ratio_below_minimum",
        "title_diversity_below_minimum",
        "content_diversity_below_minimum",
        "company_scope_relevance_below_minimum",
    ];
    report
        .correctness_reasons
        .retain(|reason| !RECOMPUTED_REASONS.contains(&reason.as_str()));
    report
        .failures
        .sort_by(|left, right| left.url.as_str().cmp(right.url.as_str()));
    report.accepted_item_count = report.items.len();
    report.distinct_title_count = report
        .items
        .iter()
        .filter_map(|item| item.title.as_deref())
        .map(|title| {
            title
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase()
        })
        .collect::<HashSet<_>>()
        .len();
    report.distinct_content_count = distinct_sanitized_content_count(&report.items);
    report.rejected_url_count = report.failures.len();
    report.acceptance_ratio_bps = if report.discovered_url_count == 0 {
        0
    } else {
        u16::try_from(ratio_bps(
            report.accepted_item_count,
            report.discovered_url_count,
        ))
        .unwrap_or(10_000)
    };
    report.latest_published_at = report
        .items
        .iter()
        .filter_map(|item| item.published_at)
        .max();
    report.dated_item_count = report
        .items
        .iter()
        .filter(|item| item.published_at.is_some())
        .count();
    report.publication_date_coverage_complete =
        report.accepted_item_count > 0 && report.dated_item_count == report.accepted_item_count;
    report.content_stale = report.publication_date_coverage_complete
        && report.latest_published_at.is_some_and(|published_at| {
            published_at
                < Utc::now()
                    - Duration::seconds(i64::from(spec.freshness.content_stale_after_seconds))
        });

    if report.discovered_url_count
        < usize::try_from(spec.correctness.min_discovered_items).unwrap_or(usize::MAX)
    {
        report
            .correctness_reasons
            .push("discovered_items_below_minimum".to_owned());
    }
    if report.accepted_item_count
        < usize::try_from(spec.correctness.min_accepted_items).unwrap_or(usize::MAX)
    {
        report
            .correctness_reasons
            .push("accepted_items_below_minimum".to_owned());
    }
    if report.acceptance_ratio_bps < spec.correctness.min_acceptance_ratio_bps {
        report
            .correctness_reasons
            .push("acceptance_ratio_below_minimum".to_owned());
    }
    if report.accepted_item_count >= 3
        && report.distinct_title_count.saturating_mul(2) < report.accepted_item_count
    {
        report
            .correctness_reasons
            .push("title_diversity_below_minimum".to_owned());
    }
    if report.accepted_item_count >= 3
        && report.distinct_content_count.saturating_mul(2) < report.accepted_item_count
    {
        report
            .correctness_reasons
            .push("content_diversity_below_minimum".to_owned());
    }
}

fn company_news_content_metrics<'a>(
    items: impl Iterator<Item = &'a feed_core::RawCrawlItem>,
) -> Value {
    let lengths = items
        .filter_map(|item| {
            item.payload
                .get("sanitized_content_chars")
                .and_then(Value::as_u64)
        })
        .collect::<Vec<_>>();
    let total = lengths.iter().sum::<u64>();
    json!({
        "accepted_pages": lengths.len(),
        "min_sanitized_content_chars": lengths.iter().min(),
        "max_sanitized_content_chars": lengths.iter().max(),
        "avg_sanitized_content_chars": if lengths.is_empty() {
            0
        } else {
            total / u64::try_from(lengths.len()).unwrap_or(1)
        },
        "title_only_pages": 0,
        "empty_body_pages": 0,
    })
}

#[derive(Clone)]
pub struct CrawlJobHandler {
    database: Database,
    crawler: RssAtomCrawler,
    recipe_crawler: HtmlRecipeCrawler,
}

impl CrawlJobHandler {
    pub fn new(
        database: Database,
        crawler: RssAtomCrawler,
        recipe_crawler: HtmlRecipeCrawler,
    ) -> Self {
        Self {
            database,
            crawler,
            recipe_crawler,
        }
    }

    async fn prepare_cross_company_item_scope(
        &self,
        current_company: &Company,
        claims: &[PublicFeedItemCompanyClaim],
    ) -> Result<BTreeMap<uuid::Uuid, Company>, JobHandlerError> {
        let mut companies = BTreeMap::from([(current_company.id, current_company.clone())]);
        let has_distinct_claim = claims.iter().any(|claim| {
            company_identity_claim_conflicts(
                current_company.id,
                &current_company.name,
                claim.company_id,
                &claim.company_name,
            )
        });
        if !has_distinct_claim {
            return Ok(companies);
        }

        for company_id in claims
            .iter()
            .map(|claim| claim.company_id)
            .collect::<HashSet<_>>()
        {
            if companies.contains_key(&company_id) {
                continue;
            }
            let company = self
                .database
                .get_company(company_id)
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                .ok_or_else(|| {
                    JobHandlerError::permanent(format!(
                        "public feed item claim references missing company {company_id}"
                    ))
                })?;
            companies.insert(company_id, company);
        }

        let mut quarantines = BTreeMap::<uuid::Uuid, FeedItemQualityQuarantine>::new();
        for claim in claims {
            let Some(claiming_company) = companies.get(&claim.company_id) else {
                continue;
            };
            let mut competing_companies = distinct_competing_companies(
                claiming_company,
                &claim.candidate_url,
                claims,
                &companies,
            );
            if company_identity_claim_conflicts(
                claiming_company.id,
                &claiming_company.name,
                current_company.id,
                &current_company.name,
            ) && !competing_companies
                .iter()
                .any(|company| company.id == current_company.id)
            {
                competing_companies.push(current_company.clone());
                competing_companies.sort_by(|left, right| {
                    left.name
                        .cmp(&right.name)
                        .then_with(|| left.id.cmp(&right.id))
                });
            }
            if competing_companies.is_empty() {
                continue;
            }
            let item = self
                .database
                .get_feed_item(claim.feed_item_id)
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
            let Some(item) = item else {
                continue;
            };
            if feed_item_has_cross_company_scope(claiming_company, &item, &competing_companies) {
                continue;
            }
            let competing_company_names = competing_companies
                .iter()
                .map(|company| company.name.clone())
                .collect::<Vec<_>>();
            quarantines.insert(
                item.id,
                FeedItemQualityQuarantine {
                    feed_item_id: item.id,
                    reason: "cross_company_item_not_explicitly_scoped".to_owned(),
                    policy: "cross-company-item-scope.v1".to_owned(),
                    reversible: true,
                    metadata: json!({
                        "candidate_url": claim.candidate_url,
                        "claiming_company_id": claiming_company.id,
                        "claiming_company_name": claiming_company.name,
                        "competing_company_names": competing_company_names,
                        "prospective_company_id": current_company.id,
                        "prospective_company_name": current_company.name,
                    }),
                },
            );
        }
        let quarantined_count = self
            .database
            .quarantine_feed_items_for_quality(&quarantines.into_values().collect::<Vec<_>>())
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
        if quarantined_count > 0 {
            info!(
                company_id = %current_company.id,
                company_name = %current_company.name,
                quarantined_count,
                "quarantined existing public items without distinct-issuer scope"
            );
        }
        Ok(companies)
    }

    async fn quarantine_compromised_publication_items(
        &self,
        company: &Company,
        source: &Source,
        assessment: PublicationTopicCompromise,
        trigger: &str,
    ) -> Result<u64, JobHandlerError> {
        let items = self
            .database
            .list_feed_items(Some(company.id), Some(source.id), 10_000, 0)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
        let quarantines = items
            .into_iter()
            .map(|item| FeedItemQualityQuarantine {
                feed_item_id: item.id,
                reason: "publication_topic_compromise_detected".to_owned(),
                policy: "publication-topic-compromise.v1".to_owned(),
                reversible: true,
                metadata: json!({
                    "company_name": company.name,
                    "source_url": source.url,
                    "sample_item_count": assessment.sample_item_count,
                    "suspicious_item_count": assessment.suspicious_item_count,
                    "trigger": trigger,
                }),
            })
            .collect::<Vec<_>>();
        self.database
            .quarantine_feed_items_for_quality(&quarantines)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))
    }
}

#[async_trait]
impl JobHandler for CrawlJobHandler {
    fn supported_job_types(&self) -> &[JobType] {
        &[JobType::CrawlSource]
    }

    async fn handle(&self, job: &Job) -> Result<(), JobHandlerError> {
        let source_id = job
            .source_id
            .ok_or_else(|| JobHandlerError::permanent("crawl_source job is missing source_id"))?;
        let source = self
            .database
            .get_source(source_id)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?
            .ok_or_else(|| {
                JobHandlerError::permanent(format!("source {source_id} does not exist"))
            })?;
        if source.status != SourceStatus::Approved {
            return Err(JobHandlerError::permanent(format!(
                "source {} is not approved",
                source.source_id
            )));
        }
        let recipe = if matches!(source.kind, SourceKind::Html | SourceKind::Browser) {
            Some(
                self.database
                    .get_active_company_news_recipe_for_source(source.id)
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                    .ok_or_else(|| {
                        JobHandlerError::permanent(format!(
                            "source {} has no active crawl recipe",
                            source.source_id
                        ))
                    })?,
            )
        } else {
            None
        };
        let run_id = self
            .database
            .begin_crawl_run(source.id, job.id)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?;

        if let Some(recipe) = recipe {
            return self
                .handle_recipe_crawl(job, &source, run_id, &recipe)
                .await;
        }

        match self.crawler.crawl(&source).await {
            Ok(mut batch) => {
                let title_diversity = feed_title_diversity(&batch.items);
                let title_quality_error = if !batch.items.is_empty()
                    && title_diversity.usable_titled_item_count == 0
                {
                    Some(
                        "feed contains no usable article titles after CMS placeholder filtering"
                            .to_owned(),
                    )
                } else if !title_diversity.passed {
                    Some(format!(
                        "feed title diversity below minimum: {} items expose only {} distinct titles",
                        batch.items.len(),
                        title_diversity.distinct_titled_item_count
                    ))
                } else {
                    None
                };
                if let Some(error) = title_quality_error {
                    self.database
                        .fail_crawl_run(run_id, &source, &error)
                        .await
                        .map_err(|database_error| {
                            JobHandlerError::retryable(format!(
                                "{error}; additionally failed to close crawl run: {database_error}"
                            ))
                        })?;
                    return Err(deterministic_feed_quality_failure(error));
                }
                if feed_scope_is_non_editorial(&source.url, &batch) {
                    let error =
                        "feed is dominated by documentation, forum, comment, or operational items"
                            .to_owned();
                    self.database
                        .fail_crawl_run(run_id, &source, &error)
                        .await
                        .map_err(|database_error| {
                            JobHandlerError::retryable(format!(
                                "{error}; additionally failed to close crawl run: {database_error}"
                            ))
                        })?;
                    return Err(deterministic_feed_quality_failure(error));
                }
                let company = self
                    .database
                    .get_company(source.company_id)
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                    .ok_or_else(|| {
                        JobHandlerError::permanent(format!(
                            "company {} does not exist",
                            source.company_id
                        ))
                    })?;
                let topic_compromise = publication_topic_compromise(&company, &batch.items);
                if topic_compromise.detected {
                    let quarantined_item_count = self
                        .quarantine_compromised_publication_items(
                            &company,
                            &source,
                            topic_compromise,
                            "runtime_feed_crawl",
                        )
                        .await?;
                    let error = format!(
                        "publication topic compromise detected: {} of {} sampled items contain \
                         gambling SEO content unrelated to {}; quarantined {} existing items",
                        topic_compromise.suspicious_item_count,
                        topic_compromise.sample_item_count,
                        company.name,
                        quarantined_item_count
                    );
                    self.database
                        .fail_crawl_run(run_id, &source, &error)
                        .await
                        .map_err(|database_error| {
                            JobHandlerError::retryable(format!(
                                "{error}; additionally failed to close crawl run: {database_error}"
                            ))
                        })?;
                    return Err(deterministic_feed_quality_failure(error));
                }
                let batch_item_urls = batch
                    .items
                    .iter()
                    .map(raw_crawl_item_identity_url)
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                let public_item_company_claims = self
                    .database
                    .list_public_feed_item_company_claims(&batch_item_urls)
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                let claim_companies = self
                    .prepare_cross_company_item_scope(&company, &public_item_company_claims)
                    .await?;
                let cross_company_scope_rejections = cross_company_raw_item_scope_rejections(
                    &company,
                    &batch.items,
                    &public_item_company_claims,
                    &claim_companies,
                );
                if !cross_company_scope_rejections.is_empty() {
                    batch.items.retain(|item| {
                        !cross_company_scope_rejections
                            .contains_key(&raw_crawl_item_identity_url(item))
                    });
                }
                if let Some(metadata) = batch.metadata.as_object_mut() {
                    metadata.insert(
                        "cross_company_scope_policy".to_owned(),
                        json!("cross-company-item-scope.v1"),
                    );
                    metadata.insert(
                        "cross_company_scope_rejected_item_count".to_owned(),
                        json!(cross_company_scope_rejections.len()),
                    );
                    metadata.insert(
                        "cross_company_scope_rejections".to_owned(),
                        json!(cross_company_scope_rejections),
                    );
                }
                let batch_item_urls = batch
                    .items
                    .iter()
                    .map(raw_crawl_item_identity_url)
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                let approved_feed_company_claims = self
                    .database
                    .list_approved_feed_item_company_claims(&batch_item_urls)
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                if let Some(claim) = distinct_company_approved_feed_claim(
                    company.id,
                    &company.name,
                    batch_item_urls.len(),
                    &approved_feed_company_claims,
                ) {
                    let error = format!(
                        "feed article identities are already claimed by {} for a distinct issuer",
                        claim.company_name
                    );
                    self.database
                        .fail_crawl_run(run_id, &source, &error)
                        .await
                        .map_err(|database_error| {
                            JobHandlerError::retryable(format!(
                                "{error}; additionally failed to close crawl run: {database_error}"
                            ))
                        })?;
                    return Err(deterministic_feed_quality_failure(error));
                }
                let company_scope_relevance = feed_company_scope_relevance(
                    &company,
                    &source.url,
                    batch.metadata.get("feed_title").and_then(Value::as_str),
                    &batch.items,
                );
                if !company_scope_relevance.passed {
                    let error = format!(
                        "shared multi-company feed company scope below minimum: {} of {} items \
                         identify {}",
                        company_scope_relevance.relevant_item_count,
                        company_scope_relevance.total_item_count,
                        company.name
                    );
                    self.database
                        .fail_crawl_run(run_id, &source, &error)
                        .await
                        .map_err(|database_error| {
                            JobHandlerError::retryable(format!(
                                "{error}; additionally failed to close crawl run: {database_error}"
                            ))
                        })?;
                    return Err(deterministic_feed_quality_failure(error));
                }
                if company_scope_relevance.required {
                    batch
                        .items
                        .retain(|item| raw_item_mentions_company(&company, item));
                    if let Some(metadata) = batch.metadata.as_object_mut() {
                        metadata.insert("company_scope_required".to_owned(), json!(true));
                        metadata.insert(
                            "company_scope_relevant_item_count".to_owned(),
                            json!(company_scope_relevance.relevant_item_count),
                        );
                        metadata.insert(
                            "company_scope_total_item_count".to_owned(),
                            json!(company_scope_relevance.total_item_count),
                        );
                        metadata.insert(
                            "company_scope_relevance_ratio_bps".to_owned(),
                            json!(company_scope_relevance.relevance_ratio_bps),
                        );
                        metadata.insert(
                            "company_scope_feed_title_corroborated".to_owned(),
                            json!(company_scope_relevance.feed_title_corroborated),
                        );
                        metadata.insert(
                            "company_scope_off_company_host_item_count".to_owned(),
                            json!(company_scope_relevance.off_company_host_item_count),
                        );
                        metadata.insert(
                            "company_scope_off_company_host_ratio_bps".to_owned(),
                            json!(company_scope_relevance.off_company_host_ratio_bps),
                        );
                    }
                }
                let repeated_content_urls = repeated_sanitized_content_urls(&batch.items);
                if let Some(metadata) = batch.metadata.as_object_mut() {
                    metadata.insert(
                        "repeated_sanitized_content_item_count".to_owned(),
                        json!(repeated_content_urls.len()),
                    );
                    metadata.insert(
                        "content_diversity_policy".to_owned(),
                        json!("feed-content-diversity.v1"),
                    );
                }
                let mut detected_source = source.clone();
                detected_source.kind = batch.detected_source_kind;
                let items = batch
                    .items
                    .into_iter()
                    .map(|raw| {
                        let normalized = if repeated_content_urls.contains(raw.url.as_str()) {
                            Err("quality quarantine: repeated_sanitized_content".to_owned())
                        } else {
                            normalize_item(&detected_source, &raw, batch.fetched_at)
                                .map_err(|error| error.to_string())
                        };
                        ProcessedCrawlItem { normalized, raw }
                    })
                    .collect::<Vec<_>>();
                self.database
                    .complete_crawl_run(
                        run_id,
                        &detected_source,
                        batch.fetched_at,
                        &items,
                        batch.metadata,
                    )
                    .await
                    .map(|_| ())
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))
            }
            Err(error) => {
                self.database
                    .fail_crawl_run(run_id, &source, &error.to_string())
                    .await
                    .map_err(|database_error| {
                        JobHandlerError::retryable(format!(
                            "{error}; additionally failed to close crawl run: {database_error}"
                        ))
                    })?;
                Err(classify_crawl_error(error))
            }
        }
    }
}

impl CrawlJobHandler {
    async fn handle_recipe_crawl(
        &self,
        job: &Job,
        source: &Source,
        crawl_run_id: uuid::Uuid,
        recipe: &CompanyNewsRecipe,
    ) -> Result<(), JobHandlerError> {
        let company = self
            .database
            .get_company(recipe.company_id)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?
            .ok_or_else(|| {
                JobHandlerError::permanent(format!("company {} does not exist", recipe.company_id))
            })?;
        let current_order = (recipe.verified_at.unwrap_or(recipe.created_at), recipe.id);
        let current_publication_identity = publication_identity_key(&recipe.spec.publication_url);
        let publication_claims = self
            .database
            .list_active_company_news_publication_claims(recipe.spec.publication_url.as_str())
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
        let publication_claimed_by_distinct_company = publication_claims
            .iter()
            .any(|claim| publication_claim_conflicts(recipe.company_id, &company.name, claim));
        let duplicates_active_publication = self
            .database
            .list_company_news_recipes(
                Some(recipe.company_id),
                Some(RecipeStatus::Active),
                1_000,
                0,
            )
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?
            .into_iter()
            .any(|other| {
                other.id != recipe.id
                    && !other.health.rebuild_required
                    && (other.verified_at.unwrap_or(other.created_at), other.id) < current_order
                    && publication_identity_key(&other.spec.publication_url)
                        == current_publication_identity
            });
        let recipe_run_id = self
            .database
            .begin_company_news_recipe_run(recipe.id, source.id, job.id, crawl_run_id)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
        let mut effective_spec = recipe.spec.clone();
        effective_spec.item_scope = effective_recipe_item_scope(
            &company,
            recipe.spec.item_scope,
            recipe.generated_by_run_id.is_some(),
            &recipe.spec.publication_url,
        );
        effective_spec.allowed_hosts = effective_recipe_allowed_hosts(
            &recipe.spec.allowed_hosts,
            recipe.generated_by_run_id.is_some(),
            &recipe.spec.publication_url,
            &recipe.spec.evidence_article_urls,
        );
        effective_spec.include_path_prefixes = effective_recipe_include_path_prefixes(
            &recipe.spec.include_path_prefixes,
            recipe.generated_by_run_id.is_some(),
            &recipe.spec.publication_url,
            &recipe.spec.evidence_article_urls,
        );
        let publication_host_excluded =
            publication_host_is_excluded(&company, &effective_spec.publication_url);
        match self.recipe_crawler.crawl(&effective_spec).await {
            Ok(mut report) => {
                let company_scope_required = effective_spec.item_scope
                    == RecipeItemScope::CompanyIdentity
                    && (shared_news_host_for_company(
                        &company,
                        effective_spec.publication_url.host_str(),
                    ) || publication_host_excluded
                        || !publication_host_is_company_related(
                            &company,
                            &effective_spec.publication_url,
                        ));
                let company_scope_total_item_count = report.items.len();
                apply_company_scope_filter(&company, &effective_spec, &mut report);
                let company_scope_relevant_item_count = report.items.len();
                let company_scope_rejected_item_count = company_scope_total_item_count
                    .saturating_sub(company_scope_relevant_item_count);
                let company_scope_relevance_ratio_bps = ratio_bps(
                    company_scope_relevant_item_count,
                    company_scope_total_item_count,
                );
                apply_dominant_editorial_namespace_filter(&effective_spec, &mut report);
                if let Some(topic_compromise) = apply_recipe_publication_topic_compromise_filter(
                    &company,
                    &effective_spec,
                    &mut report,
                ) {
                    let quarantined_item_count = self
                        .quarantine_compromised_publication_items(
                            &company,
                            source,
                            topic_compromise,
                            "runtime_recipe_crawl",
                        )
                        .await?;
                    warn!(
                        company_id = %company.id,
                        company_name = %company.name,
                        source_id = %source.id,
                        sample_item_count = topic_compromise.sample_item_count,
                        suspicious_item_count = topic_compromise.suspicious_item_count,
                        quarantined_item_count,
                        "quarantined publication after topic-compromise detection"
                    );
                }
                if publication_host_excluded {
                    report
                        .correctness_reasons
                        .push("publication_host_excluded_by_company_policy".to_owned());
                }
                if duplicates_active_publication {
                    report
                        .correctness_reasons
                        .push("duplicates_active_publication".to_owned());
                }
                if publication_claimed_by_distinct_company {
                    report
                        .correctness_reasons
                        .push("publication_claimed_by_distinct_company".to_owned());
                }
                if !publication_scope_has_editorial_evidence(
                    &effective_spec.publication_url,
                    &effective_spec.evidence_article_urls,
                    &report,
                ) {
                    report.correctness_reasons.push(
                        "publication_url_lacks_editorial_scope_and_collection_evidence".to_owned(),
                    );
                }
                if !organizational_scope_has_editorial_evidence(
                    &effective_spec.publication_url,
                    &report,
                ) {
                    report
                        .correctness_reasons
                        .push("organizational_page_lacks_editorial_collection_evidence".to_owned());
                }
                if publication_matches_evidence_article(&effective_spec)
                    || (effective_spec.include_path_prefixes.is_empty()
                        && looks_like_article_detail_url(&effective_spec.publication_url))
                {
                    report
                        .correctness_reasons
                        .push("article_url_used_as_publication".to_owned());
                }
                let cross_company_candidate_urls = report
                    .items
                    .iter()
                    .map(raw_crawl_item_identity_url)
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                let public_item_company_claims = self
                    .database
                    .list_public_feed_item_company_claims(&cross_company_candidate_urls)
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                let claim_companies = self
                    .prepare_cross_company_item_scope(&company, &public_item_company_claims)
                    .await?;
                let cross_company_scope_rejections = cross_company_raw_item_scope_rejections(
                    &company,
                    &report.items,
                    &public_item_company_claims,
                    &claim_companies,
                );
                let cross_company_scope_rejected_item_count = cross_company_scope_rejections.len();
                if cross_company_scope_rejected_item_count > 0 {
                    let mut retained_items = Vec::with_capacity(
                        report.items.len() - cross_company_scope_rejected_item_count,
                    );
                    for item in report.items.drain(..) {
                        let identity_url = raw_crawl_item_identity_url(&item);
                        if cross_company_scope_rejections.contains_key(&identity_url) {
                            report.failures.push(ArticleFetchFailure {
                                url: item.canonical_url.clone().unwrap_or(item.url),
                                reason: "cross_company_item_not_explicitly_scoped".to_owned(),
                                retryable: false,
                                error: "article identity is public for a distinct issuer and \
                                        this association lacks company-specific title, path, or \
                                        first-party host evidence"
                                    .to_owned(),
                            });
                        } else {
                            retained_items.push(item);
                        }
                    }
                    report.items = retained_items;
                    recompute_recipe_report_correctness(&effective_spec, &mut report);
                }
                let cross_company_scope_rejection_metadata = json!(&cross_company_scope_rejections);
                let mut detected_source = source.clone();
                detected_source.kind = SourceKind::Html;
                let processed = report
                    .items
                    .iter()
                    .cloned()
                    .map(|raw| ProcessedCrawlItem {
                        normalized: normalize_item(&detected_source, &raw, report.fetched_at)
                            .map_err(|error| error.to_string()),
                        raw,
                    })
                    .collect::<Vec<_>>();
                let normalization_failures = processed
                    .iter()
                    .filter(|item| item.normalized.is_err())
                    .count();
                if normalization_failures > 0 {
                    report
                        .correctness_reasons
                        .push("article_normalization_failed".to_owned());
                }
                let report_item_urls = report
                    .items
                    .iter()
                    .map(raw_crawl_item_identity_url)
                    .collect::<Vec<_>>();
                let report_signature_candidates = report
                    .items
                    .iter()
                    .filter_map(raw_crawl_item_signature_candidate)
                    .collect::<Vec<_>>();
                let approved_feed_company_claims = self
                    .database
                    .list_approved_feed_item_company_claims(&report_item_urls)
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                let distinct_company_approved_feed_claim = distinct_company_approved_feed_claim(
                    recipe.company_id,
                    &company.name,
                    report.accepted_item_count,
                    &approved_feed_company_claims,
                )
                .cloned();
                if distinct_company_approved_feed_claim.is_some() {
                    report
                        .correctness_reasons
                        .push("publication_items_claimed_by_distinct_company_feed".to_owned());
                }
                let approved_feed_signature_matches = self
                    .database
                    .list_approved_feed_item_signature_matches(
                        source.company_id,
                        &report_signature_candidates,
                    )
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                    .into_iter()
                    .collect::<HashSet<_>>();
                let mut approved_feed_matches = self
                    .database
                    .list_approved_feed_item_url_matches(source.company_id, &report_item_urls)
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                    .into_iter()
                    .collect::<HashSet<_>>();
                approved_feed_matches.extend(approved_feed_signature_matches.iter().cloned());
                let approved_feed_overlap_count = approved_feed_matches.len();
                let active_recipe_signature_matches = self
                    .database
                    .list_preferred_active_recipe_item_signature_matches(
                        source.company_id,
                        recipe.id,
                        &report_signature_candidates,
                    )
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                    .into_iter()
                    .collect::<HashSet<_>>();
                let mut active_recipe_matches = self
                    .database
                    .list_preferred_active_recipe_item_url_matches(
                        source.company_id,
                        recipe.id,
                        &report_item_urls,
                    )
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                    .into_iter()
                    .collect::<HashSet<_>>();
                active_recipe_matches.extend(active_recipe_signature_matches.iter().cloned());
                let active_recipe_overlap_count = active_recipe_matches.len();
                let overlaps_approved_feed = recipe_overlaps_approved_feed(
                    report.accepted_item_count,
                    approved_feed_overlap_count,
                );
                let overlaps_preferred_active_recipe = recipe_overlaps_active_recipe(
                    report.accepted_item_count,
                    active_recipe_overlap_count,
                );
                if overlaps_approved_feed {
                    report
                        .correctness_reasons
                        .push("overlaps_approved_feed".to_owned());
                }
                if overlaps_preferred_active_recipe {
                    report
                        .correctness_reasons
                        .push("overlaps_active_recipe".to_owned());
                }
                let historical_scope_failures = if company_scope_required {
                    self.database
                        .list_feed_items(Some(company.id), Some(source.id), 10_000, 0)
                        .await
                        .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                        .into_iter()
                        .filter(|item| !feed_item_mentions_company(&company, item))
                        .map(|item| RecipeArtifactFailure {
                            url: item.canonical_url.as_str().to_owned(),
                            reason: "article_not_company_scoped".to_owned(),
                            retryable: false,
                        })
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                let artifact_failures = report
                    .failures
                    .iter()
                    .map(|failure| RecipeArtifactFailure {
                        url: failure.url.as_str().to_owned(),
                        reason: failure.reason.clone(),
                        retryable: failure.retryable,
                    })
                    .chain(historical_scope_failures)
                    .collect::<Vec<_>>();
                let quality_quarantined_item_count = self
                    .database
                    .quarantine_recipe_artifact_failures(
                        source,
                        recipe.id,
                        recipe_run_id,
                        &artifact_failures,
                    )
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                if quality_quarantined_item_count > 0 {
                    info!(
                        recipe_id = %recipe.id,
                        source_id = %source.id,
                        quality_quarantined_item_count,
                        "quarantined previously public items rejected as listing artifacts"
                    );
                }

                if !report.correctness_passed() {
                    let transient_fetch_error = recipe_correctness_blocked_by_retryable_fetches(
                        &report,
                        &effective_spec.correctness,
                    )
                    .then(|| transient_recipe_fetch_error(&report));
                    let reason = transient_fetch_error
                        .clone()
                        .unwrap_or_else(|| report.correctness_reasons.join(", "));
                    self.database
                        .fail_crawl_run(crawl_run_id, source, &reason)
                        .await
                        .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                    let mut completion = recipe_completion_from_report(
                        &report,
                        0,
                        0,
                        json!({
                            "published": false,
                            "content_stale": report.content_stale,
                            "dated_item_count": report.dated_item_count,
                            "publication_date_coverage_complete":
                                report.publication_date_coverage_complete,
                            "distinct_title_count": report.distinct_title_count,
                            "distinct_content_count": report.distinct_content_count,
                            "approved_feed_overlap_count": approved_feed_overlap_count,
                            "approved_feed_signature_overlap_count":
                                approved_feed_signature_matches.len(),
                            "approved_feed_overlap_ratio_bps": ratio_bps(
                                approved_feed_overlap_count,
                                report.accepted_item_count,
                            ),
                            "active_recipe_overlap_count": active_recipe_overlap_count,
                            "active_recipe_signature_overlap_count":
                                active_recipe_signature_matches.len(),
                            "active_recipe_overlap_ratio_bps": ratio_bps(
                                active_recipe_overlap_count,
                                report.accepted_item_count,
                            ),
                            "quality_quarantined_item_count":
                                quality_quarantined_item_count,
                            "effective_item_scope": effective_spec.item_scope,
                            "company_scope_required": company_scope_required,
                            "company_scope_total_item_count":
                                company_scope_total_item_count,
                            "company_scope_relevant_item_count":
                                company_scope_relevant_item_count,
                            "company_scope_rejected_item_count":
                                company_scope_rejected_item_count,
                            "company_scope_relevance_ratio_bps":
                                company_scope_relevance_ratio_bps,
                            "cross_company_scope_policy":
                                "cross-company-item-scope.v1",
                            "cross_company_scope_rejected_item_count":
                                cross_company_scope_rejected_item_count,
                            "cross_company_scope_rejections":
                                cross_company_scope_rejection_metadata,
                            "distinct_company_approved_feed_claim":
                                distinct_company_approved_feed_claim.as_ref(),
                            "failures": report.failures,
                        }),
                    )?;
                    if let Some(error) = &transient_fetch_error {
                        completion.error = Some(error.clone());
                        completion.transient_error = true;
                    }
                    let outcome = self
                        .database
                        .complete_company_news_recipe_run(recipe_run_id, recipe, &completion)
                        .await
                        .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                    if let Some(error) = transient_fetch_error {
                        warn!(
                            recipe_id = %recipe.id,
                            source_id = %source.id,
                            reasons = ?report.correctness_reasons,
                            "transient article fetch failures blocked recipe validation; retrying"
                        );
                        return Err(JobHandlerError::retryable(error));
                    }
                    let company_scope_below_minimum = report
                        .correctness_reasons
                        .iter()
                        .any(|reason| reason == "company_scope_relevance_below_minimum");
                    let supersession_reason = if publication_host_excluded {
                        Some("publication_host_excluded_by_company_policy")
                    } else if distinct_company_approved_feed_claim.is_some() {
                        Some("publication_items_claimed_by_distinct_company_feed")
                    } else {
                        runtime_recipe_supersession_reason(
                            company_scope_below_minimum,
                            overlaps_approved_feed,
                            overlaps_preferred_active_recipe,
                            duplicates_active_publication,
                        )
                    };
                    let retirement_metadata = json!({
                        "trigger": "runtime_recipe_correctness",
                        "recipe_run_id": recipe_run_id,
                        "approved_feed_overlap_count":
                            approved_feed_overlap_count,
                        "approved_feed_signature_overlap_count":
                            approved_feed_signature_matches.len(),
                        "approved_feed_overlap_ratio_bps": ratio_bps(
                            approved_feed_overlap_count,
                            report.accepted_item_count,
                        ),
                        "active_recipe_overlap_count":
                            active_recipe_overlap_count,
                        "active_recipe_signature_overlap_count":
                            active_recipe_signature_matches.len(),
                        "active_recipe_overlap_ratio_bps": ratio_bps(
                            active_recipe_overlap_count,
                            report.accepted_item_count,
                        ),
                        "distinct_company_approved_feed_claim":
                            distinct_company_approved_feed_claim.as_ref(),
                        "publication_host_excluded":
                            publication_host_excluded,
                        "effective_item_scope": effective_spec.item_scope,
                        "company_scope_required": company_scope_required,
                        "company_scope_total_item_count":
                            company_scope_total_item_count,
                        "company_scope_relevant_item_count":
                            company_scope_relevant_item_count,
                        "company_scope_rejected_item_count":
                            company_scope_rejected_item_count,
                        "company_scope_relevance_ratio_bps":
                            company_scope_relevance_ratio_bps,
                        "cross_company_scope_policy":
                            "cross-company-item-scope.v1",
                        "cross_company_scope_rejected_item_count":
                            cross_company_scope_rejected_item_count,
                        "cross_company_scope_rejections":
                            cross_company_scope_rejection_metadata,
                    });
                    let retired = if let Some(supersession_reason) = supersession_reason {
                        if publication_host_excluded
                            || distinct_company_approved_feed_claim.is_some()
                        {
                            self.database
                                .retire_active_company_news_recipe_for_ownership(
                                    recipe.id,
                                    supersession_reason,
                                    retirement_metadata,
                                )
                                .await
                                .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                        } else {
                            self.database
                                .supersede_active_company_news_recipe(
                                    recipe.id,
                                    supersession_reason,
                                    retirement_metadata,
                                )
                                .await
                                .map_err(|error| JobHandlerError::retryable(error.to_string()))?
                        }
                    } else {
                        false
                    };
                    warn!(
                        recipe_id = %recipe.id,
                        source_id = %source.id,
                        rebuild_required = outcome.rebuild_required,
                        retired,
                        supersession_reason,
                        reasons = ?report.correctness_reasons,
                        "crawl recipe failed correctness gates; no items were published"
                    );
                    return Ok(());
                }

                let summary = self
                    .database
                    .complete_crawl_run(
                        crawl_run_id,
                        &detected_source,
                        report.fetched_at,
                        &processed,
                        json!({
                            "ingestion_mode": "company_news_recipe",
                            "recipe_id": recipe.id,
                            "recipe_version": recipe.version,
                            "publication_final_url": report.publication_final_url,
                            "structure_fingerprint": report.structure_fingerprint,
                            "content_stale": report.content_stale,
                            "dated_item_count": report.dated_item_count,
                            "publication_date_coverage_complete":
                                report.publication_date_coverage_complete,
                            "distinct_title_count": report.distinct_title_count,
                            "distinct_content_count": report.distinct_content_count,
                            "effective_item_scope": effective_spec.item_scope,
                            "company_scope_required": company_scope_required,
                            "company_scope_total_item_count":
                                company_scope_total_item_count,
                            "company_scope_relevant_item_count":
                                company_scope_relevant_item_count,
                            "company_scope_rejected_item_count":
                                company_scope_rejected_item_count,
                            "company_scope_relevance_ratio_bps":
                                company_scope_relevance_ratio_bps,
                            "cross_company_scope_policy":
                                "cross-company-item-scope.v1",
                            "cross_company_scope_rejected_item_count":
                                cross_company_scope_rejected_item_count,
                            "cross_company_scope_rejections":
                                cross_company_scope_rejection_metadata,
                        }),
                    )
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                self.database
                    .complete_company_news_recipe_run(
                        recipe_run_id,
                        recipe,
                        &recipe_completion_from_report(
                            &report,
                            summary.normalized_item_count,
                            summary.new_item_count,
                            json!({
                                "published": true,
                                "content_stale": report.content_stale,
                                "dated_item_count": report.dated_item_count,
                                "publication_date_coverage_complete":
                                    report.publication_date_coverage_complete,
                                "distinct_title_count": report.distinct_title_count,
                                "distinct_content_count": report.distinct_content_count,
                                "approved_feed_overlap_count": approved_feed_overlap_count,
                                "approved_feed_signature_overlap_count":
                                    approved_feed_signature_matches.len(),
                                "approved_feed_overlap_ratio_bps": ratio_bps(
                                    approved_feed_overlap_count,
                                    report.accepted_item_count,
                                ),
                                "active_recipe_overlap_count": active_recipe_overlap_count,
                                "active_recipe_signature_overlap_count":
                                    active_recipe_signature_matches.len(),
                                "active_recipe_overlap_ratio_bps": ratio_bps(
                                    active_recipe_overlap_count,
                                    report.accepted_item_count,
                                ),
                                "failed_item_count": summary.failed_item_count,
                                "quality_quarantined_item_count":
                                    quality_quarantined_item_count,
                                "effective_item_scope": effective_spec.item_scope,
                                "company_scope_required": company_scope_required,
                                "company_scope_total_item_count":
                                    company_scope_total_item_count,
                                "company_scope_relevant_item_count":
                                    company_scope_relevant_item_count,
                                "company_scope_rejected_item_count":
                                    company_scope_rejected_item_count,
                                "company_scope_relevance_ratio_bps":
                                    company_scope_relevance_ratio_bps,
                                "cross_company_scope_policy":
                                    "cross-company-item-scope.v1",
                                "cross_company_scope_rejected_item_count":
                                    cross_company_scope_rejected_item_count,
                                "cross_company_scope_rejections":
                                    cross_company_scope_rejection_metadata,
                                "failures": report.failures,
                            }),
                        )?,
                    )
                    .await
                    .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
                Ok(())
            }
            Err(error) => {
                let safe_error = error.to_string();
                self.database
                    .fail_crawl_run(crawl_run_id, source, &safe_error)
                    .await
                    .map_err(|database_error| {
                        JobHandlerError::retryable(format!(
                            "{safe_error}; additionally failed to close crawl run: {database_error}"
                        ))
                    })?;
                let outcome = self
                    .database
                    .complete_company_news_recipe_run(
                        recipe_run_id,
                        recipe,
                        &CompanyNewsRecipeRunCompletion {
                            discovered_url_count: 0,
                            accepted_item_count: 0,
                            rejected_url_count: 0,
                            normalized_item_count: 0,
                            new_item_count: 0,
                            latest_published_at: None,
                            publication_date_coverage_complete: false,
                            acceptance_ratio_bps: 0,
                            structure_fingerprint: None,
                            reasons: Vec::new(),
                            error: Some(safe_error.clone()),
                            transient_error: error.is_retryable(),
                            metadata: json!({ "published": false }),
                        },
                    )
                    .await
                    .map_err(|database_error| {
                        JobHandlerError::retryable(format!(
                            "{safe_error}; additionally failed to close recipe run: {database_error}"
                        ))
                    })?;
                if outcome.rebuild_required || !error.is_retryable() {
                    Err(JobHandlerError::permanent(safe_error))
                } else {
                    Err(JobHandlerError::retryable(safe_error))
                }
            }
        }
    }
}

fn recipe_completion_from_report(
    report: &feed_crawler::HtmlRecipeCrawlReport,
    normalized_item_count: i32,
    new_item_count: i32,
    metadata: Value,
) -> Result<CompanyNewsRecipeRunCompletion, JobHandlerError> {
    Ok(CompanyNewsRecipeRunCompletion {
        discovered_url_count: i32::try_from(report.discovered_url_count)
            .map_err(|_| JobHandlerError::permanent("recipe discovered URL count exceeds i32"))?,
        accepted_item_count: i32::try_from(report.accepted_item_count)
            .map_err(|_| JobHandlerError::permanent("recipe accepted item count exceeds i32"))?,
        rejected_url_count: i32::try_from(report.rejected_url_count)
            .map_err(|_| JobHandlerError::permanent("recipe rejected URL count exceeds i32"))?,
        normalized_item_count,
        new_item_count,
        latest_published_at: report.latest_published_at,
        publication_date_coverage_complete: report.publication_date_coverage_complete,
        acceptance_ratio_bps: i32::from(report.acceptance_ratio_bps),
        structure_fingerprint: Some(report.structure_fingerprint.clone()),
        reasons: report.correctness_reasons.clone(),
        error: None,
        transient_error: false,
        metadata,
    })
}

fn classify_crawl_error(error: CrawlError) -> JobHandlerError {
    match error {
        CrawlError::UnsupportedSourceKind(_) | CrawlError::UnsupportedUrl(_) => {
            JobHandlerError::permanent(error.to_string())
        }
        CrawlError::InvalidConfig(_)
        | CrawlError::Client(_)
        | CrawlError::Request { .. }
        | CrawlError::HttpStatus { .. }
        | CrawlError::ResponseTooLarge { .. }
        | CrawlError::InvalidFeed(_)
        | CrawlError::ItemMissingUrl
        | CrawlError::Serialize(_) => JobHandlerError::retryable(error.to_string()),
    }
}

#[derive(Clone)]
pub struct ExportJobHandler {
    database: Database,
}

impl ExportJobHandler {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl JobHandler for ExportJobHandler {
    fn supported_job_types(&self) -> &[JobType] {
        &[JobType::ExportTarget]
    }

    async fn handle(&self, job: &Job) -> Result<(), JobHandlerError> {
        let export_target_id = job.export_target_id.ok_or_else(|| {
            JobHandlerError::permanent("export_target job is missing export_target_id")
        })?;
        let target = self
            .database
            .get_export_target(export_target_id)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?
            .ok_or_else(|| {
                JobHandlerError::permanent(format!(
                    "export target {export_target_id} does not exist"
                ))
            })?;
        if !target.enabled {
            return Err(JobHandlerError::permanent(format!(
                "export target {} is disabled",
                target.target_id
            )));
        }

        let run_id = self
            .database
            .begin_export_run(target.id, job.id)
            .await
            .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
        let result = async {
            let items = self
                .database
                .list_exportable_feed_items(target.id)
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string()))?;
            export_archive(target, items)
                .await
                .map_err(classify_export_error)
        }
        .await;

        match result {
            Ok(export) => self
                .database
                .complete_export_run(
                    run_id,
                    &export.records,
                    export.commit_sha.as_deref(),
                    export.pushed,
                    serde_json::json!({
                        "changed": export.changed,
                    }),
                )
                .await
                .map_err(|error| JobHandlerError::retryable(error.to_string())),
            Err(error) => {
                self.database
                    .fail_export_run(run_id, &error.to_string())
                    .await
                    .map_err(|database_error| {
                        JobHandlerError::retryable(format!(
                            "{error}; additionally failed to close export run: {database_error}"
                        ))
                    })?;
                Err(error)
            }
        }
    }
}

fn classify_export_error(error: ExportError) -> JobHandlerError {
    match error {
        ExportError::UnsupportedLayout(_)
        | ExportError::InvalidPath(_)
        | ExportError::Invariant(_) => JobHandlerError::permanent(error.to_string()),
        ExportError::Io(_) | ExportError::Json(_) | ExportError::Join(_) | ExportError::Git(_) => {
            JobHandlerError::retryable(error.to_string())
        }
    }
}

#[derive(Clone)]
pub struct DiscoveryJobProducer {
    database: Database,
    scan_interval: std::time::Duration,
    discovery_queue_target: u32,
}

impl DiscoveryJobProducer {
    pub fn new(
        database: Database,
        scan_interval: std::time::Duration,
        discovery_queue_target: u32,
    ) -> Self {
        Self {
            database,
            scan_interval,
            discovery_queue_target,
        }
    }

    pub async fn schedule_once(&self) -> Result<u64, feed_db::DatabaseError> {
        self.database
            .enqueue_due_discovery_jobs(Utc::now(), self.discovery_queue_target)
            .await
    }

    pub async fn run_until_cancelled(&self, shutdown: CancellationToken) {
        info!("recurring job producer started");
        loop {
            if shutdown.is_cancelled() {
                break;
            }
            match self.schedule_once().await {
                Ok(discovery_jobs) if discovery_jobs > 0 => {
                    info!(discovery_jobs, "scheduled due discovery jobs");
                }
                Ok(_) => {}
                Err(error) => {
                    error!(%error, "failed to schedule discovery jobs");
                }
            }
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(self.scan_interval) => {}
            }
        }
        info!("discovery job producer stopped");
    }
}

#[derive(Clone)]
pub struct ValidationJobProducer {
    database: Database,
    scan_interval: std::time::Duration,
    validation_queue_target: u32,
}

impl ValidationJobProducer {
    pub fn new(
        database: Database,
        scan_interval: std::time::Duration,
        validation_queue_target: u32,
    ) -> Self {
        Self {
            database,
            scan_interval,
            validation_queue_target,
        }
    }

    pub async fn schedule_once(&self) -> Result<u64, feed_db::DatabaseError> {
        self.database
            .enqueue_unvalidated_candidate_jobs(Utc::now(), self.validation_queue_target)
            .await
    }

    pub async fn run_until_cancelled(&self, shutdown: CancellationToken) {
        info!("validation job producer started");
        loop {
            if shutdown.is_cancelled() {
                break;
            }
            match self.schedule_once().await {
                Ok(validation_jobs) if validation_jobs > 0 => {
                    info!(validation_jobs, "scheduled candidate validation jobs");
                }
                Ok(_) => {}
                Err(error) => {
                    error!(%error, "failed to schedule candidate validation jobs");
                }
            }
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(self.scan_interval) => {}
            }
        }
        info!("validation job producer stopped");
    }
}

#[derive(Clone)]
pub struct CrawlExportJobProducer {
    database: Database,
    scan_interval: std::time::Duration,
}

impl CrawlExportJobProducer {
    pub fn new(database: Database, scan_interval: std::time::Duration) -> Self {
        Self {
            database,
            scan_interval,
        }
    }

    pub async fn schedule_once(
        &self,
    ) -> Result<CrawlExportScheduleSummary, feed_db::DatabaseError> {
        let now = Utc::now();
        Ok(CrawlExportScheduleSummary {
            crawl_jobs: self.database.enqueue_due_crawl_jobs(now).await?,
            export_jobs: self.database.enqueue_due_export_jobs(now).await?,
        })
    }

    pub async fn run_until_cancelled(&self, shutdown: CancellationToken) {
        info!("crawl/export job producer started");
        loop {
            if shutdown.is_cancelled() {
                break;
            }
            match self.schedule_once().await {
                Ok(summary) if summary.total() > 0 => {
                    info!(
                        crawl_jobs = summary.crawl_jobs,
                        export_jobs = summary.export_jobs,
                        "scheduled due crawl/export jobs"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    error!(%error, "failed to schedule crawl/export jobs");
                }
            }
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(self.scan_interval) => {}
            }
        }
        info!("crawl/export job producer stopped");
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CrawlExportScheduleSummary {
    pub crawl_jobs: u64,
    pub export_jobs: u64,
}

impl CrawlExportScheduleSummary {
    pub const fn total(self) -> u64 {
        self.crawl_jobs + self.export_jobs
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JobBootstrapError {
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    #[error(transparent)]
    Crawler(#[from] CrawlError),
    #[error(transparent)]
    WebAdapter(#[from] WebAdapterError),
    #[error(transparent)]
    ArticleCrawler(#[from] ArticlePageError),
    #[error(transparent)]
    RecipeCrawler(#[from] RecipeCrawlError),
    #[error(transparent)]
    Scheduler(#[from] SchedulerError),
    #[error("manual company news import is disabled")]
    NewsExtractionDisabled,
    #[error("invalid job configuration: {0}")]
    InvalidConfig(String),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn deterministic_feed_quality_failures_do_not_consume_retry_capacity() {
        let error = deterministic_feed_quality_failure(
            "shared multi-company feed company scope below minimum",
        );
        assert!(!error.is_retryable());
    }

    #[test]
    fn recipe_activation_crawls_are_prioritized_ahead_of_scheduled_recrawls() {
        let company_id = uuid::Uuid::new_v4();
        let source_id = uuid::Uuid::new_v4();
        let recipe_id = uuid::Uuid::new_v4();

        let job = recipe_activation_crawl_job(company_id, source_id, recipe_id);

        assert_eq!(job.job_type, JobType::CrawlSource);
        assert_eq!(job.job_key, format!("source:{source_id}"));
        assert_eq!(job.company_id, Some(company_id));
        assert_eq!(job.source_id, Some(source_id));
        assert_eq!(job.priority, i16::MAX / 2);
        assert_eq!(job.payload["source_id"], source_id.to_string());
        assert_eq!(job.payload["recipe_id"], recipe_id.to_string());
        assert_eq!(job.payload["trigger"], "recipe_activation");
    }

    #[test]
    fn discovery_jobs_accept_recipe_builder_seeds_without_an_adapter_round_trip() {
        let newsroom = Url::parse("https://example.com/newsroom").expect("valid URL");
        let seeds = discovery_job_seeds(&json!({
            "company_id": uuid::Uuid::new_v4(),
            "seed_origin": "company_news_recipe_builder",
            "seeds": [{
                "url": newsroom,
                "role": "newsroom",
                "rank_score": 0.9
            }]
        }))
        .expect("valid discovery seeds");

        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0].url.as_str(), "https://example.com/newsroom");
        assert_eq!(seeds[0].role, "newsroom");
        assert_eq!(seeds[0].rank_score, 0.9);
    }

    #[test]
    fn recipe_publications_become_deduplicated_role_aware_discovery_seeds() {
        let request_id = uuid::Uuid::new_v4();
        let response = CompanyNewsExtractionResponse {
            schema_version: feed_web_adapter::COMPANY_NEWS_EXTRACTION_SCHEMA_VERSION.to_owned(),
            request_id,
            publications: vec![
                feed_web_adapter::CompanyNewsPublicationCandidate {
                    url: Url::parse("https://example.com/press-releases/")
                        .expect("valid press URL"),
                    rank_score: 0.95,
                },
                feed_web_adapter::CompanyNewsPublicationCandidate {
                    url: Url::parse("https://example.com/press-releases")
                        .expect("valid duplicate press URL"),
                    rank_score: 0.7,
                },
                feed_web_adapter::CompanyNewsPublicationCandidate {
                    url: Url::parse("https://engineering.example.com/blog")
                        .expect("valid engineering URL"),
                    rank_score: 0.8,
                },
            ],
            articles: vec![
                feed_web_adapter::CompanyNewsArticleCandidate {
                    url: Url::parse("https://example.com/press-releases/company-update")
                        .expect("valid press article URL"),
                    rank_score: 0.9,
                },
                feed_web_adapter::CompanyNewsArticleCandidate {
                    url: Url::parse("https://example.com/news/product-launch")
                        .expect("valid news article URL"),
                    rank_score: 0.75,
                },
            ],
            adapter_trace_id: None,
        };

        let seeds = company_news_publication_discovery_seeds(&response);

        assert_eq!(seeds.len(), 3);
        assert_eq!(seeds[0].role, "press_releases");
        assert_eq!(seeds[1].role, "engineering_blog");
        assert_eq!(seeds[2].url.as_str(), "https://example.com/news/");
        assert_eq!(seeds[2].role, "newsroom");
    }

    #[test]
    fn covered_company_publication_discovery_requires_explicit_expansion() {
        let seeds = vec![DiscoverySeed {
            url: Url::parse("https://example.com/press/").expect("valid press URL"),
            role: "press_releases".to_owned(),
            rank_score: 0.9,
        }];

        assert!(should_seed_publication_discovery(&seeds, false, false));
        assert!(!should_seed_publication_discovery(&seeds, true, false));
        assert!(should_seed_publication_discovery(&seeds, true, true));
        assert!(!should_seed_publication_discovery(&[], false, true));
    }

    fn company_fixture(name: &str, homepage_url: &str) -> Company {
        let now = Utc::now();
        Company {
            id: uuid::Uuid::new_v4(),
            company_key: company_recipe_identity_key(name),
            name: name.to_owned(),
            aliases: Vec::new(),
            ownership_status: feed_core::OwnershipStatus::Unknown,
            lifecycle_status: feed_core::LifecycleStatus::Active,
            listings: Vec::new(),
            homepage_url: Some(Url::parse(homepage_url).expect("valid homepage URL")),
            investor_relations_url: None,
            newsroom_url: None,
            blog_url: None,
            hints: Vec::new(),
            discovery_enabled: true,
            discovery_not_before: now,
            discovery_cadence_seconds: 86_400,
            metadata: json!({}),
            created_at: now,
            updated_at: now,
        }
    }

    fn raw_item_fixture(url: &str, title: &str) -> feed_core::RawCrawlItem {
        feed_core::RawCrawlItem {
            source_item_key: url.to_owned(),
            external_id: None,
            url: Url::parse(url).expect("valid item URL"),
            canonical_url: None,
            title: Some(title.to_owned()),
            summary_html: None,
            body_html: Some(format!("<article>{title}</article>")),
            published_at: Some(Utc::now()),
            payload: json!({}),
        }
    }

    fn recipe_report_fixture(items: Vec<feed_core::RawCrawlItem>) -> HtmlRecipeCrawlReport {
        let discovered_url_count = items.len();
        HtmlRecipeCrawlReport {
            fetched_at: Utc::now(),
            publication_final_url: Url::parse("https://aggregator.example/news")
                .expect("valid publication URL"),
            discovered_url_count,
            accepted_item_count: discovered_url_count,
            distinct_title_count: discovered_url_count,
            distinct_content_count: discovered_url_count,
            rejected_url_count: 0,
            acceptance_ratio_bps: 10_000,
            latest_published_at: Some(Utc::now()),
            dated_item_count: discovered_url_count,
            publication_date_coverage_complete: discovered_url_count > 0,
            structure_fingerprint: "sha256:test".to_owned(),
            correctness_reasons: Vec::new(),
            content_stale: false,
            items,
            failures: Vec::new(),
        }
    }

    fn feed_batch_fixture(items: Vec<feed_core::RawCrawlItem>) -> CrawlBatch {
        CrawlBatch {
            fetched_at: Utc::now(),
            detected_source_kind: SourceKind::Rss,
            items,
            metadata: json!({}),
        }
    }

    fn article_report_fixture(items: Vec<feed_core::RawCrawlItem>) -> HtmlArticleCrawlReport {
        HtmlArticleCrawlReport {
            fetched_at: Utc::now(),
            items,
            failures: Vec::new(),
        }
    }

    #[test]
    fn feed_title_diversity_blocks_degenerate_editorial_samples() {
        let repeated = (0..5)
            .map(|index| {
                raw_item_fixture(
                    &format!("https://example.com/node/{index}"),
                    "hsq-api-part2-active",
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            feed_title_diversity(&repeated),
            FeedTitleDiversity {
                titled_item_count: 5,
                usable_titled_item_count: 5,
                distinct_titled_item_count: 1,
                passed: false,
            }
        );

        let short_sample = repeated[..4].to_vec();
        assert!(feed_title_diversity(&short_sample).passed);

        let mut diverse = repeated.clone();
        diverse[4].title = Some("A distinct company announcement".to_owned());
        assert_eq!(
            feed_title_diversity(&diverse),
            FeedTitleDiversity {
                titled_item_count: 5,
                usable_titled_item_count: 5,
                distinct_titled_item_count: 2,
                passed: true,
            }
        );

        let mut placeholder_only = repeated;
        for item in &mut placeholder_only {
            item.title = Some("Hello world!".to_owned());
            item.body_html = Some(
                "<p>Welcome to WordPress. This is your first post. Edit or delete it.</p>"
                    .to_owned(),
            );
        }
        assert_eq!(
            feed_title_diversity(&placeholder_only),
            FeedTitleDiversity {
                titled_item_count: 5,
                usable_titled_item_count: 0,
                distinct_titled_item_count: 0,
                passed: true,
            }
        );
    }

    #[test]
    fn feed_content_diversity_blocks_reused_bodies_but_preserves_mixed_feeds() {
        let mut repeated = (0..5)
            .map(|index| {
                raw_item_fixture(
                    &format!("https://example.com/news/{index}"),
                    &format!("Distinct headline {index}"),
                )
            })
            .collect::<Vec<_>>();
        for item in &mut repeated {
            item.body_html = Some(
                "<article>The same unrelated fallback body appears on every URL.</article>"
                    .to_owned(),
            );
        }
        assert_eq!(
            feed_content_diversity(&repeated),
            FeedContentDiversity {
                item_count: 5,
                repeated_content_item_count: 5,
                passed: false,
            }
        );

        repeated[2].body_html = Some("<article>Unique body two.</article>".to_owned());
        repeated[3].body_html = Some("<article>Unique body three.</article>".to_owned());
        repeated[4].body_html = Some("<article>Unique body four.</article>".to_owned());
        assert_eq!(
            feed_content_diversity(&repeated),
            FeedContentDiversity {
                item_count: 5,
                repeated_content_item_count: 2,
                passed: true,
            }
        );
    }

    #[test]
    fn detects_gambling_seo_takeover_for_an_unrelated_company_profile() {
        let mut company = company_fixture("Infina", "https://infina.vn/");
        company.metadata = json!({
            "universe": {
                "source": "yc-directory",
                "source_metadata": {
                    "one_liner": "Leading wealth management and investing platform in Vietnam"
                }
            }
        });
        let items = (0..5)
            .map(|index| {
                let mut item = raw_item_fixture(
                    &format!("https://infina.vn/blog/casino-bonus-guide-{index}/"),
                    &format!("Online casino bonus guide {index} with free spins"),
                );
                item.body_html = Some(format!(
                    "<article>Compare casino slot games, wagering requirements, and no-deposit \
                     bonus offers in guide {index}.</article>"
                ));
                item
            })
            .collect::<Vec<_>>();

        assert_eq!(
            publication_topic_compromise(&company, &items),
            PublicationTopicCompromise {
                sample_item_count: 5,
                suspicious_item_count: 5,
                detected: true,
            }
        );
    }

    #[test]
    fn preserves_expected_gambling_publications_and_unrelated_incidental_mentions() {
        let mut casino_company = company_fixture("Century Casinos Inc.", "https://www.cnty.com/");
        casino_company.metadata = json!({
            "universe": {
                "industry": "Hotels/Resorts",
                "source_metadata": {
                    "one_liner": "Casino gaming and resort operator"
                }
            }
        });
        let gambling_items = (0..5)
            .map(|index| {
                let mut item = raw_item_fixture(
                    &format!("https://www.cnty.com/news/casino-update-{index}"),
                    &format!("Casino bonus and slot update {index}"),
                );
                item.body_html = Some(
                    "<article>The resort updated its casino, slot, and sportsbook offer.</article>"
                        .to_owned(),
                );
                item
            })
            .collect::<Vec<_>>();
        assert!(!publication_topic_compromise(&casino_company, &gambling_items).detected);

        let mut entertainment_company =
            company_fixture("PLAYSTUDIOS, Inc.", "https://www.playstudios.com/");
        entertainment_company.metadata = json!({
            "universe": {
                "industry": "Amusement and entertainment",
                "source_metadata": {
                    "one_liner": "Developer of rewarded-play mobile games"
                }
            }
        });
        assert!(!publication_topic_compromise(&entertainment_company, &gambling_items).detected);

        let mut software_company = company_fixture(
            "PLAYSTUDIOS Inc. Class A Common Stock",
            "https://ir.playstudios.com/",
        );
        software_company.metadata = json!({
            "universe": {
                "industry": "Computer Software: Prepackaged Software"
            }
        });
        let company_named_gambling_items = (0..5)
            .map(|index| {
                let mut item = raw_item_fixture(
                    &format!(
                        "https://ir.playstudios.com/news-events/press-releases/detail/{index}"
                    ),
                    &format!("PLAYSTUDIOS announces casino games update {index}"),
                );
                item.body_html = Some(
                    "<article>The company expanded its casino, slot, and poker rewards.</article>"
                        .to_owned(),
                );
                item
            })
            .collect::<Vec<_>>();
        assert!(
            !publication_topic_compromise(&software_company, &company_named_gambling_items)
                .detected
        );

        let mut payments_company = company_fixture("Paysafe Limited", "https://www.paysafe.com/");
        payments_company.metadata = json!({
            "universe": {
                "industry": "Payment processing",
                "source_metadata": {
                    "one_liner": "Payments platform serving online gaming merchants"
                }
            }
        });
        assert!(!publication_topic_compromise(&payments_company, &gambling_items).detected);

        let finance_company = company_fixture("Infina", "https://infina.vn/");
        let mut finance_items = (0..5)
            .map(|index| {
                raw_item_fixture(
                    &format!("https://infina.vn/blog/investing-guide-{index}"),
                    &format!("Long-term investing guide {index}"),
                )
            })
            .collect::<Vec<_>>();
        finance_items[0].body_html = Some(
            "<article>Gaming and casino stocks are one small part of a diversified market.</article>"
                .to_owned(),
        );
        assert!(!publication_topic_compromise(&finance_company, &finance_items).detected);

        let mut technical_item = raw_item_fixture(
            "https://www.beam.cloud/blog/serverless-gpu",
            "Serverless GPU infrastructure",
        );
        technical_item.body_html =
            Some("<article>Spin up an isolated GPU worker in seconds.</article>".to_owned());
        assert!(!raw_item_looks_like_gambling_spam(&technical_item));
    }

    #[test]
    fn compromised_recipe_sample_fails_correctness_without_publishing_residual_items() {
        let company = company_fixture("Infina", "https://infina.vn/");
        let items = (0..5)
            .map(|index| {
                let mut item = raw_item_fixture(
                    &format!("https://infina.vn/blog/online-casino-{index}"),
                    &format!("Online casino bonus {index} and free spins"),
                );
                item.body_html = Some(
                    "<article>Casino slot games with wagering and no-deposit bonuses.</article>"
                        .to_owned(),
                );
                item
            })
            .collect::<Vec<_>>();
        let mut report = recipe_report_fixture(items);
        let spec = build_company_news_recipe_spec(
            &company,
            Url::parse("https://infina.vn/blog").expect("valid publication URL"),
            Vec::new(),
            20,
        );

        let assessment =
            apply_recipe_publication_topic_compromise_filter(&company, &spec, &mut report)
                .expect("compromise is detected");

        assert_eq!(assessment.suspicious_item_count, 5);
        assert!(report.items.is_empty());
        assert!(!report.correctness_passed());
        assert!(
            report
                .correctness_reasons
                .iter()
                .any(|reason| reason == "publication_topic_compromise_detected")
        );
        assert!(
            report
                .failures
                .iter()
                .all(|failure| failure.reason == "publication_topic_compromise_detected")
        );
    }

    #[test]
    fn feed_scope_rejects_reply_forum_and_documentation_indexes() {
        let editorial = (0..5)
            .map(|index| {
                raw_item_fixture(
                    &format!("https://example.com/blog/company-update-{index}"),
                    &format!("Company update {index}"),
                )
            })
            .collect::<Vec<_>>();
        assert!(feed_scope_is_non_editorial(
            &Url::parse("https://example.com/community/rss/boardmessages?board.id=technology-blog")
                .expect("reply feed URL"),
            &feed_batch_fixture(editorial)
        ));

        let documentation = (0..5)
            .map(|index| {
                raw_item_fixture(
                    &format!("https://example.com/docs/components/widget-{index}"),
                    &format!("Widget {index}"),
                )
            })
            .collect::<Vec<_>>();
        assert!(feed_scope_is_non_editorial(
            &Url::parse("https://example.com/rss.xml").expect("feed URL"),
            &feed_batch_fixture(documentation)
        ));

        let forum = (0..5)
            .map(|index| {
                raw_item_fixture(
                    &format!("https://community.example.com/forums/topic-{index}"),
                    &format!("Forum Post: user question {index}"),
                )
            })
            .collect::<Vec<_>>();
        assert!(feed_scope_is_non_editorial(
            &Url::parse("https://community.example.com/rss").expect("feed URL"),
            &feed_batch_fixture(forum)
        ));

        let discourse = (0..5)
            .map(|index| {
                raw_item_fixture(
                    &format!("https://developer.example.com/discuss/t/user-question-{index}/123"),
                    &format!("How do I configure connector {index}?"),
                )
            })
            .collect::<Vec<_>>();
        assert!(feed_scope_is_non_editorial(
            &Url::parse("https://developer.example.com/discuss/latest.rss")
                .expect("Discourse feed URL"),
            &feed_batch_fixture(discourse)
        ));
        assert!(publication_url_has_hard_non_editorial_scope(
            &Url::parse("https://developer.example.com/discuss/c/content/community-blog/125")
                .expect("Discourse category URL")
        ));
    }

    #[test]
    fn feed_scope_preserves_press_release_documents_and_community_blogs() {
        let press_releases = (0..5)
            .map(|index| {
                raw_item_fixture(
                    &format!(
                        "https://example.com/docs/pdf/pressreleases/2026/company-update-{index}.pdf"
                    ),
                    &format!("Company announces update {index}"),
                )
            })
            .collect::<Vec<_>>();
        assert!(!feed_scope_is_non_editorial(
            &Url::parse("https://example.com/rss/press-releases").expect("feed URL"),
            &feed_batch_fixture(press_releases)
        ));

        let community_blog = (0..5)
            .map(|index| {
                raw_item_fixture(
                    &format!("https://community.example.com/cloud-security-update-{index}"),
                    &format!("Cloud security update {index}"),
                )
            })
            .collect::<Vec<_>>();
        assert!(!feed_scope_is_non_editorial(
            &Url::parse("https://community.example.com/feed").expect("feed URL"),
            &feed_batch_fixture(community_blog)
        ));
    }

    #[test]
    fn feed_quality_excludes_non_editorial_utility_items() {
        let items = (0..5)
            .map(|index| {
                raw_item_fixture(
                    &format!("https://example.com/news/subscription?form={index}"),
                    &format!("Investor updates form {index}"),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            feed_title_diversity(&items),
            FeedTitleDiversity {
                titled_item_count: 5,
                usable_titled_item_count: 0,
                distinct_titled_item_count: 0,
                passed: true,
            }
        );
        assert!(feed_scope_is_non_editorial(
            &Url::parse("https://example.com/news/feed").expect("feed URL"),
            &feed_batch_fixture(items)
        ));
    }

    #[test]
    fn unrelated_and_shared_feeds_require_a_company_relevant_majority() {
        let mut company = company_fixture(
            "Neumora Therapeutics Inc. Common Stock",
            "https://www.neumoratx.com/",
        );
        let mut items = vec![
            raw_item_fixture(
                "https://www.stocktitan.net/news/NMRA/neumora-results",
                "Neumora Therapeutics reports results",
            ),
            raw_item_fixture(
                "https://www.stocktitan.net/news/NMRA/neumora-conference",
                "Neumora to participate in a conference",
            ),
            raw_item_fixture(
                "https://www.stocktitan.net/news/OTHER/other-results",
                "Another company reports results",
            ),
            raw_item_fixture(
                "https://www.stocktitan.net/news/OTHER/other-launch",
                "Another company launches a product",
            ),
            raw_item_fixture(
                "https://www.stocktitan.net/news/OTHER/other-director",
                "Another company appoints a director",
            ),
        ];
        let shared_url = Url::parse("https://www.stocktitan.net/rss").expect("shared feed URL");

        assert_eq!(
            feed_company_scope_relevance(&company, &shared_url, None, &items),
            FeedCompanyScopeRelevance {
                required: true,
                feed_title_corroborated: false,
                off_company_host_item_count: 5,
                off_company_host_ratio_bps: 10_000,
                total_item_count: 5,
                relevant_item_count: 2,
                relevance_ratio_bps: 4_000,
                passed: false,
            }
        );

        items[2] = raw_item_fixture(
            "https://www.stocktitan.net/news/NMRA/neumora-pipeline",
            "Neumora provides a pipeline update",
        );
        assert!(feed_company_scope_relevance(&company, &shared_url, None, &items).passed);

        let dedicated_url =
            Url::parse("https://news.neumoratx.com/feed").expect("dedicated feed URL");
        let dedicated_items = vec![
            raw_item_fixture(
                "https://news.neumoratx.com/pipeline-update",
                "Pipeline update",
            ),
            raw_item_fixture(
                "https://news.neumoratx.com/conference",
                "Conference participation",
            ),
        ];
        let dedicated =
            feed_company_scope_relevance(&company, &dedicated_url, None, &dedicated_items);
        assert!(!dedicated.required);
        assert!(dedicated.passed);

        let unrelated_url =
            Url::parse("https://news.example.com/feed").expect("unrelated feed URL");
        let unrelated = feed_company_scope_relevance(&company, &unrelated_url, None, &items[3..]);
        assert!(unrelated.required);
        assert!(!unrelated.passed);

        company.metadata = json!({
            "publication_host_policy": {
                "verified_hosts": ["news.example.com"]
            }
        });
        let verified_items = vec![
            raw_item_fixture(
                "https://news.example.com/pipeline-update",
                "Pipeline update",
            ),
            raw_item_fixture(
                "https://news.example.com/conference",
                "Conference participation",
            ),
        ];
        let verified =
            feed_company_scope_relevance(&company, &unrelated_url, None, &verified_items);
        assert!(!verified.required);
        assert!(verified.passed);

        let global_market_feed =
            Url::parse("https://simplywall.st/news/rss").expect("global market feed URL");
        let global_market_scope =
            feed_company_scope_relevance(&company, &global_market_feed, None, &items[3..]);
        assert!(global_market_scope.required);
        assert!(!global_market_scope.passed);
    }

    #[test]
    fn a_matching_feed_title_corroborates_a_non_shared_brand_host() {
        let company = company_fixture(
            "European Wax Center Inc. Class A Common Stock",
            "https://investor.europeanwaxcenter.com/",
        );
        let feed_url = Url::parse("https://waxcenter.com/blogs/news.atom").expect("brand feed URL");
        let items = vec![
            raw_item_fixture(
                "https://waxcenter.com/blogs/news/waxing-vs-sugaring",
                "Waxing vs. Sugaring: Which Lasts Longer?",
            ),
            raw_item_fixture(
                "https://waxcenter.com/blogs/news/skin-care-guide",
                "A Guide to Skin Care Between Appointments",
            ),
        ];

        let without_title = feed_company_scope_relevance(&company, &feed_url, None, &items);
        assert!(without_title.required);
        assert!(!without_title.passed);

        let corroborated = feed_company_scope_relevance(
            &company,
            &feed_url,
            Some("European Wax Center - Articles"),
            &items,
        );
        assert!(!corroborated.required);
        assert!(corroborated.feed_title_corroborated);
        assert!(corroborated.passed);

        let generic_title =
            feed_company_scope_relevance(&company, &feed_url, Some("Waxing Tips"), &items);
        assert!(generic_title.required);
        assert!(!generic_title.feed_title_corroborated);
        assert!(!generic_title.passed);
    }

    #[test]
    fn official_feed_with_a_foreign_item_host_majority_requires_company_scope() {
        let company = company_fixture(
            "Marine Products Corporation Common Stock",
            "https://www.marineproductscorp.com/",
        );
        let feed_url = Url::parse("https://www.marineproductscorp.com/rss/pressrelease.aspx")
            .expect("feed URL");
        let items = vec![
            raw_item_fixture(
                "https://investors.mcbh.com/news/mastercraft-results",
                "MasterCraft Boat Holdings, Inc. Reports Results",
            ),
            raw_item_fixture(
                "https://investors.mcbh.com/news/mastercraft-launch",
                "MasterCraft Launches a New Boat",
            ),
            raw_item_fixture(
                "https://investors.mcbh.com/news/mastercraft-director",
                "MasterCraft Appoints a Director",
            ),
        ];

        let relevance =
            feed_company_scope_relevance(&company, &feed_url, Some("MasterCraft News"), &items);
        assert!(relevance.required);
        assert_eq!(relevance.off_company_host_item_count, 3);
        assert_eq!(relevance.off_company_host_ratio_bps, 10_000);
        assert_eq!(relevance.relevant_item_count, 0);
        assert!(!relevance.passed);
    }

    #[test]
    fn a_matching_feed_title_never_bypasses_shared_host_scope() {
        let company = company_fixture(
            "Neumora Therapeutics Inc. Common Stock",
            "https://www.neumoratx.com/",
        );
        let shared_url = Url::parse("https://www.stocktitan.net/rss").expect("shared feed URL");
        let unrelated = vec![
            raw_item_fixture(
                "https://www.stocktitan.net/news/OTHER/results",
                "Another company reports results",
            ),
            raw_item_fixture(
                "https://www.stocktitan.net/news/OTHER/launch",
                "Another company launches a product",
            ),
        ];

        let relevance = feed_company_scope_relevance(
            &company,
            &shared_url,
            Some("Neumora Therapeutics News"),
            &unrelated,
        );
        assert!(relevance.required);
        assert!(!relevance.feed_title_corroborated);
        assert!(!relevance.passed);
    }

    #[test]
    fn direct_shared_host_articles_are_filtered_per_company() {
        let company = company_fixture("374Water Inc. Common Stock", "https://www.374water.com/");
        let mut report = article_report_fixture(vec![
            raw_item_fixture(
                "https://www.accessnewswire.com/newsroom/en/clean-technology/374water-added-to-russell-microcap-index-1193000",
                "374Water Added to Russell Microcap Index",
            ),
            raw_item_fixture(
                "https://www.accessnewswire.com/newsroom/en/healthcare/viemed-healthcare-announces-results-1194000",
                "Viemed Healthcare Announces Quarterly Results",
            ),
            raw_item_fixture(
                "https://www.374water.com/news/platform-update",
                "Platform update",
            ),
        ]);

        assert_eq!(
            apply_direct_article_company_scope_filter(&company, &mut report),
            1
        );
        assert_eq!(report.items.len(), 2);
        assert!(report.items.iter().any(|item| {
            item.url
                .as_str()
                .contains("374water-added-to-russell-microcap")
        }));
        assert!(
            report.items.iter().any(|item| {
                item.url.as_str() == "https://www.374water.com/news/platform-update"
            })
        );
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].reason, "article_not_company_scoped");
        assert!(report.failures[0].url.as_str().contains("viemed"));
    }

    #[test]
    fn cross_company_item_scope_keeps_owner_and_named_transaction_party() {
        let chart = company_fixture(
            "Chart Industries Inc. Common Stock",
            "https://www.chartindustries.com/",
        );
        let baker = company_fixture(
            "Baker Hughes Company Class A Common Stock",
            "https://www.bakerhughes.com/",
        );
        let baker_only = raw_item_fixture(
            "https://investors.bakerhughes.com/news/baker-hughes-quarterly-results",
            "Baker Hughes Announces Quarterly Results",
        );
        let acquisition = raw_item_fixture(
            "https://investors.bakerhughes.com/news/baker-hughes-completes-chart-industries-acquisition",
            "Baker Hughes Completes Acquisition of Chart Industries",
        );

        assert!(raw_item_has_cross_company_scope(
            &baker,
            &baker_only,
            std::slice::from_ref(&chart),
        ));
        assert!(!raw_item_has_cross_company_scope(
            &chart,
            &baker_only,
            std::slice::from_ref(&baker),
        ));
        assert!(raw_item_has_cross_company_scope(
            &chart,
            &acquisition,
            std::slice::from_ref(&baker),
        ));
    }

    #[test]
    fn cross_company_item_scope_rejects_short_name_shadow_on_other_host() {
        let forum = company_fixture("Forum", "https://www.forum.market/");
        let forum_markets = company_fixture(
            "Forum Markets Incorporated Common Stock",
            "https://ir.forum-markets.com/",
        );
        let item = raw_item_fixture(
            "https://ir.forum-markets.com/news/quarterly-results",
            "Forum Reports Quarterly Results",
        );

        assert!(!raw_item_has_cross_company_scope(
            &forum,
            &item,
            std::slice::from_ref(&forum_markets),
        ));
        assert!(raw_item_has_cross_company_scope(
            &forum_markets,
            &item,
            std::slice::from_ref(&forum),
        ));
    }

    #[test]
    fn direct_shared_host_filter_checks_declared_canonical_hosts() {
        let company = company_fixture("NextTrip Inc.", "https://www.nexttrip.com/");
        let mut item = raw_item_fixture(
            "https://redirect.example/article/1194000",
            "Another issuer announces results",
        );
        item.canonical_url = Some(
            Url::parse(
                "https://www.accessnewswire.com/newsroom/en/healthcare/another-issuer-1194000",
            )
            .expect("valid canonical URL"),
        );
        let mut report = article_report_fixture(vec![item]);

        assert_eq!(
            apply_direct_article_company_scope_filter(&company, &mut report),
            1
        );
        assert!(report.items.is_empty());
        assert_eq!(report.failures.len(), 1);
    }

    #[test]
    fn direct_shared_host_filter_does_not_trust_a_fabricated_request_slug() {
        let company = company_fixture(
            "Above Food Ingredients Inc. Common Stock",
            "https://abovefood.com/",
        );
        let mut item = raw_item_fixture(
            "https://www.newsfilecorp.com/release/297587/Above-Food-Receives-NASDAQ-Determination-Letter",
            "Heliostar Metal Announces Participation in a Mining Event",
        );
        item.canonical_url = Some(
            Url::parse(
                "https://www.newsfilecorp.com/release/297587/Heliostar-Metal-Announces-Participation-in-a-Mining-Event",
            )
            .expect("valid canonical URL"),
        );
        let mut report = article_report_fixture(vec![item]);

        assert_eq!(
            apply_direct_article_company_scope_filter(&company, &mut report),
            1
        );
        assert!(report.items.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].reason, "article_not_company_scoped");
    }

    #[test]
    fn direct_shared_host_filter_rejects_ambiguous_short_url_tokens() {
        let company = company_fixture(
            "Art's-Way Manufacturing Co. Inc. Common Stock",
            "https://artsway-mfg.com/",
        );
        let mut report = article_report_fixture(vec![
            raw_item_fixture(
                "https://www.quiverquant.com/news/Art",
                "Artelo Biosciences Announces Preclinical Data Supporting ART26.12",
            ),
            raw_item_fixture(
                "https://www.quiverquant.com/news/ARTS-WAY-announces-quarterly-results",
                "ART'S WAY MANUFACTURING Announces Quarterly Results",
            ),
        ]);

        assert_eq!(
            apply_direct_article_company_scope_filter(&company, &mut report),
            1
        );
        assert_eq!(report.items.len(), 1);
        assert!(
            report.items[0]
                .title
                .as_deref()
                .is_some_and(|title| title.contains("ART'S WAY"))
        );
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].reason, "article_not_company_scoped");
        assert_eq!(
            report.failures[0].url.as_str(),
            "https://www.quiverquant.com/news/Art"
        );
    }

    #[test]
    fn direct_unrelated_articles_require_company_identity() {
        let company = company_fixture("Linum", "https://linum.ai/");
        let mut report = article_report_fixture(vec![
            raw_item_fixture(
                "https://www.crusoe.ai/resources/blog/how-linum-built-a-frontier-video-model",
                "How Linum built a frontier video model on Crusoe",
            ),
            raw_item_fixture(
                "https://www.crusoe.ai/resources/blog/cloud-capacity-update",
                "Crusoe expands cloud capacity",
            ),
        ]);

        assert_eq!(
            apply_direct_article_company_scope_filter(&company, &mut report),
            1
        );
        assert_eq!(report.items.len(), 1);
        assert!(
            report.items[0]
                .title
                .as_deref()
                .is_some_and(|title| title.contains("Linum"))
        );
        assert_eq!(report.failures[0].reason, "article_not_company_scoped");
    }

    #[test]
    fn direct_policy_exclusion_overrides_an_ambiguous_company_name() {
        let mut company = company_fixture("Lofty", "https://www.lofty.ai/");
        company.metadata = json!({
            "publication_host_policy": {
                "excluded_hosts": ["lofty.com"],
                "direct_evidence_excluded_hosts": ["https://www.lofty.com/"]
            }
        });
        let mut report = article_report_fixture(vec![raw_item_fixture(
            "https://official.lofty.com/news/lofty-platform-update",
            "Lofty announces a platform update",
        )]);

        assert_eq!(
            apply_direct_article_company_scope_filter(&company, &mut report),
            1
        );
        assert!(report.items.is_empty());
        assert_eq!(
            report.failures[0].reason,
            "article_host_excluded_by_company_policy"
        );
    }

    #[test]
    fn no_entry_points_is_a_permanent_configuration_error() {
        let error = classify_discovery_error(DiscoveryError::NoEntryPoints("ACME".to_owned()));
        assert!(!error.is_retryable());
    }

    #[test]
    fn all_fetches_failed_is_retryable() {
        let error = classify_discovery_error(DiscoveryError::AllEntryPointsFailed {
            company: "ACME".to_owned(),
            attempts: Vec::new(),
        });
        assert!(error.is_retryable());
    }

    #[test]
    fn partial_retryable_fetch_failures_are_not_recipe_correctness_evidence() {
        let mut report = recipe_report_fixture(vec![
            raw_item_fixture("https://example.com/news/one", "Company update one"),
            raw_item_fixture("https://example.com/news/two", "Company update two"),
            raw_item_fixture("https://example.com/news/three", "Company update three"),
        ]);
        report.discovered_url_count = 8;
        report.accepted_item_count = 3;
        report.rejected_url_count = 5;
        report.acceptance_ratio_bps = 3_750;
        report.correctness_reasons = vec!["acceptance_ratio_below_minimum".to_owned()];
        report.failures = (0..5)
            .map(|index| ArticleFetchFailure {
                url: Url::parse(&format!("https://example.com/news/rate-limited-{index}"))
                    .expect("failure URL"),
                reason: "http_status".to_owned(),
                retryable: true,
                error: "HTTP 429 Too Many Requests".to_owned(),
            })
            .collect();
        let policy = RecipeCorrectnessPolicy {
            min_accepted_items: 2,
            min_acceptance_ratio_bps: 5_000,
            ..RecipeCorrectnessPolicy::default()
        };

        assert!(recipe_correctness_blocked_by_retryable_fetches(
            &report, &policy
        ));

        report
            .correctness_reasons
            .push("title_diversity_below_minimum".to_owned());
        assert!(!recipe_correctness_blocked_by_retryable_fetches(
            &report, &policy
        ));
    }

    #[test]
    fn discovery_metadata_is_json() {
        let metadata = json!({ "attempts": [] });
        assert!(metadata.is_object());
    }

    #[test]
    fn publication_identity_collapses_locale_mirrors_but_not_distinct_properties() {
        let newsroom = Url::parse("https://stripe.com/newsroom").expect("URL");
        let localized = Url::parse("https://stripe.com/en-es/newsroom/").expect("URL");
        let country = Url::parse("https://stripe.com/nz/newsroom").expect("URL");
        let www = Url::parse("https://www.stripe.com/newsroom/").expect("URL");
        let insecure = Url::parse("http://stripe.com/newsroom?ref=mirror").expect("URL");
        let engineering = Url::parse("https://stripe.com/blog").expect("URL");

        assert_eq!(
            publication_identity_key(&newsroom),
            publication_identity_key(&localized)
        );
        assert_eq!(
            publication_identity_key(&newsroom),
            publication_identity_key(&country)
        );
        assert_eq!(
            publication_identity_key(&newsroom),
            publication_identity_key(&www)
        );
        assert_eq!(
            publication_identity_key(&newsroom),
            publication_identity_key(&insecure)
        );
        assert_ne!(
            publication_identity_key(&newsroom),
            publication_identity_key(&engineering)
        );
    }

    #[test]
    fn company_search_name_removes_security_descriptions_without_using_tickers() {
        assert_eq!(
            company_search_name("10x Genomics Inc. Class A Common Stock"),
            "10x Genomics Inc."
        );
        assert_eq!(
            company_search_name("111 Inc. American Depositary Shares"),
            "111 Inc."
        );
        assert_eq!(
            company_search_name(
                "51Talk Online Education Group American depositary shares each representing 60 Class A ordinary shares"
            ),
            "51Talk Online Education Group"
        );
        assert_eq!(
            company_search_name("Fiverr International Ltd. Ordinary Shares no par value"),
            "Fiverr International Ltd."
        );
        assert_eq!(
            company_search_name("First Interstate BancSystem Inc. Common Stock (DE)"),
            "First Interstate BancSystem Inc."
        );
        assert_eq!(
            company_search_name("Aemetis Inc. (DE) Common Stock"),
            "Aemetis Inc."
        );
        assert_eq!(
            company_search_name("Acacia Research Corporation (Acacia Tech) Common Stock"),
            "Acacia Research Corporation (Acacia Tech)"
        );
        assert_eq!(
            company_search_name("Atlanta Braves Holdings Inc. Series C Common Stock"),
            "Atlanta Braves Holdings Inc."
        );
        assert_eq!(
            company_search_name("BrightSpring Health Services Inc. Tangible Equity Unit"),
            "BrightSpring Health Services Inc."
        );
        assert_eq!(
            company_search_name("Hovnanian Enterprises Inc Dep Shr Srs A Pfd"),
            "Hovnanian Enterprises Inc"
        );
        assert_eq!(
            company_search_name("PPL Corporation Corporate Units"),
            "PPL Corporation"
        );
        assert_eq!(
            company_search_name("PureCycle Technologies Inc. Unit"),
            "PureCycle Technologies Inc."
        );
        assert_eq!(
            company_search_name("Cronos Group Inc. Common Share"),
            "Cronos Group Inc."
        );
        assert_eq!(company_search_name("Stripe"), "Stripe");
    }

    #[test]
    fn publication_claims_allow_security_classes_but_reject_distinct_issuers() {
        let recipe_id = uuid::Uuid::new_v4();
        let company_id = uuid::Uuid::new_v4();
        let same_issuer_claim = ActiveCompanyNewsPublicationClaim {
            recipe_id,
            company_id: uuid::Uuid::new_v4(),
            company_name: "PureCycle Technologies Inc. Common Stock".to_owned(),
        };
        let distinct_issuer_claim = ActiveCompanyNewsPublicationClaim {
            recipe_id,
            company_id: uuid::Uuid::new_v4(),
            company_name: "Sprott Inc. Common Shares".to_owned(),
        };

        assert!(!publication_claim_conflicts(
            company_id,
            "PureCycle Technologies Inc. Unit",
            &same_issuer_claim
        ));
        assert!(publication_claim_conflicts(
            company_id,
            "Sprott Focus Trust Inc. Common Stock",
            &distinct_issuer_claim
        ));

        let same_issuer_feed_claim = ApprovedFeedItemCompanyClaim {
            company_id: uuid::Uuid::new_v4(),
            company_name: "Liberty Media Corporation Series C Liberty Formula One Common Stock"
                .to_owned(),
            matched_item_count: 10,
        };
        assert!(
            distinct_company_approved_feed_claim(
                company_id,
                "Liberty Media Corporation Series A Liberty Formula One Common Stock",
                10,
                &[same_issuer_feed_claim],
            )
            .is_none()
        );
        let wrong_company_feed_claim = ApprovedFeedItemCompanyClaim {
            company_id: uuid::Uuid::new_v4(),
            company_name: "ARKO Corp. Common Stock".to_owned(),
            matched_item_count: 10,
        };
        assert_eq!(
            distinct_company_approved_feed_claim(
                company_id,
                "ARKO Petroleum Corp. Class A Common Stock",
                10,
                std::slice::from_ref(&wrong_company_feed_claim),
            ),
            Some(&wrong_company_feed_claim)
        );
        let same_issuer_source_claim = ApprovedSourceCompanyClaim {
            source_id: uuid::Uuid::new_v4(),
            company_id: uuid::Uuid::new_v4(),
            company_name: "Fox Corporation Class B Common Stock".to_owned(),
        };
        assert!(
            distinct_company_approved_source_claim(
                company_id,
                "Fox Corporation Class A Common Stock",
                &[same_issuer_source_claim],
            )
            .is_none()
        );
        let wrong_company_source_claim = ApprovedSourceCompanyClaim {
            source_id: uuid::Uuid::new_v4(),
            company_id: uuid::Uuid::new_v4(),
            company_name: "Barings BDC Inc. Common Stock".to_owned(),
        };
        assert_eq!(
            distinct_company_approved_source_claim(
                company_id,
                "Barings Corporate Investors Common Stock",
                std::slice::from_ref(&wrong_company_source_claim),
            ),
            Some(&wrong_company_source_claim)
        );
        assert!(company_names_share_recipe_issuer(
            "Carnival Plc ADS ADS",
            "Carnival Corporation Common Stock"
        ));
        assert!(!company_names_share_recipe_issuer(
            "Forum",
            "Forum Markets Incorporated Common Stock"
        ));

        for (left, right) in [
            (
                "Alphabet Inc. Class A Common Stock",
                "Alphabet Inc. Class C Capital Stock",
            ),
            (
                "Atlanta Braves Holdings Inc. Series A Common Stock",
                "Atlanta Braves Holdings Inc. Series C Common Stock",
            ),
            (
                "Hovnanian Enterprises Inc. Class A Common Stock",
                "Hovnanian Enterprises Inc Dep Shr Srs A Pfd",
            ),
            (
                "GCI Liberty Inc. Series A GCI Group Common Stock",
                "GCI Liberty Inc. Series C GCI Group Common Stock",
            ),
            (
                "Liberty Global Ltd. Class B Common Shares",
                "Liberty Global Ltd. Class C Common Shares",
            ),
            (
                "PPL Corporation Common Stock",
                "PPL Corporation Corporate Units",
            ),
            (
                "PureCycle Technologies Inc. Common stock",
                "PureCycle Technologies Inc. Unit",
            ),
        ] {
            assert_eq!(
                company_recipe_identity_key(left),
                company_recipe_identity_key(right),
                "expected the same issuer identity for {left} and {right}"
            );
        }
    }

    #[test]
    fn approved_feed_overlap_suppresses_only_redundant_recipes() {
        assert!(!recipe_overlaps_approved_feed(0, 0));
        assert!(recipe_overlaps_approved_feed(1, 1));
        assert!(recipe_overlaps_approved_feed(7, 4));
        assert!(!recipe_overlaps_approved_feed(7, 3));
        assert!(!recipe_overlaps_approved_feed(20, 1));
    }

    #[test]
    fn approved_feed_overlap_rejects_only_fully_covered_feed_candidates() {
        assert!(!feed_candidate_fully_covered_by_approved_feed(0, 0));
        assert!(!feed_candidate_fully_covered_by_approved_feed(2, 2));
        assert!(feed_candidate_fully_covered_by_approved_feed(3, 3));
        assert!(feed_candidate_fully_covered_by_approved_feed(10, 10));
        assert!(!feed_candidate_fully_covered_by_approved_feed(10, 9));
    }

    #[test]
    fn active_recipe_overlap_requires_near_duplication() {
        assert!(!recipe_overlaps_active_recipe(0, 0));
        assert!(!recipe_overlaps_active_recipe(1, 1));
        assert!(!recipe_overlaps_active_recipe(2, 2));
        assert!(recipe_overlaps_active_recipe(3, 3));
        assert!(recipe_overlaps_active_recipe(10, 8));
        assert!(!recipe_overlaps_active_recipe(10, 7));
        assert!(!recipe_overlaps_active_recipe(20, 1));
    }

    #[test]
    fn runtime_overlap_supersession_prefers_the_strongest_replacement() {
        assert_eq!(
            runtime_recipe_supersession_reason(true, true, true, true),
            Some("company_scope_relevance_below_minimum")
        );
        assert_eq!(
            runtime_recipe_supersession_reason(false, true, true, true),
            Some("overlaps_approved_feed")
        );
        assert_eq!(
            runtime_recipe_supersession_reason(false, false, true, true),
            Some("overlaps_preferred_active_recipe")
        );
        assert_eq!(
            runtime_recipe_supersession_reason(false, false, false, true),
            Some("duplicates_active_publication")
        );
        assert_eq!(
            runtime_recipe_supersession_reason(false, false, false, false),
            None
        );
    }

    #[test]
    fn evidence_path_prefix_scopes_root_level_article_slugs() {
        let publication =
            Url::parse("https://www.dhtankers.com/investor-relations/press-releases/")
                .expect("valid publication URL");
        let evidence = [
            "https://www.dhtankers.com/dht-holdings-inc-business-update-18/",
            "https://www.dhtankers.com/dht-holdings-inc-announces-fleet-upgrades/",
            "https://www.dhtankers.com/dht-holdings-inc-first-quarter-results/",
            "https://www.dhtankers.com/unrelated-company-profile/",
        ]
        .into_iter()
        .map(|url| Url::parse(url).expect("valid evidence URL"))
        .collect::<Vec<_>>();

        assert_eq!(
            evidence_article_path_prefix(&publication, &evidence).as_deref(),
            Some("/dht-holdings-inc-")
        );
        assert!(
            evidence_article_path_prefix(&publication, &evidence[..1]).is_none(),
            "one article is insufficient evidence for a generated path scope"
        );
    }

    #[test]
    fn evidence_path_prefix_never_merges_unrelated_paths_across_hosts() {
        let publication =
            Url::parse("https://www.example.com/english/products/manufacturing/engineering")
                .expect("valid publication URL");
        let evidence = [
            "https://press.example.com/english/news/3323",
            "https://investor.example.com/english/reports/second-quarter-results.pdf",
        ]
        .into_iter()
        .map(|url| Url::parse(url).expect("valid evidence URL"))
        .collect::<Vec<_>>();

        assert!(
            evidence_article_path_prefix(&publication, &evidence).is_none(),
            "a shared language directory on different hosts is not an article scope"
        );
    }

    #[test]
    fn evidence_path_prefix_rejects_a_broad_language_directory() {
        let publication =
            Url::parse("https://www.example.com/english/company/overview").expect("URL");
        let evidence = [
            "https://www.example.com/english/current-company-update",
            "https://www.example.com/english/new-product-launch",
        ]
        .into_iter()
        .map(|url| Url::parse(url).expect("valid evidence URL"))
        .collect::<Vec<_>>();

        assert!(
            evidence_article_path_prefix(&publication, &evidence).is_none(),
            "a language root is too broad to become a trusted article scope"
        );
    }

    #[test]
    fn broad_ir_publications_prefer_the_evidence_backed_news_namespace() {
        let company = company_fixture("Example Corporation", "https://example.com/");
        let publication = Url::parse("https://investor.example.com/en").expect("URL");
        let evidence = [
            "https://investor.example.com/en/news/first-quarter-results",
            "https://investor.example.com/en/news/new-board-appointment",
            "https://investor.example.com/en/news/product-investment",
        ]
        .into_iter()
        .map(|url| Url::parse(url).expect("evidence URL"))
        .collect::<Vec<_>>();
        let spec =
            build_company_news_recipe_spec(&company, publication.clone(), evidence.clone(), 20);

        assert_eq!(spec.include_path_prefixes, vec!["/en/news/"]);
        assert_eq!(
            effective_recipe_include_path_prefixes(
                &["/en/".to_owned()],
                true,
                &publication,
                &evidence,
            ),
            vec!["/en/news/"],
            "legacy broad language scope should narrow on the next revalidation"
        );

        let explicit_news = Url::parse("https://investor.example.com/en/news").expect("news URL");
        let explicit_spec = build_company_news_recipe_spec(&company, explicit_news, evidence, 20);
        assert_eq!(
            explicit_spec.include_path_prefixes,
            vec!["/en/news/"],
            "an explicit editorial listing keeps its own publication boundary"
        );
    }

    #[test]
    fn listing_prefix_normalizes_default_and_index_documents() {
        assert_eq!(
            recipe_listing_prefix(
                &Url::parse("https://example.com/investors/news/default.aspx").expect("URL")
            )
            .as_deref(),
            Some("/investors/news/")
        );
        assert_eq!(
            recipe_listing_prefix(&Url::parse("https://example.com/news/index.html").expect("URL"))
                .as_deref(),
            Some("/news/")
        );
        assert_eq!(
            recipe_listing_prefix(&Url::parse("https://example.com/news").expect("URL")).as_deref(),
            Some("/news/")
        );
    }

    #[test]
    fn listing_prefix_maps_semantic_documents_to_their_detail_directories() {
        assert_eq!(
            recipe_listing_prefix(
                &Url::parse("https://example.com/company/news-center/feature-articles.html")
                    .expect("URL")
            )
            .as_deref(),
            Some("/company/news-center/feature-articles/")
        );
        assert_eq!(
            recipe_listing_prefix(
                &Url::parse("https://example.com/company/news-center/press-releases.aspx")
                    .expect("URL")
            )
            .as_deref(),
            Some("/company/news-center/press-releases/")
        );
        assert_eq!(
            recipe_listing_prefix(
                &Url::parse(
                    "https://example.com/company/news-center/company-launches-product.html"
                )
                .expect("URL")
            )
            .as_deref(),
            Some("/company/news-center/company-launches-product.html/")
        );
    }

    #[test]
    fn publication_priority_prefers_explicit_sections_before_parent_hubs() {
        let parent =
            Url::parse("https://example.com/company/news-center.html").expect("parent URL");
        let features = Url::parse("https://example.com/company/news-center/feature-articles.html")
            .expect("feature URL");
        let press = Url::parse("https://example.com/company/news-center/press-releases.html")
            .expect("press URL");
        let evidence = [
            "https://example.com/company/news-center/feature-articles/genomics.html",
            "https://example.com/company/news-center/feature-articles/rare-disease.html",
            "https://example.com/company/news-center/press-releases/launch.html",
            "https://example.com/company/news-center/press-releases/results.html",
        ]
        .into_iter()
        .map(|url| Url::parse(url).expect("evidence URL"))
        .collect::<Vec<_>>();

        assert!(
            publication_listing_specificity(&features) > publication_listing_specificity(&parent)
        );
        assert!(publication_listing_specificity(&press) > publication_listing_specificity(&parent));
        assert_eq!(publication_evidence_support(&features, &evidence), 2);
        assert_eq!(publication_evidence_support(&press, &evidence), 2);
        assert_eq!(publication_evidence_support(&parent, &evidence), 4);

        let mut publications = [parent.clone(), features.clone(), press.clone()];
        publications.sort_by_key(|url| {
            Reverse((
                publication_listing_specificity(url),
                publication_evidence_support(url, &evidence),
            ))
        });
        assert_eq!(publications.last(), Some(&parent));
    }

    #[test]
    fn zero_yield_listing_prefix_retries_broad_and_records_durable_evidence() {
        let publication = Url::parse("https://news.example.com/Award-news").expect("listing URL");
        let mut spec = build_company_news_recipe_spec(
            &company_fixture("Example", "https://example.com/"),
            publication,
            Vec::new(),
            20,
        );
        assert_eq!(spec.include_path_prefixes, vec!["/Award-news/"]);
        assert!(recipe_should_try_broad_scope(&spec, 0));
        assert!(!recipe_should_try_broad_scope(&spec, 1));

        spec.include_path_prefixes.clear();
        let detail_url = Url::parse(
            "https://news.example.com/2026-07-24-company-receives-a-global-innovation-award",
        )
        .expect("detail URL");
        let item = feed_core::RawCrawlItem {
            source_item_key: "validated-broad-detail".to_owned(),
            external_id: Some(detail_url.as_str().to_owned()),
            url: detail_url.clone(),
            canonical_url: Some(detail_url.clone()),
            title: Some("Company receives a global innovation award".to_owned()),
            summary_html: None,
            body_html: Some("<p>Substantive release.</p>".to_owned()),
            published_at: None,
            payload: json!({}),
        };
        record_broad_scope_validation_evidence(&mut spec, &[item]);

        assert_eq!(spec.evidence_article_urls, vec![detail_url]);
        assert!(recipe_evidence_requires_broad_scope(&spec));
        assert!(
            effective_recipe_include_path_prefixes(
                &spec.include_path_prefixes,
                true,
                &spec.publication_url,
                &spec.evidence_article_urls,
            )
            .is_empty(),
            "runtime revalidation must preserve the correctness-passing broad scope"
        );
    }

    #[test]
    fn legacy_adapter_recipes_recover_safe_listing_path_scope() {
        let publication =
            Url::parse("https://example.com/investors/news/default.aspx").expect("URL");
        assert_eq!(
            effective_recipe_include_path_prefixes(&[], true, &publication, &[]),
            vec!["/investors/news/"]
        );
        assert!(
            effective_recipe_include_path_prefixes(&[], false, &publication, &[]).is_empty(),
            "operator-authored broad recipes retain their explicit scope"
        );

        let outside_evidence =
            vec![Url::parse("https://example.com/company-announces-results").expect("URL")];
        assert!(
            effective_recipe_include_path_prefixes(&[], true, &publication, &outside_evidence,)
                .is_empty(),
            "adapter evidence outside the listing path proves a broad recipe can be intentional"
        );

        let cross_host_language_scope = vec![
            Url::parse("https://press.example.com/english/news/3323").expect("URL"),
            Url::parse("https://investor.example.com/english/reports/results.pdf").expect("URL"),
        ];
        assert!(
            effective_recipe_include_path_prefixes(
                &["/english/".to_owned()],
                true,
                &Url::parse("https://www.example.com/english/products/manufacturing/engineering")
                    .expect("URL"),
                &cross_host_language_scope,
            )
            .is_empty(),
            "legacy cross-host language prefixes fall back to article-like path validation"
        );
    }

    #[test]
    fn publication_evidence_support_prioritizes_the_article_parent_listing() {
        let evidence = [
            "https://example.com/insights/current-update",
            "https://example.com/insights/current-research",
        ]
        .into_iter()
        .map(|url| Url::parse(url).expect("valid evidence URL"))
        .collect::<Vec<_>>();
        let insights = Url::parse("https://example.com/insights").expect("valid listing URL");
        let press = Url::parse("https://example.com/press").expect("valid listing URL");

        assert_eq!(publication_evidence_support(&insights, &evidence), 2);
        assert_eq!(publication_evidence_support(&press, &evidence), 0);
    }

    #[test]
    fn evidence_identity_rejects_detail_pages_but_not_explicit_listing_roots() {
        let listing = Url::parse("https://example.com/resources/blog").expect("listing URL");
        let mut spec = build_company_news_recipe_spec(
            &company_fixture("Example", "https://example.com/"),
            listing.clone(),
            vec![listing],
            20,
        );
        assert!(!publication_matches_evidence_article(&spec));

        let detail = Url::parse(
            "https://example.com/blog/current-company-product-launch-with-major-updates",
        )
        .expect("detail URL");
        spec.publication_url = detail.clone();
        spec.evidence_article_urls = vec![detail];
        assert!(publication_matches_evidence_article(&spec));
    }

    #[test]
    fn selected_recipe_overlap_suppresses_only_fully_covered_item_sets() {
        let raw_item = |url: &str, title: &str| feed_core::RawCrawlItem {
            source_item_key: url.to_owned(),
            external_id: None,
            url: Url::parse(url).expect("valid item URL"),
            canonical_url: None,
            title: Some(title.to_owned()),
            summary_html: None,
            body_html: None,
            published_at: None,
            payload: json!({}),
        };
        let selected = HashSet::from(["https://example.com/insights/current-update".to_owned()]);
        let no_signatures = HashSet::new();

        assert!(recipe_items_are_fully_covered(
            &[raw_item(
                "https://example.com/insights/current-update",
                "Current update",
            )],
            &selected,
            &no_signatures,
        ));
        assert!(!recipe_items_are_fully_covered(
            &[
                raw_item(
                    "https://example.com/insights/current-update",
                    "Current update",
                ),
                raw_item(
                    "https://example.com/research/distinct-update",
                    "Distinct update",
                ),
            ],
            &selected,
            &no_signatures,
        ));
        assert!(!recipe_items_are_fully_covered(
            &[],
            &selected,
            &no_signatures,
        ));

        let mirror_items = [
            raw_item("https://mirror.example.com/news/one", "Launch one"),
            raw_item("https://mirror.example.com/news/two", "Launch two"),
            raw_item("https://mirror.example.com/news/three", "Launch three"),
        ];
        let mirror_signatures = mirror_items
            .iter()
            .filter_map(raw_crawl_item_signature)
            .collect::<HashSet<_>>();
        assert!(recipe_items_are_fully_covered(
            &mirror_items,
            &HashSet::new(),
            &mirror_signatures,
        ));
        assert!(!recipe_items_are_fully_covered(
            &mirror_items[..2],
            &HashSet::new(),
            &mirror_signatures,
        ));
    }

    #[test]
    fn broader_candidate_replaces_only_selected_recipe_subsets() {
        let subset_recipe_id = uuid::Uuid::new_v4();
        let distinct_recipe_id = uuid::Uuid::new_v4();
        let selected = vec![
            SelectedRecipeSample {
                recipe_id: subset_recipe_id,
                publication_identity: "https://example.com/news-events".to_owned(),
                item_urls: HashSet::from([
                    "https://example.com/news/one".to_owned(),
                    "https://example.com/news/two".to_owned(),
                ]),
                item_signatures: HashSet::new(),
            },
            SelectedRecipeSample {
                recipe_id: distinct_recipe_id,
                publication_identity: "https://example.com/engineering".to_owned(),
                item_urls: HashSet::from([
                    "https://example.com/engineering/runtime".to_owned(),
                    "https://example.com/engineering/security".to_owned(),
                ]),
                item_signatures: HashSet::new(),
            },
        ];
        let broader_candidate = HashSet::from([
            "https://example.com/news/one".to_owned(),
            "https://example.com/news/two".to_owned(),
            "https://example.com/news/three".to_owned(),
        ]);

        let covered =
            selected_recipes_covered_by_candidate(&selected, &broader_candidate, &HashSet::new());

        assert_eq!(covered.len(), 1);
        assert_eq!(covered[0].recipe_id, subset_recipe_id);
    }

    #[test]
    fn recipe_publications_require_editorial_scope() {
        for url in [
            "https://example.com/blog",
            "https://investors.example.com/news-events/press-releases",
            "https://newsroom.example.com/",
            "https://devblogs.microsoft.com/",
            "https://builders.example.com/",
            "https://example.com/resources",
            "https://example.com/changelog",
            "https://example.com/latest",
            "https://example.com/posts",
            "https://example.com/reports-and-papers/technology-review",
            "https://example.com/services/engineering/insights",
            "https://example.com/careers/engineering-blog",
            "https://example.com/careers/resources/blog.html",
            "https://docs.example.com/blog",
            "https://support.example.com/news",
            "https://help.example.com/hc/en-us/sections/42412777924749-2026-Validated-Cloud-Releases",
            "https://tech.example.com/",
            "https://company.substack.com/",
            "https://medium.com/@company",
            "https://www.globenewswire.com/en/search/organization/Acme%20Corporation",
        ] {
            assert!(
                likely_company_news_publication(&Url::parse(url).expect("valid URL")),
                "expected editorial publication: {url}"
            );
        }
        for url in [
            "https://example.com/",
            "https://investors.example.com/",
            "https://investors.example.com/corporate-profile",
            "https://blog.example.com/author/alice",
            "https://blog.example.com/categories/engineering",
            "https://blog.example.com/pillar/platform",
            "https://example.com/news/news-search",
            "https://example.com/services/engineering",
            "https://example.com/products/developer",
            "https://example.com/roles/engineering",
            "https://example.com/use-cases/engineering",
            "https://example.com/blog/2026/07",
            "https://example.com/news/2026",
            "https://airjouletech.com/investors",
            "https://www.dhtankers.com/investor-relations/subscribe-press-releases/",
            "https://www.globenewswire.com/newsroom",
            "https://www.globenewswire.com/news/energy",
            "https://www.accessnewswire.com/newsroom/",
            "https://www.businesswire.com/news/",
            "https://www.investing.com/news/",
            "https://www.nasdaq.com/european-market-activity/news/company-news",
            "https://www.nasdaq.com/market-activity/quotes/press-releases",
            "https://www.nasdaq.com/press-release/",
            "https://www.prnewswire.com/news/",
            "https://www.prnewswire.com/news-releases/",
            "https://www.prnewswire.com/resources/articles",
            "https://www.prnewswire.com/ru/press-releases/",
            "https://www.stocktitan.net/news/",
            "https://www.blackrock.com/us/financial-professionals/investments/products/closed-end-funds/press-releases",
            "https://gabelli.com/insights/gabelli-media/press-releases/",
            "https://midrender.com/revideo/docs/api-reference/core/media",
            "https://support.example.com/en/article/release-notes-app-updates",
            "https://help.example.com/en/articles/product-updates",
        ] {
            assert!(
                !likely_company_news_publication(&Url::parse(url).expect("valid URL")),
                "expected non-editorial publication: {url}"
            );
        }
        assert!(likely_company_news_publication(
            &Url::parse("https://www.elastic.co/search-labs/blog").expect("valid URL")
        ));
    }

    #[test]
    fn temporal_archives_normalize_to_stable_publications() {
        assert_eq!(
            stable_publication_url(
                &Url::parse("https://blogs.shell.com/2026/07").expect("archive URL")
            )
            .as_str(),
            "https://blogs.shell.com/",
        );
        assert_eq!(
            stable_publication_url(
                &Url::parse("https://example.com/blog/2026/7").expect("archive URL")
            )
            .as_str(),
            "https://example.com/blog/",
        );
        assert_eq!(
            stable_publication_url(
                &Url::parse("https://www.cgi.com/en/press-releases/2026")
                    .expect("year archive URL")
            )
            .as_str(),
            "https://www.cgi.com/en/press-releases/",
        );
        assert_eq!(
            stable_publication_url(
                &Url::parse(
                    "https://www.illumina.com/company/news-center/press-releases/2026.html"
                )
                .expect("document year archive URL")
            )
            .as_str(),
            "https://www.illumina.com/company/news-center/press-releases/",
        );
        assert_eq!(
            stable_publication_url(
                &Url::parse("https://example.com/blog/2026/07.aspx")
                    .expect("document month archive URL")
            )
            .as_str(),
            "https://example.com/blog/",
        );
        assert_eq!(
            stable_publication_url(
                &Url::parse("https://www.velfinance.com/news/year/2026")
                    .expect("named year archive URL")
            )
            .as_str(),
            "https://www.velfinance.com/news/",
        );
        assert_eq!(
            stable_publication_url(
                &Url::parse("https://blogs.shell.com/2026").expect("hosted year archive URL")
            )
            .as_str(),
            "https://blogs.shell.com/",
        );
        let unscoped_archive =
            Url::parse("https://example.com/2026/07").expect("unscoped archive URL");
        assert_eq!(
            stable_publication_url(&unscoped_archive),
            unscoped_archive,
            "a date path on a non-editorial host must not become the company homepage"
        );
        let unscoped_year_archive =
            Url::parse("https://example.com/2026").expect("unscoped year archive URL");
        assert_eq!(
            stable_publication_url(&unscoped_year_archive),
            unscoped_year_archive,
            "a year path on a non-editorial host must not become the company homepage"
        );
        assert!(!likely_company_news_publication(
            &Url::parse("https://blogs.shell.com/2026/07").expect("archive URL")
        ));
        assert!(!likely_company_news_publication(
            &Url::parse("https://example.com/news/year/2026").expect("year archive URL")
        ));
    }

    #[test]
    fn ambiguous_base_domain_publications_must_prove_a_collection() {
        assert!(!is_editorial_subdomain(Some("airjouletech.com")));
        assert!(!is_editorial_subdomain(Some("grahammedia.com")));
        assert!(!is_editorial_subdomain(Some("airjouletech.co.uk")));
        assert!(is_editorial_subdomain(Some("tech.example.com")));
        assert!(is_editorial_subdomain(Some("blog.example.co.uk")));
        assert!(is_editorial_subdomain(Some("blogs.shell.com")));

        let publication =
            Url::parse("https://airjouletech.com/investors").expect("ambiguous publication URL");
        let mut report = HtmlRecipeCrawlReport {
            fetched_at: Utc::now(),
            publication_final_url: publication.clone(),
            discovered_url_count: 7,
            accepted_item_count: 1,
            distinct_title_count: 1,
            distinct_content_count: 1,
            rejected_url_count: 6,
            acceptance_ratio_bps: 1_428,
            latest_published_at: Some(Utc::now()),
            dated_item_count: 1,
            publication_date_coverage_complete: true,
            structure_fingerprint: "fixture".to_owned(),
            correctness_reasons: Vec::new(),
            content_stale: false,
            items: Vec::new(),
            failures: Vec::new(),
        };
        assert!(!publication_scope_has_editorial_evidence(
            &publication,
            &[],
            &report
        ));

        report.accepted_item_count = 3;
        report.distinct_title_count = 3;
        report.distinct_content_count = 3;
        report.rejected_url_count = 4;
        report.acceptance_ratio_bps = 4_285;
        let evidence = [
            Url::parse("https://airjouletech.com/investors/company-update-one")
                .expect("evidence URL"),
            Url::parse("https://airjouletech.com/investors/company-update-two")
                .expect("evidence URL"),
        ];
        assert!(publication_scope_has_editorial_evidence(
            &publication,
            &evidence,
            &report
        ));

        let year_archive = Url::parse("https://example.com/news/2026").expect("year archive URL");
        assert!(
            !publication_scope_has_editorial_evidence(&year_archive, &evidence, &report),
            "collection evidence must not rescue an inherently unstable archive URL"
        );
    }

    #[test]
    fn organizational_engineering_pages_require_collection_evidence() {
        let publication =
            Url::parse("https://example.com/careers/departments/engineering").expect("URL");
        let mut report = HtmlRecipeCrawlReport {
            fetched_at: Utc::now(),
            publication_final_url: publication.clone(),
            discovered_url_count: 1,
            accepted_item_count: 1,
            distinct_title_count: 1,
            distinct_content_count: 1,
            rejected_url_count: 0,
            acceptance_ratio_bps: 10_000,
            latest_published_at: None,
            dated_item_count: 0,
            publication_date_coverage_complete: false,
            structure_fingerprint: "fixture".to_owned(),
            correctness_reasons: Vec::new(),
            content_stale: false,
            items: Vec::new(),
            failures: Vec::new(),
        };
        assert!(!organizational_scope_has_editorial_evidence(
            &publication,
            &report,
        ));

        report.discovered_url_count = 10;
        report.accepted_item_count = 10;
        report.distinct_title_count = 10;
        report.distinct_content_count = 10;
        report.latest_published_at = Some(Utc::now());
        assert!(organizational_scope_has_editorial_evidence(
            &publication,
            &report,
        ));

        let explicit_blog =
            Url::parse("https://example.com/careers/engineering-blog").expect("URL");
        report.accepted_item_count = 0;
        report.distinct_title_count = 0;
        report.distinct_content_count = 0;
        report.latest_published_at = None;
        assert!(organizational_scope_has_editorial_evidence(
            &explicit_blog,
            &report,
        ));
    }

    #[test]
    fn infers_a_stable_editorial_root_from_an_adapter_detail_page() {
        assert_eq!(
            infer_publication_url(
                &Url::parse("https://docs.example.com/blog/design-components-name")
                    .expect("detail URL")
            )
            .expect("publication root")
            .as_str(),
            "https://docs.example.com/blog/",
        );
        assert!(
            infer_publication_url(
                &Url::parse("https://docs.example.com/blog").expect("listing URL")
            )
            .is_none()
        );
        assert!(is_stable_editorial_parent(
            &Url::parse("https://docs.example.com/blog/").expect("parent URL"),
            &Url::parse("https://docs.example.com/blog/design-components-name")
                .expect("detail URL"),
        ));
        assert!(!is_stable_editorial_parent(
            &Url::parse("https://docs.example.com/news/").expect("unrelated URL"),
            &Url::parse("https://docs.example.com/blog/design-components-name")
                .expect("detail URL"),
        ));
    }

    #[test]
    fn article_detail_urls_are_not_listing_fallbacks() {
        for url in [
            "https://example.com/news/a-long-company-announcement-with-several-words",
            "https://example.com/press-releases/2026/a-long-release/default.aspx",
            "https://example.com/blog/how-we-built-the-company-platform-from-scratch",
            "https://example.com/notices-and-results/notice-to-shareholders-and-the-market",
        ] {
            assert!(
                looks_like_article_detail_url(&Url::parse(url).expect("valid URL")),
                "expected article detail URL: {url}"
            );
        }
        for url in [
            "https://example.com/news",
            "https://example.com/news/press-releases",
            "https://example.com/press-releases/2026",
            "https://example.com/newsroom/around-the-diamond",
            "https://example.com/newsroom/apple-services",
            "https://example.com/newsroom/latest-stories",
            "https://newsroom.example.com/",
            "https://medium.com/@company",
        ] {
            assert!(
                !looks_like_article_detail_url(&Url::parse(url).expect("valid URL")),
                "expected publication listing URL: {url}"
            );
        }
    }

    #[test]
    fn official_host_matching_accepts_www_and_subdomains() {
        assert!(hosts_related(
            Some("engineering.example.com"),
            Some("www.example.com")
        ));
        assert!(hosts_related(Some("example.com"), Some("www.example.com")));
        assert!(!hosts_related(
            Some("example-news.com"),
            Some("example.com")
        ));
    }

    #[test]
    fn company_name_host_matching_ignores_legal_and_security_suffixes() {
        assert!(company_identity_name_matches_host(
            "GitLab Inc. Class A Common Stock",
            "about.gitlab.com"
        ));
        assert!(company_identity_name_matches_host(
            "Spotify Technology S.A. Ordinary Shares",
            "engineering.atspotify.com"
        ));
        assert!(company_identity_name_matches_host(
            "Netflix Inc. Common Stock",
            "netflixtechblog.com"
        ));
        assert!(!company_identity_name_matches_host(
            "Airbnb Inc. Class A Common Stock",
            "medium.com"
        ));
        assert!(!company_identity_name_matches_host(
            "Union Pacific Corporation Common Stock",
            "uprr.com"
        ));
        assert!(company_identity_name_matches_host(
            "U.S. Bancorp Common Stock",
            "ir.usbank.com"
        ));
        assert!(!company_identity_name_matches_host(
            "U.S. Bancorp Common Stock",
            "bank.com"
        ));
        assert!(company_identity_name_matches_host(
            "American International Group Inc. New Common Stock",
            "www.aig.com"
        ));
        assert!(company_identity_name_matches_host(
            "Core Molding Technologies Inc Common Stock",
            "coremt.com"
        ));
        assert!(company_identity_name_matches_host(
            "F.N.B. Corporation Common Stock",
            "www.fnb-online.com"
        ));
        assert!(company_identity_name_matches_host(
            "W. P. Carey Inc. REIT",
            "www.wpcarey.com"
        ));
        assert!(company_identity_name_matches_host(
            "Xos Inc. Common Stock",
            "www.xostrucks.com"
        ));
        assert!(company_identity_name_matches_host(
            "Banco Bilbao Vizcaya Argentaria S.A. Common Stock",
            "www.bbva.com"
        ));
        assert!(company_identity_name_matches_host(
            "Applied Optoelectronics Inc. Common Stock",
            "newsroom.ao-inc.com"
        ));
        assert!(company_identity_name_matches_host(
            "Barrett Business Services Inc. Common Stock",
            "www.bbsi.com"
        ));
        assert!(company_identity_name_matches_host(
            "CBL & Associates Properties Inc. Common Stock",
            "invest.cblproperties.com"
        ));
        assert!(company_identity_name_matches_host(
            "Root Inc. Class A Common Stock",
            "www.joinroot.com"
        ));
        assert!(!company_identity_name_matches_host(
            "Root Inc. Class A Common Stock",
            "www.uproot.com"
        ));
        assert!(!company_identity_name_matches_host(
            "Xos Inc. Common Stock",
            "xosrandom.example.com"
        ));
        assert!(!company_identity_name_matches_host(
            "American International Group Inc. New Common Stock",
            "aig-news.example.com"
        ));
        assert!(company_identity_name_matches_host(
            "Perpetuals.com Ltd American Depositary Shares",
            "www.perpetuals.com"
        ));
        assert!(!company_identity_name_matches_host(
            "Perpetuals.com Ltd American Depositary Shares",
            "lasvegassun.com"
        ));
    }

    #[test]
    fn company_scope_terms_match_distinctive_names_acronyms_and_prefixes() {
        let keros = company_fixture(
            "Keros Therapeutics Inc. Common Stock",
            "https://www.kerostx.com/",
        );
        let keros_terms = company_scope_identity_terms(&keros);
        assert!(text_mentions_company_scope_term(
            "Keros appoints a new director",
            &keros_terms
        ));
        assert!(!text_mentions_company_scope_term(
            "Another therapeutics company reports results",
            &keros_terms
        ));

        let advanced_flower = company_fixture(
            "Advanced Flower Capital Inc. Common Stock",
            "https://www.advancedflowercapital.com/",
        );
        assert!(text_mentions_company_scope_term(
            "AFC schedules its quarterly earnings release",
            &company_scope_identity_terms(&advanced_flower)
        ));

        let newgen = company_fixture(
            "NewGenIvf Group Limited Class A Ordinary Shares",
            "https://www.newgenivf.com/",
        );
        assert!(text_mentions_company_scope_term(
            "NewGen provides a strategic update",
            &company_scope_identity_terms(&newgen)
        ));

        let marketwise = company_fixture(
            "MarketWise Inc. Class A Common Stock",
            "https://marketwise.com/",
        );
        let marketwise_terms = company_scope_identity_terms(&marketwise);
        assert!(!text_mentions_company_scope_term(
            "Nordic Markets - Corporate Actions",
            &marketwise_terms
        ));
        assert!(!text_mentions_company_scope_term(
            "Market Notices",
            &marketwise_terms
        ));

        let aig = company_fixture(
            "American International Group Inc. New Common Stock",
            "https://www.aig.com/",
        );
        assert!(text_mentions_company_scope_term(
            "AIG raises the standard in women's golf",
            &company_scope_identity_terms(&aig)
        ));

        let bd = company_fixture(
            "Becton Dickinson and Company Common Stock",
            "https://example.com/",
        );
        let bd_item = feed_core::RawCrawlItem {
            source_item_key: "bd-award".to_owned(),
            external_id: None,
            url: Url::parse("https://news.bd.com/2026-bd-receives-an-innovation-award")
                .expect("valid item URL"),
            canonical_url: None,
            title: Some("BD receives a global innovation award".to_owned()),
            summary_html: None,
            body_html: None,
            published_at: None,
            payload: json!({}),
        };
        assert!(
            raw_item_mentions_company(&bd, &bd_item),
            "a two-letter acronym requires both an exact title token and a matching host label"
        );
        let unrelated_bd_item = feed_core::RawCrawlItem {
            url: Url::parse("https://news.example.com/2026-bd-innovation-award")
                .expect("valid unrelated URL"),
            ..bd_item
        };
        assert!(
            !raw_item_mentions_company(&bd, &unrelated_bd_item),
            "a short acronym alone cannot claim an unrelated news host"
        );
        let terminal_label_only_item = feed_core::RawCrawlItem {
            source_item_key: "ai-news".to_owned(),
            external_id: None,
            url: Url::parse("https://unrelated.ai/company-ai-update").expect("valid AI URL"),
            canonical_url: None,
            title: Some("AI shares a company update".to_owned()),
            summary_html: None,
            body_html: None,
            published_at: None,
            payload: json!({}),
        };
        let artificial_intelligence =
            company_fixture("Artificial Intelligence Company", "https://example.com/");
        assert!(
            !raw_item_mentions_company(&artificial_intelligence, &terminal_label_only_item),
            "a terminal DNS label cannot corroborate a two-letter acronym"
        );

        let canadian_national = company_fixture(
            "Canadian National Railway Company Common Stock",
            "https://example.com/",
        );
        let cn_item = feed_core::RawCrawlItem {
            source_item_key: "cn-dividend".to_owned(),
            external_id: None,
            url: Url::parse(
                "https://www.cn.ca/en/news/2026/07/cn-declares-third-quarter-dividend/",
            )
            .expect("valid CN URL"),
            canonical_url: None,
            title: Some("CN Declares Third-Quarter Dividend".to_owned()),
            summary_html: None,
            body_html: None,
            published_at: None,
            payload: json!({}),
        };
        assert!(
            raw_item_mentions_company(&canadian_national, &cn_item),
            "a generic railway descriptor does not belong to the two-letter public brand"
        );
        let unrelated_cn_item = feed_core::RawCrawlItem {
            url: Url::parse("https://news.example.ca/cn-dividend").expect("valid unrelated URL"),
            ..cn_item
        };
        assert!(
            !raw_item_mentions_company(&canadian_national, &unrelated_cn_item),
            "the CN title token still requires the matching company host label"
        );
    }

    #[test]
    fn company_scope_terms_ignore_narrative_alias_annotations() {
        let mut company = company_fixture("SigmanticAI", "https://sigmantic.ai/");
        company.aliases = vec![
            "BitForge (in process of incorporating as SigmanticAI Inc due to trademark)".to_owned(),
        ];
        let unrelated = [
            raw_item_fixture(
                "https://anysilicon.com/news/amkor-expands-ai-infrastructure",
                "Amkor expands advanced packaging to support AI infrastructure",
            ),
            raw_item_fixture(
                "https://anysilicon.com/news/asicland-selected-as-partner",
                "ASICLAND selected as a next-generation development partner",
            ),
            raw_item_fixture(
                "https://anysilicon.com/news/onsemi-invests-in-physical-ai",
                "onsemi invests in physical AI",
            ),
        ];

        for item in unrelated {
            assert!(
                !raw_item_mentions_company(&company, &item),
                "annotation prose cannot become company identity evidence: {}",
                item.title.as_deref().unwrap_or_default()
            );
        }
        assert!(raw_item_mentions_company(
            &company,
            &raw_item_fixture(
                "https://anysilicon.com/news/sigmanticai-platform",
                "SigmanticAI launches its verification platform",
            )
        ));

        let three_m = company_fixture("3M Company Common Stock", "https://www.3m.com/");
        assert!(raw_item_mentions_company(
            &three_m,
            &raw_item_fixture(
                "https://www.prnewswire.com/news-releases/3m-announces-results.html",
                "3M announces quarterly results",
            )
        ));
    }

    #[test]
    fn managed_vehicle_scope_requires_composite_identity_on_manager_hosts() {
        let mut fund = company_fixture(
            "BlackRock Energy and Resources Trust",
            "https://www.blackrock.com/us/individual/products/240226/",
        );
        fund.metadata = json!({
            "universe": {
                "industry": "Trusts Except Educational Religious and Charitable"
            }
        });
        assert!(shared_manager_host_for_vehicle(
            &fund,
            Some("engineering.blackrock.com")
        ));
        assert!(!raw_item_mentions_company(
            &fund,
            &raw_item_fixture(
                "https://www.blackrock.com/corporate/newsroom/blackrock-results",
                "BlackRock Reports Second Quarter Results",
            )
        ));
        assert!(raw_item_mentions_company(
            &fund,
            &raw_item_fixture(
                "https://www.blackrock.com/funds/blackrock-energy-resources-trust-distribution",
                "BlackRock Energy and Resources Trust Declares Distribution",
            )
        ));
        let manager_publication =
            Url::parse("https://www.blackrock.com/corporate/newsroom").expect("valid manager URL");
        assert_eq!(
            effective_recipe_item_scope(
                &fund,
                RecipeItemScope::PublicationBoundary,
                true,
                &manager_publication,
            ),
            RecipeItemScope::CompanyIdentity
        );
        let mut spec = build_company_news_recipe_spec(&fund, manager_publication, Vec::new(), 20);
        spec.item_scope = RecipeItemScope::CompanyIdentity;
        let mut report = recipe_report_fixture(vec![
            raw_item_fixture(
                "https://www.blackrock.com/corporate/newsroom/blackrock-results",
                "BlackRock Reports Second Quarter Results",
            ),
            raw_item_fixture(
                "https://www.blackrock.com/funds/blackrock-energy-resources-trust-distribution",
                "BlackRock Energy and Resources Trust Declares Distribution",
            ),
        ]);
        apply_company_scope_filter(&fund, &spec, &mut report);
        assert_eq!(report.accepted_item_count, 1);
        assert_eq!(
            report
                .failures
                .iter()
                .filter(|failure| failure.reason == "article_not_company_scoped")
                .count(),
            1
        );

        let parent = company_fixture("BlackRock Inc. Common Stock", "https://www.blackrock.com/");
        assert!(!shared_manager_host_for_vehicle(
            &parent,
            Some("engineering.blackrock.com")
        ));
        assert!(raw_item_mentions_company(
            &parent,
            &raw_item_fixture(
                "https://www.blackrock.com/corporate/newsroom/blackrock-results",
                "BlackRock Reports Second Quarter Results",
            )
        ));
    }

    #[test]
    fn managed_vehicle_scope_rejects_a_multiword_manager_brand_by_itself() {
        let mut fund = company_fixture(
            "Angel Oak Financial Strategies Income Term Trust",
            "https://angeloakcapital.com/investments/fins/",
        );
        fund.metadata = json!({
            "universe": {
                "industry": "Finance/Investors Services"
            }
        });
        fund.aliases = vec!["Financial Strategies Income Term Trust".to_owned()];
        assert!(company_requires_composite_vehicle_identity(&fund));
        assert!(!raw_item_mentions_company(
            &fund,
            &raw_item_fixture(
                "https://angeloakcapital.com/2026-mid-year-outlook/",
                "Angel Oak 2026 Mid-Year Outlook",
            )
        ));
        assert!(raw_item_mentions_company(
            &fund,
            &raw_item_fixture(
                "https://angeloakcapital.com/financial-strategies-income-term-trust-update/",
                "Financial Strategies Income Term Trust Portfolio Update",
            )
        ));

        let mut operating_reit = company_fixture(
            "Digital Realty Trust Inc. Common Stock",
            "https://www.digitalrealty.com/",
        );
        operating_reit.metadata = json!({
            "universe": {
                "industry": "Real Estate Investment Trusts"
            }
        });
        assert!(company_requires_composite_vehicle_identity(&operating_reit));
        assert!(!shared_manager_host_for_vehicle(
            &operating_reit,
            Some("www.digitalrealty.com")
        ));

        let mut managed_reit = company_fixture(
            "Blackstone Mortgage Trust Inc. Common Stock",
            "https://www.bxmt.com/",
        );
        managed_reit.metadata = json!({
            "universe": {
                "industry": "Real Estate Investment Trusts"
            }
        });
        assert!(shared_manager_host_for_vehicle(
            &managed_reit,
            Some("www.blackstone.com")
        ));
        assert!(!raw_item_mentions_company(
            &managed_reit,
            &raw_item_fixture(
                "https://www.blackstone.com/news/press/blackstone-results",
                "Blackstone Reports Second Quarter Results",
            )
        ));
        assert!(raw_item_mentions_company(
            &managed_reit,
            &raw_item_fixture(
                "https://www.blackstone.com/news/press/blackstone-mortgage-trust-results",
                "Blackstone Mortgage Trust Reports Second Quarter Results",
            )
        ));

        let sprott_fund =
            company_fixture("Sprott Focus Trust Inc.", "https://sprott.com/focus-trust/");
        assert!(shared_manager_host_for_vehicle(
            &sprott_fund,
            Some("sprott.com")
        ));
        assert!(!raw_item_mentions_company(
            &sprott_fund,
            &raw_item_fixture(
                "https://sprott.com/investor-relations/sprott-results",
                "Sprott Announces First Quarter Results",
            )
        ));
        assert!(!raw_item_mentions_company(
            &sprott_fund,
            &raw_item_fixture(
                "https://sprott.com/investor-relations/sprott-physical-copper-trust",
                "Sprott Physical Copper Trust Announces Filing to List on NYSE Arca",
            )
        ));
        assert!(raw_item_mentions_company(
            &sprott_fund,
            &raw_item_fixture(
                "https://sprott.com/focus-trust/sprott-focus-trust-distribution",
                "Sprott Focus Trust Declares Quarterly Distribution",
            )
        ));
    }

    #[test]
    fn adapter_cited_publications_trust_company_hosts_but_scope_cross_domain_sources() {
        let landmark = company_fixture("Landmark Bancorp", "https://www.banklandmark.com/");
        assert_eq!(
            adapter_cited_publication_item_scope(
                &landmark,
                &Url::parse("https://www.banklandmark.com/blog").expect("valid publication URL")
            ),
            RecipeItemScope::PublicationBoundary
        );
        let sallie_mae = company_fixture("SLM Corporation", "https://www.salliemae.com/");
        assert_eq!(
            adapter_cited_publication_item_scope(
                &sallie_mae,
                &Url::parse("https://news.salliemae.com/news-releases")
                    .expect("valid publication URL")
            ),
            RecipeItemScope::PublicationBoundary
        );
        let acme = company_fixture("Acme", "https://www.acme.com/");
        assert_eq!(
            adapter_cited_publication_item_scope(
                &acme,
                &Url::parse("https://www.biospace.com/press-releases")
                    .expect("valid publication URL")
            ),
            RecipeItemScope::CompanyIdentity
        );
        assert_eq!(
            adapter_cited_publication_item_scope(
                &acme,
                &Url::parse("https://finance.yahoo.com/quote/ACME/news")
                    .expect("valid publication URL")
            ),
            RecipeItemScope::CompanyIdentity
        );
        assert_eq!(
            adapter_cited_publication_item_scope(
                &acme,
                &Url::parse("https://public.com/stocks/acme/news").expect("valid publication URL")
            ),
            RecipeItemScope::CompanyIdentity
        );
        assert_eq!(
            adapter_cited_publication_item_scope(
                &acme,
                &Url::parse("https://www.paytient.com/blog").expect("valid publication URL")
            ),
            RecipeItemScope::CompanyIdentity
        );

        let mut activeloop = company_fixture("Activeloop", "https://activeloop.ai/");
        activeloop.metadata = json!({
            "publication_host_policy": {
                "verified_hosts": ["deeplake.ai"]
            }
        });
        assert_eq!(
            adapter_cited_publication_item_scope(
                &activeloop,
                &Url::parse("https://docs.deeplake.ai/blog").expect("valid publication URL")
            ),
            RecipeItemScope::PublicationBoundary
        );

        let mut lofty = company_fixture("Lofty", "https://lofty.ai/");
        lofty.metadata = json!({
            "publication_host_policy": {
                "verified_hosts": ["lofty.com"],
                "excluded_hosts": ["https://www.lofty.com/blog"]
            }
        });
        let conflicting =
            Url::parse("https://official.lofty.com/blog").expect("valid publication URL");
        assert!(publication_host_is_excluded(&lofty, &conflicting));
        assert!(!publication_host_is_company_related(&lofty, &conflicting));
        assert_eq!(
            adapter_cited_publication_item_scope(&lofty, &conflicting),
            RecipeItemScope::CompanyIdentity
        );
    }

    #[test]
    fn recipe_rebuild_inputs_distinguish_unhealthy_and_expansion_states() {
        assert!(recipe_status_is_rebuild_candidate(
            RecipeStatus::Active,
            None
        ));
        assert!(recipe_status_is_rebuild_candidate(
            RecipeStatus::Stale,
            Some("content_stale")
        ));
        assert!(!recipe_status_is_rebuild_candidate(
            RecipeStatus::Stale,
            Some("publication_owned_by_different_company")
        ));
        assert!(!recipe_status_is_rebuild_candidate(
            RecipeStatus::Superseded,
            None
        ));

        assert!(recipe_state_requires_rebuild(
            RecipeStatus::Active,
            None,
            false,
            "content_stale"
        ));
        assert!(recipe_state_requires_rebuild(
            RecipeStatus::Active,
            None,
            true,
            "fresh"
        ));
        assert!(recipe_state_requires_rebuild(
            RecipeStatus::Stale,
            Some("structure_changed"),
            false,
            "fresh"
        ));
        assert!(!recipe_state_requires_rebuild(
            RecipeStatus::Active,
            None,
            false,
            "fresh"
        ));
        assert!(!recipe_state_requires_rebuild(
            RecipeStatus::Stale,
            Some("publication_owned_by_different_company"),
            true,
            "content_stale"
        ));

        assert!(!recipe_state_is_build_input(
            RecipeStatus::Active,
            None,
            false,
            "fresh",
            false
        ));
        assert!(recipe_state_is_build_input(
            RecipeStatus::Active,
            None,
            false,
            "fresh",
            true
        ));
        assert!(!recipe_state_is_build_input(
            RecipeStatus::Stale,
            Some("publication_owned_by_different_company"),
            true,
            "content_stale",
            true
        ));

        assert!(recipe_state_is_healthy_active(
            RecipeStatus::Active,
            false,
            "fresh"
        ));
        assert!(!recipe_state_is_healthy_active(
            RecipeStatus::Active,
            false,
            "content_stale"
        ));
        assert!(!recipe_state_is_healthy_active(
            RecipeStatus::Active,
            true,
            "fresh"
        ));
    }

    #[test]
    fn legacy_adapter_recipes_recover_their_publication_boundary_scope() {
        let mut company = company_fixture("Mountain", "https://mountain.com/");
        let dedicated =
            Url::parse("https://research.mountain.com/").expect("valid publication URL");
        assert_eq!(
            effective_recipe_item_scope(
                &company,
                RecipeItemScope::CompanyIdentity,
                true,
                &dedicated
            ),
            RecipeItemScope::PublicationBoundary
        );
        assert_eq!(
            effective_recipe_item_scope(
                &company,
                RecipeItemScope::CompanyIdentity,
                false,
                &dedicated
            ),
            RecipeItemScope::CompanyIdentity
        );

        let shared =
            Url::parse("https://www.biospace.com/press-releases").expect("valid publication URL");
        assert_eq!(
            effective_recipe_item_scope(&company, RecipeItemScope::CompanyIdentity, true, &shared),
            RecipeItemScope::CompanyIdentity
        );
        let unrelated = Url::parse("https://www.paytient.com/blog").expect("valid publication URL");
        assert_eq!(
            effective_recipe_item_scope(
                &company,
                RecipeItemScope::CompanyIdentity,
                true,
                &unrelated
            ),
            RecipeItemScope::CompanyIdentity
        );
        assert_eq!(
            effective_recipe_item_scope(
                &company,
                RecipeItemScope::PublicationBoundary,
                true,
                &unrelated
            ),
            RecipeItemScope::PublicationBoundary
        );
        assert_eq!(
            effective_recipe_item_scope(
                &company,
                RecipeItemScope::PublicationBoundary,
                false,
                &unrelated
            ),
            RecipeItemScope::PublicationBoundary
        );

        company.metadata = json!({
            "publication_host_policy": {
                "excluded_hosts": ["paytient.com"]
            }
        });
        assert_eq!(
            effective_recipe_item_scope(
                &company,
                RecipeItemScope::PublicationBoundary,
                true,
                &unrelated
            ),
            RecipeItemScope::CompanyIdentity
        );

        company.metadata = json!({
            "publication_host_policy": {
                "verified_hosts": ["paytient.com"]
            }
        });
        assert_eq!(
            effective_recipe_item_scope(
                &company,
                RecipeItemScope::PublicationBoundary,
                true,
                &unrelated
            ),
            RecipeItemScope::PublicationBoundary
        );
    }

    #[test]
    fn publication_boundary_scope_supports_public_brands_and_renamed_companies() {
        let company = company_fixture("SLM Corporation Common Stock", "https://example.invalid/");
        let mut spec = build_company_news_recipe_spec(
            &company,
            Url::parse("https://www.salliemae.com/news").expect("valid publication URL"),
            Vec::new(),
            20,
        );
        spec.item_scope = RecipeItemScope::PublicationBoundary;
        let mut report = recipe_report_fixture(vec![
            raw_item_fixture(
                "https://news.salliemae.com/releases/parent-loan",
                "A new parent loan helps families cover college costs",
            ),
            raw_item_fixture(
                "https://news.salliemae.com/releases/graduate-scholarship",
                "A scholarship program expands for graduate students",
            ),
            raw_item_fixture(
                "https://news.salliemae.com/releases/quarterly-results",
                "Second-quarter financial results are available",
            ),
        ]);

        apply_company_scope_filter(&company, &spec, &mut report);

        assert_eq!(report.accepted_item_count, 3);
        assert!(report.failures.is_empty());
        assert!(report.correctness_passed());
    }

    #[test]
    fn company_scope_filter_blocks_unscoped_third_party_collections() {
        let company = company_fixture(
            "Keros Therapeutics Inc. Common Stock",
            "https://www.kerostx.com/",
        );
        let spec = build_company_news_recipe_spec(
            &company,
            Url::parse("https://www.biospace.com/press-releases/").expect("valid publication URL"),
            vec![
                Url::parse("https://www.biospace.com/press-releases/keros-appoints-a-director")
                    .expect("valid evidence URL"),
            ],
            20,
        );
        let mut report = recipe_report_fixture(vec![
            raw_item_fixture(
                "https://www.biospace.com/press-releases/keros-appoints-a-director",
                "Keros appoints a director",
            ),
            raw_item_fixture(
                "https://www.biospace.com/press-releases/other-biotech-results",
                "Another biotech reports results",
            ),
            raw_item_fixture(
                "https://www.biospace.com/press-releases/diagnostics-launch",
                "Diagnostics company launches a test",
            ),
            raw_item_fixture(
                "https://www.biospace.com/press-releases/pharma-financing",
                "Pharma company completes financing",
            ),
            raw_item_fixture(
                "https://www.biospace.com/press-releases/medical-appointment",
                "Medical company appoints an executive",
            ),
        ]);

        apply_company_scope_filter(&company, &spec, &mut report);

        assert_eq!(report.accepted_item_count, 1);
        assert_eq!(
            report
                .failures
                .iter()
                .filter(|failure| failure.reason == "article_not_company_scoped")
                .count(),
            4
        );
        assert!(
            report
                .correctness_reasons
                .iter()
                .any(|reason| reason == "company_scope_relevance_below_minimum")
        );
    }

    #[test]
    fn company_scope_filter_keeps_a_majority_and_drops_aggregator_noise() {
        let company = company_fixture(
            "Neumora Therapeutics Inc. Common Stock",
            "https://www.neumoratx.com/",
        );
        let spec = build_company_news_recipe_spec(
            &company,
            Url::parse("https://www.barchart.com/stocks/quotes/NMRA/news")
                .expect("valid publication URL"),
            Vec::new(),
            20,
        );
        let mut report = recipe_report_fixture(vec![
            raw_item_fixture(
                "https://www.barchart.com/story/news/1/neumora-results",
                "Neumora Therapeutics reports results",
            ),
            raw_item_fixture(
                "https://www.barchart.com/story/news/2/neumora-conference",
                "Neumora to participate in a conference",
            ),
            raw_item_fixture(
                "https://www.barchart.com/story/news/3/neumora-pipeline",
                "Neumora provides a pipeline update",
            ),
            raw_item_fixture(
                "https://www.barchart.com/story/news/4/amazon-update",
                "Amazon trims its research team",
            ),
            raw_item_fixture(
                "https://www.barchart.com/news/barchart-newsletters/brief",
                "Barchart Brief Newsletter",
            ),
        ]);

        apply_company_scope_filter(&company, &spec, &mut report);

        assert!(report.correctness_passed());
        assert_eq!(report.accepted_item_count, 3);
        assert_eq!(report.rejected_url_count, 2);
        assert_eq!(report.acceptance_ratio_bps, 6_000);
        assert!(report.items.iter().all(|item| {
            item.title
                .as_deref()
                .is_some_and(|title| title.to_ascii_lowercase().contains("neumora"))
        }));
    }

    #[test]
    fn company_owned_publications_do_not_require_title_level_identity() {
        let company = company_fixture("Microsoft Corporation", "https://www.microsoft.com/");
        let spec = build_company_news_recipe_spec(
            &company,
            Url::parse("https://azure.microsoft.com/blog").expect("valid publication URL"),
            Vec::new(),
            20,
        );
        let mut report = recipe_report_fixture(vec![
            raw_item_fixture(
                "https://azure.microsoft.com/blog/new-cloud-runtime",
                "A new cloud runtime is generally available",
            ),
            raw_item_fixture(
                "https://azure.microsoft.com/blog/platform-security",
                "Improving platform security",
            ),
            raw_item_fixture(
                "https://azure.microsoft.com/blog/database-performance",
                "Better database performance",
            ),
            raw_item_fixture(
                "https://azure.microsoft.com/blog/developer-tools",
                "New developer tools",
            ),
            raw_item_fixture(
                "https://azure.microsoft.com/blog/regional-expansion",
                "A regional infrastructure expansion",
            ),
        ]);

        apply_company_scope_filter(&company, &spec, &mut report);

        assert!(report.correctness_passed());
        assert_eq!(report.accepted_item_count, 5);
        assert!(report.failures.is_empty());
    }

    #[test]
    fn ambiguous_engineering_roots_keep_a_dominant_articles_namespace() {
        let company = company_fixture("Wipro Limited", "https://www.wipro.com/");
        let spec = build_company_news_recipe_spec(
            &company,
            Url::parse("https://www.wipro.com/engineering").expect("valid publication URL"),
            Vec::new(),
            20,
        );
        let mut items = (0..10)
            .map(|index| {
                raw_item_fixture(
                    &format!("https://www.wipro.com/engineering/articles/update-{index}"),
                    &format!("Engineering update {index}"),
                )
            })
            .collect::<Vec<_>>();
        items.extend([
            raw_item_fixture(
                "https://www.wipro.com/engineering/services/connected-services",
                "Connected Services",
            ),
            raw_item_fixture(
                "https://www.wipro.com/engineering/vlsi/embedded-systems",
                "Embedded Systems",
            ),
            raw_item_fixture(
                "https://www.wipro.com/engineering/cloud-car",
                "Wipro Cloud Car",
            ),
            raw_item_fixture("https://www.wipro.com/engineering/5g", "Wipro 5G Services"),
            raw_item_fixture(
                "https://www.wipro.com/engineering/product-testing",
                "Product Testing",
            ),
            raw_item_fixture("https://www.wipro.com/engineering/vlsi", "VLSI"),
            raw_item_fixture(
                "https://www.wipro.com/engineering/cloud-platform",
                "Cloud Platform",
            ),
            raw_item_fixture(
                "https://www.wipro.com/engineering/experience",
                "Engineering Experience",
            ),
            raw_item_fixture(
                "https://www.wipro.com/engineering/offerings",
                "Engineering Offerings",
            ),
        ]);
        let mut report = recipe_report_fixture(items);

        apply_dominant_editorial_namespace_filter(&spec, &mut report);

        assert_eq!(report.accepted_item_count, 10);
        assert_eq!(report.rejected_url_count, 9);
        assert_eq!(
            report
                .failures
                .iter()
                .filter(|failure| {
                    failure.reason == "article_outside_dominant_editorial_namespace"
                })
                .count(),
            9
        );
        assert!(
            report
                .items
                .iter()
                .all(|item| item.url.path().contains("/engineering/articles/"))
        );
        assert!(report.correctness_passed());
    }

    #[test]
    fn direct_engineering_posts_are_not_narrowed_without_a_dominant_child() {
        let company = company_fixture("Acme", "https://www.example.com/");
        let spec = build_company_news_recipe_spec(
            &company,
            Url::parse("https://www.example.com/engineering").expect("valid publication URL"),
            Vec::new(),
            20,
        );
        let items = (0..10)
            .map(|index| {
                raw_item_fixture(
                    &format!("https://www.example.com/engineering/update-{index}"),
                    &format!("Engineering update {index}"),
                )
            })
            .collect::<Vec<_>>();
        let mut report = recipe_report_fixture(items);

        apply_dominant_editorial_namespace_filter(&spec, &mut report);

        assert_eq!(report.accepted_item_count, 10);
        assert!(report.failures.is_empty());
        assert!(report.correctness_passed());
    }

    #[test]
    fn company_owned_recipe_allows_verified_cross_domain_profile_hosts() {
        let mut company = company_fixture(
            "Axos Financial Inc. Common Stock",
            "https://www.axosbank.com/",
        );
        company.investor_relations_url =
            Some(Url::parse("https://investors.axosfinancial.com/").expect("valid investor URL"));
        company.newsroom_url = Some(
            Url::parse("https://investors.axosfinancial.com/news-events/press-releases/")
                .expect("valid newsroom URL"),
        );

        let spec = build_company_news_recipe_spec(
            &company,
            Url::parse("https://www.axosbank.com/about-us/press-and-news")
                .expect("valid publication URL"),
            Vec::new(),
            20,
        );

        assert_eq!(
            spec.allowed_hosts,
            vec![
                "axosbank.com".to_owned(),
                "investors.axosfinancial.com".to_owned(),
            ]
        );
    }

    #[test]
    fn editorial_subdomain_recipes_do_not_expand_to_every_company_host() {
        let mut company = company_fixture("Example Corporation", "https://www.example.com/");
        company.investor_relations_url =
            Some(Url::parse("https://investors.example.com/").expect("investor URL"));
        let spec = build_company_news_recipe_spec(
            &company,
            Url::parse("https://blog.example.com/").expect("blog URL"),
            Vec::new(),
            20,
        );

        assert_eq!(spec.allowed_hosts, vec!["blog.example.com".to_owned()]);
        assert_eq!(
            effective_recipe_allowed_hosts(
                &[
                    "blog.example.com".to_owned(),
                    "developer.example.com".to_owned(),
                    "example.com".to_owned(),
                ],
                true,
                &spec.publication_url,
                &spec.evidence_article_urls,
            ),
            vec!["blog.example.com".to_owned()],
            "legacy adapter recipes narrow to the editorial publication host"
        );
        assert_eq!(
            effective_recipe_allowed_hosts(
                &[
                    "blog.example.com".to_owned(),
                    "developer.example.com".to_owned(),
                ],
                false,
                &spec.publication_url,
                &spec.evidence_article_urls,
            ),
            vec![
                "blog.example.com".to_owned(),
                "developer.example.com".to_owned(),
            ],
            "operator-authored cross-host recipes preserve their explicit scope"
        );
    }

    #[test]
    fn validation_scope_distinguishes_editorial_and_user_generated_feeds() {
        assert!(text_has_editorial_marker(
            "https://www.elastic.co/security-labs/rss/feed.xml"
        ));
        assert!(text_has_editorial_marker(
            "https://global.toyota/export/en/allnews_rss.xml"
        ));
        assert!(text_has_editorial_marker(
            "https://netflixtechblog.com/feed"
        ));
        assert!(text_has_risky_scope_marker(
            "https://community.commvault.com/feed/topics"
        ));
        assert!(text_has_risky_scope_marker(
            "https://www.docusign.com/trust/alerts/feed"
        ));
        assert!(text_has_risky_scope_marker(
            "https://notifications.qualys.com/feed"
        ));
        assert!(text_has_risky_scope_marker(
            "https://careers.example.com/jobs/feed"
        ));
        assert!(!text_has_risky_scope_marker(
            "https://engineering.atspotify.com/feed"
        ));
        assert!(is_sitemap_url(
            &Url::parse("https://example.com/sitemap.rss").expect("sitemap URL")
        ));
    }

    #[test]
    fn validation_auto_activation_prefers_unspecified_or_english_feeds() {
        assert!(has_preferred_locale(
            &Url::parse("https://about.example.com/feed.xml").expect("valid URL")
        ));
        assert!(has_preferred_locale(
            &Url::parse("https://about.example.com/en-US/feed.xml").expect("valid URL")
        ));
        assert!(!has_preferred_locale(
            &Url::parse("https://about.example.com/pt_br/feed.xml").expect("valid URL")
        ));
        assert!(!has_preferred_locale(
            &Url::parse("https://about.example.com/export/de/feed.xml").expect("valid URL")
        ));
    }

    #[test]
    fn trusted_adapter_recommendation_is_detected_from_direct_or_inherited_evidence() {
        assert!(evidence_has_web_adapter_recommendation(&json!({
            "observations": [{
                "method": "external_web_adapter",
                "external_web_adapter": {
                    "roles": ["feed"],
                    "rank_score": 0.5
                }
            }]
        })));
        assert!(evidence_has_web_adapter_recommendation(&json!({
            "observations": [{
                "method": "html_alternate",
                "external_web_adapter": {
                    "roles": ["engineering_blog"],
                    "rank_score": 0.5
                }
            }]
        })));
        assert!(!evidence_has_web_adapter_recommendation(&json!({
            "observations": [{"method": "common_path_probe"}]
        })));
    }

    #[test]
    fn validation_retries_only_transient_http_statuses() {
        let url = Url::parse("https://example.com/feed").expect("valid URL");
        assert!(validation_error_is_retryable(&CrawlError::HttpStatus {
            url: url.clone(),
            status: 503,
        }));
        assert!(validation_error_is_retryable(&CrawlError::HttpStatus {
            url: url.clone(),
            status: 429,
        }));
        assert!(!validation_error_is_retryable(&CrawlError::HttpStatus {
            url,
            status: 404,
        }));
    }

    #[test]
    fn web_adapter_failures_apply_a_shared_dependency_cooldown() {
        let rate_limited = classify_web_adapter_error(WebAdapterError::HttpStatus {
            status: 503,
            retryable: true,
            retry_after_seconds: Some(45),
            body: "temporarily unavailable".to_owned(),
        });
        assert!(rate_limited.is_retryable());
        assert_eq!(
            rate_limited.worker_cooldown(),
            Some(std::time::Duration::from_secs(45))
        );

        let transient = classify_web_adapter_error(WebAdapterError::HttpStatus {
            status: 502,
            retryable: true,
            retry_after_seconds: None,
            body: "bad gateway".to_owned(),
        });
        assert!(transient.is_retryable());
        assert_eq!(
            transient.worker_cooldown(),
            Some(WEB_ADAPTER_OUTAGE_COOLDOWN)
        );

        let permanent = classify_web_adapter_error(WebAdapterError::HttpStatus {
            status: 400,
            retryable: false,
            retry_after_seconds: None,
            body: "bad request".to_owned(),
        });
        assert!(!permanent.is_retryable());
        assert_eq!(permanent.worker_cooldown(), None);
    }
}
