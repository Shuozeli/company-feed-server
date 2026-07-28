use std::{collections::HashSet, time::Duration};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, redirect::Policy};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

pub const WEB_DISCOVERY_SCHEMA_VERSION: &str = "company-web-discovery.v2";
pub const DEFAULT_REQUEST_PATH: &str = "v1/discover";
pub const COMPANY_NEWS_EXTRACTION_SCHEMA_VERSION: &str = "company-news-extraction.v2";
pub const DEFAULT_NEWS_EXTRACTION_PATH: &str = "v1/extract-news";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebDiscoveryRequest {
    pub schema_version: String,
    pub request_id: Uuid,
    pub company: WebDiscoveryCompany,
    pub requested_roles: Vec<WebPropertyRole>,
    pub max_candidates: u32,
}

impl WebDiscoveryRequest {
    pub fn new(request_id: Uuid, company: WebDiscoveryCompany, max_candidates: u32) -> Self {
        Self {
            schema_version: WEB_DISCOVERY_SCHEMA_VERSION.to_owned(),
            request_id,
            company,
            requested_roles: WebPropertyRole::ALL.to_vec(),
            max_candidates,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebDiscoveryCompany {
    pub company_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub known_urls: Vec<Url>,
    #[serde(default)]
    pub sector: Option<String>,
    #[serde(default)]
    pub industry: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebPropertyRole {
    Homepage,
    InvestorRelations,
    Newsroom,
    PressReleases,
    CorporateBlog,
    EngineeringBlog,
    Feed,
}

impl WebPropertyRole {
    pub const ALL: [Self; 7] = [
        Self::Homepage,
        Self::InvestorRelations,
        Self::Newsroom,
        Self::PressReleases,
        Self::CorporateBlog,
        Self::EngineeringBlog,
        Self::Feed,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Homepage => "homepage",
            Self::InvestorRelations => "investor_relations",
            Self::Newsroom => "newsroom",
            Self::PressReleases => "press_releases",
            Self::CorporateBlog => "corporate_blog",
            Self::EngineeringBlog => "engineering_blog",
            Self::Feed => "feed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestedResourceKind {
    Html,
    Feed,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebDiscoveryResponse {
    pub schema_version: String,
    pub request_id: Uuid,
    pub candidates: Vec<WebDiscoveryCandidate>,
    #[serde(default)]
    pub adapter_trace_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebDiscoveryCandidate {
    pub url: Url,
    pub role: WebPropertyRole,
    pub suggested_kind: SuggestedResourceKind,
    pub rank_score: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanyNewsExtractionRequest {
    pub schema_version: String,
    pub request_id: Uuid,
    pub company: WebDiscoveryCompany,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub max_articles: u32,
}

impl CompanyNewsExtractionRequest {
    pub fn new(
        request_id: Uuid,
        company: WebDiscoveryCompany,
        window_start: DateTime<Utc>,
        window_end: DateTime<Utc>,
        max_articles: u32,
    ) -> Self {
        Self {
            schema_version: COMPANY_NEWS_EXTRACTION_SCHEMA_VERSION.to_owned(),
            request_id,
            company,
            window_start,
            window_end,
            max_articles,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanyNewsExtractionResponse {
    pub schema_version: String,
    pub request_id: Uuid,
    /// Public listing/category pages from which a durable crawl recipe can be built.
    #[serde(default)]
    pub publications: Vec<CompanyNewsPublicationCandidate>,
    pub articles: Vec<CompanyNewsArticleCandidate>,
    #[serde(default)]
    pub adapter_trace_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanyNewsPublicationCandidate {
    pub url: Url,
    pub rank_score: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanyNewsArticleCandidate {
    pub url: Url,
    pub rank_score: f64,
}

#[derive(Clone, Debug)]
pub struct WebDiscoveryAdapterConfig {
    pub base_url: Url,
    pub bearer_token: Option<String>,
    pub request_timeout: Duration,
    pub max_response_bytes: usize,
    pub max_candidates: usize,
}

impl WebDiscoveryAdapterConfig {
    pub fn validate(&self) -> Result<(), WebAdapterError> {
        if !matches!(self.base_url.scheme(), "http" | "https") {
            return Err(WebAdapterError::InvalidConfig(
                "adapter URL scheme must be HTTP or HTTPS".to_owned(),
            ));
        }
        if self.base_url.host_str().is_none() {
            return Err(WebAdapterError::InvalidConfig(
                "adapter URL must include a host".to_owned(),
            ));
        }
        if !self.base_url.username().is_empty() || self.base_url.password().is_some() {
            return Err(WebAdapterError::InvalidConfig(
                "adapter URL must not contain credentials".to_owned(),
            ));
        }
        if self.request_timeout.is_zero()
            || self.max_response_bytes == 0
            || self.max_candidates == 0
        {
            return Err(WebAdapterError::InvalidConfig(
                "adapter timeout and limits must be positive".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct WebDiscoveryAdapterClient {
    client: Client,
    endpoint: Url,
    bearer_token: Option<String>,
    max_response_bytes: usize,
    max_candidates: usize,
}

#[derive(Clone, Debug)]
pub struct CompanyNewsExtractionAdapterConfig {
    pub base_url: Url,
    pub bearer_token: Option<String>,
    pub request_timeout: Duration,
    pub max_response_bytes: usize,
    pub max_articles: usize,
}

impl CompanyNewsExtractionAdapterConfig {
    pub fn validate(&self) -> Result<(), WebAdapterError> {
        validate_adapter_config(
            &self.base_url,
            self.request_timeout,
            self.max_response_bytes,
            self.max_articles,
        )
    }
}

#[derive(Clone)]
pub struct CompanyNewsExtractionAdapterClient {
    client: Client,
    endpoint: Url,
    bearer_token: Option<String>,
    max_response_bytes: usize,
    max_articles: usize,
}

impl CompanyNewsExtractionAdapterClient {
    pub fn new(mut config: CompanyNewsExtractionAdapterConfig) -> Result<Self, WebAdapterError> {
        config.validate()?;
        if !config.base_url.path().ends_with('/') {
            let path = format!("{}/", config.base_url.path());
            config.base_url.set_path(&path);
        }
        let endpoint = config
            .base_url
            .join(DEFAULT_NEWS_EXTRACTION_PATH)
            .map_err(|error| WebAdapterError::InvalidConfig(error.to_string()))?;
        let client = Client::builder()
            .connect_timeout(config.request_timeout)
            .timeout(config.request_timeout)
            .redirect(Policy::none())
            .build()?;
        Ok(Self {
            client,
            endpoint,
            bearer_token: config.bearer_token,
            max_response_bytes: config.max_response_bytes,
            max_articles: config.max_articles,
        })
    }

    pub async fn extract_news(
        &self,
        request: &CompanyNewsExtractionRequest,
    ) -> Result<CompanyNewsExtractionResponse, WebAdapterError> {
        validate_news_extraction_request(request, self.max_articles)?;
        let mut builder = self
            .client
            .post(self.endpoint.clone())
            .header("Idempotency-Key", request.request_id.to_string())
            .json(request);
        if let Some(token) = self.bearer_token.as_deref() {
            builder = builder.bearer_auth(token);
        }

        let response = builder.send().await?;
        let status = response.status();
        let retry_after_seconds = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let body = read_limited_body(response, self.max_response_bytes).await?;
        if !status.is_success() {
            return Err(WebAdapterError::HttpStatus {
                status: status.as_u16(),
                retryable: status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error(),
                retry_after_seconds,
                body: String::from_utf8_lossy(&body).chars().take(500).collect(),
            });
        }

        let response: CompanyNewsExtractionResponse = serde_json::from_slice(&body)
            .map_err(|error| WebAdapterError::InvalidResponse(error.to_string()))?;
        validate_news_extraction_response(request, &response, self.max_articles)?;
        Ok(response)
    }
}

fn validate_adapter_config(
    base_url: &Url,
    request_timeout: Duration,
    max_response_bytes: usize,
    item_limit: usize,
) -> Result<(), WebAdapterError> {
    if !matches!(base_url.scheme(), "http" | "https") {
        return Err(WebAdapterError::InvalidConfig(
            "adapter URL scheme must be HTTP or HTTPS".to_owned(),
        ));
    }
    if base_url.host_str().is_none() {
        return Err(WebAdapterError::InvalidConfig(
            "adapter URL must include a host".to_owned(),
        ));
    }
    if !base_url.username().is_empty() || base_url.password().is_some() {
        return Err(WebAdapterError::InvalidConfig(
            "adapter URL must not contain credentials".to_owned(),
        ));
    }
    if request_timeout.is_zero() || max_response_bytes == 0 || item_limit == 0 {
        return Err(WebAdapterError::InvalidConfig(
            "adapter timeout and limits must be positive".to_owned(),
        ));
    }
    Ok(())
}

impl WebDiscoveryAdapterClient {
    pub fn new(mut config: WebDiscoveryAdapterConfig) -> Result<Self, WebAdapterError> {
        config.validate()?;
        if !config.base_url.path().ends_with('/') {
            let path = format!("{}/", config.base_url.path());
            config.base_url.set_path(&path);
        }
        let endpoint = config
            .base_url
            .join(DEFAULT_REQUEST_PATH)
            .map_err(|error| WebAdapterError::InvalidConfig(error.to_string()))?;
        let client = Client::builder()
            .connect_timeout(config.request_timeout)
            .timeout(config.request_timeout)
            .redirect(Policy::none())
            .build()?;
        Ok(Self {
            client,
            endpoint,
            bearer_token: config.bearer_token,
            max_response_bytes: config.max_response_bytes,
            max_candidates: config.max_candidates,
        })
    }

    pub async fn discover(
        &self,
        request: &WebDiscoveryRequest,
    ) -> Result<WebDiscoveryResponse, WebAdapterError> {
        validate_request(request, self.max_candidates)?;
        let mut builder = self
            .client
            .post(self.endpoint.clone())
            .header("Idempotency-Key", request.request_id.to_string())
            .json(request);
        if let Some(token) = self.bearer_token.as_deref() {
            builder = builder.bearer_auth(token);
        }

        let response = builder.send().await?;
        let status = response.status();
        let retry_after_seconds = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let body = read_limited_body(response, self.max_response_bytes).await?;
        if !status.is_success() {
            return Err(WebAdapterError::HttpStatus {
                status: status.as_u16(),
                retryable: status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error(),
                retry_after_seconds,
                body: String::from_utf8_lossy(&body).chars().take(500).collect(),
            });
        }

        let response: WebDiscoveryResponse = serde_json::from_slice(&body)
            .map_err(|error| WebAdapterError::InvalidResponse(error.to_string()))?;
        validate_response(request, &response, self.max_candidates)?;
        Ok(response)
    }
}

fn validate_request(
    request: &WebDiscoveryRequest,
    max_candidates: usize,
) -> Result<(), WebAdapterError> {
    if request.schema_version != WEB_DISCOVERY_SCHEMA_VERSION {
        return Err(WebAdapterError::InvalidRequest(format!(
            "unsupported schema version {}",
            request.schema_version
        )));
    }
    if request.company.name.trim().is_empty() {
        return Err(WebAdapterError::InvalidRequest(
            "company name cannot be empty".to_owned(),
        ));
    }
    if request.max_candidates == 0 || request.max_candidates as usize > max_candidates {
        return Err(WebAdapterError::InvalidRequest(format!(
            "max_candidates must be between 1 and {max_candidates}"
        )));
    }
    if request.requested_roles.is_empty() {
        return Err(WebAdapterError::InvalidRequest(
            "at least one web property role is required".to_owned(),
        ));
    }
    for url in &request.company.known_urls {
        validate_url_shape(url).map_err(WebAdapterError::InvalidRequest)?;
    }
    Ok(())
}

fn validate_news_extraction_request(
    request: &CompanyNewsExtractionRequest,
    max_articles: usize,
) -> Result<(), WebAdapterError> {
    if request.schema_version != COMPANY_NEWS_EXTRACTION_SCHEMA_VERSION {
        return Err(WebAdapterError::InvalidRequest(format!(
            "unsupported schema version {}",
            request.schema_version
        )));
    }
    if request.company.name.trim().is_empty() {
        return Err(WebAdapterError::InvalidRequest(
            "company name cannot be empty".to_owned(),
        ));
    }
    if request.window_start >= request.window_end {
        return Err(WebAdapterError::InvalidRequest(
            "window_start must be before window_end".to_owned(),
        ));
    }
    if request.window_end - request.window_start > ChronoDuration::days(93) {
        return Err(WebAdapterError::InvalidRequest(
            "news extraction window cannot exceed 93 days".to_owned(),
        ));
    }
    if request.max_articles == 0 || request.max_articles as usize > max_articles {
        return Err(WebAdapterError::InvalidRequest(format!(
            "max_articles must be between 1 and {max_articles}"
        )));
    }
    for url in &request.company.known_urls {
        validate_url_shape(url).map_err(WebAdapterError::InvalidRequest)?;
    }
    Ok(())
}

fn validate_news_extraction_response(
    request: &CompanyNewsExtractionRequest,
    response: &CompanyNewsExtractionResponse,
    max_articles: usize,
) -> Result<(), WebAdapterError> {
    if response.schema_version != COMPANY_NEWS_EXTRACTION_SCHEMA_VERSION {
        return Err(WebAdapterError::InvalidResponse(format!(
            "unsupported schema version {}",
            response.schema_version
        )));
    }
    if response.request_id != request.request_id {
        return Err(WebAdapterError::InvalidResponse(
            "response request_id does not match the request".to_owned(),
        ));
    }
    if response.articles.len() > request.max_articles as usize
        || response.articles.len() > max_articles
    {
        return Err(WebAdapterError::InvalidResponse(
            "adapter returned too many article URLs".to_owned(),
        ));
    }
    let mut seen = HashSet::new();
    if response.publications.len() > request.max_articles as usize
        || response.publications.len() > max_articles
    {
        return Err(WebAdapterError::InvalidResponse(
            "adapter returned too many publication URLs".to_owned(),
        ));
    }
    for publication in &response.publications {
        validate_url_shape(&publication.url).map_err(WebAdapterError::InvalidResponse)?;
        if !publication.rank_score.is_finite() || !(0.0..=1.0).contains(&publication.rank_score) {
            return Err(WebAdapterError::InvalidResponse(format!(
                "rank_score for {} must be between 0 and 1",
                publication.url
            )));
        }
        if !seen.insert(("publication", publication.url.as_str())) {
            return Err(WebAdapterError::InvalidResponse(format!(
                "adapter returned duplicate publication URL {}",
                publication.url
            )));
        }
    }
    for article in &response.articles {
        validate_article_url(&article.url).map_err(WebAdapterError::InvalidResponse)?;
        if !article.rank_score.is_finite() || !(0.0..=1.0).contains(&article.rank_score) {
            return Err(WebAdapterError::InvalidResponse(format!(
                "rank_score for {} must be between 0 and 1",
                article.url
            )));
        }
        if !seen.insert(("article", article.url.as_str())) {
            return Err(WebAdapterError::InvalidResponse(format!(
                "adapter returned duplicate article URL {}",
                article.url
            )));
        }
    }
    Ok(())
}

fn validate_response(
    request: &WebDiscoveryRequest,
    response: &WebDiscoveryResponse,
    max_candidates: usize,
) -> Result<(), WebAdapterError> {
    if response.schema_version != WEB_DISCOVERY_SCHEMA_VERSION {
        return Err(WebAdapterError::InvalidResponse(format!(
            "unsupported schema version {}",
            response.schema_version
        )));
    }
    if response.request_id != request.request_id {
        return Err(WebAdapterError::InvalidResponse(
            "response request_id does not match the request".to_owned(),
        ));
    }
    if response.candidates.len() > request.max_candidates as usize
        || response.candidates.len() > max_candidates
    {
        return Err(WebAdapterError::InvalidResponse(
            "adapter returned too many candidates".to_owned(),
        ));
    }

    let requested = request
        .requested_roles
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    for candidate in &response.candidates {
        validate_url_shape(&candidate.url).map_err(WebAdapterError::InvalidResponse)?;
        if !candidate.rank_score.is_finite() || !(0.0..=1.0).contains(&candidate.rank_score) {
            return Err(WebAdapterError::InvalidResponse(format!(
                "rank_score for {} must be between 0 and 1",
                candidate.url
            )));
        }
        if !requested.contains(&candidate.role) {
            return Err(WebAdapterError::InvalidResponse(format!(
                "adapter returned unrequested role {}",
                candidate.role.as_str()
            )));
        }
        if !seen.insert((candidate.role, candidate.url.as_str())) {
            return Err(WebAdapterError::InvalidResponse(format!(
                "adapter returned duplicate candidate {} for role {}",
                candidate.url,
                candidate.role.as_str()
            )));
        }
    }
    Ok(())
}

fn validate_url_shape(url: &Url) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("URL {url} must use HTTP or HTTPS"));
    }
    let Some(host) = url.host_str() else {
        return Err(format!("URL {url} has no host"));
    };
    if is_reserved_example_host(host) {
        return Err(format!("URL {url} uses a reserved example host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("URL {url} contains credentials"));
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

fn validate_article_url(url: &Url) -> Result<(), String> {
    validate_url_shape(url)?;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let path = url.path().to_ascii_lowercase();
    if path.is_empty() || path == "/" {
        return Err(format!(
            "article URL {url} must point to a page, not a site root"
        ));
    }
    if (host.ends_with("google.com") && path.starts_with("/search"))
        || (host.ends_with("bing.com") && path.starts_with("/search"))
    {
        return Err(format!(
            "article URL {url} must not be a search-result page"
        ));
    }
    Ok(())
}

async fn read_limited_body(
    response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, WebAdapterError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(WebAdapterError::ResponseTooLarge { limit });
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(WebAdapterError::ResponseTooLarge { limit });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Debug, thiserror::Error)]
pub enum WebAdapterError {
    #[error("invalid web adapter configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid web adapter request: {0}")]
    InvalidRequest(String),
    #[error("web adapter request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("web adapter returned HTTP {status}: {body}")]
    HttpStatus {
        status: u16,
        retryable: bool,
        retry_after_seconds: Option<u64>,
        body: String,
    },
    #[error("invalid web adapter response: {0}")]
    InvalidResponse(String),
    #[error("web adapter response exceeded {limit} bytes")]
    ResponseTooLarge { limit: usize },
}

impl WebAdapterError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Request(error) => error.is_timeout() || error.is_connect() || error.is_request(),
            Self::HttpStatus { retryable, .. } => *retryable,
            Self::InvalidConfig(_)
            | Self::InvalidRequest(_)
            | Self::InvalidResponse(_)
            | Self::ResponseTooLarge { .. } => false,
        }
    }

    pub fn retry_after_seconds(&self) -> Option<u64> {
        match self {
            Self::HttpStatus {
                retry_after_seconds,
                ..
            } => *retry_after_seconds,
            _ => None,
        }
    }

    pub fn safe_summary(&self) -> String {
        match self {
            Self::InvalidConfig(message) => {
                format!("invalid web adapter configuration: {message}")
            }
            Self::InvalidRequest(message) => format!("invalid web adapter request: {message}"),
            Self::Request(_) => "web adapter request failed".to_owned(),
            Self::HttpStatus { status, .. } => format!("web adapter returned HTTP {status}"),
            Self::InvalidResponse(message) => {
                format!("invalid web adapter response: {message}")
            }
            Self::ResponseTooLarge { limit } => {
                format!("web adapter response exceeded {limit} bytes")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
    };
    use tokio::net::TcpListener;

    use super::*;

    #[derive(Clone)]
    struct FixtureState {
        token: Arc<String>,
    }

    #[tokio::test]
    async fn client_sends_neutral_contract_and_validates_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let app = Router::new()
            .route(
                "/v1/discover",
                post(
                    |State(state): State<FixtureState>,
                     headers: HeaderMap,
                     Json(request): Json<WebDiscoveryRequest>| async move {
                        let authorization = headers
                            .get(reqwest::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .expect("authorization header");
                        assert_eq!(authorization, format!("Bearer {}", state.token.as_str()));
                        let idempotency_key = headers
                            .get("Idempotency-Key")
                            .and_then(|value| value.to_str().ok())
                            .expect("idempotency header");
                        assert_eq!(idempotency_key, request.request_id.to_string());
                        Json(WebDiscoveryResponse {
                            schema_version: WEB_DISCOVERY_SCHEMA_VERSION.to_owned(),
                            request_id: request.request_id,
                            candidates: vec![WebDiscoveryCandidate {
                                url: Url::parse("https://fixture-company.dev/newsroom")
                                    .expect("URL"),
                                role: WebPropertyRole::Newsroom,
                                suggested_kind: SuggestedResourceKind::Html,
                                rank_score: 0.8,
                            }],
                            adapter_trace_id: Some("fixture-trace".to_owned()),
                        })
                    },
                ),
            )
            .with_state(FixtureState {
                token: Arc::new("secret".to_owned()),
            });
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let client = WebDiscoveryAdapterClient::new(WebDiscoveryAdapterConfig {
            base_url: Url::parse(&format!("http://{address}/")).expect("base URL"),
            bearer_token: Some("secret".to_owned()),
            request_timeout: Duration::from_secs(5),
            max_response_bytes: 64 * 1024,
            max_candidates: 20,
        })
        .expect("client");
        let request_id = Uuid::new_v4();
        let request = WebDiscoveryRequest::new(
            request_id,
            WebDiscoveryCompany {
                company_id: Uuid::new_v4(),
                name: "Example Corporation".to_owned(),
                aliases: vec!["Example Corp".to_owned()],
                known_urls: Vec::new(),
                sector: None,
                industry: None,
            },
            20,
        );

        let response = client.discover(&request).await.expect("response");
        assert_eq!(response.request_id, request_id);
        assert_eq!(response.candidates.len(), 1);
        assert_eq!(response.candidates[0].role, WebPropertyRole::Newsroom);
        task.abort();
    }

    #[tokio::test]
    async fn rate_limit_is_retryable_and_preserves_retry_after() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let app = Router::new().route(
            "/v1/discover",
            post(|| async {
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "37")],
                    "busy",
                )
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let client = WebDiscoveryAdapterClient::new(WebDiscoveryAdapterConfig {
            base_url: Url::parse(&format!("http://{address}/")).expect("base URL"),
            bearer_token: None,
            request_timeout: Duration::from_secs(5),
            max_response_bytes: 64 * 1024,
            max_candidates: 20,
        })
        .expect("client");
        let request = WebDiscoveryRequest::new(
            Uuid::new_v4(),
            WebDiscoveryCompany {
                company_id: Uuid::new_v4(),
                name: "Example".to_owned(),
                aliases: Vec::new(),
                known_urls: Vec::new(),
                sector: None,
                industry: None,
            },
            20,
        );
        let error = client.discover(&request).await.expect_err("rate limited");
        assert!(error.is_retryable());
        assert_eq!(error.retry_after_seconds(), Some(37));
        task.abort();
    }

    #[tokio::test]
    async fn news_client_exposes_only_neutral_article_urls() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let app = Router::new().route(
            "/v1/extract-news",
            post(
                |headers: HeaderMap, Json(request): Json<CompanyNewsExtractionRequest>| async move {
                    assert_eq!(
                        headers
                            .get("Idempotency-Key")
                            .and_then(|value| value.to_str().ok()),
                        Some(request.request_id.to_string().as_str())
                    );
                    Json(CompanyNewsExtractionResponse {
                        schema_version: COMPANY_NEWS_EXTRACTION_SCHEMA_VERSION.to_owned(),
                        request_id: request.request_id,
                        publications: vec![CompanyNewsPublicationCandidate {
                            url: Url::parse("https://fixture-company.dev/news")
                                .expect("publication URL"),
                            rank_score: 0.9,
                        }],
                        articles: vec![CompanyNewsArticleCandidate {
                            url: Url::parse("https://fixture-company.dev/news/launch")
                                .expect("URL"),
                            rank_score: 0.75,
                        }],
                        adapter_trace_id: Some("neutral-trace".to_owned()),
                    })
                },
            ),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let client = CompanyNewsExtractionAdapterClient::new(CompanyNewsExtractionAdapterConfig {
            base_url: Url::parse(&format!("http://{address}/")).expect("base URL"),
            bearer_token: None,
            request_timeout: Duration::from_secs(5),
            max_response_bytes: 64 * 1024,
            max_articles: 20,
        })
        .expect("client");
        let request = CompanyNewsExtractionRequest::new(
            Uuid::new_v4(),
            WebDiscoveryCompany {
                company_id: Uuid::new_v4(),
                name: "Private Example".to_owned(),
                aliases: Vec::new(),
                known_urls: Vec::new(),
                sector: None,
                industry: None,
            },
            "2026-06-20T00:00:00Z".parse().expect("start"),
            "2026-07-21T00:00:00Z".parse().expect("end"),
            20,
        );
        let response = client.extract_news(&request).await.expect("response");
        assert_eq!(response.articles.len(), 1);
        assert_eq!(
            response.articles[0].url.as_str(),
            "https://fixture-company.dev/news/launch"
        );
        task.abort();
    }

    #[test]
    fn reserved_example_hosts_are_rejected() {
        for value in [
            "https://public.example/path/to/article",
            "https://example.com/news",
            "https://docs.localhost/news",
        ] {
            let url = Url::parse(value).expect("URL");
            assert!(validate_url_shape(&url).is_err(), "{value}");
        }
    }
}
