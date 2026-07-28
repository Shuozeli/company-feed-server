use std::{
    collections::HashSet,
    env, fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use url::Url;

use crate::{
    DEFAULT_PUBLIC_FETCH_USER_AGENT,
    domain::{ExportFormat, ExportLayout, LifecycleStatus, OwnershipStatus},
};

const DEFAULT_DATABASE_MAX_CONNECTIONS: u32 = 10;
const DEFAULT_JOB_LEASE_SECONDS: u64 = 60;
const DEFAULT_JOB_HEARTBEAT_SECONDS: u64 = 20;
const DEFAULT_JOB_POLL_MILLISECONDS: u64 = 1_000;
const DEFAULT_JOB_RETRY_BASE_SECONDS: u64 = 30;
const DEFAULT_JOB_RETRY_MAX_SECONDS: u64 = 3_600;
const DEFAULT_SCHEDULER_SCAN_SECONDS: u64 = 30;
const DEFAULT_DISCOVERY_QUEUE_TARGET: u32 = 100;
const DEFAULT_VALIDATION_QUEUE_TARGET: u32 = 100;
const DEFAULT_NEWS_EXTRACTION_SOURCE_FRESHNESS_SECONDS: i32 = 30 * 24 * 60 * 60;
const DEFAULT_VALIDATION_MAX_ITEM_AGE_DAYS: u32 = 730;
const DEFAULT_AUTO_ACTIVATION_FRESHNESS_SECONDS: i32 = 3_600;
const DEFAULT_DISCOVERY_REQUEST_TIMEOUT_SECONDS: u64 = 15;
const DEFAULT_DISCOVERY_MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_DISCOVERY_MAX_CONCURRENCY: usize = 8;
const DEFAULT_DISCOVERY_MAX_CANDIDATES: usize = 200;
const DEFAULT_WEB_DISCOVERY_ADAPTER_TIMEOUT_SECONDS: u64 = 90;
const DEFAULT_WEB_DISCOVERY_ADAPTER_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const DEFAULT_WEB_DISCOVERY_ADAPTER_MAX_CANDIDATES: usize = 20;
const DEFAULT_NEWS_EXTRACTION_ADAPTER_TIMEOUT_SECONDS: u64 = 750;
const DEFAULT_NEWS_EXTRACTION_ADAPTER_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const DEFAULT_NEWS_EXTRACTION_JOB_CONCURRENCY: u32 = 1;
const DEFAULT_NEWS_EXTRACTION_MAX_ARTICLES: usize = 20;
const DEFAULT_NEWS_EXTRACTION_FETCH_TIMEOUT_SECONDS: u64 = 20;
const DEFAULT_NEWS_EXTRACTION_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_NEWS_EXTRACTION_MAX_CONCURRENCY: usize = 24;
const DEFAULT_NEWS_EXTRACTION_MAX_PER_HOST_CONCURRENCY: usize = 8;
const DEFAULT_NEWS_EXTRACTION_MIN_CONTENT_CHARS: usize = 200;
const DEFAULT_CONTENT_CRAWL_BATCH_SIZE: usize = 100;
const DEFAULT_CONTENT_CRAWL_JOB_CONCURRENCY: u32 = 1;
const DEFAULT_CONTENT_CRAWL_FETCH_TIMEOUT_SECONDS: u64 = 20;
const DEFAULT_CONTENT_CRAWL_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_CONTENT_CRAWL_MAX_CONCURRENCY: usize = 24;
const DEFAULT_CONTENT_CRAWL_MAX_PER_HOST_CONCURRENCY: usize = 2;
const DEFAULT_CONTENT_CRAWL_MIN_CONTENT_CHARS: usize = 200;
const DEFAULT_CONTENT_CRAWL_REFRESH_SECONDS: u64 = 30 * 24 * 60 * 60;
const DEFAULT_CONTENT_CRAWL_MAX_ATTEMPTS: i32 = 5;
const DEFAULT_CRAWLER_REQUEST_TIMEOUT_SECONDS: u64 = 20;
const DEFAULT_CRAWLER_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_CRAWLER_MAX_ITEMS: usize = 500;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompaniesConfig {
    pub companies: Vec<CompanySeed>,
}

impl CompaniesConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let config: Self = load_yaml(path)?;
        let mut company_keys = HashSet::new();

        for company in &config.companies {
            if !is_company_key(&company.company_key) {
                return Err(ConfigError::Validation(format!(
                    "company key {} must be a lowercase ASCII slug",
                    company.company_key
                )));
            }
            if company.name.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "company {} has an empty name",
                    company.company_key
                )));
            }
            if !company_keys.insert(company.company_key.clone()) {
                return Err(ConfigError::Validation(format!(
                    "duplicate company key {}",
                    company.company_key
                )));
            }
            let primary_listings = company
                .listings
                .iter()
                .filter(|listing| listing.is_primary)
                .count();
            if primary_listings > 1 {
                return Err(ConfigError::Validation(format!(
                    "company {} has more than one primary listing",
                    company.company_key
                )));
            }
            for listing in &company.listings {
                if listing.ticker.trim().is_empty()
                    || listing.ticker != listing.ticker.to_ascii_uppercase()
                {
                    return Err(ConfigError::Validation(format!(
                        "company {} listing ticker {} must be non-empty and uppercase",
                        company.company_key, listing.ticker
                    )));
                }
            }
        }

        Ok(config)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanySeed {
    pub company_key: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub ownership_status: OwnershipStatus,
    #[serde(default)]
    pub lifecycle_status: LifecycleStatus,
    #[serde(default)]
    pub listings: Vec<CompanyListingSeed>,
    #[serde(default)]
    pub homepage_url: Option<Url>,
    #[serde(default)]
    pub investor_relations_url: Option<Url>,
    #[serde(default)]
    pub newsroom_url: Option<Url>,
    #[serde(default)]
    pub blog_url: Option<Url>,
    #[serde(default)]
    pub hints: Vec<Url>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanyListingSeed {
    pub ticker: String,
    #[serde(default)]
    pub exchange: Option<String>,
    #[serde(default)]
    pub is_primary: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportTargetsConfig {
    pub targets: Vec<ExportTargetSeed>,
}

impl ExportTargetsConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let config: Self = load_yaml(path)?;
        let mut target_ids = HashSet::new();

        for target in &config.targets {
            if target.target_id.trim().is_empty() {
                return Err(ConfigError::Validation(
                    "export target id cannot be empty".to_owned(),
                ));
            }
            if !target_ids.insert(target.target_id.clone()) {
                return Err(ConfigError::Validation(format!(
                    "duplicate export target id {}",
                    target.target_id
                )));
            }
            if target.repo_url.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "export target {} has an empty repo_url",
                    target.target_id
                )));
            }
            if target.branch.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "export target {} has an empty branch",
                    target.target_id
                )));
            }
            if target.cadence_seconds == 0 {
                return Err(ConfigError::Validation(format!(
                    "export target {} cadence_seconds must be positive",
                    target.target_id
                )));
            }
            if !target.metadata.is_object() {
                return Err(ConfigError::Validation(format!(
                    "export target {} metadata must be an object",
                    target.target_id
                )));
            }
        }

        Ok(config)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportTargetSeed {
    pub target_id: String,
    pub repo_url: String,
    pub local_path: PathBuf,
    #[serde(default = "default_branch")]
    pub branch: String,
    pub format: ExportFormat,
    pub layout: ExportLayout,
    #[serde(default = "default_export_cadence")]
    pub cadence_seconds: u64,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub push_enabled: bool,
    #[serde(default = "default_json_object")]
    pub metadata: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppSettings {
    pub database_url: String,
    pub database_max_connections: u32,
    pub bind_addr: SocketAddr,
    pub worker_bind_addr: SocketAddr,
    pub discovery_worker_bind_addr: SocketAddr,
    pub validation_worker_bind_addr: SocketAddr,
    pub news_extraction_worker_bind_addr: SocketAddr,
    pub content_crawl_worker_bind_addr: SocketAddr,
    pub public_fetch_user_agent: String,
    pub companies_config_path: PathBuf,
    pub export_targets_config_path: PathBuf,
    pub run_jobs: bool,
    pub schedule_jobs: bool,
    pub news_extraction_run_jobs: bool,
    pub content_crawl_run_jobs: bool,
    pub content_crawl_job_concurrency: u32,
    pub news_extraction_job_concurrency: u32,
    pub worker_id: String,
    pub job_lease: Duration,
    pub job_heartbeat: Duration,
    pub job_poll_interval: Duration,
    pub job_retry_base: Duration,
    pub job_retry_max: Duration,
    pub scheduler_scan_interval: Duration,
    pub discovery_queue_target: u32,
    pub validation_queue_target: u32,
    pub news_extraction_enabled: bool,
    pub news_extraction_source_freshness_seconds: i32,
    pub validation_auto_activate: bool,
    pub validation_public_export: bool,
    pub validation_activation_policy: ValidationActivationPolicy,
    pub validation_max_item_age_days: u32,
    pub auto_activation_freshness_seconds: i32,
    pub discovery_request_timeout: Duration,
    pub discovery_max_response_bytes: usize,
    pub discovery_max_concurrency: usize,
    pub discovery_max_candidates: usize,
    pub discovery_probe_common_paths: bool,
    pub discovery_allow_private_networks: bool,
    pub web_discovery_adapter_mode: WebDiscoveryAdapterMode,
    pub web_discovery_adapter_url: Option<Url>,
    pub web_discovery_adapter_token: Option<String>,
    pub web_discovery_adapter_timeout: Duration,
    pub web_discovery_adapter_max_response_bytes: usize,
    pub web_discovery_adapter_max_candidates: usize,
    pub news_extraction_adapter_url: Option<Url>,
    pub news_extraction_adapter_token: Option<String>,
    pub news_extraction_adapter_timeout: Duration,
    pub news_extraction_adapter_max_response_bytes: usize,
    pub news_extraction_max_articles: usize,
    pub news_extraction_fetch_timeout: Duration,
    pub news_extraction_max_response_bytes: usize,
    pub news_extraction_max_concurrency: usize,
    pub news_extraction_max_per_host_concurrency: usize,
    pub news_extraction_min_content_chars: usize,
    pub news_extraction_allow_private_networks: bool,
    pub news_extraction_public_export: bool,
    pub content_crawl_enabled: bool,
    pub content_crawl_batch_size: usize,
    pub content_crawl_fetch_timeout: Duration,
    pub content_crawl_max_response_bytes: usize,
    pub content_crawl_max_concurrency: usize,
    pub content_crawl_max_per_host_concurrency: usize,
    pub content_crawl_min_content_chars: usize,
    pub content_crawl_refresh: Duration,
    pub content_crawl_max_attempts: i32,
    pub crawler_request_timeout: Duration,
    pub crawler_max_response_bytes: usize,
    pub crawler_max_items: usize,
}

impl AppSettings {
    pub fn from_env() -> Result<Self, ConfigError> {
        let database_url = required_env("DATABASE_URL")?;
        let database_max_connections =
            parse_env("DATABASE_MAX_CONNECTIONS", DEFAULT_DATABASE_MAX_CONNECTIONS)?;
        if database_max_connections == 0 {
            return Err(ConfigError::Validation(
                "DATABASE_MAX_CONNECTIONS must be positive".to_owned(),
            ));
        }
        let bind_addr = parse_env(
            "BIND_ADDR",
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8080),
        )?;
        let worker_bind_addr = parse_env(
            "WORKER_BIND_ADDR",
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8081),
        )?;
        let discovery_worker_bind_addr = parse_env(
            "DISCOVERY_WORKER_BIND_ADDR",
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8082),
        )?;
        let validation_worker_bind_addr = parse_env(
            "VALIDATION_WORKER_BIND_ADDR",
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8083),
        )?;
        let news_extraction_worker_bind_addr = parse_env(
            "NEWS_EXTRACTION_WORKER_BIND_ADDR",
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8084),
        )?;
        let content_crawl_worker_bind_addr = parse_env(
            "CONTENT_CRAWL_WORKER_BIND_ADDR",
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8085),
        )?;
        let public_fetch_user_agent = env::var("PUBLIC_FETCH_USER_AGENT")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_PUBLIC_FETCH_USER_AGENT.to_owned());
        let run_jobs = parse_env("RUN_JOBS", true)?;
        let schedule_jobs = parse_env("SCHEDULE_JOBS", true)?;
        let news_extraction_run_jobs = parse_env("NEWS_EXTRACTION_RUN_JOBS", run_jobs)?;
        let content_crawl_run_jobs = parse_env("CONTENT_CRAWL_RUN_JOBS", run_jobs)?;
        let content_crawl_job_concurrency = parse_env(
            "CONTENT_CRAWL_JOB_CONCURRENCY",
            DEFAULT_CONTENT_CRAWL_JOB_CONCURRENCY,
        )?;
        let news_extraction_job_concurrency = parse_env(
            "NEWS_EXTRACTION_JOB_CONCURRENCY",
            DEFAULT_NEWS_EXTRACTION_JOB_CONCURRENCY,
        )?;
        if news_extraction_job_concurrency == 0 {
            return Err(ConfigError::Validation(
                "NEWS_EXTRACTION_JOB_CONCURRENCY must be positive".to_owned(),
            ));
        }
        let worker_id = env::var("WORKER_ID")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(default_worker_id);

        let job_lease =
            Duration::from_secs(parse_env("JOB_LEASE_SECONDS", DEFAULT_JOB_LEASE_SECONDS)?);
        let job_heartbeat = Duration::from_secs(parse_env(
            "JOB_HEARTBEAT_SECONDS",
            DEFAULT_JOB_HEARTBEAT_SECONDS,
        )?);
        if job_heartbeat >= job_lease {
            return Err(ConfigError::Validation(
                "JOB_HEARTBEAT_SECONDS must be less than JOB_LEASE_SECONDS".to_owned(),
            ));
        }
        let job_poll_interval = Duration::from_millis(parse_env(
            "JOB_POLL_INTERVAL_MS",
            DEFAULT_JOB_POLL_MILLISECONDS,
        )?);
        let job_retry_base = Duration::from_secs(parse_env(
            "JOB_RETRY_BASE_SECONDS",
            DEFAULT_JOB_RETRY_BASE_SECONDS,
        )?);
        let job_retry_max = Duration::from_secs(parse_env(
            "JOB_RETRY_MAX_SECONDS",
            DEFAULT_JOB_RETRY_MAX_SECONDS,
        )?);
        if job_poll_interval.is_zero() {
            return Err(ConfigError::Validation(
                "JOB_POLL_INTERVAL_MS must be positive".to_owned(),
            ));
        }
        if job_retry_base.is_zero() || job_retry_max.is_zero() {
            return Err(ConfigError::Validation(
                "job retry durations must be positive".to_owned(),
            ));
        }
        if job_retry_base > job_retry_max {
            return Err(ConfigError::Validation(
                "JOB_RETRY_BASE_SECONDS must not exceed JOB_RETRY_MAX_SECONDS".to_owned(),
            ));
        }
        let scheduler_scan_interval = Duration::from_secs(parse_env(
            "SCHEDULER_SCAN_SECONDS",
            DEFAULT_SCHEDULER_SCAN_SECONDS,
        )?);
        let discovery_queue_target =
            parse_env("DISCOVERY_QUEUE_TARGET", DEFAULT_DISCOVERY_QUEUE_TARGET)?;
        let validation_queue_target =
            parse_env("VALIDATION_QUEUE_TARGET", DEFAULT_VALIDATION_QUEUE_TARGET)?;
        let news_extraction_enabled = parse_env("NEWS_EXTRACTION_ENABLED", false)?;
        let news_extraction_source_freshness_seconds = parse_env(
            "NEWS_EXTRACTION_SOURCE_FRESHNESS_SECONDS",
            DEFAULT_NEWS_EXTRACTION_SOURCE_FRESHNESS_SECONDS,
        )?;
        let validation_auto_activate = parse_env("VALIDATION_AUTO_ACTIVATE", true)?;
        let validation_public_export = parse_env("VALIDATION_PUBLIC_EXPORT", false)?;
        let validation_activation_policy = parse_env(
            "VALIDATION_ACTIVATION_POLICY",
            ValidationActivationPolicy::Strict,
        )?;
        let validation_max_item_age_days = parse_env(
            "VALIDATION_MAX_ITEM_AGE_DAYS",
            DEFAULT_VALIDATION_MAX_ITEM_AGE_DAYS,
        )?;
        let auto_activation_freshness_seconds = parse_env(
            "AUTO_ACTIVATION_FRESHNESS_SECONDS",
            DEFAULT_AUTO_ACTIVATION_FRESHNESS_SECONDS,
        )?;
        let discovery_request_timeout = Duration::from_secs(parse_env(
            "DISCOVERY_REQUEST_TIMEOUT_SECONDS",
            DEFAULT_DISCOVERY_REQUEST_TIMEOUT_SECONDS,
        )?);
        let discovery_max_response_bytes = parse_env(
            "DISCOVERY_MAX_RESPONSE_BYTES",
            DEFAULT_DISCOVERY_MAX_RESPONSE_BYTES,
        )?;
        let discovery_max_concurrency = parse_env(
            "DISCOVERY_MAX_CONCURRENCY",
            DEFAULT_DISCOVERY_MAX_CONCURRENCY,
        )?;
        let discovery_max_candidates =
            parse_env("DISCOVERY_MAX_CANDIDATES", DEFAULT_DISCOVERY_MAX_CANDIDATES)?;
        let discovery_probe_common_paths = parse_env("DISCOVERY_PROBE_COMMON_PATHS", true)?;
        let discovery_allow_private_networks =
            parse_env("DISCOVERY_ALLOW_PRIVATE_NETWORKS", false)?;
        if scheduler_scan_interval.is_zero()
            || discovery_queue_target == 0
            || validation_queue_target == 0
            || news_extraction_source_freshness_seconds <= 0
            || validation_max_item_age_days == 0
            || discovery_request_timeout.is_zero()
            || auto_activation_freshness_seconds <= 0
        {
            return Err(ConfigError::Validation(
                "scheduler scan, queue targets, validation age, source freshness, and request duration must be positive".to_owned(),
            ));
        }
        if discovery_max_response_bytes == 0
            || discovery_max_concurrency == 0
            || discovery_max_candidates == 0
        {
            return Err(ConfigError::Validation(
                "discovery size, concurrency, and candidate limits must be positive".to_owned(),
            ));
        }
        let web_discovery_adapter_mode = parse_env(
            "WEB_DISCOVERY_ADAPTER_MODE",
            WebDiscoveryAdapterMode::Disabled,
        )?;
        let web_discovery_adapter_url = optional_url_env("WEB_DISCOVERY_ADAPTER_URL")?;
        let web_discovery_adapter_token = env::var("WEB_DISCOVERY_ADAPTER_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let web_discovery_adapter_timeout = Duration::from_secs(parse_env(
            "WEB_DISCOVERY_ADAPTER_TIMEOUT_SECONDS",
            DEFAULT_WEB_DISCOVERY_ADAPTER_TIMEOUT_SECONDS,
        )?);
        let web_discovery_adapter_max_response_bytes = parse_env(
            "WEB_DISCOVERY_ADAPTER_MAX_RESPONSE_BYTES",
            DEFAULT_WEB_DISCOVERY_ADAPTER_MAX_RESPONSE_BYTES,
        )?;
        let web_discovery_adapter_max_candidates = parse_env(
            "WEB_DISCOVERY_ADAPTER_MAX_CANDIDATES",
            DEFAULT_WEB_DISCOVERY_ADAPTER_MAX_CANDIDATES,
        )?;
        if web_discovery_adapter_mode != WebDiscoveryAdapterMode::Disabled
            && web_discovery_adapter_url.is_none()
        {
            return Err(ConfigError::Validation(
                "WEB_DISCOVERY_ADAPTER_URL is required when the adapter is enabled".to_owned(),
            ));
        }
        if web_discovery_adapter_timeout.is_zero()
            || web_discovery_adapter_max_response_bytes == 0
            || web_discovery_adapter_max_candidates == 0
        {
            return Err(ConfigError::Validation(
                "web discovery adapter timeout and limits must be positive".to_owned(),
            ));
        }
        if web_discovery_adapter_max_candidates > discovery_max_candidates {
            return Err(ConfigError::Validation(
                "WEB_DISCOVERY_ADAPTER_MAX_CANDIDATES must not exceed DISCOVERY_MAX_CANDIDATES"
                    .to_owned(),
            ));
        }
        let news_extraction_adapter_url = optional_url_env("NEWS_EXTRACTION_ADAPTER_URL")?;
        let news_extraction_adapter_token = env::var("NEWS_EXTRACTION_ADAPTER_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let news_extraction_adapter_timeout = Duration::from_secs(parse_env(
            "NEWS_EXTRACTION_ADAPTER_TIMEOUT_SECONDS",
            DEFAULT_NEWS_EXTRACTION_ADAPTER_TIMEOUT_SECONDS,
        )?);
        let news_extraction_adapter_max_response_bytes = parse_env(
            "NEWS_EXTRACTION_ADAPTER_MAX_RESPONSE_BYTES",
            DEFAULT_NEWS_EXTRACTION_ADAPTER_MAX_RESPONSE_BYTES,
        )?;
        let news_extraction_max_articles = parse_env(
            "NEWS_EXTRACTION_MAX_ARTICLES",
            DEFAULT_NEWS_EXTRACTION_MAX_ARTICLES,
        )?;
        let news_extraction_fetch_timeout = Duration::from_secs(parse_env(
            "NEWS_EXTRACTION_FETCH_TIMEOUT_SECONDS",
            DEFAULT_NEWS_EXTRACTION_FETCH_TIMEOUT_SECONDS,
        )?);
        let news_extraction_max_response_bytes = parse_env(
            "NEWS_EXTRACTION_MAX_RESPONSE_BYTES",
            DEFAULT_NEWS_EXTRACTION_MAX_RESPONSE_BYTES,
        )?;
        let news_extraction_max_concurrency = parse_env(
            "NEWS_EXTRACTION_MAX_CONCURRENCY",
            DEFAULT_NEWS_EXTRACTION_MAX_CONCURRENCY,
        )?;
        let news_extraction_max_per_host_concurrency = parse_env(
            "NEWS_EXTRACTION_MAX_PER_HOST_CONCURRENCY",
            DEFAULT_NEWS_EXTRACTION_MAX_PER_HOST_CONCURRENCY,
        )?;
        let news_extraction_min_content_chars = parse_env(
            "NEWS_EXTRACTION_MIN_CONTENT_CHARS",
            DEFAULT_NEWS_EXTRACTION_MIN_CONTENT_CHARS,
        )?;
        let news_extraction_allow_private_networks =
            parse_env("NEWS_EXTRACTION_ALLOW_PRIVATE_NETWORKS", false)?;
        let news_extraction_public_export = parse_env("NEWS_EXTRACTION_PUBLIC_EXPORT", false)?;
        if news_extraction_enabled && news_extraction_adapter_url.is_none() {
            return Err(ConfigError::Validation(
                "NEWS_EXTRACTION_ADAPTER_URL is required when manual news import is enabled"
                    .to_owned(),
            ));
        }
        if news_extraction_adapter_timeout.is_zero()
            || news_extraction_adapter_max_response_bytes == 0
            || news_extraction_max_articles == 0
            || news_extraction_fetch_timeout.is_zero()
            || news_extraction_max_response_bytes == 0
            || news_extraction_max_concurrency == 0
            || news_extraction_max_per_host_concurrency == 0
            || news_extraction_min_content_chars == 0
        {
            return Err(ConfigError::Validation(
                "manual news import timeouts and limits must be positive".to_owned(),
            ));
        }
        if news_extraction_max_per_host_concurrency > news_extraction_max_concurrency {
            return Err(ConfigError::Validation(
                "NEWS_EXTRACTION_MAX_PER_HOST_CONCURRENCY must not exceed NEWS_EXTRACTION_MAX_CONCURRENCY"
                    .to_owned(),
            ));
        }
        let content_crawl_enabled = parse_env("CONTENT_CRAWL_ENABLED", true)?;
        let content_crawl_batch_size =
            parse_env("CONTENT_CRAWL_BATCH_SIZE", DEFAULT_CONTENT_CRAWL_BATCH_SIZE)?;
        let content_crawl_fetch_timeout = Duration::from_secs(parse_env(
            "CONTENT_CRAWL_FETCH_TIMEOUT_SECONDS",
            DEFAULT_CONTENT_CRAWL_FETCH_TIMEOUT_SECONDS,
        )?);
        let content_crawl_max_response_bytes = parse_env(
            "CONTENT_CRAWL_MAX_RESPONSE_BYTES",
            DEFAULT_CONTENT_CRAWL_MAX_RESPONSE_BYTES,
        )?;
        let content_crawl_max_concurrency = parse_env(
            "CONTENT_CRAWL_MAX_CONCURRENCY",
            DEFAULT_CONTENT_CRAWL_MAX_CONCURRENCY,
        )?;
        let content_crawl_max_per_host_concurrency = parse_env(
            "CONTENT_CRAWL_MAX_PER_HOST_CONCURRENCY",
            DEFAULT_CONTENT_CRAWL_MAX_PER_HOST_CONCURRENCY,
        )?;
        let content_crawl_min_content_chars = parse_env(
            "CONTENT_CRAWL_MIN_CONTENT_CHARS",
            DEFAULT_CONTENT_CRAWL_MIN_CONTENT_CHARS,
        )?;
        let content_crawl_refresh = Duration::from_secs(parse_env(
            "CONTENT_CRAWL_REFRESH_SECONDS",
            DEFAULT_CONTENT_CRAWL_REFRESH_SECONDS,
        )?);
        let content_crawl_max_attempts = parse_env(
            "CONTENT_CRAWL_MAX_ATTEMPTS",
            DEFAULT_CONTENT_CRAWL_MAX_ATTEMPTS,
        )?;
        if content_crawl_job_concurrency == 0
            || content_crawl_batch_size == 0
            || content_crawl_fetch_timeout.is_zero()
            || content_crawl_max_response_bytes == 0
            || content_crawl_max_concurrency == 0
            || content_crawl_max_per_host_concurrency == 0
            || content_crawl_min_content_chars == 0
            || content_crawl_refresh.is_zero()
            || content_crawl_max_attempts <= 0
        {
            return Err(ConfigError::Validation(
                "content crawl timeouts and limits must be positive".to_owned(),
            ));
        }
        if content_crawl_max_per_host_concurrency > content_crawl_max_concurrency {
            return Err(ConfigError::Validation(
                "CONTENT_CRAWL_MAX_PER_HOST_CONCURRENCY must not exceed CONTENT_CRAWL_MAX_CONCURRENCY"
                    .to_owned(),
            ));
        }
        if content_crawl_job_concurrency > 32 {
            return Err(ConfigError::Validation(
                "CONTENT_CRAWL_JOB_CONCURRENCY must not exceed 32".to_owned(),
            ));
        }
        let crawler_request_timeout = Duration::from_secs(parse_env(
            "CRAWLER_REQUEST_TIMEOUT_SECONDS",
            DEFAULT_CRAWLER_REQUEST_TIMEOUT_SECONDS,
        )?);
        let crawler_max_response_bytes = parse_env(
            "CRAWLER_MAX_RESPONSE_BYTES",
            DEFAULT_CRAWLER_MAX_RESPONSE_BYTES,
        )?;
        let crawler_max_items = parse_env("CRAWLER_MAX_ITEMS", DEFAULT_CRAWLER_MAX_ITEMS)?;
        if crawler_request_timeout.is_zero()
            || crawler_max_response_bytes == 0
            || crawler_max_items == 0
        {
            return Err(ConfigError::Validation(
                "crawler timeout, response size, and item limits must be positive".to_owned(),
            ));
        }

        Ok(Self {
            database_url,
            database_max_connections,
            bind_addr,
            worker_bind_addr,
            discovery_worker_bind_addr,
            validation_worker_bind_addr,
            news_extraction_worker_bind_addr,
            content_crawl_worker_bind_addr,
            public_fetch_user_agent,
            companies_config_path: env::var_os("COMPANIES_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("configs/companies.yaml")),
            export_targets_config_path: env::var_os("EXPORT_TARGETS_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("configs/export_targets.yaml")),
            run_jobs,
            schedule_jobs,
            news_extraction_run_jobs,
            content_crawl_run_jobs,
            content_crawl_job_concurrency,
            news_extraction_job_concurrency,
            worker_id,
            job_lease,
            job_heartbeat,
            job_poll_interval,
            job_retry_base,
            job_retry_max,
            scheduler_scan_interval,
            discovery_queue_target,
            validation_queue_target,
            news_extraction_enabled,
            news_extraction_source_freshness_seconds,
            validation_auto_activate,
            validation_public_export,
            validation_activation_policy,
            validation_max_item_age_days,
            auto_activation_freshness_seconds,
            discovery_request_timeout,
            discovery_max_response_bytes,
            discovery_max_concurrency,
            discovery_max_candidates,
            discovery_probe_common_paths,
            discovery_allow_private_networks,
            web_discovery_adapter_mode,
            web_discovery_adapter_url,
            web_discovery_adapter_token,
            web_discovery_adapter_timeout,
            web_discovery_adapter_max_response_bytes,
            web_discovery_adapter_max_candidates,
            news_extraction_adapter_url,
            news_extraction_adapter_token,
            news_extraction_adapter_timeout,
            news_extraction_adapter_max_response_bytes,
            news_extraction_max_articles,
            news_extraction_fetch_timeout,
            news_extraction_max_response_bytes,
            news_extraction_max_concurrency,
            news_extraction_max_per_host_concurrency,
            news_extraction_min_content_chars,
            news_extraction_allow_private_networks,
            news_extraction_public_export,
            content_crawl_enabled,
            content_crawl_batch_size,
            content_crawl_fetch_timeout,
            content_crawl_max_response_bytes,
            content_crawl_max_concurrency,
            content_crawl_max_per_host_concurrency,
            content_crawl_min_content_chars,
            content_crawl_refresh,
            content_crawl_max_attempts,
            crawler_request_timeout,
            crawler_max_response_bytes,
            crawler_max_items,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ValidationActivationPolicy {
    #[default]
    Strict,
    TrustedAdapter,
}

impl ValidationActivationPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::TrustedAdapter => "trusted_adapter",
        }
    }
}

impl std::str::FromStr for ValidationActivationPolicy {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "strict" => Ok(Self::Strict),
            "trusted_adapter" => Ok(Self::TrustedAdapter),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WebDiscoveryAdapterMode {
    #[default]
    Disabled,
    Fallback,
    Augment,
}

impl WebDiscoveryAdapterMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Fallback => "fallback",
            Self::Augment => "augment",
        }
    }
}

impl std::str::FromStr for WebDiscoveryAdapterMode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" => Ok(Self::Disabled),
            "fallback" => Ok(Self::Fallback),
            "augment" => Ok(Self::Augment),
            _ => Err(()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("required environment variable {0} is not set")]
    MissingEnvironment(&'static str),
    #[error("invalid environment variable {name}: {value}")]
    InvalidEnvironment { name: &'static str, value: String },
    #[error("failed to read config {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    #[error("invalid configuration: {0}")]
    Validation(String),
}

fn load_yaml<T: DeserializeOwned>(path: &Path) -> Result<T, ConfigError> {
    let contents = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_owned(),
        source,
    })?;
    serde_yaml::from_str(&contents).map_err(|source| ConfigError::Parse {
        path: path.to_owned(),
        source,
    })
}

fn required_env(name: &'static str) -> Result<String, ConfigError> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(ConfigError::MissingEnvironment(name))
}

fn parse_env<T>(name: &'static str, default: T) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
{
    let Some(value) = env::var(name).ok() else {
        return Ok(default);
    };
    value
        .parse()
        .map_err(|_| ConfigError::InvalidEnvironment { name, value })
}

fn optional_url_env(name: &'static str) -> Result<Option<Url>, ConfigError> {
    let Some(value) = env::var(name).ok().filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    Url::parse(&value)
        .map(Some)
        .map_err(|_| ConfigError::InvalidEnvironment { name, value })
}

fn default_worker_id() -> String {
    let host = env::var("HOSTNAME").unwrap_or_else(|_| "company-feed".to_owned());
    format!("{host}-{}", std::process::id())
}

fn default_branch() -> String {
    "main".to_owned()
}

const fn default_export_cadence() -> u64 {
    3_600
}

const fn default_true() -> bool {
    true
}

fn default_json_object() -> Value {
    Value::Object(Default::default())
}

fn is_company_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_company_config_and_validates_urls() {
        let parsed: CompaniesConfig = serde_yaml::from_str(
            r#"
companies:
  - company_key: acme
    name: Acme
    ownership_status: private
    homepage_url: https://example.com/
    hints:
      - https://example.com/news/
"#,
        )
        .expect("valid config");

        assert_eq!(parsed.companies.len(), 1);
        assert_eq!(
            parsed.companies[0].homepage_url.as_ref().map(Url::as_str),
            Some("https://example.com/")
        );
    }

    #[test]
    fn export_defaults_are_safe() {
        let parsed: ExportTargetsConfig = serde_yaml::from_str(
            r#"
targets:
  - target_id: archive
    repo_url: git@example.com:org/archive.git
    local_path: ./archive
    format: markdown_json
    layout: by_company_date
"#,
        )
        .expect("valid config");

        let target = &parsed.targets[0];
        assert_eq!(target.branch, "main");
        assert_eq!(target.cadence_seconds, 3_600);
        assert!(target.enabled);
        assert!(!target.push_enabled);
        assert_eq!(target.metadata, default_json_object());
    }

    #[test]
    fn export_metadata_accepts_publication_scope() {
        let parsed: ExportTargetsConfig = serde_yaml::from_str(
            r#"
targets:
  - target_id: archive
    repo_url: git@example.com:org/archive.git
    local_path: ./archive
    format: markdown_json
    layout: by_company_date
    metadata:
      publication_scope: approved_public
"#,
        )
        .expect("valid config");

        assert_eq!(
            parsed.targets[0].metadata["publication_scope"],
            "approved_public"
        );
    }

    #[test]
    fn validation_activation_policy_is_explicit() {
        assert_eq!(
            "strict".parse::<ValidationActivationPolicy>(),
            Ok(ValidationActivationPolicy::Strict)
        );
        assert_eq!(
            "trusted_adapter".parse::<ValidationActivationPolicy>(),
            Ok(ValidationActivationPolicy::TrustedAdapter)
        );
        assert!("permissive".parse::<ValidationActivationPolicy>().is_err());
    }
}
