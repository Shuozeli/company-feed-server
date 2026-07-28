use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::Duration,
};

use feed_core::{
    Company, DEFAULT_PUBLIC_FETCH_USER_AGENT, DiscoveredSource, SourceKind, is_sitemap_url,
};
use feed_rs::model::FeedType;
use futures_util::{StreamExt, stream};
use reqwest::{
    Client, StatusCode,
    header::{CONTENT_TYPE, LOCATION},
    redirect::Policy,
};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

const COMMON_FEED_PATHS: &[&str] = &[
    "/feed",
    "/feed.xml",
    "/rss",
    "/rss.xml",
    "/rss/pressrelease.aspx",
    "/rss/news-releases.xml",
    "/atom.xml",
    "/news/rss",
    "/news/feed/",
    "/news/press-releases/rss",
    "/newsroom/rss",
    "/blog/feed",
    "/blog/rss.xml",
    "/news-events/press-releases/rss",
    "/investors/news-events/press-releases/rss",
    "/press-releases/rss",
    "/press-releases/feed/",
];

const SOURCE_LINK_TEXT_PHRASES: &[&str] = &[
    "newsroom",
    "press release",
    "media center",
    "media centre",
    "company news",
    "latest news",
    "news & events",
    "news and events",
    "engineering blog",
    "investor relations",
];

const SOURCE_LINK_PATH_SEGMENTS: &[&str] = &[
    "news",
    "newsroom",
    "press",
    "press-releases",
    "media",
    "blog",
    "engineering",
    "investor-relations",
];

#[derive(Clone, Debug)]
pub struct DiscoveryConfig {
    pub request_timeout: Duration,
    pub max_response_bytes: usize,
    pub max_concurrency: usize,
    pub max_candidates: usize,
    pub probe_common_paths: bool,
    pub allow_private_networks: bool,
    pub user_agent: String,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(15),
            max_response_bytes: 4 * 1024 * 1024,
            max_concurrency: 8,
            max_candidates: 200,
            probe_common_paths: true,
            allow_private_networks: false,
            user_agent: DEFAULT_PUBLIC_FETCH_USER_AGENT.to_owned(),
        }
    }
}

#[derive(Clone)]
pub struct DiscoveryClient {
    client: Client,
    config: DiscoveryConfig,
}

impl DiscoveryClient {
    pub fn new(config: DiscoveryConfig) -> Result<Self, DiscoveryError> {
        if config.max_response_bytes == 0
            || config.max_concurrency == 0
            || config.max_candidates == 0
        {
            return Err(DiscoveryError::InvalidConfig(
                "discovery size, concurrency, and candidate limits must be positive".to_owned(),
            ));
        }
        let client = Client::builder()
            .connect_timeout(config.request_timeout)
            .timeout(config.request_timeout)
            .redirect(Policy::none())
            .no_proxy()
            .user_agent(&config.user_agent)
            .build()?;
        Ok(Self { client, config })
    }

    pub async fn discover_company(
        &self,
        company: &Company,
    ) -> Result<DiscoveryReport, DiscoveryError> {
        self.discover_company_with_seeds(company, &[]).await
    }

    pub async fn discover_company_with_seeds(
        &self,
        company: &Company,
        seeds: &[DiscoverySeed],
    ) -> Result<DiscoveryReport, DiscoveryError> {
        let targets = build_fetch_targets(company, seeds, self.config.probe_common_paths)?;
        if targets.is_empty() {
            return Err(DiscoveryError::NoEntryPoints(company.name.clone()));
        }

        let results = stream::iter(targets)
            .map(|target| async move {
                let result = self.fetch(&target.url).await;
                (target, result)
            })
            .buffer_unordered(self.config.max_concurrency)
            .collect::<Vec<_>>()
            .await;

        let mut accumulator = CandidateAccumulator::new(self.config.max_candidates);
        let mut attempts = Vec::with_capacity(results.len());
        let mut entry_successes = 0_usize;

        for (target, result) in results {
            match result {
                Ok(fetched) => {
                    if target.kind == FetchTargetKind::Entry {
                        entry_successes += 1;
                    }
                    let outcome = process_fetched_target(&target, &fetched, &mut accumulator);
                    attempts.push(DiscoveryAttempt {
                        url: target.url,
                        method: target.method,
                        required: target.kind == FetchTargetKind::Entry,
                        status: Some(fetched.status.as_u16()),
                        content_type: fetched.content_type,
                        final_url: Some(fetched.final_url),
                        outcome,
                        error: None,
                    });
                }
                Err(error) => {
                    attempts.push(DiscoveryAttempt {
                        url: target.url,
                        method: target.method,
                        required: target.kind == FetchTargetKind::Entry,
                        status: error.status().map(|status| status.as_u16()),
                        content_type: None,
                        final_url: None,
                        outcome: "fetch_failed".to_owned(),
                        error: Some(error.to_string()),
                    });
                }
            }
        }

        attempts.sort_by(|left, right| left.url.as_str().cmp(right.url.as_str()));
        let candidates = accumulator.finish();
        if entry_successes == 0 && candidates.is_empty() {
            return Err(DiscoveryError::AllEntryPointsFailed {
                company: company.name.clone(),
                attempts,
            });
        }

        Ok(DiscoveryReport {
            candidates,
            attempts,
            entry_successes,
        })
    }

    async fn fetch(&self, url: &Url) -> Result<FetchedResource, FetchError> {
        let mut current_url = url.clone();
        for redirect_count in 0..=5 {
            validate_fetch_url(&current_url)?;
            if !self.config.allow_private_networks {
                validate_resolved_target(&current_url).await?;
            }
            let response = self
                .client
                .get(current_url.clone())
                .send()
                .await
                .map_err(|source| FetchError::Request {
                    url: current_url.clone(),
                    source,
                })?;
            if !self.config.allow_private_networks {
                let remote_address =
                    response
                        .remote_addr()
                        .ok_or_else(|| FetchError::RemoteAddressUnavailable {
                            url: current_url.clone(),
                        })?;
                if !is_public_ip(remote_address.ip()) {
                    return Err(FetchError::PrivateNetwork {
                        url: current_url,
                        address: remote_address.ip(),
                    });
                }
            }
            let status = response.status();
            if status.is_redirection() {
                if redirect_count == 5 {
                    return Err(FetchError::TooManyRedirects { url: current_url });
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| FetchError::InvalidRedirect {
                        url: current_url.clone(),
                    })?;
                current_url =
                    current_url
                        .join(location)
                        .map_err(|_| FetchError::InvalidRedirect {
                            url: current_url.clone(),
                        })?;
                continue;
            }
            if !status.is_success() {
                return Err(FetchError::HttpStatus {
                    url: current_url,
                    status,
                });
            }
            if response
                .content_length()
                .is_some_and(|length| length > self.config.max_response_bytes as u64)
            {
                return Err(FetchError::ResponseTooLarge {
                    url: current_url,
                    limit: self.config.max_response_bytes,
                });
            }
            let final_url = current_url;
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|source| FetchError::Request {
                    url: final_url.clone(),
                    source,
                })?;
                if body.len().saturating_add(chunk.len()) > self.config.max_response_bytes {
                    return Err(FetchError::ResponseTooLarge {
                        url: final_url,
                        limit: self.config.max_response_bytes,
                    });
                }
                body.extend_from_slice(&chunk);
            }

            return Ok(FetchedResource {
                final_url,
                status,
                content_type,
                body,
            });
        }
        unreachable!("redirect loop returns on success or the sixth redirect")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoverySeed {
    pub url: Url,
    pub role: String,
    pub rank_score: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryReport {
    pub candidates: Vec<DiscoveredSource>,
    pub attempts: Vec<DiscoveryAttempt>,
    pub entry_successes: usize,
}

impl DiscoveryReport {
    pub fn metadata(&self) -> Value {
        json!({
            "attempts": self.attempts,
            "entry_successes": self.entry_successes,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryAttempt {
    pub url: Url,
    pub method: String,
    pub required: bool,
    pub status: Option<u16>,
    pub content_type: Option<String>,
    pub final_url: Option<Url>,
    pub outcome: String,
    pub error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("invalid discovery configuration: {0}")]
    InvalidConfig(String),
    #[error(transparent)]
    Client(#[from] reqwest::Error),
    #[error("company {0} has no discovery entry points")]
    NoEntryPoints(String),
    #[error("all configured discovery entry points failed for {company}")]
    AllEntryPointsFailed {
        company: String,
        attempts: Vec<DiscoveryAttempt>,
    },
    #[error("invalid configured discovery URL {url}: {reason}")]
    InvalidEntryPoint { url: Url, reason: &'static str },
    #[error("invalid external discovery seed {url}: {reason}")]
    InvalidSeed { url: Url, reason: String },
}

impl DiscoveryError {
    pub fn metadata(&self) -> Value {
        match self {
            Self::AllEntryPointsFailed { attempts, .. } => json!({ "attempts": attempts }),
            _ => Value::Object(Default::default()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FetchTargetKind {
    Entry,
    Probe,
}

#[derive(Clone, Debug)]
struct FetchTarget {
    url: Url,
    method: String,
    kind: FetchTargetKind,
    include_html_candidate: bool,
    adapter_context: Option<AdapterContext>,
}

#[derive(Clone, Debug)]
struct AdapterContext {
    roles: Vec<String>,
    rank_score: f64,
}

impl AdapterContext {
    fn merge(&mut self, role: &str, rank_score: f64) {
        if !self.roles.iter().any(|existing| existing == role) {
            self.roles.push(role.to_owned());
            self.roles.sort();
        }
        self.rank_score = self.rank_score.max(rank_score);
    }
}

fn build_fetch_targets(
    company: &Company,
    seeds: &[DiscoverySeed],
    probe_common_paths: bool,
) -> Result<Vec<FetchTarget>, DiscoveryError> {
    let mut targets = BTreeMap::<String, FetchTarget>::new();
    let mut origins = BTreeMap::<String, (Url, Option<AdapterContext>)>::new();

    for (provenance, url) in company.discovery_entry_points() {
        validate_fetch_url(url).map_err(|error| DiscoveryError::InvalidEntryPoint {
            url: url.clone(),
            reason: error.reason(),
        })?;
        targets
            .entry(url.as_str().to_owned())
            .or_insert(FetchTarget {
                url: url.clone(),
                method: format!("configured_{provenance}"),
                kind: FetchTargetKind::Entry,
                include_html_candidate: provenance != "homepage",
                adapter_context: None,
            });
        if let Some(origin) = origin_url(url) {
            origins
                .entry(origin.as_str().to_owned())
                .or_insert((origin, None));
        }
    }

    for seed in seeds {
        validate_fetch_url(&seed.url).map_err(|error| DiscoveryError::InvalidSeed {
            url: seed.url.clone(),
            reason: error.reason().to_owned(),
        })?;
        if seed.role.trim().is_empty() {
            return Err(DiscoveryError::InvalidSeed {
                url: seed.url.clone(),
                reason: "role cannot be empty".to_owned(),
            });
        }
        if !seed.rank_score.is_finite() || !(0.0..=1.0).contains(&seed.rank_score) {
            return Err(DiscoveryError::InvalidSeed {
                url: seed.url.clone(),
                reason: "rank_score must be between 0 and 1".to_owned(),
            });
        }
        let context = AdapterContext {
            roles: vec![seed.role.clone()],
            rank_score: seed.rank_score,
        };
        targets
            .entry(seed.url.as_str().to_owned())
            .and_modify(|target| match &mut target.adapter_context {
                Some(existing) => existing.merge(&seed.role, seed.rank_score),
                None => target.adapter_context = Some(context.clone()),
            })
            .or_insert(FetchTarget {
                url: seed.url.clone(),
                method: "external_web_adapter".to_owned(),
                kind: FetchTargetKind::Entry,
                include_html_candidate: seed.role != "homepage",
                adapter_context: Some(context.clone()),
            });
        if let Some(origin) = origin_url(&seed.url) {
            origins
                .entry(origin.as_str().to_owned())
                .and_modify(|(_, existing)| match existing {
                    Some(existing) => existing.merge(&seed.role, seed.rank_score),
                    None => *existing = Some(context.clone()),
                })
                .or_insert((origin, Some(context)));
        }
    }

    if probe_common_paths {
        for (origin, adapter_context) in origins.values() {
            for path in COMMON_FEED_PATHS {
                let Ok(url) = origin.join(path) else {
                    continue;
                };
                targets
                    .entry(url.as_str().to_owned())
                    .or_insert(FetchTarget {
                        url,
                        method: "common_feed_path".to_owned(),
                        kind: FetchTargetKind::Probe,
                        include_html_candidate: false,
                        adapter_context: adapter_context.clone(),
                    });
            }
        }
    }

    Ok(targets.into_values().collect())
}

fn origin_url(url: &Url) -> Option<Url> {
    let host = url.host_str()?;
    let host = match url.host()? {
        url::Host::Ipv6(_) => format!("[{host}]"),
        _ => host.to_owned(),
    };
    let mut origin = Url::parse(&format!("{}://{host}/", url.scheme())).ok()?;
    origin.set_port(url.port()).ok()?;
    Some(origin)
}

struct FetchedResource {
    final_url: Url,
    status: StatusCode,
    content_type: Option<String>,
    body: Vec<u8>,
}

fn process_fetched_target(
    target: &FetchTarget,
    fetched: &FetchedResource,
    accumulator: &mut CandidateAccumulator,
) -> String {
    if let Some((kind, item_count)) = parse_feed_kind(&fetched.body) {
        accumulator.insert(
            fetched.final_url.clone(),
            kind,
            if target.kind == FetchTargetKind::Entry {
                0.99
            } else {
                0.96
            },
            add_adapter_context(
                target,
                json!({
                    "method": target.method,
                    "found_on": target.url,
                    "http_status": fetched.status.as_u16(),
                    "content_type": fetched.content_type,
                    "feed_validation": "valid",
                    "sample_item_count": item_count,
                }),
            ),
        );
        return "valid_feed".to_owned();
    }

    if target.kind == FetchTargetKind::Probe {
        return "not_a_feed".to_owned();
    }
    if !looks_like_html(fetched.content_type.as_deref(), &fetched.body) {
        return "unsupported_content".to_owned();
    }

    if target.include_html_candidate {
        accumulator.insert(
            fetched.final_url.clone(),
            SourceKind::Html,
            0.75,
            add_adapter_context(
                target,
                json!({
                    "method": target.method,
                    "found_on": target.url,
                    "http_status": fetched.status.as_u16(),
                    "content_type": fetched.content_type,
                }),
            ),
        );
    }
    for candidate in extract_html_candidates(&fetched.final_url, &fetched.body) {
        accumulator.insert(
            candidate.url,
            candidate.kind,
            candidate.confidence,
            add_adapter_context(target, candidate.evidence),
        );
    }
    "html_examined".to_owned()
}

fn add_adapter_context(target: &FetchTarget, mut observation: Value) -> Value {
    let Some(context) = &target.adapter_context else {
        return observation;
    };
    if let Some(object) = observation.as_object_mut() {
        object.insert(
            "external_web_adapter".to_owned(),
            json!({
                "roles": context.roles,
                "rank_score": context.rank_score,
            }),
        );
    }
    observation
}

fn parse_feed_kind(body: &[u8]) -> Option<(SourceKind, usize)> {
    let feed = feed_rs::parser::parse(body).ok()?;
    let kind = match feed.feed_type {
        FeedType::Atom => SourceKind::Atom,
        FeedType::RSS0 | FeedType::RSS1 | FeedType::RSS2 => SourceKind::Rss,
        FeedType::JSON => return None,
    };
    Some((kind, feed.entries.len()))
}

fn looks_like_html(content_type: Option<&str>, body: &[u8]) -> bool {
    if content_type.is_some_and(|value| {
        let value = value.to_ascii_lowercase();
        value.contains("text/html") || value.contains("application/xhtml")
    }) {
        return true;
    }
    let prefix = &body[..body.len().min(512)];
    String::from_utf8_lossy(prefix)
        .to_ascii_lowercase()
        .contains("<html")
}

#[derive(Debug)]
struct HtmlCandidate {
    url: Url,
    kind: SourceKind,
    confidence: f64,
    evidence: Value,
}

fn extract_html_candidates(base_url: &Url, body: &[u8]) -> Vec<HtmlCandidate> {
    let document = Html::parse_document(&String::from_utf8_lossy(body));
    let alternate_selector =
        Selector::parse("link[rel][href]").expect("static alternate selector is valid");
    let anchor_selector = Selector::parse("a[href]").expect("static anchor selector is valid");
    let mut candidates = Vec::new();

    for element in document.select(&alternate_selector) {
        let rel = element.value().attr("rel").unwrap_or_default();
        if !rel
            .split_ascii_whitespace()
            .any(|value| value.eq_ignore_ascii_case("alternate"))
        {
            continue;
        }
        let Some(href) = element.value().attr("href") else {
            continue;
        };
        let Some(url) = resolve_candidate_url(base_url, href) else {
            continue;
        };
        let content_type = element.value().attr("type").unwrap_or_default();
        let lower_type = content_type.to_ascii_lowercase();
        let lower_href = href.to_ascii_lowercase();
        if lower_type.contains("oembed") || lower_href.contains("/oembed/") {
            continue;
        }
        let kind = if lower_type.contains("atom") || lower_href.contains("atom") {
            SourceKind::Atom
        } else if lower_type.contains("rss")
            || lower_type.contains("xml")
            || lower_href.contains("rss")
            || lower_href.ends_with(".xml")
        {
            SourceKind::Rss
        } else {
            continue;
        };
        candidates.push(HtmlCandidate {
            url,
            kind,
            confidence: 0.97,
            evidence: json!({
                "method": "html_alternate",
                "found_on": base_url,
                "rel": rel,
                "content_type": content_type,
            }),
        });
    }

    for element in document.select(&anchor_selector) {
        let Some(href) = element.value().attr("href") else {
            continue;
        };
        let text = element.text().collect::<Vec<_>>().join(" ");
        let Some(url) = resolve_candidate_url(base_url, href) else {
            continue;
        };
        if url == *base_url
            || !same_company_site(base_url, &url)
            || !looks_like_source_landing_link(&text, &url)
        {
            continue;
        }
        let kind = explicit_anchor_feed_kind(&text, &url).unwrap_or(SourceKind::Html);
        candidates.push(HtmlCandidate {
            url,
            kind,
            confidence: if kind == SourceKind::Html { 0.65 } else { 0.90 },
            evidence: json!({
                "method": "keyword_link",
                "found_on": base_url,
                "link_text": normalize_text(&text),
                "inferred_feed_kind": if kind == SourceKind::Html {
                    Value::Null
                } else {
                    json!(kind.as_str())
                },
            }),
        });
    }

    candidates
}

fn explicit_anchor_feed_kind(text: &str, url: &Url) -> Option<SourceKind> {
    let has_token = |value: &str, expected: &str| {
        value
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|token| token.eq_ignore_ascii_case(expected))
    };
    let url_identity = format!("{}?{}", url.path(), url.query().unwrap_or_default());
    if has_token(text, "atom") || has_token(&url_identity, "atom") {
        Some(SourceKind::Atom)
    } else if has_token(text, "rss") || has_token(&url_identity, "rss") {
        Some(SourceKind::Rss)
    } else {
        None
    }
}

fn looks_like_source_landing_link(text: &str, url: &Url) -> bool {
    let normalized_text = normalize_text(text).to_ascii_lowercase();
    let text_match = normalized_text == "news"
        || normalized_text == "blog"
        || SOURCE_LINK_TEXT_PHRASES
            .iter()
            .any(|phrase| normalized_text.contains(phrase));
    if text_match && normalized_text.len() <= 100 {
        return true;
    }

    let Some(last_segment) = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
    else {
        return false;
    };
    let normalized_segment = last_segment
        .trim_end_matches(".html")
        .trim_end_matches(".htm")
        .trim_end_matches(".aspx")
        .to_ascii_lowercase();
    SOURCE_LINK_PATH_SEGMENTS.contains(&normalized_segment.as_str())
}

fn resolve_candidate_url(base_url: &Url, href: &str) -> Option<Url> {
    let mut url = base_url.join(href.trim()).ok()?;
    validate_fetch_url(&url).ok()?;
    url.set_fragment(None);
    (!ignored_source_candidate(&url)).then_some(url)
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn ignored_source_candidate(url: &Url) -> bool {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let path = url.path().to_ascii_lowercase();
    host == "support.google.com"
        || is_sitemap_url(url)
        || path.contains("/wp-json/oembed/")
        || path.contains("/comments/feed")
        || path.contains("/comment-feed")
}

fn same_company_site(left: &Url, right: &Url) -> bool {
    let normalize = |host: &str| {
        host.trim_end_matches('.')
            .strip_prefix("www.")
            .unwrap_or(host.trim_end_matches('.'))
            .to_ascii_lowercase()
    };
    let (Some(left), Some(right)) = (left.host_str(), right.host_str()) else {
        return false;
    };
    let left = normalize(left);
    let right = normalize(right);
    left == right
        || left
            .strip_suffix(&format!(".{right}"))
            .is_some_and(|prefix| !prefix.is_empty())
        || right
            .strip_suffix(&format!(".{left}"))
            .is_some_and(|prefix| !prefix.is_empty())
}

struct CandidateAccumulator {
    candidates: BTreeMap<(String, String), DiscoveredSource>,
    max_candidates: usize,
}

impl CandidateAccumulator {
    fn new(max_candidates: usize) -> Self {
        Self {
            candidates: BTreeMap::new(),
            max_candidates,
        }
    }

    fn insert(&mut self, mut url: Url, kind: SourceKind, confidence: f64, observation: Value) {
        url.set_fragment(None);
        if ignored_source_candidate(&url) {
            return;
        }
        let key = (kind.as_str().to_owned(), canonical_source_key(&url, kind));
        if let Some(existing) = self.candidates.get_mut(&key) {
            let should_replace_url = confidence > existing.confidence
                || (confidence == existing.confidence
                    && preferred_source_url(&url, &existing.candidate_url));
            existing.confidence = existing.confidence.max(confidence);
            if should_replace_url {
                existing.candidate_url = url;
            }
            if let Some(observations) = existing
                .evidence
                .get_mut("observations")
                .and_then(Value::as_array_mut)
            {
                observations.push(observation);
            }
            return;
        }
        if self.candidates.len() >= self.max_candidates {
            return;
        }
        self.candidates.insert(
            key,
            DiscoveredSource {
                candidate_url: url,
                candidate_kind: kind,
                confidence,
                evidence: json!({ "observations": [observation] }),
            },
        );
    }

    fn finish(self) -> Vec<DiscoveredSource> {
        self.candidates
            .into_values()
            .map(|mut candidate| {
                if candidate.candidate_kind == SourceKind::Html {
                    candidate.candidate_url.set_query(None);
                }
                candidate
            })
            .collect()
    }
}

fn canonical_source_key(url: &Url, kind: SourceKind) -> String {
    let mut canonical = url.clone();
    canonical.set_fragment(None);
    if kind == SourceKind::Html {
        canonical.set_query(None);
        let segments = canonical
            .path_segments()
            .map(|segments| {
                segments
                    .filter(|segment| !segment.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if segments.len() >= 2
            && looks_like_locale_segment(segments[0])
            && SOURCE_LINK_PATH_SEGMENTS.contains(&segments[1].to_ascii_lowercase().as_str())
        {
            canonical.set_path(&format!("/{}", segments[1..].join("/")));
        } else if canonical.path() != "/" {
            let trimmed = canonical.path().trim_end_matches('/').to_owned();
            canonical.set_path(&trimmed);
        }
    }
    if let Some(host) = canonical.host_str().map(str::to_owned)
        && let Some(without_www) = host.strip_prefix("www.")
    {
        let _ = canonical.set_host(Some(without_www));
    }
    canonical.to_string()
}

fn looks_like_locale_segment(segment: &str) -> bool {
    let parts = segment.split('-').collect::<Vec<_>>();
    matches!(parts.as_slice(), [part] if part.len() == 2 && part.chars().all(|character| character.is_ascii_alphabetic()))
        || matches!(parts.as_slice(), [language, region]
            if language.len() == 2
                && region.len() == 2
                && language.chars().all(|character| character.is_ascii_alphabetic())
                && region.chars().all(|character| character.is_ascii_alphabetic()))
}

fn preferred_source_url(candidate: &Url, existing: &Url) -> bool {
    match (candidate.query().is_none(), existing.query().is_none()) {
        (true, false) => true,
        (false, true) => false,
        _ => candidate.as_str().len() < existing.as_str().len(),
    }
}

#[derive(Debug, thiserror::Error)]
enum FetchError {
    #[error("unsupported URL {url}: {reason}")]
    UnsupportedUrl { url: Url, reason: &'static str },
    #[error("request failed for {url}: {source}")]
    Request { url: Url, source: reqwest::Error },
    #[error("HTTP {status} for {url}")]
    HttpStatus { url: Url, status: StatusCode },
    #[error("response for {url} exceeds {limit} bytes")]
    ResponseTooLarge { url: Url, limit: usize },
    #[error("DNS resolution failed for {url}: {source}")]
    DnsResolution { url: Url, source: std::io::Error },
    #[error("DNS resolution returned no addresses for {url}")]
    DnsResolutionEmpty { url: Url },
    #[error("URL {url} resolves or connects to disallowed address {address}")]
    PrivateNetwork { url: Url, address: IpAddr },
    #[error("remote address was unavailable for {url}")]
    RemoteAddressUnavailable { url: Url },
    #[error("redirect from {url} is missing or has an invalid Location header")]
    InvalidRedirect { url: Url },
    #[error("too many redirects while fetching {url}")]
    TooManyRedirects { url: Url },
}

impl FetchError {
    fn status(&self) -> Option<StatusCode> {
        match self {
            Self::HttpStatus { status, .. } => Some(*status),
            _ => None,
        }
    }

    fn reason(&self) -> &'static str {
        match self {
            Self::UnsupportedUrl { reason, .. } => reason,
            Self::Request { .. } => "request failed",
            Self::HttpStatus { .. } => "HTTP error",
            Self::ResponseTooLarge { .. } => "response too large",
            Self::DnsResolution { .. } => "DNS resolution failed",
            Self::DnsResolutionEmpty { .. } => "DNS returned no addresses",
            Self::PrivateNetwork { .. } => "private or reserved network is not allowed",
            Self::RemoteAddressUnavailable { .. } => "remote address unavailable",
            Self::InvalidRedirect { .. } => "invalid redirect",
            Self::TooManyRedirects { .. } => "too many redirects",
        }
    }
}

fn validate_fetch_url(url: &Url) -> Result<(), FetchError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FetchError::UnsupportedUrl {
            url: url.clone(),
            reason: "scheme must be HTTP or HTTPS",
        });
    }
    if url.host_str().is_none() {
        return Err(FetchError::UnsupportedUrl {
            url: url.clone(),
            reason: "host is missing",
        });
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(FetchError::UnsupportedUrl {
            url: url.clone(),
            reason: "embedded credentials are not allowed",
        });
    }
    Ok(())
}

async fn validate_resolved_target(url: &Url) -> Result<(), FetchError> {
    let Some(host) = url.host() else {
        return Err(FetchError::UnsupportedUrl {
            url: url.clone(),
            reason: "host is missing",
        });
    };
    match host {
        url::Host::Ipv4(address) => validate_public_address(url, IpAddr::V4(address)),
        url::Host::Ipv6(address) => validate_public_address(url, IpAddr::V6(address)),
        url::Host::Domain(host) => {
            let port = url
                .port_or_known_default()
                .ok_or_else(|| FetchError::UnsupportedUrl {
                    url: url.clone(),
                    reason: "port is missing",
                })?;
            let addresses = tokio::net::lookup_host((host, port))
                .await
                .map_err(|source| FetchError::DnsResolution {
                    url: url.clone(),
                    source,
                })?
                .map(|address| address.ip())
                .collect::<Vec<_>>();
            if addresses.is_empty() {
                return Err(FetchError::DnsResolutionEmpty { url: url.clone() });
            }
            for address in addresses {
                validate_public_address(url, address)?;
            }
            Ok(())
        }
    }
}

fn validate_public_address(url: &Url, address: IpAddr) -> Result<(), FetchError> {
    if is_public_ip(address) {
        Ok(())
    } else {
        Err(FetchError::PrivateNetwork {
            url: url.clone(),
            address,
        })
    }
}

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4() {
        return is_public_ipv4(mapped);
    }
    let segments = address.segments();
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] == 0x2001 && segments[1] == 0x0002)
        || (segments[0] == 0x2001 && (segments[1] & 0xfff0) == 0x0010)
        || (segments[0] == 0x2001 && (segments[1] & 0xfff0) == 0x0020)
        || (segments[0] == 0x0100 && segments[1] == 0 && segments[2] == 0 && segments[3] == 0)
        || (segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2] == 1))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::Value;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn discovery_uses_the_shared_identifiable_user_agent_by_default() {
        assert_eq!(
            DiscoveryConfig::default().user_agent,
            DEFAULT_PUBLIC_FETCH_USER_AGENT
        );
    }

    fn company(homepage: Url) -> Company {
        let now = Utc::now();
        Company {
            id: Uuid::new_v4(),
            company_key: "acme".to_owned(),
            name: "Acme".to_owned(),
            aliases: Vec::new(),
            ownership_status: feed_core::OwnershipStatus::Private,
            lifecycle_status: feed_core::LifecycleStatus::Active,
            listings: Vec::new(),
            homepage_url: Some(homepage),
            investor_relations_url: None,
            newsroom_url: None,
            blog_url: None,
            hints: Vec::new(),
            discovery_enabled: true,
            discovery_not_before: now,
            discovery_cadence_seconds: 3600,
            metadata: Value::Object(Default::default()),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn extracts_alternate_feeds_and_keyword_pages() {
        let base = Url::parse("https://example.com/news/").expect("valid URL");
        let candidates = extract_html_candidates(
            &base,
            br#"
                <html><head>
                  <link rel="alternate" type="application/rss+xml" href="/feed.xml">
                  <link rel="alternate" type="text/xml+oembed" href="/wp-json/oembed/1.0/embed">
                  <link rel="alternate" type="application/rss+xml" href="/comments/feed/">
                  <link rel="alternate" type="application/rss+xml" href="/sitemap.rss">
                </head><body>
                  <a href="../engineering/">Engineering Blog</a>
                  <a href="/rss/pressrelease.aspx">Press Release RSS Feed (opens in new window)</a>
                  <a href="/press-release-archive">Press release archive</a>
                  <a href="https://medium.com/generic-engineering">Engineering Blog</a>
                  <a href="/news/acme-launches-widget">Acme launches widget</a>
                  <a href="/about">About</a>
                </body></html>
            "#,
        );

        assert!(candidates.iter().any(|candidate| {
            candidate.kind == SourceKind::Rss
                && candidate.url.as_str() == "https://example.com/feed.xml"
        }));
        assert!(candidates.iter().any(|candidate| {
            candidate.kind == SourceKind::Html
                && candidate.url.as_str() == "https://example.com/engineering/"
        }));
        assert!(candidates.iter().any(|candidate| {
            candidate.kind == SourceKind::Rss
                && candidate.url.as_str() == "https://example.com/rss/pressrelease.aspx"
        }));
        assert!(candidates.iter().any(|candidate| {
            candidate.kind == SourceKind::Html
                && candidate.url.as_str() == "https://example.com/press-release-archive"
        }));
        assert_eq!(candidates.len(), 4);
    }

    #[test]
    fn sitemap_resources_are_not_source_candidates() {
        assert!(ignored_source_candidate(
            &Url::parse("https://example.com/post-sitemap.xml").expect("sitemap URL")
        ));
        assert!(ignored_source_candidate(
            &Url::parse("https://example.com/sitemap.rss").expect("sitemap feed URL")
        ));
        assert!(!ignored_source_candidate(
            &Url::parse("https://example.com/news/feed.xml").expect("editorial feed URL")
        ));
    }

    #[test]
    fn canonicalizes_locale_and_query_variants_without_losing_stronger_urls() {
        let mut accumulator = CandidateAccumulator::new(20);
        accumulator.insert(
            Url::parse("https://www.example.com/en-gb/newsroom/").expect("locale URL"),
            SourceKind::Html,
            0.65,
            json!({"method": "keyword_link"}),
        );
        accumulator.insert(
            Url::parse("https://example.com/newsroom").expect("canonical URL"),
            SourceKind::Html,
            0.75,
            json!({"method": "external_web_adapter"}),
        );
        accumulator.insert(
            Url::parse("https://example.com/blog?type=guide").expect("query URL"),
            SourceKind::Html,
            0.65,
            json!({"method": "keyword_link"}),
        );
        accumulator.insert(
            Url::parse("https://example.com/blog/").expect("plain URL"),
            SourceKind::Html,
            0.65,
            json!({"method": "keyword_link"}),
        );

        let candidates = accumulator.finish();
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().any(|candidate| {
            candidate.candidate_url.as_str() == "https://example.com/newsroom"
                && candidate.confidence == 0.75
        }));
        assert!(
            candidates.iter().any(|candidate| {
                candidate.candidate_url.as_str() == "https://example.com/blog/"
            })
        );
    }

    #[test]
    fn source_link_classifier_rejects_individual_news_articles() {
        assert!(!looks_like_source_landing_link(
            "Acme launches a widget",
            &Url::parse("https://example.com/news/acme-launches-widget").expect("valid URL"),
        ));
        assert!(looks_like_source_landing_link(
            "News & Events",
            &Url::parse("https://example.com/investors/news-events").expect("valid URL"),
        ));
        assert!(looks_like_source_landing_link(
            "",
            &Url::parse("https://example.com/company/newsroom.html").expect("valid URL"),
        ));
    }

    #[tokio::test]
    async fn discovers_from_a_real_http_fixture() {
        use axum::{Router, http::header, response::IntoResponse, routing::get};

        let app = Router::new()
            .route(
                "/",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "text/html")],
                        r#"<link rel="alternate" type="application/atom+xml" href="/atom.xml">"#,
                    )
                }),
            )
            .route(
                "/feed",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "application/rss+xml")],
                        r#"<?xml version="1.0"?>
                        <rss version="2.0"><channel><title>Acme</title>
                        <link>https://example.com/</link><description>News</description>
                        <item><title>Launch</title><link>https://example.com/launch</link></item>
                        </channel></rss>"#,
                    )
                        .into_response()
                }),
            )
            .route(
                "/rss/pressrelease.aspx",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "text/xml; charset=utf-8")],
                        r#"<?xml version="1.0"?>
                        <rss version="2.0"><channel><title>Acme Press Releases</title>
                        <link>https://example.com/</link><description>Press releases</description>
                        <item><title>Quarterly results</title><link>https://example.com/results</link></item>
                        </channel></rss>"#,
                    )
                        .into_response()
                }),
            )
            .route(
                "/news-events/press-releases/rss",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "application/rss+xml")],
                        r#"<?xml version="1.0"?>
                        <rss version="2.0"><channel><title>Acme Investor News</title>
                        <link>https://example.com/</link><description>Investor press releases</description>
                        <item><title>Acme reports results</title><link>https://example.com/investors/results</link></item>
                        </channel></rss>"#,
                    )
                        .into_response()
                }),
            )
            .route(
                "/blog/rss.xml",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "application/rss+xml")],
                        r#"<?xml version="1.0"?>
                        <rss version="2.0"><channel><title>Acme Blog</title>
                        <link>https://example.com/blog</link><description>Company blog</description>
                        <item><title>Inside Acme engineering</title><link>https://example.com/blog/engineering</link></item>
                        </channel></rss>"#,
                    )
                        .into_response()
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve discovery fixture");
        });

        let client = DiscoveryClient::new(DiscoveryConfig {
            request_timeout: Duration::from_secs(2),
            max_response_bytes: 1024 * 1024,
            max_concurrency: 4,
            max_candidates: 20,
            probe_common_paths: true,
            allow_private_networks: true,
            user_agent: "discovery-test".to_owned(),
        })
        .expect("build client");
        let report = client
            .discover_company(&company(
                Url::parse(&format!("http://{address}/")).expect("fixture URL"),
            ))
            .await
            .expect("discover fixture");

        assert!(report.candidates.iter().any(|candidate| {
            candidate.candidate_kind == SourceKind::Atom
                && candidate.candidate_url.path() == "/atom.xml"
        }));
        assert!(report.candidates.iter().any(|candidate| {
            candidate.candidate_kind == SourceKind::Rss && candidate.candidate_url.path() == "/feed"
        }));
        assert!(report.candidates.iter().any(|candidate| {
            candidate.candidate_kind == SourceKind::Rss
                && candidate.candidate_url.path() == "/rss/pressrelease.aspx"
        }));
        assert!(report.candidates.iter().any(|candidate| {
            candidate.candidate_kind == SourceKind::Rss
                && candidate.candidate_url.path() == "/news-events/press-releases/rss"
        }));
        assert!(report.candidates.iter().any(|candidate| {
            candidate.candidate_kind == SourceKind::Rss
                && candidate.candidate_url.path() == "/blog/rss.xml"
        }));
        assert_eq!(report.entry_successes, 1);

        task.abort();
    }

    #[tokio::test]
    async fn keeps_valid_common_feed_when_the_entry_page_fails() {
        use axum::{
            Router,
            http::{StatusCode, header},
            response::IntoResponse,
            routing::get,
        };

        let app = Router::new()
            .route(
                "/newsroom",
                get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
            )
            .route(
                "/rss/news-releases.xml",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "application/rss+xml")],
                        r#"<?xml version="1.0"?>
                        <rss version="2.0"><channel><title>Acme News</title>
                        <link>https://example.com/</link><description>News</description>
                        <item><title>Acme reports results</title><link>https://example.com/results</link></item>
                        </channel></rss>"#,
                    )
                        .into_response()
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve discovery fixture");
        });

        let client = DiscoveryClient::new(DiscoveryConfig {
            request_timeout: Duration::from_secs(2),
            max_response_bytes: 1024 * 1024,
            max_concurrency: 4,
            max_candidates: 20,
            probe_common_paths: true,
            allow_private_networks: true,
            user_agent: "discovery-test".to_owned(),
        })
        .expect("build client");
        let report = client
            .discover_company(&company(
                Url::parse(&format!("http://{address}/newsroom")).expect("fixture URL"),
            ))
            .await
            .expect("a valid feed probe is sufficient discovery evidence");

        assert_eq!(report.entry_successes, 0);
        assert!(report.candidates.iter().any(|candidate| {
            candidate.candidate_kind == SourceKind::Rss
                && candidate.candidate_url.path() == "/rss/news-releases.xml"
        }));
        assert!(report.attempts.iter().any(|attempt| {
            attempt.required
                && attempt.url.path() == "/newsroom"
                && attempt.status == Some(StatusCode::SERVICE_UNAVAILABLE.as_u16())
        }));

        task.abort();
    }

    #[tokio::test]
    async fn validates_external_adapter_seeds_before_emitting_candidates() {
        use axum::{Router, http::header, routing::get};

        let app = Router::new().route(
            "/newsroom",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/html")],
                    r#"<link rel="alternate" type="application/rss+xml" href="/feed.xml">"#,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve discovery fixture");
        });
        let client = DiscoveryClient::new(DiscoveryConfig {
            request_timeout: Duration::from_secs(2),
            max_response_bytes: 1024 * 1024,
            max_concurrency: 2,
            max_candidates: 20,
            probe_common_paths: false,
            allow_private_networks: true,
            user_agent: "discovery-test".to_owned(),
        })
        .expect("build client");
        let mut company = company(Url::parse("https://unused.example/").expect("URL"));
        company.homepage_url = None;
        let report = client
            .discover_company_with_seeds(
                &company,
                &[DiscoverySeed {
                    url: Url::parse(&format!("http://{address}/newsroom")).expect("seed URL"),
                    role: "newsroom".to_owned(),
                    rank_score: 0.61,
                }],
            )
            .await
            .expect("discover external seed");

        let candidate = report
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_url.path() == "/feed.xml")
            .expect("feed candidate");
        assert_eq!(candidate.candidate_kind, SourceKind::Rss);
        assert_eq!(
            candidate.evidence["observations"][0]["external_web_adapter"]["roles"][0],
            "newsroom"
        );
        assert_eq!(
            candidate.evidence["observations"][0]["external_web_adapter"]["rank_score"],
            0.61
        );
        assert_eq!(report.entry_successes, 1);
        task.abort();
    }

    #[test]
    fn rejects_embedded_credentials() {
        let url = Url::parse("https://user:secret@example.com/").expect("valid URL");
        assert!(validate_fetch_url(&url).is_err());
    }

    #[test]
    fn network_policy_rejects_private_and_reserved_addresses() {
        for address in [
            "0.0.0.0",
            "10.1.2.3",
            "100.64.1.2",
            "127.0.0.1",
            "169.254.1.2",
            "172.16.0.1",
            "192.168.1.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
        ] {
            let address: IpAddr = address.parse().expect("IP address");
            assert!(!is_public_ip(address), "{address} must be rejected");
        }
        for address in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            let address: IpAddr = address.parse().expect("IP address");
            assert!(is_public_ip(address), "{address} must be allowed");
        }
    }

    #[tokio::test]
    async fn resolved_target_policy_rejects_loopback_literals() {
        let url = Url::parse("http://127.0.0.1:8080/newsroom").expect("URL");
        assert!(matches!(
            validate_resolved_target(&url).await,
            Err(FetchError::PrivateNetwork { .. })
        ));
    }
}
