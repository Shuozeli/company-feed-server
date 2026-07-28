use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::Duration,
};

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, Utc};
use feed_content::{ContentProcessOptions, process_html};
use feed_core::{
    CompanyNewsRecipeSpec, CrawlBatch, DEFAULT_PUBLIC_FETCH_USER_AGENT, RawCrawlItem,
    RecipeRenderMode, Source, SourceKind, has_invalid_resource_query,
    is_non_editorial_utility_article, resource_query_pairs,
};
use feed_rs::model::FeedType;
use futures_util::{StreamExt, stream};
use reqwest::{
    Client, StatusCode,
    header::{CONTENT_TYPE, LOCATION},
    redirect::Policy,
};
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

#[derive(Clone, Debug)]
pub struct RssAtomCrawlerConfig {
    pub request_timeout: Duration,
    pub max_response_bytes: usize,
    pub max_items: usize,
    pub user_agent: String,
}

impl Default for RssAtomCrawlerConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(20),
            max_response_bytes: 8 * 1024 * 1024,
            max_items: 500,
            user_agent: DEFAULT_PUBLIC_FETCH_USER_AGENT.to_owned(),
        }
    }
}

#[derive(Clone)]
pub struct RssAtomCrawler {
    client: Client,
    config: RssAtomCrawlerConfig,
}

impl RssAtomCrawler {
    pub fn new(config: RssAtomCrawlerConfig) -> Result<Self, CrawlError> {
        if config.max_response_bytes == 0 || config.max_items == 0 {
            return Err(CrawlError::InvalidConfig(
                "crawler response and item limits must be positive".to_owned(),
            ));
        }
        let client = Client::builder()
            .connect_timeout(config.request_timeout)
            .timeout(config.request_timeout)
            .redirect(Policy::limited(5))
            .user_agent(&config.user_agent)
            .build()?;
        Ok(Self { client, config })
    }

    pub async fn crawl(&self, source: &Source) -> Result<CrawlBatch, CrawlError> {
        if !matches!(source.kind, SourceKind::Rss | SourceKind::Atom) {
            return Err(CrawlError::UnsupportedSourceKind(source.kind));
        }
        validate_url(&source.url)?;
        let response = self
            .client
            .get(source.url.clone())
            .send()
            .await
            .map_err(|error| CrawlError::Request {
                url: source.url.clone(),
                message: error.to_string(),
            })?;
        if !response.status().is_success() {
            return Err(CrawlError::HttpStatus {
                url: source.url.clone(),
                status: response.status().as_u16(),
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.config.max_response_bytes as u64)
        {
            return Err(CrawlError::ResponseTooLarge {
                url: source.url.clone(),
                limit: self.config.max_response_bytes,
            });
        }
        let final_url = response.url().clone();
        validate_url(&final_url)?;
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| CrawlError::Request {
                url: final_url.clone(),
                message: error.to_string(),
            })?;
            if bytes.len().saturating_add(chunk.len()) > self.config.max_response_bytes {
                return Err(CrawlError::ResponseTooLarge {
                    url: final_url,
                    limit: self.config.max_response_bytes,
                });
            }
            bytes.extend_from_slice(&chunk);
        }

        parse_feed_body(&final_url, &bytes, self.config.max_items)
    }
}

#[derive(Clone, Debug)]
pub struct HtmlArticleCrawlerConfig {
    pub request_timeout: Duration,
    pub max_response_bytes: usize,
    pub max_articles: usize,
    pub max_concurrency: usize,
    pub max_per_host_concurrency: usize,
    pub min_content_chars: usize,
    pub allow_private_networks: bool,
    pub user_agent: String,
}

impl Default for HtmlArticleCrawlerConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(20),
            max_response_bytes: 8 * 1024 * 1024,
            max_articles: 20,
            max_concurrency: 4,
            max_per_host_concurrency: 2,
            min_content_chars: 200,
            allow_private_networks: false,
            user_agent: DEFAULT_PUBLIC_FETCH_USER_AGENT.to_owned(),
        }
    }
}

#[derive(Clone)]
pub struct HtmlArticleCrawler {
    client: Client,
    config: HtmlArticleCrawlerConfig,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RecipeLink {
    url: Url,
    title_hint: Option<String>,
    published_at_hint: Option<DateTime<Utc>>,
    document_url: Option<Url>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HtmlArticleCrawlReport {
    pub fetched_at: DateTime<Utc>,
    pub items: Vec<RawCrawlItem>,
    pub failures: Vec<ArticleFetchFailure>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArticleFetchFailure {
    pub url: Url,
    pub reason: String,
    pub retryable: bool,
    pub error: String,
}

#[derive(Clone, Debug)]
pub struct HtmlRecipeCrawlerConfig {
    pub request_timeout: Duration,
    pub max_response_bytes: usize,
    pub max_articles: usize,
    pub max_concurrency: usize,
    pub max_per_host_concurrency: usize,
    pub min_content_chars: usize,
    pub allow_private_networks: bool,
    pub user_agent: String,
}

impl Default for HtmlRecipeCrawlerConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(20),
            max_response_bytes: 8 * 1024 * 1024,
            max_articles: 50,
            max_concurrency: 4,
            max_per_host_concurrency: 2,
            min_content_chars: 200,
            allow_private_networks: false,
            user_agent: DEFAULT_PUBLIC_FETCH_USER_AGENT.to_owned(),
        }
    }
}

#[derive(Clone)]
pub struct HtmlRecipeCrawler {
    client: Client,
    article_crawler: HtmlArticleCrawler,
    config: HtmlRecipeCrawlerConfig,
}

#[derive(Clone, Debug, Default)]
pub struct HtmlRecipeCrawlCache {
    listing_pages: HashMap<String, (Url, String)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HtmlRecipeCrawlReport {
    pub fetched_at: DateTime<Utc>,
    pub publication_final_url: Url,
    pub discovered_url_count: usize,
    pub accepted_item_count: usize,
    pub distinct_title_count: usize,
    pub distinct_content_count: usize,
    pub rejected_url_count: usize,
    pub acceptance_ratio_bps: u16,
    pub latest_published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub dated_item_count: usize,
    #[serde(default)]
    pub publication_date_coverage_complete: bool,
    pub structure_fingerprint: String,
    pub correctness_reasons: Vec<String>,
    pub content_stale: bool,
    pub items: Vec<RawCrawlItem>,
    pub failures: Vec<ArticleFetchFailure>,
}

impl HtmlRecipeCrawlReport {
    pub fn correctness_passed(&self) -> bool {
        self.correctness_reasons.is_empty()
    }
}

fn sanitized_content_text(item: &RawCrawlItem) -> Option<String> {
    item.body_html
        .as_deref()
        .or(item.summary_html.as_deref())
        .map(|content| {
            process_html(
                content,
                &ContentProcessOptions {
                    base_url: Some(item.url.as_str().to_owned()),
                    keep_images: false,
                },
            )
            .text
        })
        .filter(|content| !content.is_empty())
}

pub fn distinct_sanitized_content_count(items: &[RawCrawlItem]) -> usize {
    items
        .iter()
        .filter_map(sanitized_content_text)
        .collect::<HashSet<_>>()
        .len()
}

fn reject_repeated_sanitized_content(
    items: &mut Vec<RawCrawlItem>,
    failures: &mut Vec<ArticleFetchFailure>,
) {
    let rejected_indexes = repeated_sanitized_content_indexes(items);
    if rejected_indexes.is_empty() {
        return;
    }

    let mut retained = Vec::with_capacity(items.len() - rejected_indexes.len());
    for (index, item) in items.drain(..).enumerate() {
        if rejected_indexes.contains(&index) {
            failures.push(ArticleFetchFailure {
                url: item.url,
                reason: "repeated_sanitized_content".to_owned(),
                retryable: false,
                error: "the sanitized body is reused by another URL with a different title"
                    .to_owned(),
            });
        } else {
            retained.push(item);
        }
    }
    *items = retained;
}

fn repeated_sanitized_content_indexes(items: &[RawCrawlItem]) -> HashSet<usize> {
    let mut content_groups = HashMap::<String, Vec<usize>>::new();
    for (index, item) in items.iter().enumerate() {
        if let Some(content) = sanitized_content_text(item) {
            content_groups.entry(content).or_default().push(index);
        }
    }
    content_groups
        .into_values()
        .filter(|indexes| {
            indexes.len() > 1
                && indexes
                    .iter()
                    .filter_map(|index| items[*index].title.as_deref())
                    .map(normalized_title_key)
                    .collect::<HashSet<_>>()
                    .len()
                    > 1
        })
        .flatten()
        .collect()
}

pub fn repeated_sanitized_content_urls(items: &[RawCrawlItem]) -> HashSet<String> {
    repeated_sanitized_content_indexes(items)
        .into_iter()
        .map(|index| items[index].url.as_str().to_owned())
        .collect()
}

impl HtmlRecipeCrawler {
    pub fn new(config: HtmlRecipeCrawlerConfig) -> Result<Self, RecipeCrawlError> {
        if config.request_timeout.is_zero()
            || config.max_response_bytes == 0
            || config.max_articles == 0
            || config.max_concurrency == 0
            || config.max_per_host_concurrency == 0
            || config.min_content_chars == 0
        {
            return Err(RecipeCrawlError::InvalidConfig(
                "recipe crawler timeouts and limits must be positive".to_owned(),
            ));
        }
        if config.max_per_host_concurrency > config.max_concurrency {
            return Err(RecipeCrawlError::InvalidConfig(
                "recipe crawler per-host concurrency must not exceed global concurrency".to_owned(),
            ));
        }
        let client = Client::builder()
            .connect_timeout(config.request_timeout)
            .timeout(config.request_timeout)
            .redirect(Policy::none())
            .no_proxy()
            .user_agent(&config.user_agent)
            .build()
            .map_err(RecipeCrawlError::Client)?;
        let article_crawler = HtmlArticleCrawler::new(HtmlArticleCrawlerConfig {
            request_timeout: config.request_timeout,
            max_response_bytes: config.max_response_bytes,
            max_articles: config.max_articles,
            max_concurrency: config.max_concurrency,
            max_per_host_concurrency: config.max_per_host_concurrency,
            min_content_chars: config.min_content_chars,
            allow_private_networks: config.allow_private_networks,
            user_agent: config.user_agent.clone(),
        })
        .map_err(RecipeCrawlError::Article)?;
        Ok(Self {
            client,
            article_crawler,
            config,
        })
    }

    pub async fn crawl(
        &self,
        recipe: &CompanyNewsRecipeSpec,
    ) -> Result<HtmlRecipeCrawlReport, RecipeCrawlError> {
        let mut cache = HtmlRecipeCrawlCache::default();
        self.crawl_with_cache(recipe, &mut cache).await
    }

    pub async fn crawl_with_cache(
        &self,
        recipe: &CompanyNewsRecipeSpec,
        cache: &mut HtmlRecipeCrawlCache,
    ) -> Result<HtmlRecipeCrawlReport, RecipeCrawlError> {
        recipe
            .validate()
            .map_err(|error| RecipeCrawlError::InvalidConfig(error.to_string()))?;
        if recipe.render_mode != RecipeRenderMode::Http {
            return Err(RecipeCrawlError::UnsupportedRenderMode(recipe.render_mode));
        }
        let (publication_final_url, html) = self
            .fetch_listing_with_cache(&recipe.publication_url, cache)
            .await?;
        let (links, structure_fingerprint) = extract_recipe_links_off_runtime(
            publication_final_url.clone(),
            html,
            recipe.clone(),
            self.config.max_articles,
        )
        .await?;
        let (links, structure_fingerprint) = self
            .expand_archive_collections(links, structure_fingerprint, recipe, cache)
            .await;
        let discovered_url_count = links.len();
        let mut article_report = self
            .article_crawler
            .crawl_recipe_links(&links)
            .await
            .map_err(RecipeCrawlError::Article)?;
        repair_repeated_page_titles(&mut article_report.items);
        reject_repeated_sanitized_content(&mut article_report.items, &mut article_report.failures);
        let now = Utc::now();
        let future_cutoff =
            now + chrono::Duration::seconds(i64::from(recipe.correctness.max_future_skew_seconds));
        let mut accepted = Vec::new();
        for item in article_report.items.drain(..) {
            if let Some(out_of_scope_url) =
                [&item.url, item.canonical_url.as_ref().unwrap_or(&item.url)]
                    .into_iter()
                    .find(|url| !recipe_url_allowed(url, recipe))
            {
                let out_of_scope_url = out_of_scope_url.as_str().to_owned();
                article_report.failures.push(ArticleFetchFailure {
                    url: item.url.clone(),
                    reason: "article_outside_recipe_scope".to_owned(),
                    retryable: false,
                    error: format!(
                        "the final or canonical article URL escaped the recipe boundary: \
                         {out_of_scope_url}"
                    ),
                });
                continue;
            }
            if raw_item_matches_publication(&item, &recipe.publication_url, &publication_final_url)
            {
                article_report.failures.push(ArticleFetchFailure {
                    url: item.url,
                    reason: "publication_page_returned_as_article".to_owned(),
                    retryable: false,
                    error: "the discovered URL resolved to the publication listing page".to_owned(),
                });
                continue;
            }
            let title_chars = item
                .title
                .as_deref()
                .map(str::chars)
                .map(Iterator::count)
                .unwrap_or(0);
            let title_valid = title_chars >= usize::from(recipe.correctness.min_title_chars)
                && title_chars <= usize::from(recipe.correctness.max_title_chars);
            if !title_valid {
                article_report.failures.push(ArticleFetchFailure {
                    url: item.url,
                    reason: "title_length_outside_recipe_contract".to_owned(),
                    retryable: false,
                    error: format!(
                        "title length {title_chars} is outside {}..={}",
                        recipe.correctness.min_title_chars, recipe.correctness.max_title_chars
                    ),
                });
            } else if item
                .published_at
                .is_some_and(|published_at| published_at > future_cutoff)
            {
                article_report.failures.push(ArticleFetchFailure {
                    url: item.url,
                    reason: "future_publication_date".to_owned(),
                    retryable: false,
                    error: "publication timestamp exceeds the recipe future-skew allowance"
                        .to_owned(),
                });
            } else {
                accepted.push(item);
            }
        }
        article_report.items = accepted;
        article_report
            .failures
            .sort_by(|left, right| left.url.as_str().cmp(right.url.as_str()));
        let accepted_item_count = article_report.items.len();
        let distinct_title_count = article_report
            .items
            .iter()
            .filter_map(|item| item.title.as_deref())
            .map(normalized_title_key)
            .collect::<HashSet<_>>()
            .len();
        let distinct_content_count = distinct_sanitized_content_count(&article_report.items);
        let rejected_url_count = article_report.failures.len();
        let acceptance_ratio_bps = if discovered_url_count == 0 {
            0
        } else {
            u16::try_from(
                accepted_item_count
                    .saturating_mul(10_000)
                    .checked_div(discovered_url_count)
                    .unwrap_or(0),
            )
            .unwrap_or(10_000)
        };
        let latest_published_at = article_report
            .items
            .iter()
            .filter_map(|item| item.published_at)
            .max();
        let dated_item_count = article_report
            .items
            .iter()
            .filter(|item| item.published_at.is_some())
            .count();
        let publication_date_coverage_complete =
            accepted_item_count > 0 && dated_item_count == accepted_item_count;
        let mut correctness_reasons = Vec::new();
        if discovered_url_count
            < usize::try_from(recipe.correctness.min_discovered_items).unwrap_or(usize::MAX)
        {
            correctness_reasons.push("discovered_items_below_minimum".to_owned());
        }
        if accepted_item_count
            < usize::try_from(recipe.correctness.min_accepted_items).unwrap_or(usize::MAX)
        {
            correctness_reasons.push("accepted_items_below_minimum".to_owned());
        }
        if acceptance_ratio_bps < recipe.correctness.min_acceptance_ratio_bps {
            correctness_reasons.push("acceptance_ratio_below_minimum".to_owned());
        }
        if accepted_item_count >= 3 && distinct_title_count.saturating_mul(2) < accepted_item_count
        {
            correctness_reasons.push("title_diversity_below_minimum".to_owned());
        }
        if accepted_item_count >= 3
            && distinct_content_count.saturating_mul(2) < accepted_item_count
        {
            correctness_reasons.push("content_diversity_below_minimum".to_owned());
        }
        let content_stale = publication_date_coverage_complete
            && latest_published_at.is_some_and(|published_at| {
                published_at
                    < now
                        - chrono::Duration::seconds(i64::from(
                            recipe.freshness.content_stale_after_seconds,
                        ))
            });
        Ok(HtmlRecipeCrawlReport {
            fetched_at: article_report.fetched_at,
            publication_final_url,
            discovered_url_count,
            accepted_item_count,
            distinct_title_count,
            distinct_content_count,
            rejected_url_count,
            acceptance_ratio_bps,
            latest_published_at,
            dated_item_count,
            publication_date_coverage_complete,
            structure_fingerprint,
            correctness_reasons,
            content_stale,
            items: article_report.items,
            failures: article_report.failures,
        })
    }

    async fn expand_archive_collections(
        &self,
        links: Vec<RecipeLink>,
        structure_fingerprint: String,
        recipe: &CompanyNewsRecipeSpec,
        cache: &mut HtmlRecipeCrawlCache,
    ) -> (Vec<RecipeLink>, String) {
        const MAX_ARCHIVE_COLLECTIONS: usize = 2;

        let archive_links = links
            .iter()
            .filter(|link| {
                is_year_archive_collection(
                    &link.url,
                    link.title_hint.as_deref().unwrap_or_default(),
                )
            })
            .take(MAX_ARCHIVE_COLLECTIONS)
            .cloned()
            .collect::<Vec<_>>();
        if archive_links.is_empty() {
            return (links, structure_fingerprint);
        }

        let mut expanded_links = Vec::new();
        let mut expanded_fingerprints = Vec::new();
        for archive in &archive_links {
            let Ok((archive_final_url, archive_html)) =
                self.fetch_listing_with_cache(&archive.url, cache).await
            else {
                continue;
            };
            let Ok((archive_items, archive_fingerprint)) = extract_recipe_links_off_runtime(
                archive_final_url,
                archive_html,
                recipe.clone(),
                self.config.max_articles,
            )
            .await
            else {
                continue;
            };
            let archive_items = archive_items
                .into_iter()
                .filter(|link| {
                    !is_year_archive_collection(
                        &link.url,
                        link.title_hint.as_deref().unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>();
            if archive_items.is_empty() {
                continue;
            }
            expanded_links.extend(archive_items);
            expanded_fingerprints.push(archive_fingerprint);
        }
        if expanded_links.is_empty() {
            return (links, structure_fingerprint);
        }

        // Preserve direct listing articles ahead of archive contents, then add
        // the newest bounded archive pages in listing order. This keeps a
        // current mixed newsroom fresh while still unwrapping year indexes.
        let mut combined = links
            .into_iter()
            .filter(|link| {
                !is_year_archive_collection(
                    &link.url,
                    link.title_hint.as_deref().unwrap_or_default(),
                )
            })
            .chain(expanded_links)
            .collect::<Vec<_>>();
        let mut seen = HashSet::new();
        combined.retain(|link| seen.insert(link.url.as_str().to_owned()));
        combined.truncate(self.config.max_articles);

        expanded_fingerprints.sort();
        let expanded_structure_fingerprint = sha256_hex(
            format!(
                "{structure_fingerprint}\n{}",
                expanded_fingerprints.join("\n")
            )
            .as_bytes(),
        );
        (combined, expanded_structure_fingerprint)
    }

    async fn fetch_listing_with_cache(
        &self,
        requested_url: &Url,
        cache: &mut HtmlRecipeCrawlCache,
    ) -> Result<(Url, String), RecipeCrawlError> {
        let key = requested_url.as_str();
        if let Some(cached) = cache.listing_pages.get(key) {
            return Ok(cached.clone());
        }
        let fetched = self.fetch_listing(requested_url).await?;
        cache.listing_pages.insert(key.to_owned(), fetched.clone());
        Ok(fetched)
    }

    async fn fetch_listing(&self, requested_url: &Url) -> Result<(Url, String), RecipeCrawlError> {
        let mut current_url = requested_url.clone();
        for redirect_count in 0..=5 {
            validate_article_fetch_url(&current_url).map_err(RecipeCrawlError::Article)?;
            if !self.config.allow_private_networks {
                validate_article_resolved_target(&current_url)
                    .await
                    .map_err(RecipeCrawlError::Article)?;
            }
            let response = self
                .client
                .get(current_url.clone())
                .send()
                .await
                .map_err(|source| {
                    RecipeCrawlError::Article(ArticlePageError::Request {
                        url: current_url.clone(),
                        source,
                    })
                })?;
            if !self.config.allow_private_networks {
                let remote_address = response.remote_addr().ok_or_else(|| {
                    RecipeCrawlError::Article(ArticlePageError::RemoteAddressUnavailable {
                        url: current_url.clone(),
                    })
                })?;
                if !is_public_ip(remote_address.ip()) {
                    return Err(RecipeCrawlError::Article(
                        ArticlePageError::PrivateNetwork {
                            url: current_url,
                            address: remote_address.ip(),
                        },
                    ));
                }
            }
            if response.status().is_redirection() {
                if redirect_count == 5 {
                    return Err(RecipeCrawlError::Article(
                        ArticlePageError::TooManyRedirects { url: current_url },
                    ));
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        RecipeCrawlError::Article(ArticlePageError::InvalidRedirect {
                            url: current_url.clone(),
                        })
                    })?;
                current_url = resolve_article_redirect(&current_url, location)
                    .map_err(RecipeCrawlError::Article)?;
                continue;
            }
            if !response.status().is_success() {
                return Err(RecipeCrawlError::Article(ArticlePageError::HttpStatus {
                    url: current_url,
                    status: response.status(),
                }));
            }
            if response
                .content_length()
                .is_some_and(|length| length > self.config.max_response_bytes as u64)
            {
                return Err(RecipeCrawlError::Article(
                    ArticlePageError::ResponseTooLarge {
                        url: current_url,
                        limit: self.config.max_response_bytes,
                    },
                ));
            }
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !content_type.is_empty()
                && !content_type.contains("text/html")
                && !content_type.contains("application/xhtml+xml")
            {
                return Err(RecipeCrawlError::UnsupportedListingContentType(
                    content_type,
                ));
            }
            let final_url = current_url;
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|source| {
                    RecipeCrawlError::Article(ArticlePageError::Request {
                        url: final_url.clone(),
                        source,
                    })
                })?;
                if body.len().saturating_add(chunk.len()) > self.config.max_response_bytes {
                    return Err(RecipeCrawlError::Article(
                        ArticlePageError::ResponseTooLarge {
                            url: final_url,
                            limit: self.config.max_response_bytes,
                        },
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            let html = String::from_utf8_lossy(&body).into_owned();
            if !html.to_ascii_lowercase().contains("<html") {
                return Err(RecipeCrawlError::InvalidListing(
                    "response does not contain an HTML document".to_owned(),
                ));
            }
            return Ok((final_url, html));
        }
        unreachable!("listing redirect loop returns after success or the sixth redirect")
    }
}

impl HtmlArticleCrawler {
    pub fn new(config: HtmlArticleCrawlerConfig) -> Result<Self, ArticlePageError> {
        if config.request_timeout.is_zero()
            || config.max_response_bytes == 0
            || config.max_articles == 0
            || config.max_concurrency == 0
            || config.max_per_host_concurrency == 0
            || config.min_content_chars == 0
        {
            return Err(ArticlePageError::InvalidConfig(
                "article timeout and limits must be positive".to_owned(),
            ));
        }
        if config.max_per_host_concurrency > config.max_concurrency {
            return Err(ArticlePageError::InvalidConfig(
                "article per-host concurrency must not exceed global concurrency".to_owned(),
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

    pub async fn crawl_urls(
        &self,
        urls: &[Url],
    ) -> Result<HtmlArticleCrawlReport, ArticlePageError> {
        let candidates = urls
            .iter()
            .cloned()
            .map(|url| RecipeLink {
                url,
                title_hint: None,
                published_at_hint: None,
                document_url: None,
            })
            .collect::<Vec<_>>();
        self.crawl_candidates(&candidates).await
    }

    async fn crawl_recipe_links(
        &self,
        links: &[RecipeLink],
    ) -> Result<HtmlArticleCrawlReport, ArticlePageError> {
        self.crawl_candidates(links).await
    }

    async fn crawl_candidates(
        &self,
        candidates: &[RecipeLink],
    ) -> Result<HtmlArticleCrawlReport, ArticlePageError> {
        if candidates.len() > self.config.max_articles {
            return Err(ArticlePageError::TooManyUrls {
                count: candidates.len(),
                limit: self.config.max_articles,
            });
        }
        let fetched_at = Utc::now();
        let (candidates, mut failures) = normalize_double_encoded_article_candidates(candidates);
        let grouped_candidates = group_article_candidates_by_host(&candidates);
        let per_host_concurrency = self
            .config
            .max_concurrency
            .min(self.config.max_per_host_concurrency);
        let host_concurrency = (self.config.max_concurrency / per_host_concurrency).max(1);
        let grouped_results = stream::iter(grouped_candidates)
            .map(|host_candidates| async move {
                stream::iter(host_candidates)
                    .map(|(candidate_index, candidate)| async move {
                        let result = self
                            .fetch_article(
                                &candidate.url,
                                candidate.title_hint.as_deref(),
                                candidate.published_at_hint,
                            )
                            .await;
                        (candidate_index, candidate, result)
                    })
                    .buffered(per_host_concurrency)
                    .collect::<Vec<_>>()
                    .await
            })
            .buffer_unordered(host_concurrency)
            .collect::<Vec<Vec<_>>>()
            .await;
        let mut results = grouped_results.into_iter().flatten().collect::<Vec<_>>();
        results.sort_by_key(|(candidate_index, _, _)| *candidate_index);

        let mut items = Vec::new();
        let mut seen_canonical = HashSet::new();
        for (_, candidate, result) in results {
            let requested_url = candidate.url.clone();
            match result {
                Ok(item) => {
                    let canonical = item
                        .canonical_url
                        .as_ref()
                        .unwrap_or(&item.url)
                        .as_str()
                        .to_owned();
                    if seen_canonical.insert(canonical) {
                        items.push(item);
                    } else {
                        failures.push(ArticleFetchFailure {
                            url: requested_url,
                            reason: "duplicate_canonical_url".to_owned(),
                            retryable: false,
                            error: "another suggested URL resolved to the same canonical article"
                                .to_owned(),
                        });
                    }
                }
                Err(error) => {
                    if article_error_allows_document_listing_fallback(&error)
                        && let Some(item) = document_backed_listing_item(&candidate, fetched_at)
                    {
                        let canonical = item
                            .canonical_url
                            .as_ref()
                            .unwrap_or(&item.url)
                            .as_str()
                            .to_owned();
                        if seen_canonical.insert(canonical) {
                            items.push(item);
                            continue;
                        }
                    }
                    failures.push(ArticleFetchFailure {
                        url: requested_url,
                        reason: error.reason().to_owned(),
                        retryable: error.is_retryable(),
                        error: error.to_string(),
                    });
                }
            }
        }
        items.sort_by(|left, right| left.url.as_str().cmp(right.url.as_str()));
        reject_repeated_sanitized_content(&mut items, &mut failures);
        failures.sort_by(|left, right| left.url.as_str().cmp(right.url.as_str()));
        Ok(HtmlArticleCrawlReport {
            fetched_at,
            items,
            failures,
        })
    }

    async fn fetch_article(
        &self,
        requested_url: &Url,
        title_hint: Option<&str>,
        published_at_hint: Option<DateTime<Utc>>,
    ) -> Result<RawCrawlItem, ArticlePageError> {
        let mut current_url = requested_url.clone();
        for redirect_count in 0..=5 {
            validate_article_fetch_url(&current_url)?;
            if !self.config.allow_private_networks {
                validate_article_resolved_target(&current_url).await?;
            }
            let response = self
                .client
                .get(current_url.clone())
                .send()
                .await
                .map_err(|source| ArticlePageError::Request {
                    url: current_url.clone(),
                    source,
                })?;
            if !self.config.allow_private_networks {
                let remote_address = response.remote_addr().ok_or_else(|| {
                    ArticlePageError::RemoteAddressUnavailable {
                        url: current_url.clone(),
                    }
                })?;
                if !is_public_ip(remote_address.ip()) {
                    return Err(ArticlePageError::PrivateNetwork {
                        url: current_url,
                        address: remote_address.ip(),
                    });
                }
            }
            let status = response.status();
            if status.is_redirection() {
                if redirect_count == 5 {
                    return Err(ArticlePageError::TooManyRedirects { url: current_url });
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| ArticlePageError::InvalidRedirect {
                        url: current_url.clone(),
                    })?;
                current_url = resolve_article_redirect(&current_url, location)?;
                continue;
            }
            if !status.is_success() {
                return Err(ArticlePageError::HttpStatus {
                    url: current_url,
                    status,
                });
            }
            if response
                .content_length()
                .is_some_and(|length| length > self.config.max_response_bytes as u64)
            {
                return Err(ArticlePageError::ResponseTooLarge {
                    url: current_url,
                    limit: self.config.max_response_bytes,
                });
            }
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let final_url = current_url;
            let mut body = Vec::new();
            let mut response_stream = response.bytes_stream();
            while let Some(chunk) = response_stream.next().await {
                let chunk = chunk.map_err(|source| ArticlePageError::Request {
                    url: final_url.clone(),
                    source,
                })?;
                if body.len().saturating_add(chunk.len()) > self.config.max_response_bytes {
                    return Err(ArticlePageError::ResponseTooLarge {
                        url: final_url,
                        limit: self.config.max_response_bytes,
                    });
                }
                body.extend_from_slice(&chunk);
            }
            let extraction = extract_article_with_hints(
                requested_url,
                final_url.clone(),
                content_type.as_deref(),
                &body,
                self.config.min_content_chars,
                title_hint,
                published_at_hint,
            );
            match extraction {
                Ok(item) => return Ok(item),
                Err(original_error) if article_error_allows_framework_fallback(&original_error) => {
                    if let Some(next_data) = augment_html_with_next_data(
                        &body,
                        &final_url,
                        self.config.min_content_chars,
                        self.config.max_response_bytes,
                    ) && let Ok(mut item) = extract_article_with_hints(
                        requested_url,
                        final_url.clone(),
                        content_type.as_deref(),
                        &next_data.body,
                        self.config.min_content_chars,
                        title_hint,
                        published_at_hint,
                    ) {
                        if let Some(payload) = item.payload.as_object_mut() {
                            payload.insert(
                                "framework_fallback".to_owned(),
                                json!("next-data-json.v1"),
                            );
                            payload.insert(
                                "framework_embedded_slug".to_owned(),
                                json!(next_data.slug),
                            );
                            payload.insert(
                                "framework_embedded_identity_field".to_owned(),
                                json!(next_data.identity_field),
                            );
                            payload.insert(
                                "framework_embedded_title_field".to_owned(),
                                json!(next_data.title_field),
                            );
                            payload.insert(
                                "framework_embedded_content_field".to_owned(),
                                json!(next_data.content_field),
                            );
                            if let Some(field) = next_data.published_at_field {
                                payload.insert(
                                    "framework_embedded_published_at_field".to_owned(),
                                    json!(field),
                                );
                            }
                            if next_data
                                .published_at
                                .is_some_and(|published_at| item.published_at == Some(published_at))
                            {
                                payload.insert(
                                    "published_at_source".to_owned(),
                                    json!("next_data_json"),
                                );
                            }
                        }
                        return Ok(item);
                    }
                    if let Some(data_url) = extract_sveltekit_data_url(&final_url, &body)
                        && let Ok(data_body) = self.fetch_framework_resource(&data_url).await
                        && let Some(sveltekit_data) = augment_html_with_sveltekit_data(
                            &body,
                            &data_body,
                            &final_url,
                            self.config.min_content_chars,
                            self.config.max_response_bytes,
                        )
                        && let Ok(mut item) = extract_article_with_hints(
                            requested_url,
                            final_url.clone(),
                            content_type.as_deref(),
                            &sveltekit_data.body,
                            self.config.min_content_chars,
                            title_hint,
                            published_at_hint,
                        )
                    {
                        if let Some(payload) = item.payload.as_object_mut() {
                            payload.insert(
                                "framework_fallback".to_owned(),
                                json!("sveltekit-data-json.v1"),
                            );
                            payload.insert("framework_resource_url".to_owned(), json!(data_url));
                            payload.insert(
                                "framework_embedded_identity_field".to_owned(),
                                json!(sveltekit_data.identity_field),
                            );
                            payload.insert(
                                "framework_embedded_title_field".to_owned(),
                                json!(sveltekit_data.title_field),
                            );
                            payload.insert(
                                "framework_embedded_content_field".to_owned(),
                                json!(sveltekit_data.content_field),
                            );
                            if let Some(field) = sveltekit_data.published_at_field {
                                payload.insert(
                                    "framework_embedded_published_at_field".to_owned(),
                                    json!(field),
                                );
                            }
                            if sveltekit_data
                                .published_at
                                .is_some_and(|published_at| item.published_at == Some(published_at))
                            {
                                payload.insert(
                                    "published_at_source".to_owned(),
                                    json!("sveltekit_data_json"),
                                );
                            }
                        }
                        return Ok(item);
                    }
                    let Some(model_url) = extract_aem_model_url(&final_url, &body)? else {
                        return Err(original_error);
                    };
                    let model_body = self.fetch_framework_resource(&model_url).await?;
                    let Some(augmented_body) = augment_html_with_aem_model(
                        &body,
                        &model_body,
                        self.config.max_response_bytes,
                    ) else {
                        return Err(original_error);
                    };
                    let mut item = extract_article_with_hints(
                        requested_url,
                        final_url,
                        content_type.as_deref(),
                        &augmented_body,
                        self.config.min_content_chars,
                        title_hint,
                        published_at_hint,
                    )?;
                    if let Some(payload) = item.payload.as_object_mut() {
                        payload.insert("framework_fallback".to_owned(), json!("aem-model-json.v1"));
                        payload.insert("framework_resource_url".to_owned(), json!(model_url));
                    }
                    return Ok(item);
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("redirect loop returns after success or the sixth redirect")
    }

    async fn fetch_framework_resource(
        &self,
        requested_url: &Url,
    ) -> Result<Vec<u8>, ArticlePageError> {
        let mut current_url = requested_url.clone();
        for redirect_count in 0..=5 {
            validate_article_fetch_url(&current_url)?;
            if !same_url_origin(requested_url, &current_url) {
                return Err(ArticlePageError::InvalidRedirect { url: current_url });
            }
            if !self.config.allow_private_networks {
                validate_article_resolved_target(&current_url).await?;
            }
            let response = self
                .client
                .get(current_url.clone())
                .send()
                .await
                .map_err(|source| ArticlePageError::Request {
                    url: current_url.clone(),
                    source,
                })?;
            if !self.config.allow_private_networks {
                let remote_address = response.remote_addr().ok_or_else(|| {
                    ArticlePageError::RemoteAddressUnavailable {
                        url: current_url.clone(),
                    }
                })?;
                if !is_public_ip(remote_address.ip()) {
                    return Err(ArticlePageError::PrivateNetwork {
                        url: current_url,
                        address: remote_address.ip(),
                    });
                }
            }
            let status = response.status();
            if status.is_redirection() {
                if redirect_count == 5 {
                    return Err(ArticlePageError::TooManyRedirects { url: current_url });
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| ArticlePageError::InvalidRedirect {
                        url: current_url.clone(),
                    })?;
                current_url = resolve_article_redirect(&current_url, location)?;
                continue;
            }
            if !status.is_success() {
                return Err(ArticlePageError::HttpStatus {
                    url: current_url,
                    status,
                });
            }
            if response
                .content_length()
                .is_some_and(|length| length > self.config.max_response_bytes as u64)
            {
                return Err(ArticlePageError::ResponseTooLarge {
                    url: current_url,
                    limit: self.config.max_response_bytes,
                });
            }
            let final_url = current_url;
            let mut body = Vec::new();
            let mut response_stream = response.bytes_stream();
            while let Some(chunk) = response_stream.next().await {
                let chunk = chunk.map_err(|source| ArticlePageError::Request {
                    url: final_url.clone(),
                    source,
                })?;
                if body.len().saturating_add(chunk.len()) > self.config.max_response_bytes {
                    return Err(ArticlePageError::ResponseTooLarge {
                        url: final_url,
                        limit: self.config.max_response_bytes,
                    });
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(body);
        }
        unreachable!("framework resource redirect loop returns after success or the sixth redirect")
    }
}

fn article_error_allows_document_listing_fallback(error: &ArticlePageError) -> bool {
    matches!(
        error,
        ArticlePageError::UnsupportedContent { .. }
            | ArticlePageError::MissingArticleSignal { .. }
            | ArticlePageError::MissingArticleBody { .. }
            | ArticlePageError::InsufficientContent { .. }
    )
}

fn document_backed_listing_item(
    candidate: &RecipeLink,
    fetched_at: DateTime<Utc>,
) -> Option<RawCrawlItem> {
    let document_url = candidate.document_url.as_ref()?.clone();
    let title = candidate
        .title_hint
        .as_deref()
        .map(normalize_article_title)
        .filter(|title| is_usable_article_title(title))?;
    let requested_url = candidate.url.clone();
    let body_html = format!(
        "<p><a href=\"{}\">{}</a></p>",
        html_escape::encode_double_quoted_attribute(document_url.as_str()),
        html_escape::encode_text(&title),
    );
    Some(RawCrawlItem {
        source_item_key: sha256_hex(requested_url.as_str().as_bytes()),
        external_id: Some(requested_url.as_str().to_owned()),
        url: document_url.clone(),
        canonical_url: Some(document_url.clone()),
        title: Some(title.clone()),
        summary_html: None,
        body_html: Some(body_html),
        published_at: candidate.published_at_hint,
        payload: json!({
            "extraction_contract": "official-listing-document.v1",
            "requested_article_url": requested_url,
            "document_url": document_url,
            "title_source": "listing_anchor",
            "listing_title_hint": title,
            "listing_published_at_hint": candidate.published_at_hint,
            "published_at_source": if candidate.published_at_hint.is_some() {
                "listing_card"
            } else {
                "unknown"
            },
            "document_backed": true,
            "fetched_at": fetched_at,
        }),
    })
}

fn article_error_allows_framework_fallback(error: &ArticlePageError) -> bool {
    matches!(
        error,
        ArticlePageError::MissingArticleSignal { .. }
            | ArticlePageError::MissingArticleBody { .. }
            | ArticlePageError::InsufficientContent { .. }
    )
}

#[derive(Debug)]
struct NextDataAugmentation {
    body: Vec<u8>,
    slug: String,
    identity_field: &'static str,
    title_field: &'static str,
    content_field: &'static str,
    published_at: Option<DateTime<Utc>>,
    published_at_field: Option<&'static str>,
}

#[derive(Debug)]
struct NextDataArticle {
    title: String,
    body_html: String,
    identity_field: &'static str,
    title_field: &'static str,
    content_field: &'static str,
    published_at: Option<DateTime<Utc>>,
    published_at_field: Option<&'static str>,
    content_chars: usize,
}

fn augment_html_with_next_data(
    html: &[u8],
    article_url: &Url,
    min_content_chars: usize,
    max_response_bytes: usize,
) -> Option<NextDataAugmentation> {
    let slug = semantic_path_segments(article_url).last()?.clone();
    let document = Html::parse_document(&String::from_utf8_lossy(html));
    let next_data_selector =
        Selector::parse("script#__NEXT_DATA__[type='application/json']").ok()?;
    let mut article = None;
    let mut visited_nodes = 0;
    for element in document.select(&next_data_selector) {
        let Ok(value) = serde_json::from_str::<Value>(&element.text().collect::<String>()) else {
            continue;
        };
        collect_next_data_article(
            &value,
            article_url,
            &slug,
            min_content_chars,
            0,
            &mut visited_nodes,
            &mut article,
        );
    }
    let article = article?;
    let time = article
        .published_at
        .map_or_else(String::new, |published_at| {
            format!(
                "<time datetime=\"{}\"></time>",
                html_escape::encode_double_quoted_attribute(&published_at.to_rfc3339())
            )
        });
    let framework_article = format!(
        "<article data-company-feed-framework=\"next-data-json\"><h1>{}</h1>{time}<div itemprop=\"articleBody\">{}</div></article>",
        html_escape::encode_text(&article.title),
        article.body_html,
    );
    if html
        .len()
        .saturating_add(framework_article.len())
        .saturating_add(1)
        > max_response_bytes
    {
        return None;
    }
    let original = String::from_utf8_lossy(html);
    let lower = original.to_ascii_lowercase();
    let augmented = if let Some(body_end) = lower.rfind("</body>") {
        format!(
            "{}{}{}",
            &original[..body_end],
            framework_article,
            &original[body_end..]
        )
    } else {
        format!("{original}{framework_article}")
    };
    Some(NextDataAugmentation {
        body: augmented.into_bytes(),
        slug,
        identity_field: article.identity_field,
        title_field: article.title_field,
        content_field: article.content_field,
        published_at: article.published_at,
        published_at_field: article.published_at_field,
    })
}

#[allow(clippy::too_many_arguments)]
fn collect_next_data_article(
    value: &Value,
    article_url: &Url,
    slug: &str,
    min_content_chars: usize,
    depth: usize,
    visited_nodes: &mut usize,
    best: &mut Option<NextDataArticle>,
) {
    const MAX_NEXT_DATA_DEPTH: usize = 64;
    const MAX_NEXT_DATA_NODES: usize = 50_000;
    if depth > MAX_NEXT_DATA_DEPTH || *visited_nodes >= MAX_NEXT_DATA_NODES {
        return;
    }
    *visited_nodes += 1;
    match value {
        Value::Object(object) => {
            if let Some(candidate) =
                next_data_article_from_object(object, article_url, slug, min_content_chars)
                && best
                    .as_ref()
                    .is_none_or(|current| candidate.content_chars > current.content_chars)
            {
                *best = Some(candidate);
            }
            for child in object.values() {
                collect_next_data_article(
                    child,
                    article_url,
                    slug,
                    min_content_chars,
                    depth + 1,
                    visited_nodes,
                    best,
                );
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_next_data_article(
                    child,
                    article_url,
                    slug,
                    min_content_chars,
                    depth + 1,
                    visited_nodes,
                    best,
                );
            }
        }
        _ => {}
    }
}

fn next_data_article_from_object(
    object: &serde_json::Map<String, Value>,
    article_url: &Url,
    slug: &str,
    min_content_chars: usize,
) -> Option<NextDataArticle> {
    let identity_field = ["slug", "id", "path", "url"].into_iter().find(|field| {
        object
            .get(*field)
            .is_some_and(|value| next_data_identity_matches(value, field, article_url, slug))
    })?;
    let (title_field, title) = ["title", "headline", "name", "meta_title", "metaTitle"]
        .into_iter()
        .find_map(|field| {
            object
                .get(field)
                .and_then(Value::as_str)
                .map(normalize_article_title)
                .filter(|title| is_usable_article_title(title))
                .map(|title| (field, title))
        })?;
    let (content_field, body_html, content_chars) = [
        "content",
        "body",
        "html",
        "body_html",
        "bodyHtml",
        "body_content",
        "bodyContent",
        "article_content",
        "articleContent",
        "content_html",
        "contentHtml",
    ]
    .into_iter()
    .filter_map(|field| {
        let body_html = object.get(field).and_then(Value::as_str)?.trim();
        if !has_rich_html_structure(body_html) {
            return None;
        }
        let content_chars = process_html(
            body_html,
            &ContentProcessOptions {
                base_url: Some(article_url.as_str().to_owned()),
                keep_images: false,
            },
        )
        .text
        .chars()
        .count();
        (content_chars >= min_content_chars).then_some((field, body_html, content_chars))
    })
    .max_by_key(|(_, _, content_chars)| *content_chars)?;
    let (published_at_field, published_at) = [
        "date_view",
        "dateView",
        "published_at",
        "publishedAt",
        "datePublished",
        "publish_date",
        "publishDate",
        "created_at",
        "createdAt",
    ]
    .into_iter()
    .find_map(|field| {
        object
            .get(field)
            .and_then(Value::as_str)
            .and_then(parse_article_datetime)
            .map(|published_at| (field, published_at))
    })
    .map_or((None, None), |(field, published_at)| {
        (Some(field), Some(published_at))
    });
    Some(NextDataArticle {
        title,
        body_html: body_html.to_owned(),
        identity_field,
        title_field,
        content_field,
        published_at,
        published_at_field,
        content_chars,
    })
}

fn next_data_identity_matches(value: &Value, field: &str, article_url: &Url, slug: &str) -> bool {
    if field == "id"
        && let Some(value) = value.as_u64()
    {
        return slug.parse::<u64>().is_ok_and(|slug| slug == value);
    }
    let Some(value) = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    if matches!(field, "slug" | "id") {
        return value.trim_matches('/').eq_ignore_ascii_case(slug);
    }
    let embedded_url = match Url::parse(value) {
        Ok(url) if same_url_origin(article_url, &url) => Some(url),
        Ok(_) => None,
        Err(_) => article_url.join(value).ok(),
    };
    embedded_url
        .and_then(|url| semantic_path_segments(&url).last().cloned())
        .is_some_and(|value| value.eq_ignore_ascii_case(slug))
}

fn has_rich_html_structure(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    [
        "p",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "li",
        "ul",
        "ol",
        "blockquote",
        "pre",
        "table",
    ]
    .into_iter()
    .any(|tag| {
        let marker = format!("<{tag}");
        value.match_indices(&marker).any(|(index, _)| {
            value[index + marker.len()..]
                .chars()
                .next()
                .is_some_and(|character| {
                    character == '>' || character == '/' || character.is_ascii_whitespace()
                })
        })
    })
}

#[derive(Debug)]
struct SvelteKitDataAugmentation {
    body: Vec<u8>,
    identity_field: &'static str,
    title_field: &'static str,
    content_field: String,
    published_at: Option<DateTime<Utc>>,
    published_at_field: Option<&'static str>,
}

#[derive(Debug)]
struct SvelteKitDataArticle {
    title: String,
    body_html: String,
    identity_field: &'static str,
    title_field: &'static str,
    content_field: String,
    published_at: Option<DateTime<Utc>>,
    published_at_field: Option<&'static str>,
    content_chars: usize,
}

#[derive(Debug)]
struct SvelteKitRichContent {
    field: String,
    html: String,
    chars: usize,
}

fn extract_sveltekit_data_url(article_url: &Url, html: &[u8]) -> Option<Url> {
    let html = String::from_utf8_lossy(html).to_ascii_lowercase();
    if !html.contains("__sveltekit_") || !html.contains("/_app/immutable/") {
        return None;
    }
    let path = article_url.path().trim_end_matches('/');
    let data_path = if path.is_empty() {
        "/__data.json".to_owned()
    } else {
        format!("{path}/__data.json")
    };
    let mut data_url = article_url.clone();
    data_url.set_path(&data_path);
    data_url.set_query(None);
    data_url.set_fragment(None);
    Some(data_url)
}

fn augment_html_with_sveltekit_data(
    html: &[u8],
    data: &[u8],
    article_url: &Url,
    min_content_chars: usize,
    max_response_bytes: usize,
) -> Option<SvelteKitDataAugmentation> {
    let data = serde_json::from_slice::<Value>(data).ok()?;
    let mut article = None;
    let mut visited_nodes = 0_usize;
    for node in data.get("nodes")?.as_array()? {
        let Some(pool) = node.get("data").and_then(Value::as_array) else {
            continue;
        };
        if pool.len() > 50_000 {
            continue;
        }
        collect_sveltekit_data_article(
            pool,
            article_url,
            min_content_chars,
            &mut visited_nodes,
            &mut article,
        );
    }
    let article = article?;
    let time = article
        .published_at
        .map_or_else(String::new, |published_at| {
            format!(
                "<time datetime=\"{}\"></time>",
                html_escape::encode_double_quoted_attribute(&published_at.to_rfc3339())
            )
        });
    let framework_article = format!(
        "<article data-company-feed-framework=\"sveltekit-data-json\"><h1>{}</h1>{time}<div itemprop=\"articleBody\">{}</div></article>",
        html_escape::encode_text(&article.title),
        article.body_html,
    );
    if html
        .len()
        .saturating_add(framework_article.len())
        .saturating_add(1)
        > max_response_bytes
    {
        return None;
    }
    let original = String::from_utf8_lossy(html);
    let lower = original.to_ascii_lowercase();
    let augmented = if let Some(body_end) = lower.rfind("</body>") {
        format!(
            "{}{}{}",
            &original[..body_end],
            framework_article,
            &original[body_end..]
        )
    } else {
        format!("{original}{framework_article}")
    };
    Some(SvelteKitDataAugmentation {
        body: augmented.into_bytes(),
        identity_field: article.identity_field,
        title_field: article.title_field,
        content_field: article.content_field,
        published_at: article.published_at,
        published_at_field: article.published_at_field,
    })
}

fn collect_sveltekit_data_article(
    pool: &[Value],
    article_url: &Url,
    min_content_chars: usize,
    visited_nodes: &mut usize,
    best: &mut Option<SvelteKitDataArticle>,
) {
    const MAX_SVELTEKIT_DATA_NODES: usize = 50_000;
    for value in pool {
        if *visited_nodes >= MAX_SVELTEKIT_DATA_NODES {
            return;
        }
        *visited_nodes += 1;
        let Value::Object(object) = value else {
            continue;
        };
        if let Some(candidate) =
            sveltekit_data_article_from_object(pool, object, article_url, min_content_chars)
            && best
                .as_ref()
                .is_none_or(|current| candidate.content_chars > current.content_chars)
        {
            *best = Some(candidate);
        }
    }
}

fn sveltekit_data_article_from_object(
    pool: &[Value],
    object: &serde_json::Map<String, Value>,
    article_url: &Url,
    min_content_chars: usize,
) -> Option<SvelteKitDataArticle> {
    let identity_field = ["slug", "id", "path", "url"].into_iter().find(|field| {
        object
            .get(*field)
            .and_then(|reference| sveltekit_resolved_value(pool, reference))
            .is_some_and(|value| sveltekit_data_identity_matches(value, field, article_url))
    })?;
    let (title_field, title) = ["title", "headline", "name", "meta_title", "metaTitle"]
        .into_iter()
        .find_map(|field| {
            object
                .get(field)
                .and_then(|reference| sveltekit_resolved_value(pool, reference))
                .and_then(Value::as_str)
                .map(normalize_article_title)
                .filter(|title| is_usable_article_title(title))
                .map(|title| (field, title))
        })?;
    let mut visited_composites = HashSet::new();
    let mut visited_nodes = 0_usize;
    let mut content = None;
    for (field, reference) in object {
        collect_sveltekit_rich_content(
            pool,
            reference,
            field,
            article_url,
            min_content_chars,
            0,
            &mut visited_nodes,
            &mut visited_composites,
            &mut content,
        );
    }
    let content = content?;
    let (published_at_field, published_at) = [
        "publishedDate",
        "published_at",
        "publishedAt",
        "datePublished",
        "publish_date",
        "publishDate",
        "created_at",
        "createdAt",
        "firstPublished",
    ]
    .into_iter()
    .find_map(|field| {
        object
            .get(field)
            .and_then(|reference| sveltekit_resolved_value(pool, reference))
            .and_then(parse_sveltekit_datetime)
            .map(|published_at| (field, published_at))
    })
    .map_or((None, None), |(field, published_at)| {
        (Some(field), Some(published_at))
    });
    Some(SvelteKitDataArticle {
        title,
        body_html: content.html,
        identity_field,
        title_field,
        content_field: content.field,
        published_at,
        published_at_field,
        content_chars: content.chars,
    })
}

#[allow(clippy::too_many_arguments)]
fn collect_sveltekit_rich_content(
    pool: &[Value],
    reference: &Value,
    field: &str,
    article_url: &Url,
    min_content_chars: usize,
    depth: usize,
    visited_nodes: &mut usize,
    visited_composites: &mut HashSet<usize>,
    best: &mut Option<SvelteKitRichContent>,
) {
    const MAX_SVELTEKIT_DATA_DEPTH: usize = 64;
    const MAX_SVELTEKIT_DATA_NODES: usize = 50_000;
    if depth > MAX_SVELTEKIT_DATA_DEPTH || *visited_nodes >= MAX_SVELTEKIT_DATA_NODES {
        return;
    }
    let Some(index) = sveltekit_reference_index(reference) else {
        return;
    };
    let Some(value) = pool.get(index) else {
        return;
    };
    *visited_nodes += 1;
    match value {
        Value::String(html) if sveltekit_rich_content_field(field) => {
            let html = html.trim();
            if !has_rich_html_structure(html) {
                return;
            }
            let chars = process_html(
                html,
                &ContentProcessOptions {
                    base_url: Some(article_url.as_str().to_owned()),
                    keep_images: false,
                },
            )
            .text
            .chars()
            .count();
            if chars >= min_content_chars
                && best.as_ref().is_none_or(|current| chars > current.chars)
            {
                *best = Some(SvelteKitRichContent {
                    field: field.to_owned(),
                    html: html.to_owned(),
                    chars,
                });
            }
        }
        Value::Object(object) => {
            if !visited_composites.insert(index) {
                return;
            }
            for (child_field, child_reference) in object {
                collect_sveltekit_rich_content(
                    pool,
                    child_reference,
                    child_field,
                    article_url,
                    min_content_chars,
                    depth + 1,
                    visited_nodes,
                    visited_composites,
                    best,
                );
            }
        }
        Value::Array(values) => {
            if !visited_composites.insert(index) {
                return;
            }
            for child_reference in values {
                collect_sveltekit_rich_content(
                    pool,
                    child_reference,
                    field,
                    article_url,
                    min_content_chars,
                    depth + 1,
                    visited_nodes,
                    visited_composites,
                    best,
                );
            }
        }
        _ => {}
    }
}

fn sveltekit_resolved_value<'a>(pool: &'a [Value], reference: &Value) -> Option<&'a Value> {
    pool.get(sveltekit_reference_index(reference)?)
}

fn sveltekit_reference_index(reference: &Value) -> Option<usize> {
    usize::try_from(reference.as_u64()?).ok()
}

fn sveltekit_rich_content_field(field: &str) -> bool {
    matches!(
        field,
        "text"
            | "content"
            | "body"
            | "html"
            | "body_html"
            | "bodyHtml"
            | "body_content"
            | "bodyContent"
            | "article_content"
            | "articleContent"
            | "content_html"
            | "contentHtml"
            | "oldPostsContent"
    )
}

fn sveltekit_data_identity_matches(value: &Value, field: &str, article_url: &Url) -> bool {
    let slug = semantic_path_segments(article_url).last().cloned();
    if field == "id"
        && let Some(value) = value.as_u64()
    {
        return slug
            .as_deref()
            .and_then(|slug| slug.parse::<u64>().ok())
            .is_some_and(|slug| slug == value);
    }
    let Some(value) = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    if matches!(field, "slug" | "id") {
        return slug
            .as_deref()
            .is_some_and(|slug| value.trim_matches('/').eq_ignore_ascii_case(slug));
    }
    let embedded_url = match Url::parse(value) {
        Ok(url) if same_url_origin(article_url, &url) => Some(url),
        Ok(_) => None,
        Err(_) => article_url.join(value).ok(),
    };
    embedded_url.is_some_and(|embedded_url| {
        same_url_origin(article_url, &embedded_url)
            && embedded_url
                .path()
                .trim_end_matches('/')
                .eq_ignore_ascii_case(article_url.path().trim_end_matches('/'))
    })
}

fn parse_sveltekit_datetime(value: &Value) -> Option<DateTime<Utc>> {
    if let Some(value) = value.as_str() {
        return parse_article_datetime(value);
    }
    let timestamp = value.as_i64()?;
    let parsed = if timestamp.unsigned_abs() >= 100_000_000_000 {
        DateTime::<Utc>::from_timestamp_millis(timestamp)
    } else {
        DateTime::<Utc>::from_timestamp(timestamp, 0)
    }?;
    is_plausible_article_datetime(&parsed).then_some(parsed)
}

fn extract_aem_model_url(article_url: &Url, html: &[u8]) -> Result<Option<Url>, ArticlePageError> {
    let document = Html::parse_document(&String::from_utf8_lossy(html));
    let model_selector = selector("link[rel][href]")?;
    Ok(document
        .select(&model_selector)
        .filter(|element| {
            element
                .value()
                .attr("rel")
                .is_some_and(|rel| rel.split_ascii_whitespace().any(|part| part == "preload"))
                && element
                    .value()
                    .attr("as")
                    .is_some_and(|value| value.eq_ignore_ascii_case("fetch"))
        })
        .filter_map(|element| element.value().attr("href"))
        .filter(|href| {
            href.split(['?', '#'])
                .next()
                .is_some_and(|path| path.ends_with(".model.json"))
        })
        .filter_map(|href| article_url.join(href).ok())
        .filter(|url| same_url_origin(article_url, url))
        .max_by_key(|url| url.path().len()))
}

fn same_url_origin(left: &Url, right: &Url) -> bool {
    left.scheme().eq_ignore_ascii_case(right.scheme())
        && left
            .host_str()
            .zip(right.host_str())
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
        && left.port_or_known_default() == right.port_or_known_default()
}

#[derive(Debug)]
struct AemRichTextComponent {
    html: String,
    editorial_context: bool,
}

fn augment_html_with_aem_model(
    html: &[u8],
    model: &[u8],
    max_response_bytes: usize,
) -> Option<Vec<u8>> {
    let model = serde_json::from_slice::<Value>(model).ok()?;
    let mut components = Vec::new();
    collect_aem_rich_text_components(&model, false, 0, &mut components);
    let has_editorial_components = components
        .iter()
        .any(|component| component.editorial_context);
    let article_body = components
        .into_iter()
        .filter(|component| !has_editorial_components || component.editorial_context)
        .map(|component| component.html)
        .collect::<Vec<_>>()
        .join("\n");
    if article_body.trim().is_empty() {
        return None;
    }
    let framework_article = format!(
        r#"<div data-company-feed-framework="aem-model-json" itemprop="articleBody">{article_body}</div>"#
    );
    if html
        .len()
        .saturating_add(framework_article.len())
        .saturating_add(1)
        > max_response_bytes
    {
        return None;
    }
    let original = String::from_utf8_lossy(html);
    let lower = original.to_ascii_lowercase();
    let augmented = if let Some(body_end) = lower.rfind("</body>") {
        format!(
            "{}{}{}",
            &original[..body_end],
            framework_article,
            &original[body_end..]
        )
    } else {
        format!("{original}{framework_article}")
    };
    Some(augmented.into_bytes())
}

fn collect_aem_rich_text_components(
    value: &Value,
    editorial_context: bool,
    depth: usize,
    output: &mut Vec<AemRichTextComponent>,
) {
    const MAX_AEM_MODEL_DEPTH: usize = 64;
    if depth > MAX_AEM_MODEL_DEPTH {
        return;
    }
    match value {
        Value::Object(object) => {
            let component_type = object
                .get(":type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let editorial_context =
                editorial_context || aem_editorial_context_marker(component_type);
            if object
                .get("richText")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && let Some(html) = object.get("text").and_then(Value::as_str)
                && !html.trim().is_empty()
            {
                output.push(AemRichTextComponent {
                    html: html.to_owned(),
                    editorial_context,
                });
                return;
            }
            if let Some(items) = object.get(":items").and_then(Value::as_object) {
                let mut visited = HashSet::new();
                if let Some(order) = object.get(":itemsOrder").and_then(Value::as_array) {
                    for key in order.iter().filter_map(Value::as_str) {
                        let Some(child) = items.get(key) else {
                            continue;
                        };
                        visited.insert(key);
                        collect_aem_rich_text_components(
                            child,
                            editorial_context || aem_editorial_context_marker(key),
                            depth + 1,
                            output,
                        );
                    }
                }
                for (key, child) in items {
                    if visited.contains(key.as_str()) {
                        continue;
                    }
                    collect_aem_rich_text_components(
                        child,
                        editorial_context || aem_editorial_context_marker(key),
                        depth + 1,
                        output,
                    );
                }
                return;
            }
            for child in object.values() {
                collect_aem_rich_text_components(child, editorial_context, depth + 1, output);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_aem_rich_text_components(child, editorial_context, depth + 1, output);
            }
        }
        _ => {}
    }
}

fn aem_editorial_context_marker(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    [
        "main_content",
        "article",
        "blog",
        "press_release",
        "press-release",
        "news_content",
        "post_content",
        "post-content",
    ]
    .iter()
    .any(|marker| value.contains(marker))
}

#[cfg(test)]
fn extract_article(
    requested_url: &Url,
    final_url: Url,
    content_type: Option<&str>,
    bytes: &[u8],
    min_content_chars: usize,
) -> Result<RawCrawlItem, ArticlePageError> {
    extract_article_with_title_hint(
        requested_url,
        final_url,
        content_type,
        bytes,
        min_content_chars,
        None,
    )
}

#[cfg(test)]
fn extract_article_with_title_hint(
    requested_url: &Url,
    final_url: Url,
    content_type: Option<&str>,
    bytes: &[u8],
    min_content_chars: usize,
    title_hint: Option<&str>,
) -> Result<RawCrawlItem, ArticlePageError> {
    extract_article_with_hints(
        requested_url,
        final_url,
        content_type,
        bytes,
        min_content_chars,
        title_hint,
        None,
    )
}

fn extract_article_with_hints(
    requested_url: &Url,
    final_url: Url,
    content_type: Option<&str>,
    bytes: &[u8],
    min_content_chars: usize,
    title_hint: Option<&str>,
    published_at_hint: Option<DateTime<Utc>>,
) -> Result<RawCrawlItem, ArticlePageError> {
    let html_text = String::from_utf8_lossy(bytes);
    let content_type_is_html = content_type.is_none_or(|value| {
        let value = value.to_ascii_lowercase();
        value.contains("text/html") || value.contains("application/xhtml+xml")
    });
    if !content_type_is_html || !html_text.to_ascii_lowercase().contains("<html") {
        return Err(ArticlePageError::UnsupportedContent {
            url: final_url,
            content_type: content_type.unwrap_or("unknown").to_owned(),
        });
    }
    let document = Html::parse_document(&html_text);
    let has_usable_listing_title_hint = title_hint
        .map(normalize_article_title)
        .is_some_and(|title| is_usable_article_title(&title));
    let declared_canonical_url = extract_canonical_url(&document, &final_url)?;
    let (canonical_url, replaced_canonical_url, canonical_repair_reason) =
        if is_malformed_embedded_scheme_canonical(&declared_canonical_url) {
            (
                final_url.clone(),
                Some(declared_canonical_url),
                Some("malformed_declared_canonical"),
            )
        } else if has_usable_listing_title_hint
            && should_replace_site_root_canonical(&final_url, &declared_canonical_url)
        {
            (
                final_url.clone(),
                Some(declared_canonical_url),
                Some("site_root_canonical_with_listing_evidence"),
            )
        } else {
            (declared_canonical_url, None, None)
        };
    let article_selector = selector("article")?;
    let main_selector = selector("main")?;
    let role_main_selector = selector("[role='main']")?;
    if let Some(listing_url) = [&final_url, &canonical_url]
        .into_iter()
        .find(|url| is_obvious_article_listing_path(url))
    {
        return Err(ArticlePageError::ObviousListingPath {
            url: listing_url.clone(),
        });
    }
    let article_elements = document
        .select(&article_selector)
        .filter(|element| {
            !element
                .value()
                .attr("role")
                .is_some_and(|role| role.eq_ignore_ascii_case("presentation"))
                && !element
                    .value()
                    .attr("aria-hidden")
                    .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        })
        .collect::<Vec<_>>();
    let article_h1_selector = selector("h1")?;
    let article_elements_with_h1 = article_elements
        .iter()
        .filter(|element| element.select(&article_h1_selector).next().is_some())
        .count();
    let max_article_content_chars = article_elements
        .iter()
        .map(|element| {
            normalize(&element.text().collect::<Vec<_>>().join(" "))
                .chars()
                .count()
        })
        .max()
        .unwrap_or_default();
    let article_element = if article_elements.len() > 1 {
        article_elements.iter().copied().max_by_key(|element| {
            normalize(&element.text().collect::<Vec<_>>().join(" "))
                .chars()
                .count()
        })
    } else {
        article_elements.first().copied()
    };
    let semantic_body = select_semantic_article_body(&document, min_content_chars)?;
    let componentized_body = if is_article_like_path(&final_url) {
        select_componentized_rich_text_body(&document, min_content_chars)?
    } else {
        None
    };
    let semantic_body_element_chars = semantic_body
        .map(|(body, _)| sanitized_article_body_candidate_chars(body))
        .unwrap_or_default();
    let selected_componentized_body = componentized_body
        .as_ref()
        .filter(|body| body.content_chars >= semantic_body_element_chars);
    let semantic_body_chars = selected_componentized_body
        .map(|body| body.content_chars)
        .unwrap_or(semantic_body_element_chars);
    let has_semantic_body =
        semantic_body_element_chars >= min_content_chars || selected_componentized_body.is_some();
    let metadata_published_at = extract_published_at(&document)?;
    let data_layer_published_at =
        if metadata_published_at.is_none() && is_article_like_path(&final_url) {
            extract_data_layer_published_at(&document)?
        } else {
            None
        };
    let leading_body_published_at = if metadata_published_at.is_none()
        && data_layer_published_at.is_none()
        && is_article_like_path(&final_url)
    {
        semantic_body.and_then(|(body, _)| extract_leading_semantic_published_at(body))
    } else {
        None
    };
    let signals = article_signals(
        &document,
        &final_url,
        article_element.is_some(),
        metadata_published_at.is_some(),
        has_usable_listing_title_hint && has_semantic_body,
        has_semantic_body,
    )?;
    if signals.is_empty() {
        return Err(ArticlePageError::MissingArticleSignal { url: final_url });
    }
    let has_strong_article_metadata = signals.iter().any(|signal| {
        matches!(
            *signal,
            "published_time" | "open_graph_article" | "json_ld_article"
        )
    });
    let has_listing_title_and_semantic_body =
        signals.contains(&"article_like_path_with_listing_title_and_semantic_body");
    let has_url_backed_metadata_title_and_semantic_body =
        signals.contains(&"article_like_path_with_metadata_title_and_semantic_body");
    let has_isolated_semantic_body = selected_componentized_body.is_some()
        || semantic_body.is_some_and(|(body, _)| {
            semantic_body_element_chars >= min_content_chars
                && body.select(&article_selector).take(2).count() <= 1
        });
    let multiple_articles_are_disambiguated = has_strong_article_metadata
        || has_listing_title_and_semantic_body
        || has_url_backed_metadata_title_and_semantic_body
        || (signals.contains(&"article_like_path_with_h1") && has_isolated_semantic_body);
    if article_elements.len() > 1 && !multiple_articles_are_disambiguated {
        return Err(ArticlePageError::MultipleArticleCollection {
            url: final_url,
            article_count: article_elements.len(),
        });
    }
    let semantic_body_outweighs_cards = has_isolated_semantic_body
        && semantic_body_chars >= min_content_chars
        && semantic_body_chars > max_article_content_chars.saturating_mul(2);
    if is_card_grid_without_primary_article(
        article_elements.len(),
        article_elements_with_h1,
        max_article_content_chars,
    ) && !semantic_body_outweighs_cards
    {
        return Err(ArticlePageError::MultipleArticleCollection {
            url: final_url,
            article_count: article_elements.len(),
        });
    }
    let semantic_body_is_materially_larger = has_semantic_body && {
        let article_chars = article_element
            .map(|element| {
                normalize(&element.text().collect::<Vec<_>>().join(" "))
                    .chars()
                    .count()
            })
            .unwrap_or_default();
        semantic_body_chars >= min_content_chars
            && semantic_body_chars > article_chars.saturating_mul(2)
    };
    let prefer_semantic_body = selected_componentized_body.is_some()
        || article_elements.len() > 1
        || signals.contains(&"article_like_path_with_listing_title_and_semantic_body")
        || has_url_backed_metadata_title_and_semantic_body
        || (has_strong_article_metadata && semantic_body_is_materially_larger);
    let (body_html, body_selector) =
        if prefer_semantic_body && let Some(body) = selected_componentized_body {
            (body.html.clone(), body.selector)
        } else {
            let (body_element, body_selector) =
                if prefer_semantic_body && let Some(body) = semantic_body {
                    body
                } else if let Some(element) = article_element {
                    (element, "article")
                } else if let Some(element) = document.select(&main_selector).next() {
                    (element, "main")
                } else if let Some(element) = document.select(&role_main_selector).next() {
                    (element, "[role='main']")
                } else {
                    select_semantic_article_body(&document, min_content_chars)?.ok_or_else(
                        || ArticlePageError::MissingArticleBody {
                            url: final_url.clone(),
                        },
                    )?
                };
            (article_body_candidate_html(body_element), body_selector)
        };
    let processed = process_html(
        &body_html,
        &ContentProcessOptions {
            base_url: Some(final_url.as_str().to_owned()),
            keep_images: false,
        },
    );
    let content_chars = processed.text.chars().count();
    if content_chars < min_content_chars {
        return Err(ArticlePageError::InsufficientContent {
            url: final_url,
            content_chars,
            minimum: min_content_chars,
        });
    }
    if is_multi_heading_card_grid(
        article_elements.len(),
        article_elements_with_h1,
        content_chars,
    ) {
        return Err(ArticlePageError::MultipleArticleCollection {
            url: final_url,
            article_count: article_elements.len(),
        });
    }
    let extracted_title = extract_title(&document, &final_url, title_hint)?.ok_or_else(|| {
        ArticlePageError::MissingTitle {
            url: final_url.clone(),
        }
    })?;
    if is_listing_hint_over_collection_page(
        &final_url,
        extracted_title.source,
        extracted_title.replaced_page_title.as_deref(),
        article_elements.len(),
    ) {
        return Err(ArticlePageError::GenericListingTitle {
            url: final_url,
            title: extracted_title
                .replaced_page_title
                .unwrap_or(extracted_title.title),
        });
    }
    if [&final_url, &canonical_url]
        .into_iter()
        .any(|url| is_non_editorial_utility_article(&extracted_title.title, url))
    {
        return Err(ArticlePageError::GenericListingTitle {
            url: final_url,
            title: extracted_title.title,
        });
    }
    if is_shallow_collection_hub(
        &final_url,
        &extracted_title.title,
        title_hint,
        ShallowCollectionEvidence {
            article_count: article_elements.len(),
            article_elements_with_h1,
            max_article_content_chars,
            content_chars,
            link_count: processed.metrics.link_count,
            body_selector,
            body_text: &processed.text,
        },
    ) {
        return Err(ArticlePageError::GenericListingTitle {
            url: final_url,
            title: extracted_title.title,
        });
    }
    if is_short_multi_article_collection(
        &final_url,
        &extracted_title.title,
        article_elements.len(),
        content_chars,
        processed.metrics.link_count,
    ) {
        return Err(ArticlePageError::MultipleArticleCollection {
            url: final_url,
            article_count: article_elements.len(),
        });
    }
    if has_explicit_archive_heading(&document, &extracted_title.title)? {
        return Err(ArticlePageError::GenericListingTitle {
            url: final_url,
            title: extracted_title.title,
        });
    }
    if [&final_url, &canonical_url].into_iter().any(|url| {
        is_localized_publication_root(url, &extracted_title.title, processed.metrics.link_count)
    }) {
        return Err(ArticlePageError::GenericListingTitle {
            url: final_url,
            title: extracted_title.title,
        });
    }
    if [&final_url, &canonical_url].into_iter().any(|url| {
        is_counted_taxonomy_collection_title(
            url,
            &extracted_title.title,
            processed.metrics.link_count,
        )
    }) {
        return Err(ArticlePageError::GenericListingTitle {
            url: final_url,
            title: extracted_title.title,
        });
    }
    if [&final_url, &canonical_url]
        .into_iter()
        .any(|url| is_breadcrumb_prefixed_collection_title(url, &extracted_title.title))
    {
        return Err(ArticlePageError::GenericListingTitle {
            url: final_url,
            title: extracted_title.title,
        });
    }
    if let Some(archive_url) = [&final_url, &canonical_url]
        .into_iter()
        .find(|url| is_year_archive_collection(url, &extracted_title.title))
    {
        return Err(ArticlePageError::YearArchiveCollection {
            url: archive_url.clone(),
        });
    }
    let has_only_weak_path_signals = signals.iter().all(|signal| {
        matches!(
            *signal,
            "article_like_path_with_h1"
                | "article_like_path_with_listing_title_and_semantic_body"
                | "article_like_path_with_metadata_title_and_semantic_body"
        )
    });
    if has_only_weak_path_signals {
        let embedded_navigation_link_count =
            count_embedded_navigation_links(&body_html, &final_url)?;
        let has_publication_date = metadata_published_at.is_some()
            || leading_body_published_at.is_some()
            || published_at_hint.is_some();
        if is_undated_embedded_navigation_collection(
            &extracted_title.title,
            embedded_navigation_link_count,
            has_publication_date,
        ) {
            return Err(ArticlePageError::HighLinkDensityCollection {
                url: final_url,
                link_count: processed
                    .metrics
                    .link_count
                    .saturating_add(embedded_navigation_link_count),
                content_chars,
            });
        }
        if is_undated_short_slug_topic_collection(
            &final_url,
            &extracted_title.title,
            processed.metrics.link_count,
            content_chars,
            has_publication_date,
        ) {
            return Err(ArticlePageError::HighLinkDensityCollection {
                url: final_url,
                link_count: processed.metrics.link_count,
                content_chars,
            });
        }
        if is_undated_listing_anchor_collection(
            &extracted_title.title,
            extracted_title.source,
            processed.metrics.link_count,
            content_chars,
            has_publication_date,
        ) {
            return Err(ArticlePageError::HighLinkDensityCollection {
                url: final_url,
                link_count: processed.metrics.link_count,
                content_chars,
            });
        }
        if is_weak_collection_page(&final_url, &extracted_title.title) {
            return Err(ArticlePageError::GenericListingTitle {
                url: final_url,
                title: extracted_title.title,
            });
        }
        if is_high_link_density_collection(
            &extracted_title.title,
            processed.metrics.link_count,
            content_chars,
            body_selector,
            has_publication_date,
        ) {
            return Err(ArticlePageError::HighLinkDensityCollection {
                url: final_url,
                link_count: processed.metrics.link_count,
                content_chars,
            });
        }
    }
    let summary = first_attr(
        &document,
        &[
            ("meta[property='og:description']", "content"),
            ("meta[name='description']", "content"),
        ],
    )?;
    let source_item_key = sha256_hex(canonical_url.as_str().as_bytes());

    let (published_at, published_at_source) = if let Some(published_at) = metadata_published_at {
        (Some(published_at), "article_page")
    } else if let Some(published_at) = data_layer_published_at {
        (Some(published_at), "article_page_data_layer")
    } else if let Some(published_at) = leading_body_published_at {
        (Some(published_at), "article_page_leading_text")
    } else if let Some(published_at) = published_at_hint {
        (Some(published_at), "listing_card")
    } else {
        (None, "unknown")
    };

    Ok(RawCrawlItem {
        source_item_key,
        external_id: Some(canonical_url.as_str().to_owned()),
        url: final_url.clone(),
        canonical_url: Some(canonical_url.clone()),
        title: Some(extracted_title.title),
        summary_html: summary
            .map(|value| format!("<p>{}</p>", html_escape::encode_text(value.trim()))),
        body_html: Some(body_html),
        published_at,
        payload: json!({
            "extraction_contract": "generic-public-article.v1",
            "requested_url": requested_url,
            "final_url": final_url,
            "canonical_url": canonical_url,
            "replaced_canonical_url": replaced_canonical_url,
            "canonical_repair_reason": canonical_repair_reason,
            "content_type": content_type,
            "article_signals": signals,
            "article_element_count": article_elements.len(),
            "article_elements_with_h1": article_elements_with_h1,
            "max_article_content_chars": max_article_content_chars,
            "article_body_selector": body_selector,
            "article_body_component_count": selected_componentized_body
                .map(|body| body.component_count)
                .unwrap_or_default(),
            "title_source": extracted_title.source,
            "listing_title_hint": title_hint,
            "listing_published_at_hint": published_at_hint,
            "published_at_source": published_at_source,
            "replaced_page_title": extracted_title.replaced_page_title,
            "sanitized_content_chars": content_chars,
            "minimum_content_chars": min_content_chars,
        }),
    })
}

fn select_semantic_article_body(
    document: &Html,
    min_content_chars: usize,
) -> Result<Option<(ElementRef<'_>, &'static str)>, ArticlePageError> {
    // These are common semantic body containers emitted by CMSs and static
    // site builders. They are evaluated only after independent article
    // semantics have passed, and the generic Webflow class is deliberately
    // last. Within one selector, prefer the most substantive container.
    const BODY_SELECTORS: &[&str] = &[
        "[itemprop='articleBody']",
        "article#article-content",
        ".article-body",
        ".article-content",
        "[class~='article--content']",
        "[class*='blog-article__content']",
        ".entry-content",
        ".post-content",
        ".blog-post-content",
        ".post-body",
        ".post_body",
        ".post-content-section",
        ".blog_rich-text",
        ".blog_content_wrapper",
        "[class*='Blogpost_body']",
        ".post-rich-text",
        ".rich-text",
        ".rich-text-container",
        ".richtext-editor-place",
        ".w-richtext",
        "[class~='prose']",
        "[class~='chakra-container']",
        ".press-single-main-content",
        ".press-release",
        ".wd_news_body",
        ".et_pb_post_content",
        ".com-content-article__body",
        ".body-content",
        ".main_content",
        ".content-asset-body-wrapper",
        ".bodyCopyContainer",
        "[class~='field--name-body'][class~='field__item']",
        "[class~='field--name-body'] .field__item",
        ".rte_field",
        ".staticcontent",
        "#hs_cos_wrapper_post_body",
        "#articleContent",
        ".field-content",
        ".custom_code .code-wrap",
        "[data-elementor-type='single-post'] .elementor-widget-theme-post-content .elementor-widget-container",
        "[data-elementor-type='wp-post'] .elementor-widget-text-editor .elementor-widget-container",
        "[class*='RichTextRenderer__BlogPostContent']",
        "[class*='ArticlePage'] [class*='RichText']",
        ".single_blog.wysiwyg-content",
        "[class~='content'][class*='max-w-']",
        "[class*='singlePostContent']",
        "header[data-framer-name='Content']",
        "[data-framer-name='Content'][data-framer-component-type='RichTextContainer']",
        "[data-framer-name='Content'] [data-framer-component-type='RichTextContainer']",
        "[data-framer-name='content'][data-framer-component-type='RichTextContainer']",
        "[data-framer-name='content'] [data-framer-component-type='RichTextContainer']",
        "[data-framer-name='Body Content'][data-framer-component-type='RichTextContainer']",
        "[data-framer-name='Body Content'] [data-framer-component-type='RichTextContainer']",
        "[data-framer-name='Blog'] [data-framer-component-type='RichTextContainer']",
        "[data-framer-component-type='RichTextContainer']",
        "[class*='press-release__body']",
        "[class*='news-details-info']",
        "[class$='rich-text__body']",
        "#newsContent",
        "#content",
    ];
    let mut largest_thin_match = None;
    for query in BODY_SELECTORS {
        let selector = selector(query)?;
        let best = document
            .select(&selector)
            .map(|element| (element, sanitized_article_body_candidate_chars(element)))
            .max_by_key(|(_, chars)| *chars);
        if let Some((element, chars)) = best {
            if chars >= min_content_chars {
                return Ok(Some((element, *query)));
            }
            if largest_thin_match
                .as_ref()
                .is_none_or(|(_, _, largest_chars)| chars > *largest_chars)
            {
                largest_thin_match = Some((element, *query, chars));
            }
        }
    }
    if let Some(element) = select_generic_paragraph_cluster(document, min_content_chars)? {
        return Ok(Some((element, "generic:paragraph-cluster.v1")));
    }
    Ok(largest_thin_match.map(|(element, query, _)| (element, query)))
}

fn article_body_candidate_html(element: ElementRef<'_>) -> String {
    // Some CMSs use an ordinary chrome element such as `header` or `aside`
    // as the specifically selected article-body container. The sanitizer
    // intentionally drops those tags as page chrome, so preserve the
    // candidate's children after its independent article/body validation.
    serialize_element_html_with_text_fallback(
        element,
        matches!(element.value().name(), "header" | "aside"),
    )
}

fn serialize_element_html_with_text_fallback(element: ElementRef<'_>, inner: bool) -> String {
    // html5ever's serializer can panic on malformed table/foster-parenting
    // trees that the parser otherwise accepts. Never let a hostile or broken
    // public page kill the job-runner task: retain its visible text as safe
    // HTML and allow the ordinary correctness gates to decide whether it is
    // substantive enough.
    serialize_element_html_or_text_fallback(element, || {
        if inner {
            element.inner_html()
        } else {
            element.html()
        }
    })
}

fn serialize_element_html_or_text_fallback(
    element: ElementRef<'_>,
    serialize: impl FnOnce() -> String,
) -> String {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(serialize)).unwrap_or_else(|_| {
        let text = element
            .text()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "<div data-html-serialization-fallback=\"html5ever-panic.v1\"><p>{}</p></div>",
            html_escape::encode_text(&text)
        )
    })
}

fn sanitized_article_body_candidate_chars(element: ElementRef<'_>) -> usize {
    process_html(
        &article_body_candidate_html(element),
        &ContentProcessOptions {
            base_url: None,
            keep_images: false,
        },
    )
    .text
    .chars()
    .count()
}

struct ComponentizedArticleBody {
    html: String,
    selector: &'static str,
    content_chars: usize,
    component_count: usize,
}

fn select_componentized_rich_text_body(
    document: &Html,
    min_content_chars: usize,
) -> Result<Option<ComponentizedArticleBody>, ArticlePageError> {
    const SELECTOR: &str = "componentized:richtext-editor-place.v1";
    const MAX_COMPONENTS: usize = 128;
    const MIN_COMPONENT_CHARS: usize = 40;

    let component_selector = selector("main .richtext-editor-place")?;
    let article_selector = selector("article")?;
    let mut components = Vec::new();
    let mut content_chars = 0_usize;

    for element in document.select(&component_selector).take(MAX_COMPONENTS) {
        if title_candidate_is_hidden(&element)
            || element.select(&article_selector).next().is_some()
            || element
                .ancestors()
                .filter_map(ElementRef::wrap)
                .any(|ancestor| {
                    matches!(
                        ancestor.value().name(),
                        "article" | "aside" | "footer" | "form" | "header" | "nav"
                    )
                })
        {
            continue;
        }
        let html = serialize_element_html_with_text_fallback(element, true);
        let chars = process_html(
            &html,
            &ContentProcessOptions {
                base_url: None,
                keep_images: false,
            },
        )
        .text
        .chars()
        .count();
        if chars < MIN_COMPONENT_CHARS {
            continue;
        }
        content_chars = content_chars.saturating_add(chars);
        components.push(html);
    }

    if components.len() < 2 || content_chars < min_content_chars {
        return Ok(None);
    }
    let component_count = components.len();
    Ok(Some(ComponentizedArticleBody {
        html: format!(
            "<div data-componentized-article-body=\"v1\">{}</div>",
            components.join("\n")
        ),
        selector: SELECTOR,
        content_chars,
        component_count,
    }))
}

fn select_generic_paragraph_cluster<'a>(
    document: &'a Html,
    min_content_chars: usize,
) -> Result<Option<ElementRef<'a>>, ArticlePageError> {
    const MAX_GENERIC_CONTAINERS: usize = 1_024;

    let container_selector = selector("div, section")?;
    let paragraph_selector = selector("p")?;
    let link_selector = selector("a")?;
    let chrome_selector = selector("nav, header, footer, aside, form")?;
    let article_selector = selector("article")?;
    let paragraph_floor = min_content_chars.saturating_mul(2);
    let single_paragraph_floor = min_content_chars.saturating_mul(4);
    let mut best = None;

    for element in document
        .select(&container_selector)
        .take(MAX_GENERIC_CONTAINERS)
    {
        if element
            .ancestors()
            .filter_map(ElementRef::wrap)
            .any(|ancestor| {
                matches!(
                    ancestor.value().name(),
                    "nav" | "header" | "footer" | "aside" | "form"
                )
            })
            || element.select(&chrome_selector).next().is_some()
            || element.select(&article_selector).take(2).count() > 1
        {
            continue;
        }

        let mut paragraph_count = 0_usize;
        let mut paragraph_chars = 0_usize;
        for paragraph in element.select(&paragraph_selector) {
            let chars = normalize(&paragraph.text().collect::<Vec<_>>().join(" "))
                .chars()
                .count();
            if chars > 0 {
                paragraph_count += 1;
                paragraph_chars = paragraph_chars.saturating_add(chars);
            }
        }
        if paragraph_chars < paragraph_floor
            || (paragraph_count < 2 && paragraph_chars < single_paragraph_floor)
        {
            continue;
        }

        let total_chars = normalize(&element.text().collect::<Vec<_>>().join(" "))
            .chars()
            .count();
        if total_chars == 0 {
            continue;
        }
        let link_chars = element
            .select(&link_selector)
            .map(|link| {
                normalize(&link.text().collect::<Vec<_>>().join(" "))
                    .chars()
                    .count()
            })
            .fold(0_usize, usize::saturating_add);
        if link_chars.saturating_mul(2) > total_chars {
            continue;
        }

        let score = paragraph_chars
            .saturating_mul(paragraph_chars)
            .checked_div(total_chars)
            .unwrap_or_default();
        let should_replace =
            best.as_ref()
                .is_none_or(|(_, best_score, best_paragraph_chars, best_total_chars)| {
                    score > *best_score
                        || (score == *best_score
                            && (paragraph_chars > *best_paragraph_chars
                                || (paragraph_chars == *best_paragraph_chars
                                    && total_chars < *best_total_chars)))
                });
        if should_replace {
            best = Some((element, score, paragraph_chars, total_chars));
        }
    }

    Ok(best.map(|(element, _, _, _)| element))
}

#[derive(Debug, Eq, PartialEq)]
struct ExtractedTitle {
    title: String,
    source: &'static str,
    replaced_page_title: Option<String>,
}

fn extract_title(
    document: &Html,
    url: &Url,
    title_hint: Option<&str>,
) -> Result<Option<ExtractedTitle>, ArticlePageError> {
    let normalized_hint = title_hint
        .map(normalize_article_title)
        .filter(|title| is_usable_article_title(title));
    let mut metadata_titles = Vec::<(String, &'static str)>::new();
    for (query, attribute, source) in [
        ("meta[property='og:title']", "content", "social_metadata"),
        ("meta[name='twitter:title']", "content", "social_metadata"),
    ] {
        let selector = selector(query)?;
        metadata_titles.extend(
            document
                .select(&selector)
                .filter_map(|element| element.value().attr(attribute))
                .map(normalize_article_title)
                .filter(|title| !title.is_empty())
                .map(|title| (title, source)),
        );
    }
    let title_selector = selector("title")?;
    metadata_titles.extend(
        document
            .select(&title_selector)
            .map(|element| normalize_article_title(&element.text().collect::<Vec<_>>().join(" ")))
            .filter(|title| !title.is_empty())
            .map(|title| (title, "document_title")),
    );
    let metadata_consensus =
        metadata_titles
            .iter()
            .enumerate()
            .find_map(|(candidate_index, (candidate, source))| {
                (is_usable_article_title(candidate)
                    && title_agrees_with_url_path(candidate, url)
                    && metadata_titles
                        .iter()
                        .enumerate()
                        .any(|(support_index, (support, _))| {
                            candidate_index != support_index
                                && is_usable_article_title(support)
                                && titles_are_decorated_variants(candidate, support)
                        }))
                .then(|| (candidate.clone(), *source))
            });
    let hint_has_metadata_support = normalized_hint.as_ref().is_some_and(|hint| {
        metadata_titles.iter().any(|(metadata_title, _)| {
            is_usable_article_title(metadata_title)
                && titles_are_decorated_variants(hint, metadata_title)
        })
    });
    let mut first_generic = None;
    for query in ["article h1", "main h1", "h1"] {
        let selector = selector(query)?;
        let mut h1_candidates = HashSet::new();
        for element in document.select(&selector) {
            if title_candidate_is_hidden(&element) {
                continue;
            }
            let title = normalize_article_title(&element.text().collect::<Vec<_>>().join(" "));
            if title.is_empty() {
                continue;
            }
            h1_candidates.insert(title);
        }
        if h1_candidates.is_empty() {
            continue;
        }
        let usable_h1_candidates = h1_candidates
            .iter()
            .filter(|title| is_usable_article_title(title))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(title) = normalized_hint.as_ref().and_then(|hint| {
            usable_h1_candidates
                .iter()
                .find(|candidate| titles_are_decorated_variants(candidate, hint))
                .cloned()
        }) {
            return Ok(Some(ExtractedTitle {
                title,
                source: "h1",
                replaced_page_title: None,
            }));
        }
        if let Some(title) = metadata_titles.iter().find_map(|(metadata_title, _)| {
            is_usable_article_title(metadata_title)
                .then(|| {
                    usable_h1_candidates
                        .iter()
                        .find(|candidate| titles_are_decorated_variants(candidate, metadata_title))
                        .cloned()
                })
                .flatten()
        }) {
            return Ok(Some(ExtractedTitle {
                title,
                source: "h1",
                replaced_page_title: None,
            }));
        }
        if let Some((title, source)) = metadata_consensus.as_ref() {
            let replaced_page_title = usable_h1_candidates
                .iter()
                .max_by(|left, right| {
                    left.chars()
                        .count()
                        .cmp(&right.chars().count())
                        .then_with(|| left.cmp(right))
                })
                .cloned()
                .or_else(|| h1_candidates.iter().min().cloned());
            return Ok(Some(ExtractedTitle {
                title: title.clone(),
                source,
                replaced_page_title,
            }));
        }
        if hint_has_metadata_support && let Some(title) = normalized_hint.clone() {
            let replaced_page_title = usable_h1_candidates
                .iter()
                .max_by(|left, right| {
                    left.chars()
                        .count()
                        .cmp(&right.chars().count())
                        .then_with(|| left.cmp(right))
                })
                .cloned()
                .or_else(|| h1_candidates.iter().min().cloned());
            return Ok(Some(ExtractedTitle {
                title,
                source: "listing_anchor",
                replaced_page_title,
            }));
        }
        if usable_h1_candidates.len() > 1
            && let Some((title, source)) = metadata_titles
                .iter()
                .find(|(title, _)| is_usable_article_title(title))
        {
            return Ok(Some(ExtractedTitle {
                title: title.clone(),
                source,
                replaced_page_title: None,
            }));
        }
        if let Some(title) = usable_h1_candidates.into_iter().max_by(|left, right| {
            left.chars()
                .count()
                .cmp(&right.chars().count())
                .then_with(|| left.cmp(right))
        }) {
            return Ok(Some(ExtractedTitle {
                title,
                source: "h1",
                replaced_page_title: None,
            }));
        }
        if let Some(title) = h1_candidates.into_iter().min() {
            first_generic.get_or_insert(title);
        }
        // A structurally narrower H1 scope existed but contained only page
        // chrome. Do not widen to an unrelated site-header H1; continue with
        // independently scoped metadata and semantic headline fallbacks.
        break;
    }
    if first_generic.is_some()
        && let Some(title) = normalized_hint
    {
        return Ok(Some(ExtractedTitle {
            title,
            source: "listing_anchor",
            replaced_page_title: first_generic,
        }));
    }
    for (title, source) in metadata_titles {
        if is_usable_article_title(&title) {
            return Ok(Some(ExtractedTitle {
                title,
                source,
                replaced_page_title: None,
            }));
        }
        first_generic.get_or_insert(title);
    }
    let semantic_heading_selector = selector(
        "[itemprop='headline'], \
         main h2[class*='title'], main h3[class*='title'], \
         article header h2, article header h3",
    )?;
    let semantic_heading_candidates = document
        .select(&semantic_heading_selector)
        .filter(|element| !title_candidate_is_hidden(element))
        .map(|element| normalize_article_title(&element.text().collect::<Vec<_>>().join(" ")))
        .filter(|title| is_usable_article_title(title))
        .collect::<HashSet<_>>();
    if semantic_heading_candidates.len() == 1 {
        return Ok(semantic_heading_candidates
            .into_iter()
            .next()
            .map(|title| ExtractedTitle {
                title,
                source: "semantic_heading",
                replaced_page_title: first_generic,
            }));
    }
    if let Some(title) = normalized_hint {
        return Ok(Some(ExtractedTitle {
            title,
            source: "listing_anchor",
            replaced_page_title: first_generic,
        }));
    }
    if let Some(title) = first_generic {
        return Err(ArticlePageError::GenericListingTitle {
            url: url.clone(),
            title,
        });
    }
    Ok(None)
}

fn title_candidate_is_hidden(element: &ElementRef<'_>) -> bool {
    std::iter::once(*element)
        .chain(element.ancestors().take(16).filter_map(ElementRef::wrap))
        .any(|candidate| {
            let attributes = candidate.value();
            attributes.attr("hidden").is_some()
                || attributes
                    .attr("aria-hidden")
                    .is_some_and(|value| value.eq_ignore_ascii_case("true"))
                || attributes.attr("class").is_some_and(|classes| {
                    classes.split_ascii_whitespace().any(|class| {
                        matches!(
                            class.to_ascii_lowercase().as_str(),
                            "hidden"
                                | "invisible"
                                | "sr-only"
                                | "visually-hidden"
                                | "w-condition-invisible"
                        )
                    })
                })
                || attributes.attr("style").is_some_and(|style| {
                    let compact = style
                        .chars()
                        .filter(|character| !character.is_ascii_whitespace())
                        .flat_map(char::to_lowercase)
                        .collect::<String>();
                    compact.contains("display:none") || compact.contains("visibility:hidden")
                })
        })
}

fn is_usable_article_title(title: &str) -> bool {
    let title = normalize_article_title(title);
    let chars = title.chars().count();
    (5..=300).contains(&chars)
        && title.chars().any(char::is_alphabetic)
        && parse_article_datetime(&title).is_none()
        && !looks_like_embedded_css(&title)
        && !looks_like_letter_spaced_title(&title)
        && !is_generic_listing_title(&title)
}

fn normalize_article_title(title: &str) -> String {
    let title = normalize(title);
    let lowercase = title.to_ascii_lowercase();
    for prefix in ["read more about "] {
        if lowercase.starts_with(prefix) {
            let remainder = normalize(&title[prefix.len()..]);
            if remainder.chars().count() >= 5
                && remainder.chars().any(char::is_alphabetic)
                && !is_generic_listing_title(&remainder)
            {
                return remainder;
            }
        }
    }
    title
}

fn looks_like_embedded_css(title: &str) -> bool {
    let title = title.to_ascii_lowercase();
    title.contains('{')
        && title.contains('}')
        && title.contains(':')
        && title.contains(';')
        && (title.contains(".cls-")
            || title.contains("stroke-width:")
            || title.contains("fill:")
            || title.contains("font-family:")
            || title.contains("display:")
            || title.contains("visibility:"))
}

fn looks_like_letter_spaced_title(title: &str) -> bool {
    let words = title.split_whitespace().collect::<Vec<_>>();
    words.len() >= 5
        && words
            .iter()
            .all(|word| word.chars().count() == 1 && word.chars().all(char::is_alphabetic))
}

fn normalized_title_key(title: &str) -> String {
    normalize(title).to_ascii_lowercase()
}

fn title_agrees_with_url_path(title: &str, url: &Url) -> bool {
    fn identity_tokens(value: &str) -> HashSet<String> {
        value
            .split(|character: char| !character.is_ascii_alphanumeric())
            .map(str::to_ascii_lowercase)
            .filter(|token| token.len() >= 4)
            .filter(|token| {
                !matches!(
                    token.as_str(),
                    "article"
                        | "blog"
                        | "common"
                        | "company"
                        | "news"
                        | "press"
                        | "release"
                        | "releases"
                        | "report"
                        | "reports"
                        | "stock"
                        | "update"
                        | "updates"
                )
            })
            .collect()
    }

    let title_tokens = identity_tokens(title);
    let path_tokens = identity_tokens(url.path());
    let shared = title_tokens.intersection(&path_tokens).collect::<Vec<_>>();
    shared.len() >= 2 || shared.iter().any(|token| token.len() >= 8)
}

fn titles_are_decorated_variants(left: &str, right: &str) -> bool {
    let left = normalized_title_key(left);
    let right = normalized_title_key(right);
    if left == right {
        return true;
    }
    [
        (" | ", true),
        (" - ", true),
        (" – ", true),
        (" — ", true),
        (": ", false),
    ]
    .iter()
    .any(|(separator, symmetric)| {
        right
            .strip_prefix(&format!("{left}{separator}"))
            .is_some_and(|suffix| !suffix.trim().is_empty())
            || left
                .strip_prefix(&format!("{right}{separator}"))
                .is_some_and(|suffix| !suffix.trim().is_empty())
            || (*symmetric
                && (right
                    .strip_suffix(&format!("{separator}{left}"))
                    .is_some_and(|prefix| !prefix.trim().is_empty())
                    || left
                        .strip_suffix(&format!("{separator}{right}"))
                        .is_some_and(|prefix| !prefix.trim().is_empty())))
    })
}

fn listing_hint_embeds_repeated_title(current_title: &str, title_hint: &str) -> bool {
    let current_key = normalized_title_key(current_title);
    if current_key.chars().count() < 20 {
        return false;
    }
    normalized_title_key(title_hint).contains(&current_key)
}

fn repair_repeated_page_titles(items: &mut [RawCrawlItem]) {
    let mut counts = HashMap::new();
    let mut hint_keys = HashMap::<String, HashSet<String>>::new();
    for title in items.iter().filter_map(|item| item.title.as_deref()) {
        *counts.entry(normalized_title_key(title)).or_insert(0_usize) += 1;
    }
    for item in items.iter() {
        let Some(current_title) = item.title.as_deref() else {
            continue;
        };
        let Some(title_hint) = item
            .payload
            .get("listing_title_hint")
            .and_then(Value::as_str)
            .map(normalize_article_title)
            .filter(|title| is_usable_article_title(title))
        else {
            continue;
        };
        hint_keys
            .entry(normalized_title_key(current_title))
            .or_default()
            .insert(normalized_title_key(&title_hint));
    }
    for item in items {
        let Some(current_title) = item.title.as_deref() else {
            continue;
        };
        let current_key = normalized_title_key(current_title);
        if counts.get(&current_key).copied().unwrap_or_default() < 2
            || hint_keys
                .get(&current_key)
                .map(HashSet::len)
                .unwrap_or_default()
                < 2
        {
            continue;
        }
        let Some(title_hint) = item
            .payload
            .get("listing_title_hint")
            .and_then(Value::as_str)
            .map(normalize_article_title)
            .filter(|title| is_usable_article_title(title))
        else {
            continue;
        };
        if normalized_title_key(&title_hint) == current_key {
            continue;
        }
        if listing_hint_embeds_repeated_title(current_title, &title_hint) {
            continue;
        }
        let replaced_title = item.title.replace(title_hint);
        if let Some(payload) = item.payload.as_object_mut() {
            payload.insert("title_source".to_owned(), json!("listing_anchor_repair"));
            payload.insert("replaced_page_title".to_owned(), json!(replaced_title));
        }
    }
}

fn is_generic_listing_title(title: &str) -> bool {
    const GENERIC_TITLES: &[&str] = &[
        "additional insights",
        "404 error",
        "404 page not found",
        "archives",
        "bra talk",
        "brochure",
        "calendar",
        "categories",
        "category",
        "clientes",
        "coming soon",
        "corporate",
        "customer",
        "dashboard",
        "dividends",
        "downloads",
        "earnings",
        "ecommerce",
        "embedded",
        "esg news",
        "facebook",
        "features",
        "footwear",
        "general information",
        "guides & articles",
        "heritage",
        "images",
        "images .",
        "industry",
        "insights",
        "investor",
        "lighting",
        "latest articles",
        "newer posts",
        "older posts",
        "page not found",
        "previous",
        "previous posts",
        "producto",
        "results",
        "results:",
        "rss feeds",
        "see more",
        "shipping",
        "shoptalk",
        "subscribe",
        "tax forms",
        "templates",
        "the latest product offerings",
        "vaccines",
        "webinars",
        "all articles",
        "all news",
        "all blog posts",
        "all multimedia",
        "all news releases",
        "all photos",
        "all posts",
        "all stories",
        "all videos",
        "ambassadors",
        "analyst coverage",
        "analysts",
        "about us",
        "annual filings",
        "annual meeting",
        "annual meeting materials",
        "annual meetings",
        "annual proxy",
        "annual report",
        "annual reports",
        "annual reports & proxies",
        "annual reports and proxies",
        "annual reports and proxy statements",
        "announcements",
        "api success",
        "arrow icon",
        "article summary",
        "blog post",
        "case study",
        "chain reaction newsletter archive",
        "code examples",
        "community",
        "cookies policy",
        "cookies policy (opens in new window)",
        "corporate news",
        "cve analysis",
        "developer spotlight",
        "explore all",
        "image link",
        "investor communications sign up",
        "key articles",
        "learn more : gts north america",
        "leadership perspectives",
        "media inquiries",
        "media releases",
        "newsletter sign-up",
        "newsletters",
        "our company",
        "partnerships",
        "press details",
        "product updates",
        "release details",
        "snapshots",
        "solution briefs",
        "strategy",
        "sustainability leadership",
        "trending topics",
        "we announced",
        "agreement manager",
        "all industries",
        "app center",
        "artificial intelligence",
        "artificial intelligence insights",
        "blackrock investment institute",
        "building",
        "cautionary statement",
        "common api tasks",
        "common pool problems",
        "contract lifecycle management",
        "cricut contact information",
        "customer experience insights",
        "cybersecurity",
        "data center",
        "data-driven finance",
        "delaware",
        "developer support articles",
        "developer tools",
        "developer trending topics",
        "digital transformation insights",
        "document generation",
        "electronic signature",
        "enode news",
        "environment",
        "esignature",
        "evolving energy",
        "family of brands",
        "gunshot detection",
        "healthy spaces podcast",
        "home care business",
        "home care marketing",
        "home care technology",
        "identify",
        "inspiration",
        "investment ideas",
        "investment team voices",
        "investor learning",
        "life@gen",
        "lifestyle",
        "lucid tips and updates",
        "market signals podcast",
        "maryland",
        "media and news",
        "natural disasters",
        "newborn skincare",
        "oncology",
        "our story",
        "our thinking",
        "people & impact",
        "poolsmart",
        "product and innovation",
        "research and reports",
        "safety & security",
        "sdks and tools",
        "sign up for our investor news alerts",
        "sign up for our investors news alert",
        "skin conditions",
        "solutions & innovation",
        "stories and perspectives",
        "street view video",
        "the honeylove edit",
        "trends & ideas",
        "vanta release notes",
        "virginia",
        "water treatment",
        "weekly market performance",
        "west virginia",
        "woodside fact checker",
        "workflow builder",
        "articles",
        "awards & recognition",
        "awards and recognition",
        "asset alerts",
        "asset library",
        "audited financial statements",
        "blog",
        "board of directors",
        "brand guide",
        "brand guides",
        "brand resources",
        "capabilities statement",
        "case studies",
        "changelog",
        "clinical case studies",
        "clinical evidence",
        "clinical trials",
        "click here",
        "code of conduct",
        "committee composition",
        "committee charters",
        "committees",
        "company announcements",
        "company fact sheet",
        "company & portfolio news",
        "company news",
        "company news and press releases",
        "company overview",
        "company statements",
        "communiqués de presse",
        "conferences & presentations",
        "conferences and presentations",
        "congresses",
        "contact us",
        "contact the board",
        "contact investor relations",
        "contact ir",
        "contact info",
        "contacts",
        "conditions générales d'utilisation (cgu)",
        "contact",
        "corporate governance",
        "corporate governance guidelines",
        "corporate press kits",
        "corporate profile",
        "cookie policy",
        "data & analytics",
        "data and analytics",
        "customer stories",
        "customers",
        "developers",
        "dividend history",
        "dossiers de presse",
        "earnings calls",
        "earnings releases",
        "editorial policy",
        "email alerts",
        "email alerts & rss newsfeeds",
        "event calendar",
        "events & presentations",
        "events and presentations",
        "events calendar",
        "executive management",
        "financial",
        "fixed income",
        "finance",
        "financial news",
        "financial information",
        "financial press releases and webcasts",
        "financial news overview",
        "fact sheets",
        "faqs",
        "featured videos",
        "filings & reports",
        "frequently asked questions",
        "general news",
        "general news overview",
        "glossary",
        "governance",
        "governance documents",
        "governance overview",
        "headline",
        "home",
        "image gallery",
        "image library",
        "infographics",
        "insights & media",
        "insights and media",
        "insights library",
        "investor contacts",
        "investor email alerts",
        "investor faqs",
        "investor news",
        "investor overview",
        "investor relations",
        "investor conferences",
        "investor presentations",
        "investor resources",
        "investors",
        "why invest",
        "in the news",
        "in the press",
        "information request form",
        "ir calendar",
        "ir updates",
        "latest news",
        "latest stories",
        "leadership",
        "learn more",
        "linkedin",
        "know more",
        "management",
        "media coverage",
        "media faqs",
        "media gallery",
        "market announcements",
        "media and analyst contacts",
        "media center",
        "media centre",
        "media contacts and materials",
        "media contacts",
        "media hub",
        "media kit",
        "media request",
        "submit media request",
        "media asset library",
        "media information",
        "media inquiry form",
        "media library & contacts",
        "media logos",
        "media relations contacts",
        "media relations",
        "media resources",
        "media room",
        "news",
        "news & events",
        "news & press releases",
        "news and events",
        "news and press releases",
        "news detail",
        "news details",
        "new product announcements",
        "news release",
        "news release detail",
        "news release details",
        "news releases",
        "news releases details",
        "newsroom",
        "next page",
        "multimedia",
        "multimedia library",
        "newsletter",
        "non-gaap reconciliation",
        "other archives",
        "overview",
        "partnering news",
        "photos and videos",
        "podcasts",
        "presentations",
        "presentations & events",
        "presentations & webcasts",
        "presentations and webcasts",
        "press kit",
        "press release",
        "press release detail",
        "press release details",
        "press releases",
        "press releases detail",
        "press releases details",
        "press kits",
        "press room",
        "product press kits",
        "people and culture",
        "product reviews",
        "product resources",
        "publications",
        "quarterly earnings",
        "quarterly earnings materials",
        "quarterly results",
        "réactions",
        "réseaux sociaux",
        "scientific publications",
        "shareholder services",
        "sign up today",
        "site map",
        "site-seeing gallery",
        "social channels",
        "social media disclosure",
        "statements",
        "read the full article",
        "read more",
        "read more data and research articles",
        "research",
        "revues de presse",
        "search for more",
        "sec filings",
        "stock information",
        "stockholder faqs",
        "stories",
        "success stories",
        "tax documents",
        "terms of use",
        "never miss an update: sign up for updates, exclusive insights, and product releases.",
        "subscribe to visualize",
        "subscribe using the form below",
        "thank you for subscribing",
        "thank you for subscribing.",
        "updates",
        "updates and statements",
        "view all",
        "view article",
        "view media kit",
        "view more",
        "view our webcasts",
        "view transcript",
        "view code examples on github",
        "video hub",
        "video library",
        "vulnerability disclosure",
        "whistleblower hotline",
        "white papers",
        "whitepapers",
        "why invest?",
        "webcasts",
        "webcasts & presentations",
        "webcasts and presentations",
        "xbrl files",
        "your browser is unsupported",
        "voir tout",
    ];

    let title = normalize(title)
        .trim_end_matches(['→', '›', '»'])
        .trim()
        .to_ascii_lowercase();
    let title = [
        " (opens in a new window)",
        " (opens in new window)",
        " opens in a new window",
        " opens in new window",
        " arrow_forward",
        " chevron_right",
        " open_in_new",
    ]
    .iter()
    .find_map(|suffix| title.strip_suffix(suffix))
    .unwrap_or(&title);
    let matches_generic_title = |candidate: &str| {
        GENERIC_TITLES.iter().any(|generic| {
            candidate == *generic
                || has_short_generic_site_suffix(candidate, generic)
                || [
                    "investor relations",
                    "press release detail",
                    "press release details",
                    "press releases detail",
                    "press releases details",
                ]
                .contains(generic)
                    && [" | ", " - ", " – ", " — ", ": "]
                        .iter()
                        .any(|separator| candidate.ends_with(&format!("{separator}{generic}")))
        })
    };
    let is_short_collection_section = |section: &str| {
        let section = section.trim();
        let word_count = section.split_whitespace().count();
        !section.is_empty()
            && word_count <= 4
            && !section.chars().any(|character| character.is_ascii_digit())
            && (matches_generic_title(section)
                || (word_count <= 3 && (section.ends_with(" blog") || section.ends_with(" blogs"))))
    };
    let is_collection_breadcrumb = {
        let sections = title.split(" | ").collect::<Vec<_>>();
        (2..=4).contains(&sections.len())
            && sections
                .iter()
                .all(|section| is_short_collection_section(section))
    };
    matches_generic_title(title)
        || is_collection_breadcrumb
        || title
            .strip_prefix("image ")
            .is_some_and(matches_generic_title)
        || title
            .rsplit(['>', '›', '»'])
            .next()
            .map(str::trim)
            .is_some_and(|tail| tail != title && matches_generic_title(tail))
        || (title.split_whitespace().count() <= 2 && title.starts_with("contact "))
        || (title.split_whitespace().count() <= 5
            && title.starts_with("contact ")
            && title.ends_with(" media relations"))
        || (title.split_whitespace().count() <= 4
            && title.starts_with("about ")
            && !title.chars().any(|character| character.is_ascii_digit()))
        || is_pagination_control_title(title)
        || is_date_only_title(title)
        || (title.split_whitespace().count() == 2 && title.ends_with(" blogs"))
        || (title.split_whitespace().count() <= 3 && title.ends_with(" icon"))
        || title.split_once(" | ").is_some_and(|(section, category)| {
            section.split_whitespace().count() <= 3
                && section.ends_with(" blogs")
                && category.split_whitespace().count() <= 4
        })
        || [" | ", " - ", " – ", " — "].iter().any(|separator| {
            title.split_once(separator).is_some_and(|(section, site)| {
                section.split_whitespace().count() <= 4
                    && (section.ends_with(" archives") || section.ends_with(" stories"))
                    && site.split_whitespace().count() <= 4
            })
        })
        || (title.split_whitespace().count() <= 4
            && (title.ends_with(" annual report") || title.ends_with(" proxy statement")))
        || (title.split_whitespace().count() <= 3
            && (title.starts_with('|') || title.ends_with('|')))
        || (title.split_whitespace().count() <= 5
            && (title.ends_with(" sub menu") || title.ends_with(" submenu")))
        || title.ends_with(". select to filter.")
        || (title.split_whitespace().count() <= 4
            && !title.chars().any(|character| character.is_ascii_digit())
            && [
                " events",
                " glossary",
                " in the news",
                " media",
                " podcasts",
                " research",
                " shorts",
                " stories",
                " tv",
                " white papers",
            ]
            .iter()
            .any(|suffix| title.ends_with(suffix)))
        || title.split_once(" - ").is_some_and(|(section, site)| {
            site == "newsroom"
                && section.split_whitespace().count() <= 3
                && section.ends_with(" stories")
        })
}

fn is_pagination_control_title(title: &str) -> bool {
    let words = title.split_whitespace().collect::<Vec<_>>();
    matches!(
        words.as_slice(),
        ["show", "page", page] if page.bytes().all(|byte| byte.is_ascii_digit())
    ) || matches!(
        words.as_slice(),
        ["show", count, "per", "page"]
            if count.bytes().all(|byte| byte.is_ascii_digit())
    )
}

fn is_date_only_title(title: &str) -> bool {
    let words = title
        .split_whitespace()
        .map(|word| word.trim_end_matches([',', '.']))
        .collect::<Vec<_>>();
    let is_month = |word: &str| {
        matches!(
            word,
            "january"
                | "jan"
                | "february"
                | "feb"
                | "march"
                | "mar"
                | "april"
                | "apr"
                | "may"
                | "june"
                | "jun"
                | "july"
                | "jul"
                | "august"
                | "aug"
                | "september"
                | "sep"
                | "sept"
                | "october"
                | "oct"
                | "november"
                | "nov"
                | "december"
                | "dec"
        )
    };
    let is_year = |word: &str| word.len() == 4 && word.bytes().all(|byte| byte.is_ascii_digit());
    let is_day = |word: &str| word.parse::<u8>().is_ok_and(|day| (1..=31).contains(&day));

    matches!(words.as_slice(), [month, year] if is_month(month) && is_year(year))
        || matches!(
            words.as_slice(),
            [month, day, year] if is_month(month) && is_day(day) && is_year(year)
        )
        || matches!(
            words.as_slice(),
            ["day:", day, month, year]
                if is_day(day) && is_month(month) && is_year(year)
        )
}

fn has_short_generic_site_suffix(title: &str, generic: &str) -> bool {
    [" | ", " - ", " – ", " — ", ": "]
        .iter()
        .filter_map(|separator| title.strip_prefix(&format!("{generic}{separator}")))
        .any(|suffix| {
            let suffix = suffix.trim();
            !suffix.is_empty()
                && suffix.chars().count() <= 80
                && !suffix.chars().any(|character| character.is_ascii_digit())
                && suffix.split_whitespace().count() <= 4
        })
}

fn extract_published_at(document: &Html) -> Result<Option<DateTime<Utc>>, ArticlePageError> {
    for (query, attribute) in [
        ("meta[property='article:published_time']", "content"),
        ("meta[name='date']", "content"),
        ("meta[name='publish_date']", "content"),
        ("meta[name='publish-date']", "content"),
        ("meta[name='published-date']", "content"),
        ("meta[name='publication_date']", "content"),
        ("meta[name='publication-date']", "content"),
        ("meta[name='pubdate']", "content"),
    ] {
        let selector = selector(query)?;
        if let Some(published_at) = document
            .select(&selector)
            .filter_map(|element| element.value().attr(attribute))
            .find_map(parse_article_datetime)
        {
            return Ok(Some(published_at));
        }
    }

    let time_selector = selector("time[datetime]")?;
    let h1_selector = selector("h1")?;
    let contextual_time = document
        .select(&time_selector)
        .enumerate()
        .filter_map(|(document_order, element)| {
            let published_at =
                parse_article_datetime(element.value().attr("datetime").unwrap_or_default())?;
            let h1_ancestor_distance =
                element
                    .ancestors()
                    .take(8)
                    .enumerate()
                    .find_map(|(distance, node)| {
                        ElementRef::wrap(node)
                            .is_some_and(|ancestor| ancestor.select(&h1_selector).next().is_some())
                            .then_some(distance)
                    })?;
            Some((h1_ancestor_distance, document_order, published_at))
        })
        .min_by_key(|(h1_ancestor_distance, document_order, _)| {
            (*h1_ancestor_distance, *document_order)
        })
        .map(|(_, _, published_at)| published_at);
    if contextual_time.is_some() {
        return Ok(contextual_time);
    }

    let json_ld_selector = selector("script[type='application/ld+json']")?;
    let json_ld_published_at = document.select(&json_ld_selector).find_map(|element| {
        serde_json::from_str::<Value>(&element.text().collect::<String>())
            .ok()
            .as_ref()
            .and_then(json_ld_article_published_at)
    });
    if json_ld_published_at.is_some() {
        return Ok(json_ld_published_at);
    }
    let has_open_graph_article = first_attr(document, &[("meta[property='og:type']", "content")])?
        .is_some_and(|value| value.eq_ignore_ascii_case("article"));
    if has_open_graph_article {
        let web_page_published_at = document.select(&json_ld_selector).find_map(|element| {
            serde_json::from_str::<Value>(&element.text().collect::<String>())
                .ok()
                .as_ref()
                .and_then(json_ld_web_page_published_at)
        });
        if web_page_published_at.is_some() {
            return Ok(web_page_published_at);
        }
    }
    if let Some(published_at) = extract_h1_local_visible_published_at(document)? {
        return Ok(Some(published_at));
    }

    Ok(document
        .select(&time_selector)
        .filter_map(|element| element.value().attr("datetime"))
        .find_map(parse_article_datetime))
}

fn extract_data_layer_published_at(
    document: &Html,
) -> Result<Option<DateTime<Utc>>, ArticlePageError> {
    let script_selector = selector("script")?;
    Ok(document.select(&script_selector).find_map(|element| {
        let script = element.text().collect::<String>();
        ["firstPublishDate", "originalPublishDate"]
            .into_iter()
            .find_map(|field| parse_named_script_datetime(&script, field))
    }))
}

fn parse_named_script_datetime(script: &str, field: &str) -> Option<DateTime<Utc>> {
    script.match_indices(field).find_map(|(offset, _)| {
        let field_end = offset + field.len();
        let field_has_boundaries = script[..offset]
            .chars()
            .next_back()
            .is_none_or(|character| !character.is_ascii_alphanumeric() && character != '_')
            && script[field_end..]
                .chars()
                .next()
                .is_none_or(|character| !character.is_ascii_alphanumeric() && character != '_');
        if !field_has_boundaries {
            return None;
        }
        let suffix = &script[field_end..];
        let delimiter_offset = suffix
            .char_indices()
            .find_map(|(offset, character)| matches!(character, ':' | '=').then_some(offset))
            .filter(|offset| *offset <= 32)?;
        let value = suffix[delimiter_offset + 1..].trim_start();
        let quote = value
            .chars()
            .next()
            .filter(|quote| matches!(quote, '\'' | '"'))?;
        let value = &value[quote.len_utf8()..];
        let end = value.find(quote)?;
        (end <= 64)
            .then(|| parse_article_datetime(&value[..end]))
            .flatten()
    })
}

fn extract_h1_local_visible_published_at(
    document: &Html,
) -> Result<Option<DateTime<Utc>>, ArticlePageError> {
    let marked_element_selector = selector("[class],[id]")?;
    let h1_selector = selector("h1")?;
    let mut dates = document
        .select(&marked_element_selector)
        .filter(|element| !title_candidate_is_hidden(element))
        .filter(|element| {
            let has_date_marker = ["class", "id"].into_iter().any(|attribute| {
                element.value().attr(attribute).is_some_and(|value| {
                    let marker = value
                        .chars()
                        .filter(|character| character.is_ascii_alphanumeric())
                        .flat_map(char::to_lowercase)
                        .collect::<String>();
                    ["publishdate", "publisheddate", "datepublished", "postdate"]
                        .into_iter()
                        .any(|candidate| marker.contains(candidate))
                })
            });
            has_date_marker || parse_explicit_visible_publication_datetime(element).is_some()
        })
        .filter(|element| {
            // A date marker is page-level evidence only when it shares a
            // small non-page-chrome wrapper with the H1. This admits common
            // article-header layouts while excluding dates on related cards
            // elsewhere in main/body.
            element
                .ancestors()
                .take(3)
                .filter_map(ElementRef::wrap)
                .filter(|ancestor| !matches!(ancestor.value().name(), "html" | "body" | "main"))
                .any(|ancestor| ancestor.select(&h1_selector).next().is_some())
        })
        .filter_map(|element| {
            let text = normalize(&element.text().collect::<Vec<_>>().join(" "));
            parse_article_datetime(&text)
                .or_else(|| parse_explicit_publication_datetime_text(&text))
        })
        .collect::<Vec<_>>();
    dates.sort();
    dates.dedup();
    Ok(if dates.len() == 1 { dates.pop() } else { None })
}

fn parse_explicit_visible_publication_datetime(element: &ElementRef<'_>) -> Option<DateTime<Utc>> {
    parse_explicit_publication_datetime_text(&normalize(
        &element.text().collect::<Vec<_>>().join(" "),
    ))
}

fn parse_explicit_publication_datetime_text(value: &str) -> Option<DateTime<Utc>> {
    let lowercase = value.to_ascii_lowercase();
    ["published on", "published", "posted on", "posted"]
        .into_iter()
        .find_map(|label| {
            lowercase.strip_prefix(label)?;
            let date = value
                .get(label.len()..)?
                .trim_start_matches(|character: char| {
                    character.is_whitespace() || matches!(character, ':' | '-' | '–' | '—' | '•')
                });
            (!date.is_empty())
                .then(|| parse_article_datetime(date))
                .flatten()
        })
}

fn extract_leading_semantic_published_at(body: ElementRef<'_>) -> Option<DateTime<Utc>> {
    let mut dates = body
        .text()
        .map(normalize)
        .filter(|text| !text.is_empty())
        .take(8)
        .filter_map(|text| {
            parse_article_datetime(&text).or_else(|| {
                // Corporate news templates often render a compact byline such
                // as "Chart Industries | May 7, 2025". Accept only the final
                // delimiter-bounded field so ordinary prose containing a date
                // cannot become publication metadata.
                [" | ", " • ", " — ", " – "]
                    .into_iter()
                    .filter_map(|delimiter| text.rsplit_once(delimiter))
                    .map(|(_, suffix)| suffix.trim())
                    .find_map(parse_article_datetime)
            })
        })
        .collect::<Vec<_>>();
    dates.sort();
    dates.dedup();
    if dates.len() == 1 { dates.pop() } else { None }
}

fn parse_article_datetime(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    let normalized_dotted_month = normalize_dotted_abbreviated_month_date(value);
    let parsed = DateTime::parse_from_rfc3339(value)
        .or_else(|_| DateTime::parse_from_rfc2822(value))
        .map(|value| value.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            [
                "%Y-%m-%dT%H:%M:%S%.f",
                "%Y-%m-%d %H:%M:%S%.f",
                "%d-%b-%Y %H:%M:%S",
                "%d-%B-%Y %H:%M:%S",
                "%d %b %Y %H:%M:%S",
                "%d %B %Y %H:%M:%S",
            ]
            .into_iter()
            .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
            .map(|value| value.and_utc())
        })
        .or_else(|| {
            std::iter::once(value)
                .chain(normalized_dotted_month.as_deref())
                .find_map(|candidate| {
                    [
                        "%Y-%m-%d",
                        "%B %d, %Y",
                        "%B %e, %Y",
                        "%b %d, %Y",
                        "%b %e, %Y",
                        "%B %d %Y",
                        "%B %e %Y",
                        "%b %d %Y",
                        "%b %e %Y",
                        "%d %B %Y",
                        "%e %B %Y",
                        "%d %b %Y",
                        "%e %b %Y",
                    ]
                    .into_iter()
                    .find_map(|format| NaiveDate::parse_from_str(candidate, format).ok())
                })
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .map(|value| value.and_utc())
        });
    parsed.filter(is_plausible_article_datetime)
}

fn normalize_dotted_abbreviated_month_date(value: &str) -> Option<String> {
    let (month, remainder) = value.split_once('.')?;
    if !remainder.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let month = match month.to_ascii_lowercase().as_str() {
        "jan" => "Jan",
        "feb" => "Feb",
        "mar" => "Mar",
        "apr" => "Apr",
        "may" => "May",
        "jun" => "Jun",
        "jul" => "Jul",
        "aug" => "Aug",
        "sep" | "sept" => "Sep",
        "oct" => "Oct",
        "nov" => "Nov",
        "dec" => "Dec",
        _ => return None,
    };
    Some(format!("{month}{remainder}"))
}

fn is_plausible_article_datetime(value: &DateTime<Utc>) -> bool {
    (1990..=Utc::now().year().saturating_add(2)).contains(&value.year())
}

fn json_ld_article_published_at(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::Array(values) => values.iter().find_map(json_ld_article_published_at),
        Value::Object(object) => {
            let own_date = object
                .get("@type")
                .is_some_and(json_ld_type_is_article)
                .then(|| object.get("datePublished"))
                .flatten()
                .and_then(Value::as_str)
                .and_then(parse_article_datetime);
            own_date.or_else(|| object.values().find_map(json_ld_article_published_at))
        }
        _ => None,
    }
}

fn json_ld_web_page_published_at(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::Array(values) => values.iter().find_map(json_ld_web_page_published_at),
        Value::Object(object) => {
            let own_date = object
                .get("@type")
                .is_some_and(json_ld_type_is_web_page)
                .then(|| object.get("datePublished"))
                .flatten()
                .and_then(Value::as_str)
                .and_then(parse_article_datetime);
            own_date.or_else(|| object.values().find_map(json_ld_web_page_published_at))
        }
        _ => None,
    }
}

fn json_ld_type_is_article(value: &Value) -> bool {
    match value {
        Value::String(value) => {
            let value = value.to_ascii_lowercase();
            [
                "article",
                "newsarticle",
                "blogposting",
                "liveblogposting",
                "socialmediaposting",
            ]
            .iter()
            .any(|kind| value == *kind || value.ends_with(&format!("/{kind}")))
        }
        Value::Array(values) => values.iter().any(json_ld_type_is_article),
        _ => false,
    }
}

fn json_ld_type_is_web_page(value: &Value) -> bool {
    match value {
        Value::String(value) => {
            let value = value.to_ascii_lowercase();
            value == "webpage" || value.ends_with("/webpage")
        }
        Value::Array(values) => values.iter().any(json_ld_type_is_web_page),
        _ => false,
    }
}

fn article_signals(
    document: &Html,
    url: &Url,
    has_article_element: bool,
    has_published_at: bool,
    has_listing_title_and_semantic_body: bool,
    has_semantic_body: bool,
) -> Result<Vec<&'static str>, ArticlePageError> {
    let mut signals = Vec::new();
    if has_article_element {
        signals.push("article_element");
    }
    if has_published_at {
        signals.push("published_time");
    }
    if first_attr(document, &[("meta[property='og:type']", "content")])?
        .is_some_and(|value| value.eq_ignore_ascii_case("article"))
    {
        signals.push("open_graph_article");
    }
    let json_ld_selector = selector("script[type='application/ld+json']")?;
    let has_article_json_ld = document.select(&json_ld_selector).any(|element| {
        serde_json::from_str::<Value>(&element.text().collect::<String>())
            .ok()
            .as_ref()
            .is_some_and(json_ld_contains_article)
    });
    if has_article_json_ld {
        signals.push("json_ld_article");
    }
    let h1_selector = selector("h1")?;
    if is_article_like_path(url) && document.select(&h1_selector).next().is_some() {
        signals.push("article_like_path_with_h1");
    }
    if is_listing_proven_article_like_path(url) && has_listing_title_and_semantic_body {
        signals.push("article_like_path_with_listing_title_and_semantic_body");
    }
    if is_article_like_path(url)
        && has_semantic_body
        && first_attr(
            document,
            &[
                ("meta[property='og:title']", "content"),
                ("meta[name='twitter:title']", "content"),
            ],
        )?
        .map(|title| normalize_article_title(&title))
        .is_some_and(|title| {
            is_usable_article_title(&title) && title_agrees_with_url_path(&title, url)
        })
    {
        signals.push("article_like_path_with_metadata_title_and_semantic_body");
    }
    Ok(signals)
}

fn json_ld_contains_article(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(json_ld_contains_article),
        Value::Object(object) => {
            object.get("@type").is_some_and(json_ld_type_is_article)
                || object.values().any(json_ld_contains_article)
        }
        _ => false,
    }
}

const ARTICLE_ROOTS: &[&str] = &[
    "announcement",
    "announcements",
    "article",
    "articles",
    "blog",
    "blog-post",
    "blog-posts",
    "blogs",
    "changelog",
    "changelogs",
    "company-news",
    "engineering",
    "insights",
    "in-the-news",
    "investor-news",
    "journal",
    "latest-news",
    "market-announcements",
    "media-center",
    "media-centre",
    "media-room",
    "news-and-media",
    "news",
    "news-and-events",
    "news-events",
    "news-release",
    "news-releases",
    "newsroom",
    "press",
    "press-center",
    "press-centre",
    "press-room",
    "press-release",
    "press-releases",
    "pressreleases",
    "pressroom",
    "post",
    "posts",
    "research",
    "stories",
    "updates",
    "what-s-new",
    "whats-new",
];

const LISTING_PROVEN_ARTICLE_ROOTS: &[&str] = &[
    "perspectives",
    "publications",
    "research-and-press",
    "resources",
];

const LISTING_SEGMENTS: &[&str] = &[
    "archive",
    "archives",
    "author",
    "authors",
    "cat",
    "categoria",
    "categorias",
    "category",
    "categories",
    "collection",
    "collections",
    "complete-archive",
    "content-type",
    "contributor",
    "contributors",
    "label",
    "label-name",
    "labels",
    "list",
    "lists",
    "page",
    "pagination",
    "pillar",
    "production-platform",
    "production_platform",
    "series",
    "tag",
    "tags",
    "tagged",
    "topic",
    "topics",
    "type",
    "types",
    "user",
    "users",
];

const LISTING_SEGMENT_PREFIXES: &[&str] = &[
    "author-",
    "category-",
    "category.",
    "filter-blog-",
    "page-",
    "tag-",
    "topic-",
];
const TERMINAL_LISTING_SEGMENTS: &[&str] = &[
    "about",
    "about-us",
    "accessibility",
    "accessibility-statement",
    "acquisitions",
    "all",
    "all-posts",
    "all-stories",
    "ambassadors",
    "analyst-coverage",
    "analysts",
    "api-success",
    "annual-filings",
    "annual-meeting",
    "annual-meeting-materials",
    "annual-general-meeting-materials",
    "annual-meetings",
    "annual-proxy",
    "annual-report",
    "annual-reports",
    "annual-reports-and-proxy-statements",
    "articles-list",
    "awards",
    "awards-and-recognition",
    "awards-recognition",
    "asset-alerts",
    "asset-library",
    "audited-financial-statements",
    "board-of-directors",
    "brand-guide",
    "brand-guides",
    "brand-resources",
    "capabilities-statement",
    "case-studies",
    "clinical-evidence",
    "clinical-trials",
    "code-examples",
    "code-of-conduct",
    "committee-composition",
    "committee-charters",
    "committees",
    "committees-board",
    "company-announcements",
    "company-fact-sheet",
    "company-overview",
    "company-presentations",
    "company-statements",
    "company-voices",
    "composites-value-proposition",
    "conferences-and-presentations",
    "congresses",
    "contact-us",
    "contact",
    "contact-the-board",
    "contact-ir",
    "contact-media-relations",
    "contacts",
    "corporate-governance",
    "corporate-governance-guidelines",
    "corporate-profile",
    "corporate-news",
    "corporate-press-kits",
    "clawback-policy",
    "cookie-policy",
    "customer-stories",
    "customers",
    "cve-analysis",
    "conditions-generales-dutilisation",
    "coverage",
    "dividend-history",
    "dossiers-de-presse",
    "donnees-personnelles",
    "document-center",
    "developer-spotlight",
    "earnings-and-news",
    "earnings-calls",
    "email-alerts",
    "email-alerts-and-rss-newsfeeds",
    "emergency-resource-center",
    "employee-stories",
    "editorial-policy",
    "event-calendar",
    "events",
    "events-and-presentations",
    "events-calendar",
    "events-presentations",
    "executive-management",
    "fact-center",
    "fact-sheets",
    "featured-videos",
    "financial-information",
    "financial-news",
    "financial-releases",
    "financials",
    "filings-and-reports",
    "frequently-asked-questions",
    "general-news",
    "governance",
    "governance-documents",
    "governance-overview",
    "glossary",
    "image-gallery",
    "image-library",
    "infographics",
    "industrial-business",
    "informative-resources",
    "information-request-form",
    "investor-contacts",
    "investor-email-alerts",
    "investor-faqs",
    "investor-conferences",
    "investor-overview",
    "investor-presentations",
    "investor-resources",
    "investors",
    "why-invest",
    "ir-calendar",
    "ir-updates",
    "key-articles",
    "leadership",
    "latest-press-releases",
    "latest-stories",
    "leadership-perspectives",
    "logo-gallery",
    "management",
    "media",
    "media-contacts",
    "media-hub",
    "media-kit",
    "media-assets",
    "media-and-analyst-contacts",
    "media-and-tpa-contacts",
    "media-center-search",
    "media-library",
    "media-coverage",
    "media-faqs",
    "media-gallery",
    "media-inquiry-form",
    "media-inquiries",
    "media-logos",
    "media-relations",
    "media-relations-contacts",
    "media-resources",
    "media-releases",
    "media-toolkit",
    "media-centre-archives",
    "media-center-archives",
    "news-press-center",
    "multimedia",
    "multimedia-library",
    "newsletter",
    "newsletters",
    "news-alerts",
    "news-and-insights",
    "news-and-media",
    "news-and-stories",
    "news-articles",
    "news-archive",
    "news-search",
    "non-gaap-reconciliation",
    "officers-directors",
    "our-approach",
    "our-company",
    "our-solutions",
    "overview",
    "our-events",
    "partnering-news",
    "partnerships",
    "past-events",
    "performance-and-aftermarket",
    "performance-report",
    "photos-and-videos",
    "podcasts",
    "presentations",
    "presentations-and-webcasts",
    "presentations-events",
    "presentations-reports",
    "press-room",
    "press-contacts",
    "press-release-archive",
    "press-details",
    "press-kits",
    "product-pipeline",
    "product-pipeline-media-resources",
    "product-press-kits",
    "product-reviews",
    "product-updates",
    "publications",
    "quarterly-earnings",
    "quarterly-earnings-materials",
    "quarterly-results",
    "reactions",
    "recent-post",
    "recent-posts",
    "release-details",
    "releases",
    "reports-and-releases",
    "reseaux-sociaux",
    "resources",
    "revues-de-presse",
    "sec-filings",
    "search",
    "shareholder-services",
    "stockholder-faqs",
    "stock-tax-information",
    "senior-management",
    "site-map",
    "sitemap",
    "social-channels",
    "social-media-disclosure",
    "social-media",
    "social_media.html",
    "spaces-media",
    "stay-connected",
    "stock-information",
    "tax-documents",
    "tech-and-engineering",
    "statements",
    "snapshots",
    "subscribe",
    "subscribe-press-releases",
    "subscribe-to-news",
    "subscriptions",
    "success-stories",
    "solution-briefs",
    "sustainability",
    "sustainability-leadership",
    "terms-of-use",
    "thought-leadership",
    "trending-topics",
    "webinars",
    "white-papers",
    "whistleblower-hotline",
    "updates-and-statements",
    "unsubscribe",
    "view-all",
    "video-hub",
    "video-library",
    "webcasts",
    "whitepapers",
    "xbrl-files",
];

fn is_listing_segment(segment: &str) -> bool {
    LISTING_SEGMENTS.contains(&segment)
        || LISTING_SEGMENT_PREFIXES
            .iter()
            .any(|prefix| segment.starts_with(prefix))
}

fn semantic_path_segments(url: &Url) -> Vec<String> {
    let mut segments = url
        .path_segments()
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    if !has_meaningful_query(url)
        && segments.last().is_some_and(|segment| {
            matches!(
                segment.as_str(),
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
        })
    {
        segments.pop();
    }
    if let Some(last) = segments.last_mut()
        && let Some(stem) = [".aspx", ".html", ".asp", ".htm", ".php"]
            .iter()
            .find_map(|suffix| last.strip_suffix(suffix))
    {
        *last = stem.to_owned();
    }
    segments
}

fn is_obvious_article_listing_path(url: &Url) -> bool {
    if has_invalid_resource_query(url) {
        return true;
    }
    // A bounded, validated editorial resource query is the detail identity for
    // many legacy CMSs even when the path itself is a collection document
    // such as `news-releases.html`. Invalid/empty identifiers remain rejected
    // above, while filter, locale, analytics, and pagination noise never enter
    // `meaningful_query_pairs`.
    if has_meaningful_query(url) {
        return false;
    }
    let segments = semantic_path_segments(url);
    if segments.is_empty() && !has_meaningful_query(url) {
        return true;
    }
    if segments.iter().any(|segment| is_listing_segment(segment)) {
        return true;
    }
    if segments.windows(2).any(|pair| {
        matches!(pair[0].as_str(), "blog" | "blogs") && matches!(pair[1].as_str(), "hub" | "hubs")
    }) {
        return true;
    }
    if let [.., year, month] = segments.as_slice()
        && year.len() == 4
        && year.bytes().all(|byte| byte.is_ascii_digit())
        && month
            .parse::<u8>()
            .is_ok_and(|month| (1..=12).contains(&month))
    {
        return true;
    }
    if segments.last().is_some_and(|segment| {
        segment
            .strip_prefix('p')
            .is_some_and(|page| !page.is_empty() && page.bytes().all(|byte| byte.is_ascii_digit()))
    }) {
        return true;
    }
    if let [.., parent, last] = segments.as_slice()
        && last.ends_with("-list")
        && (parent.ends_with("latest-news")
            || matches!(parent.as_str(), "multimedia" | "photos" | "videos")
                && last == &format!("{parent}-list"))
    {
        return true;
    }
    if segments.last().is_some_and(|segment| {
        TERMINAL_LISTING_SEGMENTS.contains(&segment.as_str())
            || segment.starts_with("news-archive-")
            || is_year_named_archive_segment(segment)
            || (segments
                .iter()
                .any(|ancestor| matches!(ancestor.as_str(), "investor-relations" | "investors"))
                && (segment.starts_with("why-own-")
                    || (segment.ends_with("-ownership-restriction")
                        && segment.split('-').count() <= 4)))
    }) {
        return true;
    }
    let Some(root_position) = segments
        .iter()
        .position(|segment| ARTICLE_ROOTS.contains(&segment.as_str()))
    else {
        return false;
    };
    let trailing = &segments[root_position + 1..];
    trailing.is_empty()
        || trailing.last().is_some_and(|segment| {
            ARTICLE_ROOTS.contains(&segment.as_str())
                || is_listing_segment(segment)
                || TERMINAL_LISTING_SEGMENTS.contains(&segment.as_str())
        })
}

fn is_year_named_archive_segment(segment: &str) -> bool {
    let is_year = |value: &str| value.len() == 4 && value.bytes().all(|byte| byte.is_ascii_digit());
    segment.strip_suffix("-news-archive").is_some_and(is_year)
        || [
            "news-release-",
            "news-releases-",
            "press-release-",
            "press-releases-",
        ]
        .iter()
        .any(|prefix| segment.strip_prefix(prefix).is_some_and(is_year))
        || [
            "-news-release",
            "-news-releases",
            "-press-release",
            "-press-releases",
        ]
        .iter()
        .any(|suffix| segment.strip_suffix(suffix).is_some_and(is_year))
}

fn is_weak_collection_page(url: &Url, title: &str) -> bool {
    let title = normalize(title).to_ascii_lowercase();
    let word_count = title.split_whitespace().count();
    if title.is_empty()
        || word_count > 6
        || title.chars().any(|character| character.is_ascii_digit())
    {
        return false;
    }
    if title == "the blog"
        || ["browse all ", "recent ", "see all ", "view all "]
            .iter()
            .any(|prefix| title.starts_with(prefix))
    {
        return true;
    }
    let has_site_separator = [" | ", " - ", " – ", " — ", ": "]
        .iter()
        .any(|separator| title.contains(separator));
    let ends_with_collection_noun = [
        " articles",
        " blog",
        " coverage",
        " news",
        " press releases",
        " stories",
    ]
    .iter()
    .any(|suffix| title.ends_with(suffix));
    if ends_with_collection_noun && !has_site_separator {
        return true;
    }
    let Some((section, site_title)) = title.split_once(" - ") else {
        return false;
    };
    if section.split_whitespace().count() > 2 || !site_title.ends_with(" blog") {
        return false;
    }
    semantic_path_segments(url)
        .last()
        .map(|segment| segment.replace(['-', '_'], " "))
        .is_some_and(|segment| normalize(&segment).eq_ignore_ascii_case(section))
}

fn is_short_multi_article_collection(
    url: &Url,
    title: &str,
    article_count: usize,
    content_chars: usize,
    link_count: usize,
) -> bool {
    if article_count < 4 {
        return false;
    }
    let normalized_title = normalize(title);
    if normalized_title.is_empty()
        || normalized_title.split_whitespace().count() > 3
        || normalized_title
            .chars()
            .any(|character| character.is_ascii_digit())
    {
        return false;
    }
    let Some(terminal_segment) = semantic_path_segments(url).last().cloned() else {
        return false;
    };
    if semantic_label_key(&normalized_title) != semantic_label_key(&terminal_segment) {
        return false;
    }

    content_chars < 500 || (article_count >= 10 && link_count >= 20)
}

fn is_listing_hint_over_collection_page(
    url: &Url,
    title_source: &str,
    replaced_page_title: Option<&str>,
    article_count: usize,
) -> bool {
    if article_count < 4 || title_source != "listing_anchor" {
        return false;
    }
    let Some(page_title) = replaced_page_title.filter(|title| is_generic_listing_title(title))
    else {
        return false;
    };
    let Some(terminal_segment) = semantic_path_segments(url).last().cloned() else {
        return false;
    };
    semantic_label_key(page_title) == semantic_label_key(&terminal_segment)
}

fn is_card_grid_without_primary_article(
    article_count: usize,
    article_elements_with_h1: usize,
    max_article_content_chars: usize,
) -> bool {
    article_count >= 10 && article_elements_with_h1 == 0 && max_article_content_chars < 1_000
}

fn is_multi_heading_card_grid(
    article_count: usize,
    article_elements_with_h1: usize,
    content_chars: usize,
) -> bool {
    article_count >= 10 && article_elements_with_h1 >= 4 && content_chars < 1_500
}

struct ShallowCollectionEvidence<'a> {
    article_count: usize,
    article_elements_with_h1: usize,
    max_article_content_chars: usize,
    content_chars: usize,
    link_count: usize,
    body_selector: &'a str,
    body_text: &'a str,
}

fn is_shallow_collection_hub(
    url: &Url,
    title: &str,
    listing_title_hint: Option<&str>,
    evidence: ShallowCollectionEvidence<'_>,
) -> bool {
    let ShallowCollectionEvidence {
        article_count,
        article_elements_with_h1,
        max_article_content_chars,
        content_chars,
        link_count,
        body_selector,
        body_text,
    } = evidence;
    let segments = semantic_path_segments(url);
    let Some(root_position) = segments
        .iter()
        .position(|segment| ARTICLE_ROOTS.contains(&segment.as_str()))
    else {
        return false;
    };
    if segments.len() != root_position + 2 {
        return false;
    }
    let normalized_body = normalize(body_text).to_ascii_lowercase();
    if link_count >= 10 && normalized_body.starts_with("showing posts ") {
        return true;
    }
    let explicit_filter_collection = article_count == 0
        && article_elements_with_h1 == 0
        && link_count >= 20
        && normalized_body.contains("there is a lack of results to match selected filters")
        && normalized_body.contains("please adjust the filter options to broaden results");
    let navigation_index = article_count == 0
        && article_elements_with_h1 == 0
        && link_count >= 20
        && normalized_body.starts_with("news releases topics media contacts ");
    if explicit_filter_collection || navigation_index {
        return true;
    }
    let title_is_short = {
        let title = normalize(title);
        !title.is_empty()
            && title.split_whitespace().count() <= 6
            && !title.chars().any(|character| character.is_ascii_digit())
    };
    if !title_is_short {
        return false;
    }
    let short_listing_title = listing_title_hint.is_some_and(|hint| {
        let hint = normalize(hint);
        !hint.is_empty()
            && hint.split_whitespace().count() <= 4
            && !hint.chars().any(|character| character.is_ascii_digit())
    });
    let all_prefixed_listing_grid = article_count >= 4
        && listing_title_hint.is_some_and(|hint| {
            let normalized_hint = normalize(hint);
            normalized_hint
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("all "))
                && semantic_label_key(&normalized_hint[4..]) == semantic_label_key(title)
                && segments
                    .last()
                    .is_some_and(|segment| semantic_label_key(segment) == semantic_label_key(title))
        });
    let generic_cluster_card_grid = article_count >= 10
        && content_chars < 1_500
        && body_selector == "generic:paragraph-cluster.v1"
        && short_listing_title;
    let featured_article_pager = content_chars < 1_000
        && normalized_body.contains("featured articles")
        && normalized_body.contains("previous page")
        && normalized_body.contains("next page");
    let wrapped_article_card_collection = article_count <= 1
        && article_elements_with_h1 == 0
        && max_article_content_chars < 1_000
        && content_chars < 2_000
        && link_count >= 15
        && normalized_body
            .match_indices("read article")
            .take(4)
            .count()
            == 4;
    let repeated_read_more_collection = article_count == 0
        && article_elements_with_h1 == 0
        && body_selector == "generic:paragraph-cluster.v1"
        && link_count >= 8
        && normalized_body.match_indices("read more").take(7).count() == 7;

    all_prefixed_listing_grid
        || generic_cluster_card_grid
        || featured_article_pager
        || wrapped_article_card_collection
        || repeated_read_more_collection
}

fn semantic_label_key(value: &str) -> String {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty() && !token.eq_ignore_ascii_case("and"))
        .flat_map(|token| token.chars().flat_map(char::to_lowercase))
        .collect()
}

fn is_breadcrumb_prefixed_collection_title(url: &Url, title: &str) -> bool {
    let segments = semantic_path_segments(url);
    let Some(root_position) = segments
        .iter()
        .position(|segment| ARTICLE_ROOTS.contains(&segment.as_str()))
    else {
        return false;
    };
    if segments.len() < root_position + 3 {
        return false;
    }
    let section_prefix = normalize(
        &segments[root_position..segments.len() - 1]
            .iter()
            .map(|segment| segment.replace(['-', '_'], " "))
            .collect::<Vec<_>>()
            .join(" "),
    )
    .to_ascii_lowercase();
    let title = normalize(title).to_ascii_lowercase();
    title
        .strip_prefix(&format!("{section_prefix} "))
        .is_some_and(|remainder| {
            !remainder.is_empty()
                && remainder.split_whitespace().count() <= 6
                && title.split_whitespace().count() <= 10
        })
}

fn is_localized_publication_root(url: &Url, title: &str, link_count: usize) -> bool {
    let segments = semantic_path_segments(url);
    let Some(root_position) = segments
        .iter()
        .position(|segment| ARTICLE_ROOTS.contains(&segment.as_str()))
    else {
        return false;
    };
    let trailing = &segments[root_position + 1..];
    if trailing.len() != 1
        || !looks_like_locale_path_segment(&trailing[0])
        || !url.path().ends_with('/')
    {
        return false;
    }
    let has_locale_query = url.query_pairs().any(|(key, _)| {
        matches!(
            key.trim().to_ascii_lowercase().as_str(),
            "lang" | "language" | "language_id" | "locale"
        )
    });
    let title = normalize(title).to_ascii_lowercase();
    let has_collection_title = [
        " blog",
        " newsroom",
        " press",
        " press room",
        " presse",
        " espace presse",
    ]
    .iter()
    .any(|suffix| title == suffix.trim() || title.ends_with(suffix));
    has_locale_query || link_count >= 20 || has_collection_title
}

fn is_counted_taxonomy_collection_title(url: &Url, title: &str, link_count: usize) -> bool {
    if link_count < 100 {
        return false;
    }
    let segments = semantic_path_segments(url);
    let Some(root_position) = segments
        .iter()
        .position(|segment| ARTICLE_ROOTS.contains(&segment.as_str()))
    else {
        return false;
    };
    if segments.len() != root_position + 2 {
        return false;
    }
    let title = normalize(title);
    if title.split_whitespace().count() > 6 || !title.ends_with(')') {
        return false;
    }
    title
        .rfind('(')
        .and_then(|offset| title[offset + 1..title.len() - 1].parse::<u16>().ok())
        .is_some_and(|count| count <= 1_000)
}

fn looks_like_locale_path_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    (bytes.len() == 2 && bytes.iter().all(u8::is_ascii_alphabetic))
        || (bytes.len() == 5
            && matches!(bytes[2], b'-' | b'_')
            && bytes[..2].iter().all(u8::is_ascii_alphabetic)
            && bytes[3..].iter().all(u8::is_ascii_alphabetic))
}

fn is_high_link_density_collection(
    title: &str,
    link_count: usize,
    content_chars: usize,
    body_selector: &str,
    has_publication_date: bool,
) -> bool {
    let has_short_title = normalize(title).split_whitespace().count() <= 6;
    has_short_title
        && ((link_count >= 20
            && link_count.saturating_mul(1_000) >= content_chars.saturating_mul(15))
            || (!has_publication_date
                && body_selector == "generic:paragraph-cluster.v1"
                && link_count >= 50
                && link_count.saturating_mul(1_000) >= content_chars.saturating_mul(8)))
}

fn is_undated_short_slug_topic_collection(
    url: &Url,
    title: &str,
    link_count: usize,
    content_chars: usize,
    has_publication_date: bool,
) -> bool {
    if has_publication_date {
        return false;
    }
    let normalized_title = normalize(title);
    let title_word_count = normalized_title.split_whitespace().count();
    if normalized_title.is_empty()
        || title_word_count > 4
        || normalized_title
            .chars()
            .any(|character| character.is_ascii_digit())
    {
        return false;
    }
    let Some(terminal_segment) = semantic_path_segments(url).last().cloned() else {
        return false;
    };
    if semantic_label_key(&normalized_title) != semantic_label_key(&terminal_segment) {
        return false;
    }
    (title_word_count <= 3 && content_chars < 400)
        || (link_count >= 8 && link_count.saturating_mul(1_000) >= content_chars.saturating_mul(8))
}

fn is_undated_listing_anchor_collection(
    title: &str,
    title_source: &str,
    link_count: usize,
    content_chars: usize,
    has_publication_date: bool,
) -> bool {
    if has_publication_date || title_source != "listing_anchor" {
        return false;
    }
    let normalized_title = normalize(title);
    !normalized_title.is_empty()
        && normalized_title.split_whitespace().count() <= 6
        && link_count >= 10
        && link_count.saturating_mul(1_000) >= content_chars.saturating_mul(10)
}

fn count_embedded_navigation_links(
    body_html: &str,
    base_url: &Url,
) -> Result<usize, ArticlePageError> {
    let document = Html::parse_fragment(body_html);
    let selector =
        selector("data[value], [data-href], [data-url], [data-link], [data-target-url]")?;
    let mut urls = HashSet::new();
    for element in document.select(&selector) {
        for attribute in [
            "value",
            "data-href",
            "data-url",
            "data-link",
            "data-target-url",
        ] {
            let Some(value) = element
                .value()
                .attr(attribute)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let Ok(mut url) = base_url.join(value) else {
                continue;
            };
            if !matches!(url.scheme(), "http" | "https") || !same_url_origin(base_url, &url) {
                continue;
            }
            url.set_fragment(None);
            if url.path().trim_end_matches('/') == base_url.path().trim_end_matches('/')
                && url.query() == base_url.query()
            {
                continue;
            }
            urls.insert(url.to_string());
        }
    }
    Ok(urls.len())
}

fn is_undated_embedded_navigation_collection(
    title: &str,
    embedded_navigation_link_count: usize,
    has_publication_date: bool,
) -> bool {
    if has_publication_date || embedded_navigation_link_count < 8 {
        return false;
    }
    let title = normalize(title);
    !title.is_empty()
        && title.split_whitespace().count() <= 6
        && !title.chars().any(|character| character.is_ascii_digit())
}

fn is_year_archive_collection(url: &Url, title: &str) -> bool {
    let segments = semantic_path_segments(url);
    if let [.., year, month] = segments.as_slice()
        && year.len() == 4
        && year.chars().all(|character| character.is_ascii_digit())
        && month
            .parse::<u8>()
            .is_ok_and(|month| (1..=12).contains(&month))
    {
        let normalized_title = normalize(title).to_ascii_lowercase();
        let month_names = [
            "january",
            "february",
            "march",
            "april",
            "may",
            "june",
            "july",
            "august",
            "september",
            "october",
            "november",
            "december",
        ];
        if month_names
            .iter()
            .any(|month_name| normalized_title == format!("{month_name} {year}"))
        {
            return true;
        }
    }
    let Some(year) = segments.last() else {
        return false;
    };
    if year.len() != 4 || !year.chars().all(|character| character.is_ascii_digit()) {
        return false;
    }
    let normalized_title = normalize(title).to_ascii_lowercase();
    let contains_archive_marker = normalized_title
        .split(|character: char| !character.is_alphanumeric())
        .any(|word| matches!(word, "archive" | "archives"));
    let is_explicit_press_release_archive = normalized_title == format!("press releases in {year}");
    let is_static_archive_cta = normalized_title == "search for more";
    let is_short_newsroom_title = normalized_title.split_whitespace().count() <= 4
        && (normalized_title == "newsroom" || normalized_title.ends_with(" newsroom"));
    let is_year_prefixed_brand_title = [" - ", " – ", " — ", " | ", ": "].iter().any(|separator| {
        normalized_title
            .strip_prefix(&format!("{year}{separator}"))
            .is_some_and(|suffix| !suffix.is_empty() && suffix.split_whitespace().count() <= 6)
    });
    if contains_archive_marker
        || is_explicit_press_release_archive
        || is_static_archive_cta
        || is_short_newsroom_title
        || is_year_prefixed_brand_title
    {
        return true;
    }
    let mut without_year = normalized_title.clone();
    if let Some(year_offset) = without_year.find(year) {
        without_year.replace_range(year_offset..year_offset + year.len(), "");
    }
    let remainder = without_year.trim_matches(|character: char| {
        character.is_whitespace() || matches!(character, '|' | ':' | '-' | '–' | '—')
    });
    is_generic_listing_title(remainder)
        || matches!(
            remainder,
            "archive" | "archives" | "news archive" | "news archives" | "year archive"
        )
}

fn is_article_like_path(url: &Url) -> bool {
    let segments = semantic_path_segments(url);
    let Some(root_position) = segments
        .iter()
        .position(|segment| ARTICLE_ROOTS.contains(&segment.as_str()))
    else {
        return is_root_level_article_on_dedicated_editorial_host(url);
    };
    let trailing = &segments[root_position + 1..];
    !is_obvious_article_listing_path(url)
        && trailing
            .last()
            .is_some_and(|segment| segment.chars().any(char::is_alphabetic))
}

fn is_listing_proven_article_like_path(url: &Url) -> bool {
    if is_article_like_path(url) {
        return true;
    }
    // Some official publications use collection names such as `resources` or
    // `perspectives` as their editorial namespace. They remain collection roots
    // for every other signal: only an independently observed listing title plus
    // a qualified semantic body may promote one of their detail descendants.
    let segments = semantic_path_segments(url);
    if let [.., root, item_id] = segments.as_slice()
        && root == "updates"
        && !item_id.is_empty()
        && item_id.bytes().all(|byte| byte.is_ascii_digit())
        && item_id.parse::<u64>().is_ok_and(|item_id| item_id > 0)
        && !is_obvious_article_listing_path(url)
    {
        return true;
    }
    let Some(root_position) = segments
        .iter()
        .position(|segment| LISTING_PROVEN_ARTICLE_ROOTS.contains(&segment.as_str()))
    else {
        return false;
    };
    let trailing = &segments[root_position + 1..];
    !is_obvious_article_listing_path(url)
        && trailing.last().is_some_and(|slug| {
            !TERMINAL_LISTING_SEGMENTS.contains(&slug.as_str())
                && slug.chars().any(char::is_alphabetic)
        })
}

fn is_root_level_article_on_dedicated_editorial_host(url: &Url) -> bool {
    let normalized_url_host = normalized_host(url.host_str().unwrap_or_default());
    let Some(host_label) = normalized_url_host.split('.').next() else {
        return false;
    };
    if !matches!(
        host_label,
        "blog"
            | "blogs"
            | "engineering"
            | "insights"
            | "journal"
            | "media"
            | "news"
            | "newsroom"
            | "press"
            | "research"
            | "stories"
            | "updates"
    ) {
        return false;
    }
    let segments = semantic_path_segments(url);
    let [slug] = segments.as_slice() else {
        return false;
    };
    !is_listing_segment(slug)
        && !TERMINAL_LISTING_SEGMENTS.contains(&slug.as_str())
        && slug
            .split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|token| !token.is_empty())
            .count()
            >= 2
}

fn raw_item_matches_publication(
    item: &RawCrawlItem,
    requested_publication_url: &Url,
    final_publication_url: &Url,
) -> bool {
    [&item.url, item.canonical_url.as_ref().unwrap_or(&item.url)]
        .into_iter()
        .any(|item_url| {
            same_publication_resource(item_url, requested_publication_url)
                || same_publication_resource(item_url, final_publication_url)
        })
}

fn same_publication_resource(left: &Url, right: &Url) -> bool {
    publication_resource_key(left) == publication_resource_key(right)
}

fn publication_resource_key(url: &Url) -> String {
    let host = url.host_str().map(normalized_host).unwrap_or_default();
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    let mut segments = url
        .path_segments()
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let mut query_pairs = meaningful_query_pairs(url);
    if query_pairs.is_empty()
        && segments.last().is_some_and(|segment| {
            matches!(
                segment.as_str(),
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
        })
    {
        segments.pop();
    }
    query_pairs.sort();
    let query = if query_pairs.is_empty() {
        String::new()
    } else {
        format!(
            "?{}",
            query_pairs
                .into_iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("&")
        )
    };
    format!("{host}{port}/{}{query}", segments.join("/"))
}

fn has_meaningful_query(url: &Url) -> bool {
    !meaningful_query_pairs(url).is_empty()
}

fn meaningful_query_pairs(url: &Url) -> Vec<(String, String)> {
    resource_query_pairs(url)
}

fn extract_canonical_url(document: &Html, base_url: &Url) -> Result<Url, ArticlePageError> {
    let Some(value) = first_attr(document, &[("link[rel='canonical']", "href")])? else {
        return Ok(base_url.clone());
    };
    let canonical =
        base_url
            .join(value.trim())
            .map_err(|_| ArticlePageError::InvalidCanonicalUrl {
                url: base_url.clone(),
            })?;
    validate_article_fetch_url(&canonical)?;
    Ok(canonical)
}

fn should_replace_site_root_canonical(final_url: &Url, canonical_url: &Url) -> bool {
    normalized_host(final_url.host_str().unwrap_or_default())
        == normalized_host(canonical_url.host_str().unwrap_or_default())
        && final_url.port_or_known_default() == canonical_url.port_or_known_default()
        && canonical_url.path().trim_matches('/').is_empty()
        && !has_meaningful_query(canonical_url)
        && is_article_like_path(final_url)
}

fn is_malformed_embedded_scheme_canonical(canonical_url: &Url) -> bool {
    canonical_url
        .host_str()
        .is_some_and(|host| matches!(host.to_ascii_lowercase().as_str(), "http" | "https"))
}

fn has_explicit_archive_heading(
    document: &Html,
    extracted_title: &str,
) -> Result<bool, ArticlePageError> {
    let heading_selector = selector("h1")?;
    Ok(document.select(&heading_selector).any(|heading| {
        let has_archive_marker = ["class", "id"].into_iter().any(|attribute| {
            heading
                .value()
                .attr(attribute)
                .map(str::to_ascii_lowercase)
                .is_some_and(|value| {
                    [
                        "archive-title",
                        "archive_title",
                        "archive__title",
                        "category-title",
                        "category_title",
                        "category__title",
                        "taxonomy-title",
                        "taxonomy_title",
                        "taxonomy__title",
                    ]
                    .iter()
                    .any(|marker| value.contains(marker))
                })
        });
        has_archive_marker
            && normalize_article_title(&heading.text().collect::<Vec<_>>().join(" "))
                == extracted_title
    }))
}

fn first_attr(
    document: &Html,
    candidates: &[(&str, &str)],
) -> Result<Option<String>, ArticlePageError> {
    for (query, attribute) in candidates {
        let selector = selector(query)?;
        if let Some(value) = document
            .select(&selector)
            .find_map(|element| element.value().attr(attribute))
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(Some(value.to_owned()));
        }
    }
    Ok(None)
}

fn selector(value: &str) -> Result<Selector, ArticlePageError> {
    Selector::parse(value).map_err(|_| {
        ArticlePageError::InvalidConfig(format!("invalid built-in article selector {value}"))
    })
}

async fn extract_recipe_links_off_runtime(
    base_url: Url,
    html: String,
    recipe: CompanyNewsRecipeSpec,
    hard_limit: usize,
) -> Result<(Vec<RecipeLink>, String), RecipeCrawlError> {
    tokio::task::spawn_blocking(move || extract_recipe_links(&base_url, &html, &recipe, hard_limit))
        .await
        .map_err(|error| {
            RecipeCrawlError::InvalidListing(format!("listing DOM extraction task failed: {error}"))
        })?
}

fn extract_recipe_links(
    base_url: &Url,
    html: &str,
    recipe: &CompanyNewsRecipeSpec,
    hard_limit: usize,
) -> Result<(Vec<RecipeLink>, String), RecipeCrawlError> {
    let document = Html::parse_document(html);
    let configured_selector = Selector::parse(&recipe.article_link_selector)
        .map_err(|_| RecipeCrawlError::InvalidSelector(recipe.article_link_selector.clone()))?;
    let nested_anchor_selector = Selector::parse("a[href]").map_err(|_| {
        RecipeCrawlError::InvalidConfig("built-in anchor selector failed".to_owned())
    })?;
    let nested_heading_selector =
        Selector::parse("h1,h2,h3,h4,h5,h6,[role='heading']").map_err(|_| {
            RecipeCrawlError::InvalidConfig("built-in heading selector failed".to_owned())
        })?;
    let image_alt_selector = Selector::parse("img[alt]").map_err(|_| {
        RecipeCrawlError::InvalidConfig("built-in image alt selector failed".to_owned())
    })?;
    let listing_date_selector = Selector::parse("time,p,span,div,[datetime]").map_err(|_| {
        RecipeCrawlError::InvalidConfig("built-in listing date selector failed".to_owned())
    })?;
    let mut urls = Vec::new();
    let mut seen = HashMap::new();
    let mut signatures = Vec::new();
    let limit = usize::try_from(recipe.max_links)
        .unwrap_or(usize::MAX)
        .min(hard_limit);
    // Large corporate blogs can place hundreds of category, product, and
    // audience links ahead of the actual article cards. Scan a wider but still
    // bounded candidate window so those taxonomy links can be demoted without
    // exhausting the article budget before the first current post.
    let scan_limit = limit.saturating_mul(50).min(2_000).max(limit);
    for element in document.select(&configured_selector) {
        let anchor = if element.value().attr("href").is_some() {
            element
        } else if let Some(anchor) = element.select(&nested_anchor_selector).next() {
            anchor
        } else {
            continue;
        };
        let Some(href) = anchor.value().attr("href").map(str::trim) else {
            continue;
        };
        if href.is_empty() || href.starts_with('#') || href.starts_with("javascript:") {
            continue;
        }
        let Ok(mut url) = base_url.join(href) else {
            continue;
        };
        url.set_fragment(None);
        if validate_article_fetch_url(&url).is_err() || !recipe_url_allowed(&url, recipe) {
            continue;
        }
        let anchor_text = normalize_article_title(&anchor.text().collect::<Vec<_>>().join(" "));
        let title_hint = anchor
            .select(&nested_heading_selector)
            .map(|heading| normalize_article_title(&heading.text().collect::<Vec<_>>().join(" ")))
            .filter(|title| is_usable_article_title(title))
            .max_by_key(|title| title.chars().count())
            .or_else(|| explicit_anchor_title_hint(anchor, &image_alt_selector))
            .or_else(|| {
                (anchor_text.is_empty() && has_bounded_listing_item_ancestor(anchor))
                    .then(|| nearest_unique_heading_hint(anchor, &nested_heading_selector))
                    .flatten()
            })
            .or_else(|| {
                (!anchor_text.is_empty() && !is_usable_article_title(&anchor_text))
                    .then(|| {
                        nearest_unique_heading_hint(anchor, &nested_heading_selector).or_else(
                            || nearest_preceding_heading_hint(anchor, &nested_heading_selector),
                        )
                    })
                    .flatten()
            })
            .or_else(|| is_usable_article_title(&anchor_text).then_some(anchor_text))
            .map(|title| normalize_article_title(&title));
        if let Some(title_hint) = &title_hint {
            let title_chars = title_hint.chars().count();
            if title_chars < usize::from(recipe.correctness.min_title_chars)
                || title_chars > usize::from(recipe.correctness.max_title_chars)
            {
                continue;
            }
        }
        let published_at_hint = extract_listing_date_hint(anchor, &listing_date_selector);
        let document_url = matching_listing_document_url(
            anchor,
            base_url,
            recipe,
            title_hint.as_deref(),
            &nested_anchor_selector,
        );
        let canonical = url.as_str().to_owned();
        if let Some(existing_index) = seen.get(&canonical).copied() {
            let existing: &mut RecipeLink = &mut urls[existing_index];
            if title_hint.as_ref().is_some_and(|candidate| {
                existing.title_hint.as_ref().is_none_or(|current| {
                    listing_title_hint_priority(candidate, &url)
                        > listing_title_hint_priority(current, &url)
                })
            }) {
                existing.title_hint = title_hint;
            }
            if existing.published_at_hint.is_none() {
                existing.published_at_hint = published_at_hint;
            }
            if existing.document_url.is_none() {
                existing.document_url = document_url;
            }
            continue;
        }
        seen.insert(canonical, urls.len());
        let class = anchor
            .value()
            .attr("class")
            .unwrap_or_default()
            .split_ascii_whitespace()
            .take(8)
            .collect::<Vec<_>>()
            .join(".");
        signatures.push(format!("{}|{class}", anchor.value().name()));
        urls.push(RecipeLink {
            url,
            title_hint,
            published_at_hint,
            document_url,
        });
        if urls.len() >= scan_limit {
            break;
        }
    }
    // `sort_by` is stable, so links with the same confidence retain their DOM order.
    // News listings normally use that order for recency. Do not rank numeric or deeper
    // paths ahead of flat paths: dated archive URLs would otherwise displace current
    // articles on sites that changed their URL scheme.
    urls.sort_by(|left, right| {
        article_candidate_priority(right).cmp(&article_candidate_priority(left))
    });
    urls.truncate(limit);
    signatures.sort();
    signatures.dedup();
    let structure_fingerprint = sha256_hex(signatures.join("\n").as_bytes());
    Ok((urls, structure_fingerprint))
}

fn listing_title_hint_priority(title: &str, url: &Url) -> (bool, usize) {
    (
        title_agrees_with_url_path(title, url),
        title.chars().count(),
    )
}

fn matching_listing_document_url(
    anchor: ElementRef<'_>,
    base_url: &Url,
    recipe: &CompanyNewsRecipeSpec,
    title_hint: Option<&str>,
    anchor_selector: &Selector,
) -> Option<Url> {
    let requested_url = base_url.join(anchor.value().attr("href")?.trim()).ok()?;
    let title_key = title_hint.map(normalized_title_key)?;
    for scope in anchor
        .ancestors()
        .take(4)
        .filter_map(ElementRef::wrap)
        .filter(|scope| is_bounded_listing_scope(*scope))
    {
        let mut matches = scope
            .select(anchor_selector)
            .filter_map(|document_anchor| {
                let href = document_anchor.value().attr("href")?.trim();
                let document_url = base_url.join(href).ok()?;
                let document_title =
                    normalize_article_title(&document_anchor.text().collect::<Vec<_>>().join(" "));
                (validate_article_fetch_url(&document_url).is_ok()
                    && recipe_host_allowed(&document_url, recipe)
                    && is_release_document_url(&document_url)
                    && !same_publication_resource(&document_url, &requested_url)
                    && is_usable_article_title(&document_title)
                    && normalized_title_key(&document_title) == title_key)
                    .then_some(document_url)
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        matches.dedup();
        if matches.len() == 1 {
            return matches.into_iter().next();
        }
        if !matches.is_empty() {
            return None;
        }
    }
    None
}

fn is_release_document_url(url: &Url) -> bool {
    url.path().to_ascii_lowercase().ends_with(".pdf")
}

fn normalize_double_encoded_article_candidates(
    candidates: &[RecipeLink],
) -> (Vec<RecipeLink>, Vec<ArticleFetchFailure>) {
    let original_urls = candidates
        .iter()
        .map(|candidate| candidate.url.as_str())
        .collect::<HashSet<_>>();
    let mut normalized = Vec::with_capacity(candidates.len());
    let mut failures = Vec::new();
    for candidate in candidates {
        let Some(repaired_url) = repair_double_encoded_utf8_url(&candidate.url) else {
            normalized.push(candidate.clone());
            continue;
        };
        if original_urls.contains(repaired_url.as_str()) {
            failures.push(ArticleFetchFailure {
                url: candidate.url.clone(),
                reason: "double_encoded_utf8_url_alias".to_owned(),
                retryable: false,
                error: format!(
                    "double-encoded UTF-8 URL duplicates the canonical listing URL {repaired_url}"
                ),
            });
            continue;
        }
        let mut repaired = candidate.clone();
        repaired.url = repaired_url;
        normalized.push(repaired);
    }
    (normalized, failures)
}

fn repair_double_encoded_utf8_url(url: &Url) -> Option<Url> {
    let repaired_path = repair_double_encoded_utf8_path(url.path())?;
    let mut repaired = url.clone();
    repaired.set_path(&repaired_path);
    (repaired != *url).then_some(repaired)
}

fn repair_double_encoded_utf8_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut repaired = String::with_capacity(path.len());
    let mut offset = 0;
    let mut changed = false;
    while offset < bytes.len() {
        let starts_double_encoded_byte = |offset: usize| {
            offset + 4 < bytes.len()
                && bytes[offset] == b'%'
                && bytes[offset + 1] == b'2'
                && bytes[offset + 2] == b'5'
                && bytes[offset + 3].is_ascii_hexdigit()
                && bytes[offset + 4].is_ascii_hexdigit()
        };
        if !starts_double_encoded_byte(offset) {
            repaired.push(char::from(bytes[offset]));
            offset += 1;
            continue;
        }

        let run_start = offset;
        let mut decoded = Vec::new();
        while starts_double_encoded_byte(offset) {
            let high = hex_value(bytes[offset + 3])?;
            let low = hex_value(bytes[offset + 4])?;
            decoded.push((high << 4) | low);
            offset += 5;
        }
        let valid_non_ascii_utf8 =
            decoded.iter().any(|byte| !byte.is_ascii()) && std::str::from_utf8(&decoded).is_ok();
        if !valid_non_ascii_utf8 {
            repaired.push_str(&path[run_start..offset]);
            continue;
        }
        for encoded_byte in path.as_bytes()[run_start..offset].chunks_exact(5) {
            repaired.push('%');
            repaired.push(char::from(encoded_byte[3]));
            repaired.push(char::from(encoded_byte[4]));
        }
        changed = true;
    }
    changed.then_some(repaired)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn explicit_anchor_title_hint(
    anchor: ElementRef<'_>,
    image_alt_selector: &Selector,
) -> Option<String> {
    for attribute in ["title", "aria-label", "data-title"] {
        if let Some(title) = anchor
            .value()
            .attr(attribute)
            .map(normalize_article_title)
            .filter(|title| is_usable_article_title(title))
        {
            return Some(title);
        }
    }

    let mut image_alts = anchor
        .select(image_alt_selector)
        .filter_map(|image| image.value().attr("alt"))
        .map(normalize_article_title)
        .filter(|title| is_usable_article_title(title) && !looks_like_visual_alt_caption(title))
        .collect::<Vec<_>>();
    image_alts.sort();
    image_alts.dedup();
    (image_alts.len() == 1)
        .then(|| image_alts.into_iter().next())
        .flatten()
}

fn looks_like_visual_alt_caption(title: &str) -> bool {
    let normalized = normalized_title_key(title);
    let words = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    if words.len() > 12 {
        return false;
    }
    words.iter().any(|word| {
        matches!(
            *word,
            "artwork"
                | "background"
                | "banner"
                | "diagram"
                | "graphic"
                | "graphics"
                | "icon"
                | "image"
                | "illustration"
                | "logo"
                | "painting"
                | "photo"
                | "photograph"
                | "picture"
                | "portrait"
                | "screenshot"
                | "seascape"
                | "thumbnail"
                | "watercolor"
        )
    }) || [
        "article cover",
        "blog cover",
        "cover image",
        "featured image",
        "hero image",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase))
}

fn nearest_unique_heading_hint(
    anchor: ElementRef<'_>,
    heading_selector: &Selector,
) -> Option<String> {
    for scope in anchor
        .ancestors()
        .take(4)
        .filter_map(ElementRef::wrap)
        .filter(|scope| is_bounded_listing_scope(*scope))
    {
        let mut headings = scope
            .select(heading_selector)
            .filter(|heading| !title_candidate_is_hidden(heading))
            .map(|heading| normalize_article_title(&heading.text().collect::<Vec<_>>().join(" ")))
            .filter(|heading| is_usable_article_title(heading))
            .collect::<Vec<_>>();
        headings.sort();
        headings.dedup();
        if headings.len() == 1 {
            return headings.into_iter().next();
        }
        if !headings.is_empty() {
            return None;
        }
    }
    None
}

fn has_bounded_listing_item_ancestor(anchor: ElementRef<'_>) -> bool {
    anchor
        .ancestors()
        .take(4)
        .filter_map(ElementRef::wrap)
        .any(|ancestor| {
            ancestor
                .value()
                .attr("role")
                .is_some_and(|role| role.eq_ignore_ascii_case("listitem"))
                || ancestor.value().attr("class").is_some_and(|classes| {
                    classes
                        .split_ascii_whitespace()
                        .any(|class| class.eq_ignore_ascii_case("w-dyn-item"))
                })
        })
}

fn nearest_preceding_heading_hint(
    anchor: ElementRef<'_>,
    heading_selector: &Selector,
) -> Option<String> {
    // Some listings put the headline in one element and a generic CTA in the
    // following paragraph:
    //
    // <h4>Company launches product</h4>
    // <p>July 1, 2026<br><a href="...">Read more</a></p>
    //
    // Inspect only the immediately preceding element sibling at each bounded
    // card level. Looking farther across siblings could borrow the headline
    // from a neighboring card.
    for scope in
        std::iter::once(anchor).chain(anchor.ancestors().take(3).filter_map(ElementRef::wrap))
    {
        let Some(sibling) = scope.prev_siblings().find_map(ElementRef::wrap) else {
            continue;
        };
        if let Some(heading) = unique_heading_hint(sibling, heading_selector) {
            return Some(heading);
        }
    }
    None
}

fn unique_heading_hint(element: ElementRef<'_>, heading_selector: &Selector) -> Option<String> {
    let name = element.value().name();
    let is_heading = matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
        || element
            .value()
            .attr("role")
            .is_some_and(|role| role.eq_ignore_ascii_case("heading"));
    if is_heading {
        return Some(normalize_article_title(
            &element.text().collect::<Vec<_>>().join(" "),
        ))
        .filter(|heading| is_usable_article_title(heading));
    }
    if !is_bounded_listing_scope(element) {
        return None;
    }

    let mut headings = element
        .select(heading_selector)
        .map(|heading| normalize_article_title(&heading.text().collect::<Vec<_>>().join(" ")))
        .filter(|heading| is_usable_article_title(heading))
        .collect::<Vec<_>>();
    headings.sort();
    headings.dedup();
    (headings.len() == 1)
        .then(|| headings.into_iter().next())
        .flatten()
}

fn extract_listing_date_hint(
    anchor: ElementRef<'_>,
    date_selector: &Selector,
) -> Option<DateTime<Utc>> {
    // Listing-card dates are useful fallback evidence when the article page
    // itself omits publication metadata. Stay within the anchor's nearest
    // few ancestors and require exactly one date in that scope so a whole
    // listing cannot accidentally donate a neighboring article's timestamp.
    for scope in anchor
        .ancestors()
        .take(4)
        .filter_map(ElementRef::wrap)
        .filter(|scope| is_bounded_listing_scope(*scope))
    {
        let mut candidates = scope
            .select(date_selector)
            .filter(|element| !title_candidate_is_hidden(element))
            .filter_map(|element| {
                element
                    .value()
                    .attr("datetime")
                    .and_then(parse_article_datetime)
                    .or_else(|| {
                        parse_article_datetime(&normalize(
                            &element.text().collect::<Vec<_>>().join(" "),
                        ))
                    })
            })
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        if candidates.len() == 1 {
            return candidates.into_iter().next();
        }
    }
    None
}

fn is_bounded_listing_scope(scope: ElementRef<'_>) -> bool {
    const MAX_LISTING_CARD_DESCENDANTS: usize = 512;

    scope
        .descendants()
        .take(MAX_LISTING_CARD_DESCENDANTS + 1)
        .count()
        <= MAX_LISTING_CARD_DESCENDANTS
}

fn article_candidate_priority(link: &RecipeLink) -> (bool, bool, bool) {
    let segments = semantic_path_segments(&link.url);
    let has_strong_detail_root = segments.iter().any(|segment| {
        matches!(
            segment.as_str(),
            "article" | "news-release" | "press-release"
        )
    });
    let has_embedded_taxonomy_marker = segments.iter().any(|segment| {
        matches!(
            segment.as_str(),
            "audience" | "audiences" | "content-type" | "product" | "products"
        ) || segment
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|token| {
                matches!(
                    token,
                    "author"
                        | "authors"
                        | "category"
                        | "categories"
                        | "tag"
                        | "tags"
                        | "topic"
                        | "topics"
                )
            })
    });
    (
        !is_weak_collection_page(&link.url, link.title_hint.as_deref().unwrap_or_default()),
        !has_embedded_taxonomy_marker,
        has_strong_detail_root || has_meaningful_query(&link.url),
    )
}

fn recipe_url_allowed(url: &Url, recipe: &CompanyNewsRecipeSpec) -> bool {
    if !recipe_host_allowed(url, recipe) {
        return false;
    }
    let path = url.path();
    if path.is_empty() || path == "/" {
        return false;
    }
    if is_obvious_article_listing_path(url) {
        return false;
    }
    if recipe
        .exclude_path_prefixes
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return false;
    }
    if recipe.include_path_prefixes.is_empty() {
        return is_article_like_path(url);
    }
    recipe.include_path_prefixes.iter().any(|prefix| {
        path.starts_with(prefix) && path.trim_end_matches('/') != prefix.trim_end_matches('/')
    })
}

fn recipe_host_allowed(url: &Url, recipe: &CompanyNewsRecipeSpec) -> bool {
    let Some(host) = url.host_str().map(normalized_host) else {
        return false;
    };
    let allowed_hosts = recipe
        .allowed_hosts
        .iter()
        .map(|allowed| normalized_host(allowed))
        .collect::<Vec<_>>();
    if allowed_hosts.contains(&host) {
        return true;
    }
    let related_to_allowed_host = allowed_hosts.iter().any(|allowed| {
        host.ends_with(&format!(".{allowed}")) || allowed.ends_with(&format!(".{host}"))
    });
    related_to_allowed_host && implicit_recipe_host_is_safe(url, &host)
}

fn implicit_recipe_host_is_safe(url: &Url, normalized_host: &str) -> bool {
    const NON_PRODUCTION_LABELS: &[&str] = &[
        "preview", "sandbox", "stage", "staging", "test", "testing", "uat",
    ];
    const UTILITY_LABELS: &[&str] = &[
        "developer",
        "developers",
        "doc",
        "docs",
        "documentation",
        "help",
        "support",
        "tutorial",
        "tutorials",
        "tutoriales",
    ];
    const EDITORIAL_PATH_TOKENS: &[&str] = &[
        "blog",
        "blogs",
        "changelog",
        "changelogs",
        "engineering",
        "insights",
        "news",
        "newsroom",
        "press",
        "release",
        "releases",
        "research",
        "stories",
        "updates",
    ];

    let host_label = normalized_host.split('.').next().unwrap_or_default();
    if NON_PRODUCTION_LABELS.contains(&host_label)
        || host_label
            .split(['-', '_'])
            .next_back()
            .is_some_and(|suffix| NON_PRODUCTION_LABELS.contains(&suffix))
        || host_label.ends_with("prod")
    {
        return false;
    }
    if !UTILITY_LABELS.contains(&host_label) {
        return true;
    }
    url.path()
        .split(['/', '-', '_'])
        .filter(|token| !token.is_empty())
        .any(|token| EDITORIAL_PATH_TOKENS.contains(&token.to_ascii_lowercase().as_str()))
}

fn group_article_candidates_by_host(candidates: &[RecipeLink]) -> Vec<Vec<(usize, RecipeLink)>> {
    let mut group_indexes = HashMap::new();
    let mut groups = Vec::<Vec<(usize, RecipeLink)>>::new();
    for (candidate_index, candidate) in candidates.iter().enumerate() {
        let host_key = candidate
            .url
            .host_str()
            .map(normalized_host)
            .unwrap_or_else(|| candidate.url.as_str().to_owned());
        let group_index = *group_indexes.entry(host_key).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[group_index].push((candidate_index, candidate.clone()));
    }
    groups
}

fn normalized_host(host: &str) -> String {
    host.trim().trim_start_matches("www.").to_ascii_lowercase()
}

fn validate_article_fetch_url(url: &Url) -> Result<(), ArticlePageError> {
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(ArticlePageError::UnsupportedUrl(url.clone()));
    }
    Ok(())
}

fn resolve_article_redirect(current_url: &Url, location: &str) -> Result<Url, ArticlePageError> {
    let mut next_url =
        current_url
            .join(location)
            .map_err(|_| ArticlePageError::InvalidRedirect {
                url: current_url.clone(),
            })?;
    if current_url.scheme() == "https"
        && next_url.scheme() == "http"
        && current_url.host_str() == next_url.host_str()
        && current_url.port() == next_url.port()
    {
        next_url
            .set_scheme("https")
            .map_err(|_| ArticlePageError::InvalidRedirect {
                url: current_url.clone(),
            })?;
    }
    Ok(next_url)
}

async fn validate_article_resolved_target(url: &Url) -> Result<(), ArticlePageError> {
    let Some(host) = url.host() else {
        return Err(ArticlePageError::UnsupportedUrl(url.clone()));
    };
    match host {
        url::Host::Ipv4(address) => validate_article_public_address(url, IpAddr::V4(address)),
        url::Host::Ipv6(address) => validate_article_public_address(url, IpAddr::V6(address)),
        url::Host::Domain(host) => {
            let port = url
                .port_or_known_default()
                .ok_or_else(|| ArticlePageError::UnsupportedUrl(url.clone()))?;
            let addresses = tokio::net::lookup_host((host, port))
                .await
                .map_err(|source| ArticlePageError::DnsResolution {
                    url: url.clone(),
                    source,
                })?
                .map(|address| address.ip())
                .collect::<Vec<_>>();
            if addresses.is_empty() {
                return Err(ArticlePageError::DnsResolutionEmpty { url: url.clone() });
            }
            for address in addresses {
                validate_article_public_address(url, address)?;
            }
            Ok(())
        }
    }
}

fn validate_article_public_address(url: &Url, address: IpAddr) -> Result<(), ArticlePageError> {
    if is_public_ip(address) {
        Ok(())
    } else {
        Err(ArticlePageError::PrivateNetwork {
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

#[derive(Debug, thiserror::Error)]
pub enum ArticlePageError {
    #[error("invalid article crawler configuration: {0}")]
    InvalidConfig(String),
    #[error(transparent)]
    Client(#[from] reqwest::Error),
    #[error("article request has {count} URLs, exceeding the limit of {limit}")]
    TooManyUrls { count: usize, limit: usize },
    #[error("unsupported public article URL: {0}")]
    UnsupportedUrl(Url),
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
    #[error("redirect from {url} is missing or invalid")]
    InvalidRedirect { url: Url },
    #[error("too many redirects while fetching {url}")]
    TooManyRedirects { url: Url },
    #[error("unsupported content type {content_type} for {url}")]
    UnsupportedContent { url: Url, content_type: String },
    #[error("page {url} has no generic article signal")]
    MissingArticleSignal { url: Url },
    #[error("page {url} is an obvious article-listing path")]
    ObviousListingPath { url: Url },
    #[error("page {url} has no generic article body")]
    MissingArticleBody { url: Url },
    #[error("page {url} has no usable title")]
    MissingTitle { url: Url },
    #[error("page {url} has generic listing title {title:?}")]
    GenericListingTitle { url: Url, title: String },
    #[error(
        "page {url} has listing-like link density ({link_count} links across {content_chars} content characters)"
    )]
    HighLinkDensityCollection {
        url: Url,
        link_count: usize,
        content_chars: usize,
    },
    #[error(
        "page {url} contains {article_count} article cards without individual-article metadata"
    )]
    MultipleArticleCollection { url: Url, article_count: usize },
    #[error("page {url} is a year archive rather than an individual article")]
    YearArchiveCollection { url: Url },
    #[error("page {url} has {content_chars} content characters; minimum is {minimum}")]
    InsufficientContent {
        url: Url,
        content_chars: usize,
        minimum: usize,
    },
    #[error("page {url} has an invalid canonical URL")]
    InvalidCanonicalUrl { url: Url },
}

#[derive(Debug, thiserror::Error)]
pub enum RecipeCrawlError {
    #[error("invalid recipe crawler configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid recipe link selector: {0}")]
    InvalidSelector(String),
    #[error("recipe render mode {0:?} requires a configured browser adapter")]
    UnsupportedRenderMode(RecipeRenderMode),
    #[error("unsupported listing content type: {0}")]
    UnsupportedListingContentType(String),
    #[error("invalid listing page: {0}")]
    InvalidListing(String),
    #[error(transparent)]
    Client(reqwest::Error),
    #[error(transparent)]
    Article(ArticlePageError),
}

impl RecipeCrawlError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Client(error) => error.is_timeout() || error.is_connect() || error.is_request(),
            Self::Article(error) => error.is_retryable(),
            Self::InvalidConfig(_)
            | Self::InvalidSelector(_)
            | Self::UnsupportedRenderMode(_)
            | Self::UnsupportedListingContentType(_)
            | Self::InvalidListing(_) => false,
        }
    }
}

impl ArticlePageError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Request { source, .. } | Self::Client(source) => {
                source.is_timeout() || source.is_connect() || source.is_request()
            }
            Self::HttpStatus { status, .. } => {
                *status == StatusCode::REQUEST_TIMEOUT
                    || *status == StatusCode::TOO_MANY_REQUESTS
                    || status.is_server_error()
            }
            Self::DnsResolution { .. }
            | Self::DnsResolutionEmpty { .. }
            | Self::RemoteAddressUnavailable { .. } => true,
            Self::InvalidConfig(_)
            | Self::TooManyUrls { .. }
            | Self::UnsupportedUrl(_)
            | Self::ResponseTooLarge { .. }
            | Self::PrivateNetwork { .. }
            | Self::InvalidRedirect { .. }
            | Self::TooManyRedirects { .. }
            | Self::UnsupportedContent { .. }
            | Self::MissingArticleSignal { .. }
            | Self::ObviousListingPath { .. }
            | Self::MissingArticleBody { .. }
            | Self::MissingTitle { .. }
            | Self::GenericListingTitle { .. }
            | Self::HighLinkDensityCollection { .. }
            | Self::MultipleArticleCollection { .. }
            | Self::YearArchiveCollection { .. }
            | Self::InsufficientContent { .. }
            | Self::InvalidCanonicalUrl { .. } => false,
        }
    }

    pub const fn reason(&self) -> &'static str {
        match self {
            Self::InvalidConfig(_) => "invalid_config",
            Self::Client(_) | Self::Request { .. } => "request_failed",
            Self::TooManyUrls { .. } => "too_many_urls",
            Self::UnsupportedUrl(_) => "unsupported_url",
            Self::HttpStatus { .. } => "http_status",
            Self::ResponseTooLarge { .. } => "response_too_large",
            Self::DnsResolution { .. } => "dns_resolution_failed",
            Self::DnsResolutionEmpty { .. } => "dns_resolution_empty",
            Self::PrivateNetwork { .. } => "private_network",
            Self::RemoteAddressUnavailable { .. } => "remote_address_unavailable",
            Self::InvalidRedirect { .. } => "invalid_redirect",
            Self::TooManyRedirects { .. } => "too_many_redirects",
            Self::UnsupportedContent { .. } => "unsupported_content",
            Self::MissingArticleSignal { .. } => "missing_article_signal",
            Self::ObviousListingPath { .. } => "obvious_listing_path",
            Self::MissingArticleBody { .. } => "missing_article_body",
            Self::MissingTitle { .. } => "missing_title",
            Self::GenericListingTitle { .. } => "generic_listing_title",
            Self::HighLinkDensityCollection { .. } => "high_link_density_collection",
            Self::MultipleArticleCollection { .. } => "multiple_article_collection",
            Self::YearArchiveCollection { .. } => "year_archive_collection",
            Self::InsufficientContent { .. } => "insufficient_content",
            Self::InvalidCanonicalUrl { .. } => "invalid_canonical_url",
        }
    }
}

fn parse_feed_body(
    feed_url: &Url,
    bytes: &[u8],
    max_items: usize,
) -> Result<CrawlBatch, CrawlError> {
    let feed = feed_rs::parser::parse(bytes)
        .map_err(|error| CrawlError::InvalidFeed(error.to_string()))?;
    let feed_type = feed.feed_type.clone();
    let detected_source_kind = match feed_type {
        FeedType::Atom => SourceKind::Atom,
        FeedType::RSS0 | FeedType::RSS1 | FeedType::RSS2 => SourceKind::Rss,
        FeedType::JSON => {
            return Err(CrawlError::InvalidFeed(
                "JSON Feed is not supported by the RSS/Atom crawler".to_owned(),
            ));
        }
    };
    let total_entries = feed.entries.len();
    let mut skipped_entries = 0_usize;
    let items = feed
        .entries
        .into_iter()
        .take(max_items)
        .filter_map(|entry| match raw_item_from_entry(feed_url, entry) {
            Ok(item) => Some(item),
            Err(_) => {
                skipped_entries += 1;
                None
            }
        })
        .collect::<Vec<_>>();
    let title = feed.title.map(|title| title.content);
    let fetched_at = chrono::Utc::now();

    Ok(CrawlBatch {
        fetched_at,
        detected_source_kind,
        metadata: json!({
            "feed_url": feed_url,
            "feed_title": title,
            "feed_type": format!("{feed_type:?}"),
            "total_entries": total_entries,
            "limited_to": max_items,
            "skipped_entries": skipped_entries,
        }),
        items,
    })
}

fn raw_item_from_entry(
    feed_url: &Url,
    entry: feed_rs::model::Entry,
) -> Result<RawCrawlItem, CrawlError> {
    let url = entry
        .links
        .iter()
        .filter(|link| {
            link.rel
                .as_deref()
                .is_none_or(|relation| relation == "alternate")
        })
        .find_map(|link| parse_public_url(&link.href))
        .or_else(|| parse_public_url(&entry.id))
        .ok_or(CrawlError::ItemMissingUrl)?;
    let external_id = if entry.id.trim().is_empty() {
        url.as_str().to_owned()
    } else {
        entry.id.trim().to_owned()
    };
    let source_item_key = sha256_hex(format!("{}\0{external_id}", feed_url.as_str()).as_bytes());
    let title = entry.title.as_ref().map(|title| normalize(&title.content));
    let summary_html = entry
        .summary
        .as_ref()
        .map(|summary| content_as_html(&summary.content, summary.content_type.as_str()));
    let body_html = entry
        .content
        .as_ref()
        .and_then(|content| {
            content
                .body
                .as_ref()
                .map(|body| content_as_html(body, content.content_type.as_str()))
        })
        .or_else(|| summary_html.clone());
    let published_at = entry
        .published
        .or(entry.updated)
        .filter(is_plausible_article_datetime);
    let payload = serde_json::to_value(&entry)?;

    Ok(RawCrawlItem {
        source_item_key,
        external_id: Some(external_id),
        canonical_url: Some(url.clone()),
        url,
        title,
        summary_html,
        body_html,
        published_at,
        payload,
    })
}

fn content_as_html(content: &str, content_type: &str) -> String {
    let content_type = content_type.to_ascii_lowercase();
    if content_type.contains("html")
        || content_type.contains("xhtml")
        || content_type.ends_with("+xml")
    {
        content.to_owned()
    } else {
        format!("<p>{}</p>", html_escape::encode_text(content))
    }
}

fn normalize(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_public_url(value: &str) -> Option<Url> {
    let url = Url::parse(value.trim()).ok()?;
    validate_url(&url).ok()?;
    Some(url)
}

fn validate_url(url: &Url) -> Result<(), CrawlError> {
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(CrawlError::UnsupportedUrl(url.clone()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(CrawlError::UnsupportedUrl(url.clone()));
    }
    Ok(())
}

fn sha256_hex(value: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(value)))
}

#[derive(Debug, thiserror::Error)]
pub enum CrawlError {
    #[error("invalid crawler configuration: {0}")]
    InvalidConfig(String),
    #[error(transparent)]
    Client(#[from] reqwest::Error),
    #[error("source kind {0} is not supported by the RSS/Atom crawler")]
    UnsupportedSourceKind(SourceKind),
    #[error("unsupported public URL: {0}")]
    UnsupportedUrl(Url),
    #[error("request failed for {url}: {message}")]
    Request { url: Url, message: String },
    #[error("HTTP {status} for {url}")]
    HttpStatus { url: Url, status: u16 },
    #[error("response for {url} exceeds {limit} bytes")]
    ResponseTooLarge { url: Url, limit: usize },
    #[error("invalid RSS/Atom feed: {0}")]
    InvalidFeed(String),
    #[error("feed entry has no public article URL")]
    ItemMissingUrl,
    #[error("failed to serialize feed entry: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::Value;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn public_fetch_defaults_use_the_shared_identifiable_user_agent() {
        assert_eq!(
            RssAtomCrawlerConfig::default().user_agent,
            DEFAULT_PUBLIC_FETCH_USER_AGENT
        );
        assert_eq!(
            HtmlArticleCrawlerConfig::default().user_agent,
            DEFAULT_PUBLIC_FETCH_USER_AGENT
        );
        assert_eq!(
            HtmlRecipeCrawlerConfig::default().user_agent,
            DEFAULT_PUBLIC_FETCH_USER_AGENT
        );
    }

    #[test]
    fn element_serializer_panic_falls_back_to_escaped_visible_text() {
        let document = Html::parse_document("<main><p>Safe &amp; visible article text</p></main>");
        let main = document
            .select(&Selector::parse("main").expect("valid selector"))
            .next()
            .expect("main element");

        let html = serialize_element_html_or_text_fallback(main, || {
            panic!("no ElemInfo");
        });

        assert!(html.contains("data-html-serialization-fallback=\"html5ever-panic.v1\""));
        assert!(html.contains("Safe &amp; visible article text"));
    }

    #[test]
    fn parses_rss_entries_into_raw_items() {
        let batch = parse_feed_body(
            &Url::parse("https://example.com/feed.xml").expect("valid URL"),
            br#"<?xml version="1.0"?>
            <rss version="2.0"><channel>
              <title>Acme News</title>
              <link>https://example.com/</link>
              <description>News</description>
              <item>
                <guid>launch-1</guid>
                <title>  Acme launches product  </title>
                <link>https://example.com/news/launch</link>
                <description><![CDATA[<p>Launch summary</p>]]></description>
                <pubDate>Wed, 16 Jul 2025 00:00:00 GMT</pubDate>
              </item>
            </channel></rss>"#,
            100,
        )
        .expect("parse RSS");

        assert_eq!(batch.detected_source_kind, SourceKind::Rss);
        assert_eq!(batch.items.len(), 1);
        assert_eq!(batch.items[0].external_id.as_deref(), Some("launch-1"));
        assert_eq!(
            batch.items[0].title.as_deref(),
            Some("Acme launches product")
        );
        assert_eq!(
            batch.items[0].url.as_str(),
            "https://example.com/news/launch"
        );
        assert!(batch.items[0].summary_html.as_deref().is_some());
    }

    #[tokio::test]
    async fn fetches_a_feed_over_http() {
        use axum::{Router, http::header, routing::get};

        let app = Router::new().route(
            "/feed",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/atom+xml")],
                    r#"<?xml version="1.0"?>
                    <feed xmlns="http://www.w3.org/2005/Atom">
                      <title>Acme</title><id>https://example.com/feed</id>
                      <updated>2025-07-16T00:00:00Z</updated>
                      <entry><title>Launch</title><id>launch-2</id>
                      <updated>2025-07-16T00:00:00Z</updated>
                      <link href="https://example.com/launch-2"/>
                      <content type="html">&lt;p&gt;Body&lt;/p&gt;</content></entry>
                    </feed>"#,
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
                .expect("serve crawler fixture");
        });
        let now = Utc::now();
        let source = Source {
            id: Uuid::new_v4(),
            source_id: "acme-feed".to_owned(),
            company_id: Uuid::new_v4(),
            kind: SourceKind::Rss,
            url: Url::parse(&format!("http://{address}/feed")).expect("fixture URL"),
            status: feed_core::SourceStatus::Approved,
            freshness_slo_seconds: 3600,
            browser_required: false,
            public_export_allowed: true,
            discovery_confidence: Some(1.0),
            metadata: Value::Object(Default::default()),
            created_at: now,
            updated_at: now,
        };
        let crawler = RssAtomCrawler::new(RssAtomCrawlerConfig::default()).expect("build crawler");
        let batch = crawler.crawl(&source).await.expect("crawl fixture");
        assert_eq!(batch.detected_source_kind, SourceKind::Atom);
        assert_eq!(batch.items.len(), 1);
        assert_eq!(batch.items[0].external_id.as_deref(), Some("launch-2"));

        task.abort();
    }

    #[test]
    fn extracts_only_substantive_generic_article_pages() {
        let requested = Url::parse("https://example.com/news/launch").expect("requested URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:type" content="article">
            <meta property="og:title" content="Example launch">
            <meta property="article:published_time" content="2026-07-20T12:00:00Z">
            <link rel="canonical" href="/news/launch">
            </head><body><article><h1>Example launch</h1><p>{}</p></article><article><h2>Related update</h2></article></body></html>"#,
            "Substantive independently fetched article body. ".repeat(10),
        );
        let item = extract_article(
            &requested,
            requested.clone(),
            Some("text/html; charset=utf-8"),
            body.as_bytes(),
            200,
        )
        .expect("extract article");

        assert_eq!(item.title.as_deref(), Some("Example launch"));
        assert_eq!(item.canonical_url.as_ref(), Some(&requested));
        assert_eq!(
            item.published_at,
            Some("2026-07-20T12:00:00Z".parse().expect("published time"))
        );
        assert!(
            item.payload["sanitized_content_chars"]
                .as_u64()
                .is_some_and(|length| length >= 200)
        );
        assert_eq!(item.payload["article_element_count"], json!(2));
    }

    #[test]
    fn extracts_naive_and_json_ld_publication_dates() {
        assert_eq!(
            parse_article_datetime("2026-07-09T07:00:00"),
            Some("2026-07-09T07:00:00Z".parse().expect("naive ISO time"))
        );
        assert_eq!(
            parse_article_datetime("2026-07-17 09:00:00"),
            Some("2026-07-17T09:00:00Z".parse().expect("naive SQL time"))
        );
        assert_eq!(
            parse_article_datetime("16-Jun-2026 06:05:22"),
            Some(
                "2026-06-16T06:05:22Z"
                    .parse()
                    .expect("hyphenated abbreviated-month time")
            )
        );
        assert_eq!(
            parse_article_datetime("October 27, 2021"),
            Some("2021-10-27T00:00:00Z".parse().expect("long month date"))
        );
        assert_eq!(
            parse_article_datetime("Dec. 3 2015"),
            Some(
                "2015-12-03T00:00:00Z"
                    .parse()
                    .expect("dotted abbreviated month date")
            )
        );
        assert_eq!(
            parse_article_datetime("Sept. 7, 2020"),
            Some(
                "2020-09-07T00:00:00Z"
                    .parse()
                    .expect("four-letter dotted September date")
            )
        );
        assert_eq!(
            json_ld_article_published_at(&json!({
                "@type": "SocialMediaPosting",
                "datePublished": "2022-02-27T09:22:21+00:00"
            })),
            Some(
                "2022-02-27T09:22:21Z"
                    .parse()
                    .expect("social media posting date")
            )
        );
        assert_eq!(
            parse_article_datetime("July 20, 26"),
            None,
            "two-digit years must not silently become years in the first century"
        );
        assert_eq!(
            parse_article_datetime("1970-01-01T00:00:00Z"),
            None,
            "epoch sentinel metadata is not an article publication date"
        );

        let requested =
            Url::parse("https://example.com/press-releases/detail/1/launch").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <script type="application/ld+json">
              {{"@context":"https://schema.org","@graph":[
                {{"@type":"NewsArticle","headline":"Example launch",
                  "datePublished":"2026-07-09T07:00:00-04:00"}}
              ]}}
            </script>
            <link rel="canonical" href="/press-releases/detail/1/launch">
            </head><body><main><h1>Example launch</h1><p>{}</p></main></body></html>"#,
            "Substantive independently fetched article body. ".repeat(10),
        );
        let item = extract_article(
            &requested,
            requested.clone(),
            Some("text/html; charset=utf-8"),
            body.as_bytes(),
            200,
        )
        .expect("extract JSON-LD article");

        assert_eq!(
            item.published_at,
            Some("2026-07-09T11:00:00Z".parse().expect("offset JSON-LD time"))
        );
        assert!(
            item.payload["article_signals"]
                .as_array()
                .is_some_and(|signals| signals.iter().any(|signal| signal == "json_ld_article"))
        );

        let web_page_body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:type" content="article">
            <script type="application/ld+json">
              {{"@context":"https://schema.org","@graph":[
                {{"@type":"WebPage","datePublished":"2026-06-17T16:00:55+00:00"}}
              ]}}
            </script>
            </head><body><main><h1>Example launch</h1><p>{}</p></main></body></html>"#,
            "Substantive independently fetched article body. ".repeat(10),
        );
        let item = extract_article(
            &requested,
            requested.clone(),
            Some("text/html"),
            web_page_body.as_bytes(),
            200,
        )
        .expect("OpenGraph article may use its Yoast WebPage publication date");
        assert_eq!(
            item.published_at,
            Some("2026-06-17T16:00:55Z".parse().expect("WebPage date"))
        );

        let unqualified_web_page_body =
            web_page_body.replace(r#"<meta property="og:type" content="article">"#, "");
        let item = extract_article(
            &requested,
            requested.clone(),
            Some("text/html"),
            unqualified_web_page_body.as_bytes(),
            200,
        )
        .expect("the main/H1 path signal still identifies the article");
        assert_eq!(
            item.published_at, None,
            "WebPage datePublished alone must not become article publication evidence"
        );
    }

    #[test]
    fn extracts_hyphenated_published_date_meta() {
        let url = Url::parse("https://example.com/news/company-update").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:type" content="article">
            <meta name="published-date" content="16-Jun-2026 06:05:22">
            </head><body><article><h1>Company update</h1><p>{}</p></article></body></html>"#,
            "Substantive independently fetched company article body. ".repeat(10),
        );

        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("extract article with published-date metadata");

        assert_eq!(
            item.published_at,
            Some(
                "2026-06-16T06:05:22Z"
                    .parse()
                    .expect("published-date meta timestamp")
            )
        );
        assert_eq!(item.payload["published_at_source"], "article_page");
    }

    #[test]
    fn prefers_page_h1_time_over_an_earlier_related_card_time() {
        let url = Url::parse("https://example.com/insights/company-update").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head><title>Company update</title></head><body>
            <article>
              <h3>Related older update</h3>
              <time datetime="2025-04-02">April 2, 2025</time>
            </article>
            <div class="article-hero">
              <time datetime="2026-07-07">July 7, 2026</time>
              <h1>Company update</h1>
            </div>
            <div class="rich-text"><p>{}</p></div>
            <article><h3>Another related update</h3></article>
            </body></html>"#,
            "Substantive independently fetched insight article body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("the page-level time should identify the article date");
        assert_eq!(
            item.published_at,
            Some("2026-07-07T00:00:00Z".parse().expect("page date"))
        );
        assert_eq!(item.payload["article_body_selector"], json!(".rich-text"));
    }

    #[test]
    fn accepts_one_visible_publish_date_local_to_the_page_h1() {
        let url = Url::parse("https://example.com/blog/company-update").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head><title>Company update</title></head><body>
            <div class="article-header">
              <h1>Company update</h1>
              <div class="blogPublishDate">April 16, 2025</div>
            </div>
            <aside class="w-richtext"><p>{}</p></aside>
            <section class="related-card">
              <h2>Related update</h2>
              <div class="post-date">January 3, 2024</div>
            </section>
            </body></html>"#,
            "Substantive independently fetched article body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("the H1-local visible publication date identifies the page date");

        assert_eq!(
            item.published_at,
            Some("2025-04-16T00:00:00Z".parse().expect("page date"))
        );
        assert_eq!(item.payload["published_at_source"], json!("article_page"));
        assert_eq!(item.payload["article_body_selector"], json!(".w-richtext"));
    }

    #[test]
    fn repairs_only_same_host_https_downgrade_redirects() {
        let current =
            Url::parse("https://example.com/blog/company-update").expect("current article URL");
        let repaired =
            resolve_article_redirect(&current, "http://example.com/blog/company-update/")
                .expect("same-host redirect");
        assert_eq!(
            repaired.as_str(),
            "https://example.com/blog/company-update/"
        );

        let cross_host =
            resolve_article_redirect(&current, "http://cdn.example.com/blog/company-update")
                .expect("cross-host redirect remains structurally valid");
        assert_eq!(
            cross_host.as_str(),
            "http://cdn.example.com/blog/company-update"
        );
    }

    #[test]
    fn accepts_an_explicit_visible_published_on_date_local_to_the_page_h1() {
        let url = Url::parse("https://example.com/blog/company-update").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head><title>Company update</title></head><body>
            <div class="blog-post-header">
              <div class="blog-post-header_date">
                <span>Published on</span><span>July 11, 2026</span>
              </div>
              <h1>Company update</h1>
            </div>
            <aside class="text-rich-text w-richtext"><p>{}</p></aside>
            <section class="related-card">
              <h2>Related update</h2>
              <div class="card-date">Published on July 12, 2025</div>
            </section>
            </body></html>"#,
            "Substantive independently fetched article body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("the explicit H1-local publication label identifies the page date");

        assert_eq!(
            item.published_at,
            Some("2026-07-11T00:00:00Z".parse().expect("page date"))
        );
        assert_eq!(item.payload["published_at_source"], json!("article_page"));
        assert_eq!(item.payload["article_body_selector"], json!(".w-richtext"));
    }

    #[test]
    fn rejects_long_listing_pages_without_article_semantics() {
        let url = Url::parse("https://example.com/news").expect("URL");
        let body = format!(
            "<!doctype html><html><head><title>News listing</title></head><body><main><p>{}</p></main></body></html>",
            "A long listing page is still not an individual article. ".repeat(10),
        );
        let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect_err("listing must be rejected");
        assert!(matches!(error, ArticlePageError::ObviousListingPath { .. }));
        assert_eq!(error.reason(), "obvious_listing_path");
    }

    #[test]
    fn accepts_article_like_path_with_h1_and_substantive_main_content() {
        let url = Url::parse("https://example.com/blog/a-substantive-company-update")
            .expect("article URL");
        let body = format!(
            "<!doctype html><html><head><title>Company update</title></head><body><main><h1>Company update</h1><p>{}</p></main></body></html>",
            "Substantive independently fetched article body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("path and H1 identify an individual article");
        assert_eq!(item.title.as_deref(), Some("Company update"));
        assert_eq!(
            item.payload["article_signals"],
            json!(["article_like_path_with_h1"])
        );
    }

    #[test]
    fn rejects_weak_topic_news_page_as_a_collection() {
        let url = Url::parse("https://example.com/news/consumer-products-services")
            .expect("collection URL");
        let body = format!(
            "<!doctype html><html><head><title>Consumer Products and Services News</title></head><body><main><h1>Consumer Products and Services News</h1><p>{}</p></main></body></html>",
            "A sector landing page containing many unrelated release summaries. ".repeat(10),
        );
        let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect_err("weak path plus collection title must not identify an article");
        assert!(matches!(
            error,
            ArticlePageError::GenericListingTitle { .. }
        ));
        assert_eq!(error.reason(), "generic_listing_title");
    }

    #[test]
    fn rejects_weak_short_page_with_listing_link_density() {
        let url = Url::parse("https://example.com/news/press-releases/financial")
            .expect("collection URL");
        let links = (0..20)
            .map(|index| format!("<a href=\"/news/item-{index}\">Release {index}</a>"))
            .collect::<String>();
        let body = format!(
            "<!doctype html><html><head><title>Quarterly performance</title></head><body><main><h1>Quarterly performance</h1><p>{}</p>{links}</main></body></html>",
            "A compact category introduction with a release index. ".repeat(8),
        );
        let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect_err("high link density must identify the weak collection page");
        assert!(matches!(
            error,
            ArticlePageError::HighLinkDensityCollection { .. }
        ));
        assert_eq!(error.reason(), "high_link_density_collection");

        assert!(is_high_link_density_collection(
            "Cleaning Best Practices",
            50,
            5_000,
            "generic:paragraph-cluster.v1",
            false,
        ));
        assert!(!is_high_link_density_collection(
            "Cleaning Best Practices",
            50,
            5_000,
            "generic:paragraph-cluster.v1",
            true,
        ));
        assert!(!is_high_link_density_collection(
            "Cleaning Best Practices",
            50,
            5_000,
            "main",
            false,
        ));
    }

    #[test]
    fn rejects_undated_short_slug_topic_pages_but_preserves_substantive_posts() {
        let topic_url =
            Url::parse("https://example.com/insights/capital-flows").expect("topic URL");
        assert!(is_undated_short_slug_topic_collection(
            &topic_url,
            "Capital flows",
            0,
            278,
            false,
        ));
        assert!(is_undated_short_slug_topic_collection(
            &Url::parse("https://example.com/blog/movilidad").expect("category URL"),
            "Movilidad",
            19,
            2_223,
            false,
        ));
        assert!(!is_undated_short_slug_topic_collection(
            &Url::parse("https://example.com/blog/after-the-signature").expect("post URL"),
            "After the Signature",
            0,
            7_799,
            false,
        ));
        assert!(!is_undated_short_slug_topic_collection(
            &Url::parse("https://example.com/blog/building-modern-data-pipelines")
                .expect("post URL"),
            "Building Modern Data Pipelines",
            1,
            233,
            false,
        ));
        assert!(!is_undated_short_slug_topic_collection(
            &topic_url,
            "Capital flows",
            30,
            2_000,
            true,
        ));
        assert!(is_undated_listing_anchor_collection(
            "Hiring & Onboarding",
            "listing_anchor",
            12,
            1_000,
            false,
        ));
        assert!(!is_undated_listing_anchor_collection(
            "Hiring & Onboarding",
            "h1",
            12,
            1_000,
            false,
        ));
        assert!(!is_undated_listing_anchor_collection(
            "Hiring & Onboarding",
            "listing_anchor",
            12,
            1_000,
            true,
        ));
    }

    #[test]
    fn rejects_undated_weak_page_with_embedded_navigation_cards() {
        let url =
            Url::parse("https://example.com/press-room/public-policy-centre").expect("page URL");
        let cards = (0..8)
            .map(|index| {
                format!(
                    r#"<section><data class="href" value="/press-room/policy-{index}"></data><h2>Policy topic {index}</h2><p>Static navigation summary for policy topic {index}.</p></section>"#
                )
            })
            .collect::<String>();
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:title" content="Public Policy Centre">
            <script>dataLayer[0].firstPublishDate = "2020-01-15";</script>
            <title>Public Policy Centre</title></head>
            <body><main><h1>Public Policy Centre</h1>
            <p>{}</p>{cards}</main></body></html>"#,
            "A long-lived policy landing page that organizes reference material. ".repeat(10),
        );
        let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect_err("embedded navigation cards must identify the undated static page");
        assert!(matches!(
            error,
            ArticlePageError::HighLinkDensityCollection { link_count: 8, .. }
        ));
        assert_eq!(error.reason(), "high_link_density_collection");
    }

    #[test]
    fn preserves_dated_article_with_embedded_navigation_links() {
        let url = Url::parse("https://example.com/press-room/company-update").expect("article URL");
        let related = (0..8)
            .map(|index| {
                format!(r#"<data class="href" value="/press-room/related-{index}"></data>"#)
            })
            .collect::<String>();
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="article:published_time" content="2026-07-20T12:00:00Z">
            <title>Company Update</title></head>
            <body><main><h1>Company Update</h1><p>{}</p>{related}</main></body></html>"#,
            "Substantive independently fetched article body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("publication-date evidence must preserve the individual article");
        assert_eq!(item.title.as_deref(), Some("Company Update"));
        assert!(item.published_at.is_some());
    }

    #[test]
    fn extracts_data_layer_publish_date_without_using_it_as_article_identity() {
        let url =
            Url::parse("https://example.com/news/articles/company-update").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head><title>Company Update</title>
            <script>
              window.dataLayer = window.dataLayer || [];
              dataLayer[0] = dataLayer[0] || {{}};
              dataLayer[0].firstPublishDate = "2026-05-29";
              dataLayer[0].modifiedDate = "2026-07-20";
            </script></head>
            <body><main><h1>Company Update</h1><p>{}</p></main></body></html>"#,
            "Substantive independently fetched article body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("path and H1 establish the article independently of its data-layer date");
        assert_eq!(
            item.published_at,
            Some(
                DateTime::parse_from_rfc3339("2026-05-29T00:00:00Z")
                    .expect("date")
                    .with_timezone(&Utc)
            )
        );
        assert_eq!(
            item.payload["published_at_source"],
            json!("article_page_data_layer")
        );
        assert_eq!(
            item.payload["article_signals"],
            json!(["article_like_path_with_h1"])
        );
    }

    #[test]
    fn rejects_multiple_article_cards_without_individual_article_metadata() {
        let url = Url::parse("https://example.com/blog/developer-tools").expect("collection URL");
        let body = format!(
            "<!doctype html><html><head><title>Developer tools</title></head><body><main><h1>Developer tools</h1><article><h2>First update</h2><p>{}</p></article><article><h2>Second update</h2><p>{}</p></article></main></body></html>",
            "A category card summary rather than the requested page body. ".repeat(6),
            "Another category card summary rather than the requested page body. ".repeat(6),
        );
        let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect_err("a card collection must not become its first article card");
        assert!(matches!(
            error,
            ArticlePageError::MultipleArticleCollection {
                article_count: 2,
                ..
            }
        ));
        assert_eq!(error.reason(), "multiple_article_collection");
    }

    #[test]
    fn rejects_short_slug_matched_card_grid_despite_page_level_article_metadata() {
        let url = Url::parse("https://example.com/blog/power-apps").expect("collection URL");
        let cards = (0..12)
            .map(|index| {
                format!(
                    "<article><h2>Update {index}</h2><p>{}</p></article>",
                    "A category card summary rather than an individual article body. ".repeat(5)
                )
            })
            .collect::<String>();
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:type" content="article">
            <meta property="article:published_time" content="2026-07-01T00:00:00Z">
            <title>Power Apps</title></head>
            <body><main><h1>Power Apps</h1>{cards}</main></body></html>"#,
        );
        let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect_err("page-level article metadata must not turn a short card grid into an item");
        assert!(matches!(
            error,
            ArticlePageError::MultipleArticleCollection {
                article_count: 12,
                ..
            }
        ));
    }

    #[test]
    fn aggregates_componentized_article_body_ahead_of_related_card_grids() {
        let url = Url::parse(
            "https://example.com/en/insights/articles/workforce-readiness-drives-results",
        )
        .expect("article URL");
        let cards = (0..12)
            .map(|index| {
                format!(
                    "<article class=\"content-card\"><h2>Related card {index}</h2><p>{}</p></article>",
                    "A short related-card description. ".repeat(3)
                )
            })
            .collect::<String>();
        let body = format!(
            r#"<!doctype html><html><head>
            <title>Workforce Readiness Drives Better Results</title>
            <meta property="og:type" content="website">
            <meta property="og:title" content="Workforce Readiness Drives Better Results">
            </head><body>
            <h1 style="display:none">Workforce Readiness Drives Better Results</h1>
            <main class="page-main">
              <section class="layout-rich-text"><div class="richtext-editor-place">
                <p>First substantive article component. {}</p>
              </div></section>
              <section class="layout-rich-text"><div class="richtext-editor-place">
                <p>Second substantive article component. {}</p>
              </div></section>
              <section class="related-content">{cards}</section>
            </main></body></html>"#,
            "The workforce strategy connects skills to measurable business outcomes. ".repeat(8),
            "Leaders align training, roles, and operating models before scaling technology. "
                .repeat(8),
        );

        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("bounded rich-text components establish a primary article body");

        assert_eq!(
            item.payload["article_body_selector"],
            json!("componentized:richtext-editor-place.v1")
        );
        assert_eq!(item.payload["article_body_component_count"], json!(2));
        let body_html = item.body_html.expect("article body");
        assert!(body_html.contains("First substantive article component"));
        assert!(body_html.contains("Second substantive article component"));
        assert!(!body_html.contains("Related card"));
    }

    #[test]
    fn rejects_shallow_cms_collection_hubs_after_title_repair() {
        let topic_url =
            Url::parse("https://example.com/insights/data-science-analytics/").expect("topic URL");
        assert!(is_shallow_collection_hub(
            &topic_url,
            "Data Technology - Example",
            Some("Data Technology"),
            ShallowCollectionEvidence {
                article_count: 12,
                article_elements_with_h1: 0,
                max_article_content_chars: 866,
                content_chars: 866,
                link_count: 20,
                body_selector: "generic:paragraph-cluster.v1",
                body_text: "A collection of expert articles and case studies.",
            },
        ));

        let press_url = Url::parse("https://example.com/press-releases/performance/")
            .expect("press category URL");
        assert!(is_shallow_collection_hub(
            &press_url,
            "Performance",
            Some("Performance"),
            ShallowCollectionEvidence {
                article_count: 0,
                article_elements_with_h1: 0,
                max_article_content_chars: 0,
                content_chars: 363,
                link_count: 10,
                body_selector: "main",
                body_text: "Performance description Featured Articles First release Second release Previous page Next page",
            },
        ));

        let card_collection_url =
            Url::parse("https://example.com/news/the-journal/").expect("collection URL");
        assert!(is_shallow_collection_hub(
            &card_collection_url,
            "Our Brands",
            Some("Our Brands"),
            ShallowCollectionEvidence {
                article_count: 1,
                article_elements_with_h1: 0,
                max_article_content_chars: 816,
                content_chars: 1_295,
                link_count: 20,
                body_selector: ".entry-content",
                body_text: "First update Read article Second update Read article Third update Read article Fourth update Read article",
            },
        ));

        let explicit_category_url =
            Url::parse("https://example.com/blog/aviation").expect("category URL");
        assert!(is_shallow_collection_hub(
            &explicit_category_url,
            "Universal Technical Institute Blog | UTI | Aviation",
            Some("Aviation"),
            ShallowCollectionEvidence {
                article_count: 1,
                article_elements_with_h1: 0,
                max_article_content_chars: 2_200,
                content_chars: 3_016,
                link_count: 66,
                body_selector: "main",
                body_text: "SHOWING POSTS IN Aviation First story READ FULL STORY",
            },
        ));

        assert!(is_shallow_collection_hub(
            &Url::parse("https://example.com/blog/strategy").expect("filter collection URL"),
            "Newsletter Q2 2026",
            None,
            ShallowCollectionEvidence {
                article_count: 0,
                article_elements_with_h1: 0,
                max_article_content_chars: 0,
                content_chars: 42_000,
                link_count: 329,
                body_selector: "#content",
                body_text: "Filters Filter by strategy. There is a lack of results to match selected filters. Please adjust the filter options to broaden results. First story Second story.",
            },
        ));
        assert!(is_shallow_collection_hub(
            &Url::parse("https://example.com/news/nr.html").expect("navigation collection URL"),
            "Example Holdings",
            None,
            ShallowCollectionEvidence {
                article_count: 0,
                article_elements_with_h1: 0,
                max_article_content_chars: 0,
                content_chars: 50_000,
                link_count: 625,
                body_selector: "[role='main']",
                body_text: "News Releases Topics Media Contacts Example Holdings Example Securities Other Group Companies EMEA Americas Asia ex-Japan News Release Subscription Service RSS Before 2020",
            },
        ));
        assert!(is_shallow_collection_hub(
            &Url::parse("https://example.com/company/news-and-media/local-announcements")
                .expect("all-prefixed collection URL"),
            "Local Announcements",
            Some("All Local Announcements"),
            ShallowCollectionEvidence {
                article_count: 6,
                article_elements_with_h1: 1,
                max_article_content_chars: 1_944,
                content_chars: 1_581,
                link_count: 6,
                body_selector: "article",
                body_text: "A collection containing several local announcement cards.",
            },
        ));
        assert!(!is_shallow_collection_hub(
            &Url::parse("https://example.com/blog/april-product-announcements")
                .expect("individual announcement URL"),
            "April Product Announcements",
            Some("April Product Announcements"),
            ShallowCollectionEvidence {
                article_count: 6,
                article_elements_with_h1: 1,
                max_article_content_chars: 1_944,
                content_chars: 1_581,
                link_count: 6,
                body_selector: "article",
                body_text: "A substantive product announcement with related cards.",
            },
        ));

        let repeated_cta_url =
            Url::parse("https://example.com/insights/sustainability").expect("collection URL");
        assert!(is_shallow_collection_hub(
            &repeated_cta_url,
            "Sustainability insights",
            Some("Sustainability insights"),
            ShallowCollectionEvidence {
                article_count: 0,
                article_elements_with_h1: 0,
                max_article_content_chars: 0,
                content_chars: 3_000,
                link_count: 12,
                body_selector: "generic:paragraph-cluster.v1",
                body_text: "First card Read More Second card Read More Third card Read More Fourth card Read More Fifth card Read More Sixth card Read More Seventh card Read More",
            },
        ));
        assert!(!is_shallow_collection_hub(
            &Url::parse("https://example.com/blog/new-feature-release").expect("article URL"),
            "New Feature Release",
            Some("New Feature Release"),
            ShallowCollectionEvidence {
                article_count: 0,
                article_elements_with_h1: 0,
                max_article_content_chars: 0,
                content_chars: 1_000,
                link_count: 12,
                body_selector: "generic:paragraph-cluster.v1",
                body_text: "Substantive feature release body. Related one Read More Related two Read More Related three Read More Related four Read More",
            },
        ));

        assert!(!is_shallow_collection_hub(
            &Url::parse("https://example.com/blog/power-apps").expect("article URL"),
            "Power Apps",
            Some("Power Apps"),
            ShallowCollectionEvidence {
                article_count: 11,
                article_elements_with_h1: 0,
                max_article_content_chars: 903,
                content_chars: 2_400,
                link_count: 25,
                body_selector: "article",
                body_text: "A substantive individual article with several related cards.",
            },
        ));
        assert!(is_card_grid_without_primary_article(12, 0, 96));
        assert!(is_card_grid_without_primary_article(15, 0, 903));
        assert!(!is_card_grid_without_primary_article(15, 0, 1_000));
        assert!(!is_card_grid_without_primary_article(12, 0, 2_277));
        assert!(!is_card_grid_without_primary_article(12, 1, 96));
        assert!(is_multi_heading_card_grid(21, 21, 434));
        assert!(!is_multi_heading_card_grid(12, 1, 434));
        assert!(!is_multi_heading_card_grid(12, 12, 1_500));
        assert!(is_listing_hint_over_collection_page(
            &Url::parse("https://example.com/innovation/artificial-intelligence/")
                .expect("collection URL"),
            "listing_anchor",
            Some("Artificial Intelligence"),
            32,
        ));
        assert!(is_listing_hint_over_collection_page(
            &Url::parse("https://example.com/newsroom/people-impact/").expect("collection URL"),
            "listing_anchor",
            Some("People & Impact"),
            15,
        ));
        assert!(!is_listing_hint_over_collection_page(
            &Url::parse("https://example.com/news/company-launch").expect("article URL"),
            "listing_anchor",
            Some("News"),
            12,
        ));
    }

    #[test]
    fn preserves_substantive_short_titled_article_with_related_cards() {
        let url = Url::parse("https://example.com/blog/power-apps").expect("article URL");
        let related = (0..11)
            .map(|index| format!("<article><h2>Related {index}</h2><p>Short card.</p></article>"))
            .collect::<String>();
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:type" content="article">
            <meta property="article:published_time" content="2026-07-01T00:00:00Z">
            <title>Power Apps</title></head>
            <body><main><article><h1>Power Apps</h1><p>{}</p></article>{related}</main></body></html>"#,
            "A substantive individual product article with implementation detail. ".repeat(30),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("a substantive primary article must survive related cards");
        assert_eq!(item.title.as_deref(), Some("Power Apps"));
    }

    #[test]
    fn accepts_listing_proven_article_body_despite_unrelated_article_cards() {
        let url =
            Url::parse("https://example.com/press-release/company-update").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head><title>News</title></head><body>
            <h1>Company update</h1>
            <article><h2>Related release one</h2><p>Short card.</p></article>
            <div class="article-content"><p>{}</p></div>
            <article><h2>Related release two</h2><p>Another short card.</p></article>
            </body></html>"#,
            "Substantive independently fetched company release body. ".repeat(10),
        );
        let item = extract_article_with_title_hint(
            &url,
            url.clone(),
            Some("text/html"),
            body.as_bytes(),
            200,
            Some("Company update"),
        )
        .expect("listing title plus semantic body should disambiguate related cards");
        assert_eq!(item.title.as_deref(), Some("Company update"));
        assert_eq!(item.payload["article_element_count"], json!(2));
        assert_eq!(
            item.payload["article_body_selector"],
            json!(".article-content")
        );
        assert!(
            item.payload["article_signals"]
                .as_array()
                .is_some_and(|signals| signals.contains(&json!(
                    "article_like_path_with_listing_title_and_semantic_body"
                )))
        );
    }

    #[test]
    fn accepts_isolated_semantic_body_despite_unrelated_article_cards() {
        let url = Url::parse("https://example.com/stories/a-substantive-company-update")
            .expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head><title>Company update</title></head><body>
            <h1>Company update</h1>
            <article><h2>Related story one</h2><p>Short card.</p></article>
            <div class="blog-article__content"><p>{}</p></div>
            <article><h2>Related story two</h2><p>Another short card.</p></article>
            </body></html>"#,
            "Substantive independently fetched company story body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("article path, H1, and an isolated semantic body disambiguate related cards");
        assert_eq!(item.title.as_deref(), Some("Company update"));
        assert_eq!(item.payload["article_element_count"], json!(2));
        assert_eq!(
            item.payload["article_body_selector"],
            json!("[class*='blog-article__content']")
        );
        assert_eq!(
            item.payload["article_signals"],
            json!(["article_element", "article_like_path_with_h1"])
        );
    }

    #[test]
    fn prefers_semantic_article_content_id_over_related_card_content() {
        let url = Url::parse("https://example.com/news-features/article/2026/company-update")
            .expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head><title>Company update</title></head><body>
            <h1>Company update</h1>
            <article id="article-content"><div class="section--content"><p>{}</p></div></article>
            <aside>
              <article><div class="article--content">Related story one</div></article>
              <article><div class="article--content">Related story two</div></article>
            </aside>
            </body></html>"#,
            "Substantive independently fetched company feature body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("the semantic article root should outrank related-card content");
        assert_eq!(item.title.as_deref(), Some("Company update"));
        assert_eq!(item.payload["article_element_count"], json!(3));
        assert_eq!(
            item.payload["article_body_selector"],
            json!("article#article-content")
        );
        assert!(
            item.body_html
                .as_deref()
                .is_some_and(|html| html.contains("independently fetched company feature body"))
        );
    }

    #[test]
    fn skips_thin_earlier_body_selector_for_substantive_later_body() {
        let url = Url::parse("https://example.com/blog/company-update").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <title>Company update</title>
            <script type="application/ld+json">{{"@type":"BlogPosting","headline":"Company update"}}</script>
            </head><body>
            <h1>Company update</h1>
            <article class="text-rich-text w-richtext"><p>{}</p></article>
            <section>
              <article class="post-card"><div class="post-content"></div></article>
              <article class="post-card"><div class="post-content"></div></article>
            </section>
            </body></html>"#,
            "Substantive independently fetched Webflow article body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("empty card containers must not shadow a substantive later body selector");
        assert_eq!(item.title.as_deref(), Some("Company update"));
        assert_eq!(item.payload["article_element_count"], json!(3));
        assert_eq!(item.payload["article_body_selector"], json!(".w-richtext"));
        assert!(
            item.body_html
                .as_deref()
                .is_some_and(|html| html.contains("independently fetched Webflow article body"))
        );
    }

    #[test]
    fn extracts_bounded_generic_paragraph_cluster_without_a_site_selector() {
        let url = Url::parse("https://example.com/company-announces-a-major-update")
            .expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head><title>Company announces a major update</title>
            <meta property="og:type" content="article"></head><body>
            <h1>Company announces a major update</h1>
            <div class="custom-layout">
              <div class="share-tools"><a href="/share">Share</a></div>
              <div class="custom-copy"><p>{}</p><p>{}</p></div>
              <div class="related-links">
                <a href="/company-announces-another-update">Another update</a>
                <a href="/company-reports-results">Company reports results</a>
              </div>
            </div>
            <footer><div><p>{}</p><p>{}</p></div></footer>
            </body></html>"#,
            "Substantive first paragraph of the independently fetched company update. ".repeat(6),
            "Substantive second paragraph with additional operational detail. ".repeat(6),
            "Long footer marketing paragraph that is not article content. ".repeat(12),
            "Long footer legal paragraph that is also not article content. ".repeat(12),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("paragraph density should recover a generic article body");
        assert_eq!(
            item.payload["article_body_selector"],
            json!("generic:paragraph-cluster.v1")
        );
        let body_html = item.body_html.as_deref().expect("article body");
        assert!(body_html.contains("independently fetched company update"));
        assert!(!body_html.contains("footer marketing"));
        assert!(!body_html.contains("Another update"));
    }

    #[test]
    fn accepts_news_release_path_with_h1_and_substantive_main_content() {
        let url =
            Url::parse("https://example.com/news-releases/2026/july/a-substantive-company-update")
                .expect("article URL");
        let body = format!(
            "<!doctype html><html><head><title>Company update</title></head><body><main><h1>Company update</h1><p>{}</p></main></body></html>",
            "Substantive independently fetched news release body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("news release path and H1 identify an individual article");
        assert_eq!(item.title.as_deref(), Some("Company update"));
        assert_eq!(
            item.payload["article_signals"],
            json!(["article_like_path_with_h1"])
        );
    }

    #[test]
    fn rejects_nested_collection_and_default_listing_paths() {
        assert!(is_obvious_article_listing_path(
            &Url::parse("https://example.com/").expect("URL")
        ));
        for archive_url in [
            "https://example.com/news/2024-news-archive/",
            "https://example.com/news/press-releases-2020/",
            "https://example.com/news/2020-press-releases/",
            "https://example.com/blog/employee-stories/",
        ] {
            assert!(
                is_obvious_article_listing_path(&Url::parse(archive_url).expect("archive URL")),
                "{archive_url}"
            );
        }
        assert!(!is_obvious_article_listing_path(
            &Url::parse("https://example.com/?content_id=682").expect("URL")
        ));
        assert!(!is_obvious_article_listing_path(
            &Url::parse("https://example.com/news-releases.html?item=1384").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/newsroom/press-releases").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/press-releases/default.aspx").expect("URL")
        ));
        assert!(is_article_like_path(
            &Url::parse("https://example.com/news/index.php?content_id=682").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/news/index.php?utm_source=test").expect("URL")
        ));
        for collection_url in [
            "https://example.com/news-and-stories?category=771",
            "https://example.com/latest-press-releases?category=financial",
            "https://example.com/blogs/recent-post?page=2",
            "https://example.com/press-releases/press-release-detail?id=",
            "https://example.com/news/index.php?id=%27payload",
            "https://example.com/blog/api-success",
            "https://example.com/blog/code-examples",
            "https://example.com/news/corporate-news",
            "https://example.com/blog/developer-spotlight",
            "https://example.com/news/latest-stories",
            "https://example.com/news/leadership-perspectives",
            "https://example.com/news/newsletters",
            "https://example.com/press/press-details",
            "https://example.com/news/release-details",
            "https://example.com/blog/solution-briefs",
            "https://example.com/blog/sustainability-leadership",
            "https://example.com/blog/trending-topics",
            "https://example.com/blog/cat/engineering",
            "https://example.com/news/search",
        ] {
            assert!(
                is_obvious_article_listing_path(
                    &Url::parse(collection_url).expect("collection URL")
                ),
                "{collection_url}"
            );
        }
        assert!(is_article_like_path(
            &Url::parse(
                "https://example.com/press-releases/press-release-details/2026/launch/default.aspx"
            )
            .expect("URL")
        ));
        assert!(is_article_like_path(
            &Url::parse(
                "https://example.com/press-room/2025-high-school-innovation-and-entrepreneurship-award"
            )
            .expect("press-room detail URL")
        ));
        assert!(!is_obvious_article_listing_path(
            &Url::parse(
                "https://blog.google/products-and-platforms/products/search/google-images-25th-anniversary/"
            )
            .expect("product-search article URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/category/news/company-launch").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/blog/collections/engineering").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/news/archives").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/news/complete-archive").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/blog/list/investment").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/stories/categories/science").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/blog/category-security").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/blog/user/12345").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/blog/contributors/alice").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/community/blog/label-name/platform").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/blog/hub/financial-tips").expect("URL")
        ));
        assert!(is_article_like_path(
            &Url::parse("https://example.com/hub/blog/a-substantive-company-update").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/news/email-alerts/default.aspx").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/investor-relations/subscribe-press-releases/")
                .expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/investor-relations/why-invest/default.aspx")
                .expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/investor-relations/clawback-policy/").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/investor-relations/governance/executive-management/default.aspx")
                .expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/newsroom/brand-guides").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/news/news-archive-2020-2019/").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse(
                "https://example.com/investor-relations/five-ownership-restriction/default.aspx"
            )
            .expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/investor-relations/why-own-example/default.aspx")
                .expect("URL")
        ));
        assert!(is_article_like_path(
            &Url::parse("https://example.com/blog/why-own-a-rental-property").expect("URL")
        ));
        assert!(is_article_like_path(
            &Url::parse("https://example.com/news/webinar").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/news/news-alerts").expect("URL")
        ));
        assert!(is_article_like_path(
            &Url::parse("https://example.com/news/media-resources/news/company-launch")
                .expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/press/coverage").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/blog/pillar/security").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/blogs/news/tagged/security").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/news/events").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/investors/governance-documents").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/insights/white-papers").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse(
                "https://example.com/news-releases/business-technology-latest-news/software-list"
            )
            .expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/investors/press-releases/P10").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/blog/2026/07").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/blog/all-stories.html").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/blog/posts/").expect("URL")
        ));
        assert!(is_article_like_path(
            &Url::parse("https://example.com/blog/company-wins-the-fortune-500-list").expect("URL")
        ));
        assert!(is_article_like_path(
            &Url::parse("https://example.com/journal/company-launch").expect("URL")
        ));
        assert!(is_article_like_path(
            &Url::parse("https://example.com/blog-posts/company-launch").expect("URL")
        ));
        assert!(is_article_like_path(
            &Url::parse("https://example.com/posts/company-launch").expect("URL")
        ));
        assert!(is_article_like_path(
            &Url::parse("https://example.com/changelog/company-launch").expect("URL")
        ));
        assert!(is_article_like_path(
            &Url::parse("https://blog.example.com/current-company-launch").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/current-company-launch").expect("URL")
        ));
        assert!(is_article_like_path(
            &Url::parse("https://example.com/news/events/company-conference").expect("URL")
        ));
        assert!(!is_article_like_path(
            &Url::parse("https://example.com/blog/in-the-news/").expect("URL")
        ));
    }

    #[test]
    fn recipe_link_selection_accepts_blog_post_collection_detail_links() {
        let base_url = Url::parse("https://example.com/blog").expect("listing URL");
        let html = r#"<!doctype html><html><body><main>
            <a href="/blog-posts">All blog posts</a>
            <a href="/blog-posts/current-product-launch">Current product launch</a>
            <a href="/blog-posts/current-research-update">Current research update</a>
        </main></body></html>"#;
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: Vec::new(),
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, html, &recipe, 10).expect("extract article links");

        assert_eq!(
            links.iter().map(|link| link.url.path()).collect::<Vec<_>>(),
            vec![
                "/blog-posts/current-product-launch",
                "/blog-posts/current-research-update",
            ]
        );
    }

    #[test]
    fn recipe_links_preserve_same_card_release_documents_as_thin_page_fallbacks() {
        let base_url = Url::parse("https://www.example.com/news-release").expect("listing URL");
        let html = r#"<!doctype html><html><body><main>
            <div class="release-card">
                <div>May 17, 2026</div>
                <a href="/news-release/notice-regarding-media-articles">
                    Notice Regarding Media Articles (PDF/69KB)
                </a>
                <a href="https://library.example.com/assets/media-articles.pdf">
                    Notice Regarding Media Articles (PDF/69KB)
                </a>
            </div>
        </main></body></html>"#;
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: vec!["/news-release/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::PublicationBoundary,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, html, &recipe, 10).expect("extract article links");

        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].document_url.as_ref().map(Url::as_str),
            Some("https://library.example.com/assets/media-articles.pdf")
        );
        let item = document_backed_listing_item(&links[0], Utc::now())
            .expect("build document-backed item");
        assert_eq!(
            item.external_id.as_deref(),
            Some("https://www.example.com/news-release/notice-regarding-media-articles")
        );
        assert_eq!(
            item.url.as_str(),
            "https://library.example.com/assets/media-articles.pdf"
        );
        assert_eq!(
            item.payload["extraction_contract"],
            json!("official-listing-document.v1")
        );
        assert_eq!(item.payload["document_backed"], json!(true));
        assert!(item.published_at.is_some());
    }

    #[test]
    fn recipe_links_do_not_treat_a_direct_pdf_as_its_own_article_fallback() {
        let base_url = Url::parse("https://www.example.com/news-release").expect("listing URL");
        let html = r#"<!doctype html><html><body><main>
            <div class="release-card">
                <a href="/news-release/media-articles.pdf">
                    Notice Regarding Media Articles (PDF/69KB)
                </a>
            </div>
        </main></body></html>"#;
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: vec!["/news-release/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::PublicationBoundary,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, html, &recipe, 10).expect("extract article links");

        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].url.as_str(),
            "https://www.example.com/news-release/media-articles.pdf"
        );
        assert!(links[0].document_url.is_none());
    }

    #[test]
    fn recipe_link_selection_accepts_root_slugs_only_on_editorial_subdomains() {
        let blog_url = Url::parse("https://blog.example.com/").expect("blog URL");
        let html = r#"<!doctype html><html><body><main>
            <a href="./current-product-launch">Current product launch</a>
            <a href="./current-research-update">Current research update</a>
        </main></body></html>"#;
        let mut recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: blog_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["blog.example.com".to_owned()],
            include_path_prefixes: Vec::new(),
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&blog_url, html, &recipe, 10).expect("extract article links");
        assert_eq!(links.len(), 2);

        let media_url = Url::parse("https://media.example.com/").expect("media URL");
        recipe.publication_url = media_url.clone();
        recipe.allowed_hosts = vec!["media.example.com".to_owned()];
        let (links, _) =
            extract_recipe_links(&media_url, html, &recipe, 10).expect("extract media links");
        assert_eq!(links.len(), 2);

        let main_site_url = Url::parse("https://example.com/").expect("main site URL");
        recipe.publication_url = main_site_url.clone();
        recipe.allowed_hosts = vec!["example.com".to_owned()];
        let (links, _) = extract_recipe_links(&main_site_url, html, &recipe, 10)
            .expect("filter main-site root links");
        assert!(links.is_empty());
    }

    #[test]
    fn recipe_host_scope_accepts_aliases_and_parent_hosts_without_sibling_expansion() {
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: Url::parse("https://research.example.com/blog")
                .expect("publication URL"),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["research.example.com".to_owned()],
            include_path_prefixes: Vec::new(),
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::PublicationBoundary,
            evidence_article_urls: Vec::new(),
        };

        assert!(recipe_host_allowed(
            &Url::parse("https://www.research.example.com/blog/current-result").expect("alias URL"),
            &recipe,
        ));
        assert!(recipe_host_allowed(
            &Url::parse("https://www.example.com/quantum/blog/current-result").expect("parent URL"),
            &recipe,
        ));
        assert!(!recipe_host_allowed(
            &Url::parse("https://developer.example.com/article/api-concepts").expect("sibling URL"),
            &recipe,
        ));
    }

    #[test]
    fn recipe_host_scope_blocks_implicit_utility_and_non_production_hosts() {
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: Url::parse("https://example.com/news").expect("publication URL"),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: Vec::new(),
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::PublicationBoundary,
            evidence_article_urls: Vec::new(),
        };

        assert!(!recipe_host_allowed(
            &Url::parse("https://tutoriales.example.com/es/articles/how-to-reset")
                .expect("tutorial URL"),
            &recipe,
        ));
        assert!(!recipe_host_allowed(
            &Url::parse("https://preview.example.com/news/current-result").expect("preview URL"),
            &recipe,
        ));
        assert!(recipe_host_allowed(
            &Url::parse("https://docs.example.com/changelog/current-release")
                .expect("changelog URL"),
            &recipe,
        ));

        let mut explicitly_evidenced = recipe;
        explicitly_evidenced.allowed_hosts = vec!["tutoriales.example.com".to_owned()];
        assert!(recipe_host_allowed(
            &Url::parse("https://tutoriales.example.com/es/articles/how-to-reset")
                .expect("explicit tutorial URL"),
            &explicitly_evidenced,
        ));
    }

    #[test]
    fn accepts_framer_article_split_across_rich_text_containers() {
        let url =
            Url::parse("https://blog.example.com/current-product-launch").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head>
                <meta property="og:type" content="website">
                <meta property="og:title" content="Example Blog - Current product launch">
            </head><body>
                <header data-framer-name="Content">
                    <main data-framer-name="Hero">
                        <div data-framer-component-type="RichTextContainer">
                            <h1>Current product launch</h1>
                        </div>
                        <article role="presentation"><iframe title="Product video"></iframe></article>
                        <div data-framer-component-type="RichTextContainer"><p>{}</p></div>
                    </main>
                    <div data-framer-component-type="RichTextContainer"><p>{}</p></div>
                    <div data-framer-component-type="RichTextContainer"><p>{}</p></div>
                </header>
            </body></html>"#,
            "Substantive first section of the company article. ".repeat(4),
            "Substantive second section of the company article. ".repeat(4),
            "Substantive third section of the company article. ".repeat(4),
        );

        let item = extract_article_with_title_hint(
            &url,
            url.clone(),
            Some("text/html"),
            body.as_bytes(),
            200,
            Some("Current product launch"),
        )
        .expect("extract split Framer article");

        assert_eq!(
            item.payload["article_body_selector"],
            json!("header[data-framer-name='Content']")
        );
        assert_eq!(item.payload["article_element_count"], json!(0));
        assert!(
            item.body_html
                .as_deref()
                .is_some_and(|body| body.contains("Substantive third section"))
        );
    }

    #[test]
    fn recipe_link_selection_prioritizes_detail_pages_over_navigation_collections() {
        let base_url = Url::parse("https://example.com/newsroom").expect("listing URL");
        let mut html = String::from("<!doctype html><html><body><main>");
        for sector in [
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
            "juliet", "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo",
            "sierra", "tango",
        ] {
            html.push_str(&format!(
                "<a href=\"/news/sector-{sector}\">Sector {sector} News</a>"
            ));
        }
        html.push_str(
            "<a href=\"/news-release/2026/07/21/launch-one\">Acme launches product one</a>\
             <a href=\"/news-release/2026/07/20/launch-two\">Acme launches product two</a>\
             </main></body></html>",
        );
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: Vec::new(),
            exclude_path_prefixes: Vec::new(),
            max_links: 2,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };
        let (links, _) = extract_recipe_links(&base_url, &html, &recipe, 2)
            .expect("extract ranked recipe links");
        assert_eq!(links.len(), 2);
        assert!(
            links
                .iter()
                .all(|link| link.url.path().starts_with("/news-release/"))
        );
    }

    #[test]
    fn recipe_link_selection_preserves_listing_order_across_url_scheme_changes() {
        let base_url = Url::parse("https://example.com/blog").expect("listing URL");
        let html = r#"<!doctype html><html><body><main>
            <a href="/blog/current-flat-one/">Current product launch one</a>
            <a href="/blog/current-flat-two/">Current product launch two</a>
            <a href="/blog/2022/04/18/archived-one/">Archived product launch one</a>
            <a href="/blog/2021/03/17/archived-two/">Archived product launch two</a>
        </main></body></html>"#;
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: vec!["/blog/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 2,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, html, &recipe, 2).expect("extract ordered links");

        assert_eq!(
            links.iter().map(|link| link.url.path()).collect::<Vec<_>>(),
            vec!["/blog/current-flat-one/", "/blog/current-flat-two/",]
        );
    }

    #[test]
    fn listing_card_date_is_a_fallback_but_never_overrides_the_article_page() {
        let base_url = Url::parse("https://example.com/blog").expect("listing URL");
        let html = r#"<!doctype html><html><body><main>
            <div class="tile">
              <a href="/blog/current-update"><img alt=""></a>
              <div class="copy">
                <p>October 27, 2021</p>
                <a href="/blog/current-update"><h4>Current company update</h4></a>
                <p>Summary text that is not a date.</p>
              </div>
            </div>
        </main></body></html>"#;
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: vec!["/blog/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };
        let (links, _) =
            extract_recipe_links(&base_url, html, &recipe, 10).expect("extract dated card");
        assert_eq!(links.len(), 1);
        let listing_date = "2021-10-27T00:00:00Z"
            .parse()
            .expect("listing published time");
        assert_eq!(links[0].published_at_hint, Some(listing_date));

        let article_url = links[0].url.clone();
        let undated_body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"></head><body><article><h1>Current company update</h1><p>{}</p></article></body></html>"#,
            "Substantive independently fetched article body. ".repeat(10),
        );
        let item = extract_article_with_hints(
            &article_url,
            article_url.clone(),
            Some("text/html"),
            undated_body.as_bytes(),
            200,
            links[0].title_hint.as_deref(),
            links[0].published_at_hint,
        )
        .expect("use independently observed listing date");
        assert_eq!(item.published_at, Some(listing_date));
        assert_eq!(item.payload["published_at_source"], "listing_card");

        let dated_body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"><meta property="article:published_time" content="2026-07-20T12:00:00Z"></head><body><article><h1>Current company update</h1><p>{}</p></article></body></html>"#,
            "Substantive independently fetched article body. ".repeat(10),
        );
        let item = extract_article_with_hints(
            &article_url,
            article_url.clone(),
            Some("text/html"),
            dated_body.as_bytes(),
            200,
            links[0].title_hint.as_deref(),
            links[0].published_at_hint,
        )
        .expect("prefer article page date");
        assert_eq!(
            item.published_at,
            Some("2026-07-20T12:00:00Z".parse().expect("article page date"))
        );
        assert_eq!(item.payload["published_at_source"], "article_page");
    }

    #[test]
    fn listing_date_hint_does_not_scan_or_borrow_from_an_unbounded_collection() {
        let base_url = Url::parse("https://example.com/blog").expect("listing URL");
        let noise = (0..600)
            .map(|index| format!("<span>Navigation entry {index}</span>"))
            .collect::<String>();
        let html = format!(
            r#"<!doctype html><html><body><main>
                <time datetime="2020-01-01">Unrelated page date</time>
                {noise}
                <a href="/blog/current-update">Current company update</a>
            </main></body></html>"#
        );
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: vec!["/blog/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, &html, &recipe, 10).expect("extract article link");

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].published_at_hint, None);
    }

    #[test]
    fn recipe_link_selection_keeps_empty_overlay_anchors_for_page_validation() {
        let base_url = Url::parse("https://example.com/blog").expect("listing URL");
        let html = r#"<!doctype html><html><body><main>
            <article>
                <a class="card-overlay" href="/blog/current-product-launch"></a>
                <h2>Current product launch</h2>
            </article>
        </main></body></html>"#;
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: vec!["/blog/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 1,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, html, &recipe, 1).expect("extract overlay link");

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url.path(), "/blog/current-product-launch");
        assert_eq!(links[0].title_hint, None);
    }

    #[test]
    fn recipe_link_selection_merges_a_later_title_for_the_same_overlay_url() {
        let base_url = Url::parse("https://example.com/resources").expect("listing URL");
        let html = r#"<!doctype html><html><body><main>
            <div class="resource-card">
                <a data-framer-name="Post Image"
                   href="/resources/emr-data-migration-made-simple"><img alt=""></a>
                <a data-framer-name="Content"
                   href="/resources/emr-data-migration-made-simple">
                    <h5>EMR Data Migration Made Simple</h5>
                    <p>A summary that must not replace the card headline.</p>
                </a>
            </div>
        </main></body></html>"#;
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: vec!["/resources/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 1,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::PublicationBoundary,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, html, &recipe, 1).expect("merge duplicate anchors");

        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].title_hint.as_deref(),
            Some("EMR Data Migration Made Simple")
        );
    }

    #[test]
    fn recipe_link_selection_recovers_a_semantic_list_item_title_and_visible_date() {
        let base_url = Url::parse("https://example.com/research-and-press").expect("listing URL");
        let html = r#"<!doctype html><html><body><main>
            <div role="listitem" class="collection-item w-dyn-item">
                <a href="/research-and-press/company-appoints-new-director"
                   class="link-block w-inline-block"></a>
                <div class="date w-condition-invisible">November 6, 2024</div>
                <div class="date date-orig">June 23, 2022</div>
                <h1>Company appoints a new director</h1>
            </div>
        </main></body></html>"#;
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: vec!["/research-and-press/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 1,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::PublicationBoundary,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, html, &recipe, 1).expect("extract list item");

        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].title_hint.as_deref(),
            Some("Company appoints a new director")
        );
        assert_eq!(
            links[0].published_at_hint,
            Some("2022-06-23T00:00:00Z".parse().expect("visible date"))
        );
    }

    #[test]
    fn recipe_link_selection_uses_explicit_titles_for_image_links() {
        let base_url = Url::parse("https://example.com/news").expect("listing URL");
        let html = r#"<!doctype html><html><body><main>
            <a href="/news/title-attribute" title="Title attribute company update">
                <img src="/one.jpg" alt="">
            </a>
            <a href="/news/aria-label" aria-label="ARIA label company update"></a>
            <a href="/news/data-title" data-title="Data title company update">Read more</a>
            <a href="/news/image-alt">
                <img src="/three.jpg" alt="Company expands digital banking">
            </a>
            <a href="/news/decorative-cover">
                <img src="/four.jpg" alt="Classic painting used as the article cover">
            </a>
            <a href="/news/decorative-banner">
                <img src="/five.jpg" alt="Blue hero banner with the company logo and white headline">
            </a>
        </main></body></html>"#;
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: vec!["/news/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 6,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, html, &recipe, 6).expect("extract titled image links");

        assert_eq!(
            links
                .iter()
                .map(|link| link.title_hint.as_deref())
                .collect::<Vec<_>>(),
            vec![
                Some("Title attribute company update"),
                Some("ARIA label company update"),
                Some("Data title company update"),
                Some("Company expands digital banking"),
                None,
                None,
            ]
        );
    }

    #[test]
    fn recipe_link_selection_never_borrows_the_previous_card_heading() {
        let base_url = Url::parse("https://example.com/blog").expect("listing URL");
        let html = r#"<!doctype html><html><body><main>
            <div class="card">
                <h5>First company update</h5>
                <p>First summary.</p>
                <div class="action"><a href="/blog/first">Read more</a></div>
            </div>
            <div class="card">
                <h5>Second company update</h5>
                <p>Second summary.</p>
                <div class="action"><a href="/blog/second">Read more</a></div>
            </div>
        </main></body></html>"#;
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: vec!["/blog/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 2,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, html, &recipe, 2).expect("extract adjacent cards");

        assert_eq!(
            links
                .iter()
                .map(|link| link.title_hint.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("First company update"), Some("Second company update"),]
        );
    }

    #[test]
    fn recipe_link_selection_prefers_card_heading_over_wrapped_card_text() {
        let base_url = Url::parse("https://example.com/blog").expect("listing URL");
        let html = r#"<!doctype html><html><body><main>
            <a class="article-card" href="/blog/current-product-launch">
                <div class="category">Product announcements</div>
                <h3>Current product launch</h3>
                <p>A longer summary describing the product launch in detail.</p>
            </a>
        </main></body></html>"#;
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: vec!["/blog/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 1,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, html, &recipe, 1).expect("extract card link");

        assert_eq!(
            links[0].title_hint.as_deref(),
            Some("Current product launch")
        );
    }

    #[test]
    fn recipe_link_selection_recovers_heading_before_generic_cta_paragraph() {
        let base_url = Url::parse("https://example.com/newsroom").expect("listing URL");
        let html = r#"<!doctype html><html><body><main>
            <section class="news-card">
                <h4><strong>Company opens two new technology centers</strong></h4>
                <p>July, 2026<br><a href="/news/company-opens-centers">READ MORE</a></p>
            </section>
        </main></body></html>"#;
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: vec!["/news/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 1,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, html, &recipe, 1).expect("extract CTA link");

        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].title_hint.as_deref(),
            Some("Company opens two new technology centers")
        );
    }

    #[test]
    fn recipe_link_selection_never_uses_generic_cta_as_title_hint() {
        let base_url = Url::parse("https://example.com/newsroom").expect("listing URL");
        let html = r#"<!doctype html><html><body><main>
            <p><a href="/news/current-update">READ MORE</a></p>
        </main></body></html>"#;
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: vec!["/news/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 1,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, html, &recipe, 1).expect("extract CTA link");

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].title_hint, None);
    }

    #[test]
    fn recipe_link_selection_strips_descriptive_read_more_cta_prefix() {
        let base_url = Url::parse("https://example.com/newsroom").expect("listing URL");
        let html = r#"<!doctype html><html><body><main>
            <a href="/news/current-update">
                Read more about Acme launches its next-generation platform
            </a>
        </main></body></html>"#;
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: vec!["/news/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 1,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, html, &recipe, 1).expect("extract descriptive CTA");

        assert_eq!(
            links[0].title_hint.as_deref(),
            Some("Acme launches its next-generation platform")
        );
    }

    #[test]
    fn recipe_link_selection_demotes_embedded_taxonomy_urls() {
        let base_url = Url::parse("https://example.com/blog").expect("listing URL");
        let html = r#"<!doctype html><html><body><main>
            <a href="/blog/ac_blog_category-biobanking">Biobanking resources</a>
            <a href="/blog/ac_blog_tag-sample-storage">Sample storage resources</a>
            <a href="/learning-center/blog/current-product-launch">Current product launch</a>
            <a href="/learning-center/blog/current-research-update">Current research update</a>
        </main></body></html>"#;
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: Vec::new(),
            exclude_path_prefixes: Vec::new(),
            max_links: 2,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, html, &recipe, 2).expect("extract article links");

        assert_eq!(
            links.iter().map(|link| link.url.path()).collect::<Vec<_>>(),
            vec![
                "/learning-center/blog/current-product-launch",
                "/learning-center/blog/current-research-update",
            ]
        );
    }

    #[test]
    fn normalizes_a_valid_double_encoded_utf8_alias_only_when_canonical_is_present() {
        let canonical = Url::parse("https://example.com/news/company%E2%80%99s-update?view=full")
            .expect("canonical URL");
        let alias = Url::parse("https://example.com/news/company%25E2%2580%2599s-update?view=full")
            .expect("alias URL");
        assert_eq!(
            repair_double_encoded_utf8_url(&alias).as_ref(),
            Some(&canonical)
        );
        assert!(
            repair_double_encoded_utf8_url(
                &Url::parse("https://example.com/news/literal%2520token").expect("literal URL")
            )
            .is_none(),
            "ASCII percent escapes can be intentional and must remain unchanged"
        );
        assert!(
            repair_double_encoded_utf8_url(
                &Url::parse("https://example.com/news/invalid%25FFbyte").expect("invalid URL")
            )
            .is_none(),
            "invalid UTF-8 byte runs must remain unchanged"
        );

        let candidates = [
            RecipeLink {
                url: canonical.clone(),
                title_hint: Some("Company’s update".to_owned()),
                published_at_hint: None,
                document_url: None,
            },
            RecipeLink {
                url: alias.clone(),
                title_hint: Some("Company’s update".to_owned()),
                published_at_hint: None,
                document_url: None,
            },
        ];
        let (normalized, failures) = normalize_double_encoded_article_candidates(&candidates);
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].url, canonical);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].url, alias);
        assert_eq!(failures[0].reason, "double_encoded_utf8_url_alias");
        assert!(!failures[0].retryable);
    }

    #[test]
    fn recipe_link_selection_scans_past_large_product_taxonomy_navigation() {
        let base_url = Url::parse("https://example.com/en-us/blog").expect("listing URL");
        let taxonomy_links = (0..450)
            .map(|index| {
                let taxonomy = match index % 3 {
                    0 => "product",
                    1 => "content-type",
                    _ => "audience",
                };
                format!(
                    r#"<a href="/en-us/blog/{taxonomy}/collection-{index}/">Collection {index}</a>"#
                )
            })
            .collect::<String>();
        let article_links = (0..20)
            .map(|index| {
                format!(
                    r#"<a href="/en-us/blog/current-company-article-{index}/">Current company article {index}</a>"#
                )
            })
            .collect::<String>();
        let html =
            format!("<!doctype html><html><body>{taxonomy_links}{article_links}</body></html>");
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: base_url.clone(),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a[href]".to_owned(),
            allowed_hosts: vec!["example.com".to_owned()],
            include_path_prefixes: vec!["/en-us/blog/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 20,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };

        let (links, _) =
            extract_recipe_links(&base_url, &html, &recipe, 20).expect("extract article links");

        assert_eq!(links.len(), 20);
        assert!(
            links
                .iter()
                .all(|link| link.url.path().contains("/current-company-article-"))
        );
    }

    #[test]
    fn prefers_substantive_h1_over_generic_social_title() {
        let url = Url::parse("https://example.com/news/major-product-launch").expect("URL");
        let body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"><meta property="og:title" content="Press Releases"></head><body><article><h1>Acme launches its new platform</h1><p>{}</p></article></body></html>"#,
            "Substantive independently fetched company article body. ".repeat(8),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("substantive H1 should override generic metadata");
        assert_eq!(
            item.title.as_deref(),
            Some("Acme launches its new platform")
        );
    }

    #[test]
    fn prefers_article_h1_over_longer_site_header_h1() {
        let url = Url::parse("https://example.com/news/production-results").expect("URL");
        let body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"></head><body><header><h1>EXAMPLE The Power of Ideas .cls-1,.cls-2{{stroke-width:0px;}}.cls-2{{fill:#c00;}}</h1></header><article><h1>Production and Sales Results for June 2026</h1><p>{}</p></article></body></html>"#,
            "Substantive independently fetched company article body. ".repeat(8),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("the structurally scoped article H1 should win");
        assert_eq!(
            item.title.as_deref(),
            Some("Production and Sales Results for June 2026")
        );
        assert_eq!(item.payload["title_source"], json!("h1"));
    }

    #[test]
    fn rejects_embedded_css_as_a_title_and_uses_social_metadata() {
        let url = Url::parse("https://example.com/news/production-results").expect("URL");
        let body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"><meta property="og:title" content="Production and Sales Results for June 2026"></head><body><main><h1>EXAMPLE The Power of Ideas .cls-1,.cls-2{{stroke-width:0px;}}.cls-2{{fill:#c00;}}</h1><p>{}</p></main></body></html>"#,
            "Substantive independently fetched company article body. ".repeat(8),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("embedded CSS must not become the public headline");
        assert_eq!(
            item.title.as_deref(),
            Some("Production and Sales Results for June 2026")
        );
        assert_eq!(item.payload["title_source"], json!("social_metadata"));
    }

    #[test]
    fn uses_substantive_social_title_after_generic_framework_h1() {
        let url = Url::parse("https://example.com/news/details/product-launch").expect("URL");
        let body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"><meta property="og:title" content="Acme launches its new platform"><title>News Details</title></head><body><article><h1>News Details</h1><p>{}</p></article></body></html>"#,
            "Substantive independently fetched company article body. ".repeat(8),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("substantive social metadata should override a generic framework heading");
        assert_eq!(
            item.title.as_deref(),
            Some("Acme launches its new platform")
        );
        assert_eq!(
            item.payload.get("title_source").and_then(Value::as_str),
            Some("social_metadata")
        );
    }

    #[test]
    fn uses_unique_semantic_heading_after_generic_investor_detail_title() {
        let url = Url::parse("https://example.com/news/news-details/2026/acme-launch/default.aspx")
            .expect("URL");
        let body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"><title>Press Release Details</title></head><body><main><h1>Press Release Details</h1><div class="evergreen-news-headline"><h3 class="evergreen-item-detail-title evergreen-news-title">Acme launches its new platform</h3></div><p>{}</p></main></body></html>"#,
            "Substantive independently fetched company article body. ".repeat(8),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("semantic detail heading should replace generic investor-site chrome");
        assert_eq!(
            item.title.as_deref(),
            Some("Acme launches its new platform")
        );
        assert_eq!(item.payload["title_source"], json!("semantic_heading"));
        assert_eq!(
            item.payload["replaced_page_title"],
            json!("Press Release Details")
        );
    }

    #[test]
    fn treats_breadcrumb_detail_heading_as_generic_page_chrome() {
        let url = Url::parse(
            "https://example.com/news/press-release-details/2026/acme-launch/default.aspx",
        )
        .expect("URL");
        let body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"><title>Newsroom</title></head><body><main><h1>Home Home &gt; News &gt; Press Releases &gt; Press Release Details</h1><h3 class="module-news-title">Acme opens a new research center</h3><p>{}</p></main></body></html>"#,
            "Substantive independently fetched company article body. ".repeat(8),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("breadcrumb chrome should not replace the detail headline");
        assert_eq!(
            item.title.as_deref(),
            Some("Acme opens a new research center")
        );
        assert_eq!(item.payload["title_source"], json!("semantic_heading"));
    }

    #[test]
    fn rejects_article_page_with_only_generic_listing_titles() {
        let url = Url::parse("https://example.com/news/latest").expect("URL");
        let body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"><meta property="og:title" content="Press Releases | Acme"><title>Newsroom | Acme</title></head><body><article><h1>Press Releases</h1><p>{}</p></article></body></html>"#,
            "Substantive but non-article listing page body. ".repeat(10),
        );
        let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect_err("generic listing titles must not produce a news item");
        assert!(
            matches!(error, ArticlePageError::GenericListingTitle { .. }),
            "unexpected generic-title error: {error:?}"
        );
        assert_eq!(error.reason(), "generic_listing_title");
        assert!(!error.is_retryable());
    }

    #[test]
    fn rejects_non_editorial_utility_paths_even_with_article_markup() {
        let url = Url::parse("https://example.com/news/newsletter-sign-up").expect("URL");
        let body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"><meta property="og:title" content="Supermicro Newsroom"></head><body><article><h1>Supermicro Newsroom</h1><p>{}</p></article></body></html>"#,
            "Enter an email address to receive investor alerts and newsletters. ".repeat(10),
        );
        let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect_err("subscription forms are not editorial articles");

        assert!(matches!(
            error,
            ArticlePageError::GenericListingTitle { .. }
        ));
        assert_eq!(error.reason(), "generic_listing_title");
    }

    #[test]
    fn rejects_media_asset_and_news_updates_hubs() {
        for (url, title, content) in [
            (
                "https://example.com/newsroom/materials-for-media/",
                "Acme fact sheet, logo, and images",
                "Download Acme logos, fact sheets, executive portraits, and high-resolution images. ",
            ),
            (
                "https://blog.example.com/technology/developer-tools/",
                "Developer tools news and updates | Acme Blog",
                "Browse the latest developer-tools announcements, explainers, and featured stories. ",
            ),
        ] {
            let url = Url::parse(url).expect("URL");
            let body = format!(
                r#"<!doctype html><html><head><meta property="og:type" content="article"><meta property="og:title" content="{title}"></head><body><article><h1>{title}</h1><p>{}</p></article></body></html>"#,
                content.repeat(10),
            );
            let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
                .expect_err("collection hubs are not individual editorial articles");

            assert!(matches!(
                error,
                ArticlePageError::GenericListingTitle { .. }
            ));
            assert_eq!(error.reason(), "generic_listing_title");
        }
    }

    #[test]
    fn rejects_canonical_listing_path_even_with_article_metadata() {
        let url =
            Url::parse("https://example.com/company/blog/channel/home/2").expect("fetched URL");
        let body = format!(
            r#"<!doctype html><html><head><link rel="canonical" href="https://example.com/blog"><meta property="og:type" content="article"></head><body><article><h1>Company blog</h1><p>{}</p></article></body></html>"#,
            "A listing page can contain misleading article markup. ".repeat(10),
        );
        let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect_err("a canonical URL must not bypass the listing-path gate");
        assert!(matches!(error, ArticlePageError::ObviousListingPath { .. }));
        assert_eq!(error.reason(), "obvious_listing_path");

        for (url, title) in [
            (
                "https://example.com/investors/media/news-releases/2026/",
                "News releases 2026 archive - Acme",
            ),
            (
                "https://example.com/investors/press-releases/2025",
                "Press Releases in 2025",
            ),
            ("https://example.com/news/2026", "Search for more"),
            ("https://example.com/news/2025/", "Acme Newsroom"),
            ("https://example.com/news/2025/", "2025 - Acme Corporation"),
        ] {
            assert!(
                is_year_archive_collection(&Url::parse(url).expect("archive URL"), title),
                "{url} with title {title:?}"
            );
        }
        assert!(!is_year_archive_collection(
            &Url::parse("https://blog.zeplin.io/product-news/2025/").expect("yearly roundup URL"),
            "Everything we released in 2025",
        ));
        assert!(!is_year_archive_collection(
            &Url::parse("https://pr.example.com/english/news/3323").expect("numeric article URL"),
            "Company reports June 2026 revenue",
        ));
    }

    #[test]
    fn repairs_a_site_root_canonical_with_independent_listing_evidence() {
        let url = Url::parse("https://example.com/blog/current-product-launch")
            .expect("fetched article URL");
        let body = format!(
            r#"<!doctype html><html><head><link rel="canonical" href="https://www.example.com/"><meta property="og:type" content="article"></head><body><article><h1>Current product launch</h1><p>{}</p></article></body></html>"#,
            "Substantive independently fetched company article body. ".repeat(10),
        );
        let item = extract_article_with_title_hint(
            &url,
            url.clone(),
            Some("text/html"),
            body.as_bytes(),
            200,
            Some("Current product launch"),
        )
        .expect("repair a broken site-root canonical");

        assert_eq!(item.canonical_url.as_ref(), Some(&url));
        assert_eq!(
            item.payload["replaced_canonical_url"],
            json!("https://www.example.com/")
        );
        assert_eq!(
            item.payload["canonical_repair_reason"],
            json!("site_root_canonical_with_listing_evidence")
        );
    }

    #[test]
    fn repairs_an_embedded_scheme_canonical_but_rejects_archive_headings() {
        let article_url =
            Url::parse("https://www.example.com/blog/current-product-launch").expect("article URL");
        let article = format!(
            r#"<!doctype html><html><head><link rel="canonical" href="https://https/www.example.com/blog/current-product-launch"><meta property="og:type" content="article"></head><body><article><h1>Current product launch</h1><p>{}</p></article></body></html>"#,
            "Substantive independently fetched company article body. ".repeat(10),
        );
        let item = extract_article_with_title_hint(
            &article_url,
            article_url.clone(),
            Some("text/html"),
            article.as_bytes(),
            200,
            Some("Current product launch"),
        )
        .expect("repair malformed canonical");
        assert_eq!(item.canonical_url.as_ref(), Some(&article_url));
        assert_eq!(
            item.payload["canonical_repair_reason"],
            json!("malformed_declared_canonical")
        );
        assert_eq!(
            item.payload["replaced_canonical_url"],
            json!("https://https/www.example.com/blog/current-product-launch")
        );

        let archive_url =
            Url::parse("https://www.example.com/blog/credit-basics/").expect("archive URL");
        let archive = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"></head><body><h1 class="page-title ast-archive-title">Credit Basics</h1><article><p>{}</p></article></body></html>"#,
            "A taxonomy archive can still expose substantive card text. ".repeat(10),
        );
        let error = extract_article_with_title_hint(
            &archive_url,
            archive_url.clone(),
            Some("text/html"),
            archive.as_bytes(),
            200,
            Some("Credit Basics"),
        )
        .expect_err("an explicit archive heading must remain a collection");
        assert!(
            matches!(error, ArticlePageError::GenericListingTitle { .. }),
            "unexpected archive error: {error:?}"
        );
    }

    #[test]
    fn rejects_terminal_collection_outside_an_editorial_root() {
        let url =
            Url::parse("https://example.com/resource-center/white-papers").expect("collection URL");
        let body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"></head><body><article><h1>White Papers</h1><p>{}</p></article></body></html>"#,
            "A collection template can emit misleading article metadata. ".repeat(10),
        );
        let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect_err("a terminal collection must not become an article");
        assert!(matches!(error, ArticlePageError::ObviousListingPath { .. }));
        assert_eq!(error.reason(), "obvious_listing_path");
    }

    #[test]
    fn rejects_locale_and_breadcrumb_prefixed_editorial_collections() {
        let locale_url = Url::parse("https://example.com/newsroom/fr/").expect("collection URL");
        let locale_body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"></head><body><main><h1>Notre espace presse</h1><p>{}</p></main></body></html>"#,
            "A localized publication root can emit misleading article metadata. ".repeat(10),
        );
        let error = extract_article(
            &locale_url,
            locale_url.clone(),
            Some("text/html"),
            locale_body.as_bytes(),
            200,
        )
        .expect_err("a locale publication root is not an individual article");
        assert!(matches!(
            error,
            ArticlePageError::GenericListingTitle { .. }
        ));

        let short_slug_url =
            Url::parse("https://example.com/blog/ga").expect("short article slug URL");
        let short_slug_body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"><meta property="article:published_time" content="2026-07-01T12:00:00Z"></head><body><article><h1>General availability and our seed round</h1><p>{}</p></article></body></html>"#,
            "A substantive company launch article can use a two-letter slug. ".repeat(10),
        );
        let short_slug_item = extract_article(
            &short_slug_url,
            short_slug_url.clone(),
            Some("text/html"),
            short_slug_body.as_bytes(),
            200,
        )
        .expect("a two-letter article slug without collection evidence remains valid");
        assert_eq!(
            short_slug_item.title.as_deref(),
            Some("General availability and our seed round")
        );
        let taxonomy_url =
            Url::parse("https://example.com/blog/ai").expect("taxonomy collection URL");
        assert!(is_counted_taxonomy_collection_title(
            &taxonomy_url,
            "Artificial intelligence (309)",
            1_764,
        ));
        assert!(!is_counted_taxonomy_collection_title(
            &taxonomy_url,
            "AI market outlook (2026)",
            1_764,
        ));

        let media_url =
            Url::parse("https://example.com/newsroom/media/manufacturing").expect("media URL");
        let media_body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"></head><body><main><h1>Newsroom Media Manufacturing</h1><p>{}</p></main></body></html>"#,
            "A static media asset collection can emit misleading article metadata. ".repeat(10),
        );
        let error = extract_article_with_title_hint(
            &media_url,
            media_url.clone(),
            Some("text/html"),
            media_body.as_bytes(),
            200,
            Some("Manufacturing"),
        )
        .expect_err("breadcrumb-prefixed media pages are collections");
        assert!(matches!(
            error,
            ArticlePageError::GenericListingTitle { .. }
        ));
    }

    #[test]
    fn rejects_year_archive_but_preserves_numeric_article_ids() {
        let archive_url = Url::parse("https://example.com/news/2026").expect("archive URL");
        let archive_body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"></head><body><article><h1>Newsroom - 2026</h1><p>{}</p></article></body></html>"#,
            "A yearly archive can contain misleading article markup. ".repeat(10),
        );
        let error = extract_article(
            &archive_url,
            archive_url.clone(),
            Some("text/html"),
            archive_body.as_bytes(),
            200,
        )
        .expect_err("a generic year archive must not become an article");
        assert!(matches!(
            error,
            ArticlePageError::YearArchiveCollection { .. }
        ));
        assert_eq!(error.reason(), "year_archive_collection");

        let archive_without_year_in_title_body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"></head><body><article><h1>News Archive</h1><p>{}</p></article></body></html>"#,
            "A yearly archive can contain misleading article markup. ".repeat(10),
        );
        let error = extract_article_with_title_hint(
            &archive_url,
            archive_url.clone(),
            Some("text/html"),
            archive_without_year_in_title_body.as_bytes(),
            200,
            Some("News Archive"),
        )
        .expect_err("an archive title need not repeat the year to be a collection");
        assert!(matches!(
            error,
            ArticlePageError::GenericListingTitle { .. }
                | ArticlePageError::YearArchiveCollection { .. }
        ));

        let article_url = Url::parse("https://example.com/news/2026").expect("article URL");
        let article_body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"></head><body><article><h1>Acme launches its new platform</h1><p>{}</p></article></body></html>"#,
            "Substantive independently fetched company article body. ".repeat(10),
        );
        extract_article(
            &article_url,
            article_url.clone(),
            Some("text/html"),
            article_body.as_bytes(),
            200,
        )
        .expect("a numeric article ID with a substantive title remains valid");

        let month_archive_url =
            Url::parse("https://example.com/blog/2026/07").expect("month archive URL");
        let month_archive_body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"></head><body><article><h1>July 2026</h1><p>{}</p></article></body></html>"#,
            "A monthly archive can contain misleading article markup. ".repeat(10),
        );
        let error = extract_article(
            &month_archive_url,
            month_archive_url.clone(),
            Some("text/html"),
            month_archive_body.as_bytes(),
            200,
        )
        .expect_err("a generic month archive must not become an article");
        assert!(matches!(error, ArticlePageError::ObviousListingPath { .. }));
        assert_eq!(error.reason(), "obvious_listing_path");
    }

    #[test]
    fn uses_listing_title_hint_when_page_h1_is_generic() {
        let url = Url::parse("https://example.com/news/company-launch").expect("URL");
        let body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"><meta property="og:title" content="Acme | Building the future"></head><body><article><h1>News</h1><p>{}</p></article></body></html>"#,
            "Substantive independently fetched company article body. ".repeat(8),
        );
        let item = extract_article_with_title_hint(
            &url,
            url.clone(),
            Some("text/html"),
            body.as_bytes(),
            200,
            Some("Acme launches its new platform"),
        )
        .expect("listing title should repair generic page metadata");
        assert_eq!(
            item.title.as_deref(),
            Some("Acme launches its new platform")
        );
        assert_eq!(item.payload["title_source"], json!("listing_anchor"));
        assert_eq!(item.payload["replaced_page_title"], json!("News"));
    }

    #[test]
    fn chooses_the_most_substantive_h1() {
        let url = Url::parse("https://example.com/blog/company-update").expect("URL");
        let body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"><meta property="og:title" content="Company blog"></head><body><main><h1 class="sr-only">Acme</h1><h1>Acme announces a major platform update</h1><p>{}</p></main></body></html>"#,
            "Substantive independently fetched company article body. ".repeat(8),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("long substantive H1 should win");
        assert_eq!(
            item.title.as_deref(),
            Some("Acme announces a major platform update")
        );
        assert_eq!(item.payload["title_source"], json!("h1"));
    }

    #[test]
    fn ignores_hidden_modal_h1_when_selecting_the_article_headline() {
        let url = Url::parse("https://example.com/insights/how-we-select-stocks").expect("URL");
        let body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"></head><body><main><article><h1>How Do We Select Micro-Cap Stocks?</h1><p>{}</p></article><div class="modal" style="display: none"><h1>Welcome to Example Investment Partners</h1></div></main></body></html>"#,
            "Substantive independently fetched company article body. ".repeat(8),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("hidden subscription chrome must not replace the article headline");
        assert_eq!(
            item.title.as_deref(),
            Some("How Do We Select Micro-Cap Stocks?")
        );
        assert_eq!(item.payload["title_source"], json!("h1"));
    }

    #[test]
    fn repairs_repeated_sitewide_titles_from_listing_hints() {
        let mut items = (0..2)
            .map(|index| RawCrawlItem {
                source_item_key: format!("item-{index}"),
                external_id: None,
                url: Url::parse(&format!("https://example.com/news/item-{index}")).expect("URL"),
                canonical_url: None,
                title: Some("Acme | Building the future".to_owned()),
                summary_html: None,
                body_html: Some("<p>Body</p>".to_owned()),
                published_at: None,
                payload: json!({
                    "listing_title_hint": format!("Distinct company update {index}")
                }),
            })
            .collect::<Vec<_>>();
        repair_repeated_page_titles(&mut items);
        assert_eq!(items[0].title.as_deref(), Some("Distinct company update 0"));
        assert!(
            items
                .iter()
                .all(|item| { item.payload["title_source"] == json!("listing_anchor_repair") })
        );
    }

    #[test]
    fn repeated_title_repair_preserves_true_recurring_release_titles() {
        let current_title = "Acme announces progress on its share buyback programme";
        let mut items = [("July 1, 2026", "item-1"), ("July 8, 2026", "item-2")]
            .into_iter()
            .map(|(date, key)| RawCrawlItem {
                source_item_key: key.to_owned(),
                external_id: None,
                url: Url::parse(&format!("https://example.com/news/{key}")).expect("URL"),
                canonical_url: None,
                title: Some(current_title.to_owned()),
                summary_html: None,
                body_html: Some("<p>Body</p>".to_owned()),
                published_at: None,
                payload: json!({
                    "listing_title_hint": format!("{date} {current_title}")
                }),
            })
            .collect::<Vec<_>>();

        repair_repeated_page_titles(&mut items);

        assert!(
            items
                .iter()
                .all(|item| item.title.as_deref() == Some(current_title))
        );
    }

    #[test]
    fn title_selection_uses_listing_and_metadata_consensus_over_unrelated_h1s() {
        let url = Url::parse("https://example.com/blog/legal-ai-stack").expect("URL");
        let body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"><meta property="og:title" content="AI Stack for Legal Professionals - Example"></head><body><main><h1>AI Stack for Legal Professionals</h1><p>{}</p><aside><h1>Longer unrelated recommended article headline</h1></aside></main></body></html>"#,
            "Substantive independently fetched company article body. ".repeat(8),
        );

        let item = extract_article_with_title_hint(
            &url,
            url.clone(),
            Some("text/html"),
            body.as_bytes(),
            200,
            Some("AI Stack for Legal Professionals"),
        )
        .expect("listing and metadata consensus should identify the article title");

        assert_eq!(
            item.title.as_deref(),
            Some("AI Stack for Legal Professionals")
        );
    }

    #[test]
    fn title_selection_matches_document_title_to_the_correct_h1() {
        let url = Url::parse("https://example.com/blog/mobile-testing").expect("URL");
        let body = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"><title>Agentic Mobile Testing 101</title></head><body><main><h1>Agentic Mobile Testing 101</h1><p>{}</p><aside><h1>The much longer unrelated recommended article headline</h1></aside></main></body></html>"#,
            "Substantive independently fetched company article body. ".repeat(8),
        );

        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("document metadata should disambiguate multiple H1 elements");

        assert_eq!(item.title.as_deref(), Some("Agentic Mobile Testing 101"));
        assert_eq!(item.payload["title_source"], json!("h1"));
    }

    #[test]
    fn title_selection_uses_url_backed_metadata_consensus_over_one_unrelated_h1() {
        let url = Url::parse(
            "https://www.zacks.com/stock/news/2959634/\
             smith-nephew-expands-asc-platform-to-support-value-based-care",
        )
        .expect("URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:type" content="article">
            <meta property="og:title" content="Smith+Nephew Expands ASC Platform to Support Value-Based Care">
            <meta name="twitter:title" content="Smith+Nephew Expands ASC Platform to Support Value-Based Care">
            <title>Smith+Nephew Expands ASC Platform to Support Value-Based Care - Zacks.com</title>
            </head><body><main><article>
            <h1>S&amp;P 500 Q2 Earnings: Stripping Out Outsized Impact of GOOGL &amp; MU</h1>
            <p>{}</p>
            </article></main></body></html>"#,
            "Smith and Nephew expanded its ambulatory surgery center platform. ".repeat(8),
        );

        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("metadata consensus should replace an unrelated recommendation headline");

        assert_eq!(
            item.title.as_deref(),
            Some("Smith+Nephew Expands ASC Platform to Support Value-Based Care")
        );
        assert_eq!(item.payload["title_source"], json!("social_metadata"));
        assert_eq!(
            item.payload["replaced_page_title"],
            json!("S&P 500 Q2 Earnings: Stripping Out Outsized Impact of GOOGL & MU")
        );
    }

    #[test]
    fn publication_identity_preserves_resource_query_parameters() {
        let publication = Url::parse("https://www.example.com/news/").expect("URL");
        let tracked =
            Url::parse("http://example.com/news/index.html?utm_source=test").expect("URL");
        let query_article =
            Url::parse("https://example.com/news/index.php?content_id=682").expect("URL");
        let category = Url::parse("https://example.com/news/?category=776&lang=en").expect("URL");
        let scanner_noise = Url::parse("https://example.com/news/?host=X&value=test").expect("URL");
        assert!(same_publication_resource(&publication, &tracked));
        assert!(same_publication_resource(&publication, &category));
        assert!(same_publication_resource(&publication, &scanner_noise));
        assert!(!same_publication_resource(&publication, &query_article));
        assert!(!is_generic_listing_title(
            "Acme launches its new platform | Blog"
        ));
        assert!(!is_generic_listing_title(
            "Changelog - June 11, 2026 | Acme"
        ));
        assert!(!is_generic_listing_title(
            "Press Release: Acme launches its new platform"
        ));
        assert!(is_generic_listing_title("Press Releases | Acme"));
        assert!(is_generic_listing_title("Headline"));
        assert!(is_generic_listing_title("Press Release Details"));
        assert!(is_generic_listing_title("News Release Details"));
        assert!(is_generic_listing_title("Press Releases Details"));
        assert!(is_generic_listing_title(
            "Home Home > News > Press Releases > Press Release Details"
        ));
        assert!(is_generic_listing_title("AMCI - Press Releases Detail"));
        assert!(is_generic_listing_title("Investor Email Alerts"));
        assert!(is_generic_listing_title("Gabelli White Papers"));
        assert!(is_generic_listing_title("Governance Documents"));
        assert!(is_generic_listing_title("White Papers"));
        assert!(is_generic_listing_title("All Posts"));
        assert!(is_generic_listing_title("All Posts →"));
        assert!(is_generic_listing_title("Blog Post"));
        assert!(is_generic_listing_title("CGI Blogs"));
        assert!(is_generic_listing_title("Investor Resources"));
        assert!(is_generic_listing_title("Why Invest"));
        assert!(is_generic_listing_title("Why Invest?"));
        assert!(is_generic_listing_title("Cookie Policy"));
        assert!(is_generic_listing_title(
            "Cookies Policy opens in new window"
        ));
        assert!(is_generic_listing_title("ANF Blog | Insights | News"));
        assert!(is_generic_listing_title("Our Company Sub Menu"));
        assert!(!is_generic_listing_title(
            "AI Governance Guidance & Expert Insights | Blog | Okta"
        ));
        assert!(is_generic_listing_title("Brand Guides"));
        assert!(is_generic_listing_title("Presentations & Events | Acme"));
        assert!(is_generic_listing_title(
            "Company Voices . Select to filter."
        ));
        assert!(is_generic_listing_title("Webcasts & presentations"));
        assert!(is_generic_listing_title("Annual Reports & Proxies"));
        assert!(is_generic_listing_title("Contact Info"));
        assert!(is_generic_listing_title("Contact Tigo"));
        assert!(is_generic_listing_title("Contact | Supermicro"));
        assert!(is_generic_listing_title("Click here"));
        assert!(is_generic_listing_title("Release Details"));
        assert!(is_generic_listing_title("Press Details"));
        assert!(is_generic_listing_title("Arrow Icon"));
        assert!(is_generic_listing_title("Stars Icon"));
        assert!(is_generic_listing_title("Image People & Impact"));
        assert!(is_generic_listing_title("Image Link"));
        assert!(is_generic_listing_title("Scientist Stories"));
        assert!(is_generic_listing_title("Scientist Stories | Pfizer"));
        assert!(is_generic_listing_title("Company & Portfolio News"));
        assert!(is_generic_listing_title(
            "Updates Archives - Cowrywise Blog"
        ));
        assert!(is_generic_listing_title("We announced"));
        assert!(is_generic_listing_title("Know More"));
        assert!(is_generic_listing_title("Finance"));
        assert!(is_generic_listing_title("Read the full article"));
        assert!(is_generic_listing_title("View Media Kit"));
        assert!(is_generic_listing_title("LinkedIn"));
        assert!(is_generic_listing_title("Next Page"));
        assert!(is_generic_listing_title("Previous"));
        assert!(is_generic_listing_title("RSS Feeds"));
        assert!(is_generic_listing_title("About ADT"));
        assert!(is_generic_listing_title("Guides & Articles"));
        assert!(is_generic_listing_title("General Information"));
        assert!(is_generic_listing_title("The latest product offerings"));
        assert!(is_generic_listing_title("Gen Blogs | Impact"));
        assert!(is_generic_listing_title("Gen Blogs | Family of Brands"));
        assert!(is_generic_listing_title("Categories"));
        assert!(is_generic_listing_title("FY26 Annual Report"));
        assert!(is_generic_listing_title("2026 Proxy Statement"));
        assert!(is_generic_listing_title("News Releases Details"));
        assert!(is_generic_listing_title("Show page 16"));
        assert!(is_generic_listing_title("Show 100 per page"));
        assert!(is_generic_listing_title("Media Resources arrow_forward"));
        for collection_title in [
            "All Articles | Ready.net",
            "Clinical Case Studies",
            "Contact Frontier Media Relations",
            "Fixed Income",
            "Insights Library",
            "Media Request | National Vision",
            "Submit Media Request",
            "New Product Announcements",
            "Other Archives",
            "People and Culture",
            "Sign up today",
            "Site-Seeing Gallery",
        ] {
            assert!(
                is_generic_listing_title(collection_title),
                "{collection_title:?}"
            );
        }
        for section_title in [
            "404 Error",
            "404 Page Not Found",
            "Agreement Manager",
            "All Industries",
            "Artificial Intelligence",
            "Archives",
            "BlackRock Investment Institute",
            "Bra Talk",
            "Brochure",
            "Calendar",
            "Clientes",
            "Coming Soon",
            "Corporate",
            "Customer",
            "Dashboard",
            "Developer Support Articles",
            "Dividends",
            "Downloads",
            "Earnings",
            "Ecommerce",
            "Embedded",
            "ESG News",
            "Facebook",
            "Features",
            "Footwear",
            "Heritage",
            "Home Care Marketing",
            "Images .",
            "Industry",
            "Insights",
            "Investor",
            "Investment Team Voices",
            "Latest Articles",
            "Lighting",
            "Newer Posts",
            "Older Posts",
            "Page Not Found",
            "Previous Posts",
            "Producto",
            "Research and Reports",
            "Results:",
            "See More",
            "Shipping",
            "ShopTalk",
            "Stories and Perspectives",
            "Subscribe",
            "Tax Forms",
            "Templates",
            "Vaccines",
            "Webinars",
            "Weekly Market Performance",
        ] {
            assert!(is_generic_listing_title(section_title), "{section_title:?}");
        }
        assert!(is_generic_listing_title("View Transcript"));
        assert!(is_generic_listing_title(
            "Never miss an update: Sign up for updates, exclusive insights, and product releases."
        ));
        assert!(is_generic_listing_title("NextNRG |"));
        assert!(is_generic_listing_title("| Draftwise"));
        assert!(!is_generic_listing_title(
            "Company launches a new product |"
        ));
        assert!(!is_usable_article_title("April 16, 2019"));
        assert!(!is_usable_article_title("Feb. 27 2022"));
        assert!(!is_usable_article_title("Sept. 7, 2020"));
        assert!(!is_usable_article_title("July 2023"));
        assert!(!is_usable_article_title("Day: 8 July 2026"));
        assert!(!is_usable_article_title("L a u n c h e s"));
        assert!(!is_usable_article_title(
            "O p o r t u n i d a d e n o T r á f e g o P a g o"
        ));
        assert_eq!(
            normalize_article_title("Read more about Acme launches a new platform"),
            "Acme launches a new platform"
        );
        assert!(is_usable_article_title(
            "Company reports first-quarter results on April 16, 2019"
        ));
        assert!(!is_generic_listing_title(
            "New platform advances cancer research"
        ));
    }

    #[test]
    fn accepts_semantic_cms_body_after_article_signal_passes() {
        let url =
            Url::parse("https://example.com/blog/a-webflow-company-update").expect("article URL");
        let body = format!(
            "<!doctype html><html><head><title>Company update</title></head><body><h1>Company update</h1><div class=\"w-richtext\"><p>{}</p></div></body></html>",
            "Substantive independently fetched CMS article body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("article signal plus semantic CMS body identifies an article");
        assert_eq!(item.title.as_deref(), Some("Company update"));
        assert_eq!(item.payload["article_body_selector"], json!(".w-richtext"));
    }

    #[test]
    fn accepts_a_dnn_main_content_article_body_after_article_signal_passes() {
        let url =
            Url::parse("https://example.com/News-Highlights/News/News-Release/company-update")
                .expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <script type="application/ld+json">
              {{"@type":"NewsArticle","headline":"Company update",
                "datePublished":"2026-07-20T12:00:00Z"}}
            </script>
            </head><body>
            <h1>Company update</h1>
            <div class="article details">
              <div class="main_content"><p>{}</p></div>
            </div>
            <div class="footer"><h2>Latest news</h2><p>{}</p></div>
            </body></html>"#,
            "Substantive independently fetched DNN release body. ".repeat(10),
            "Unrelated footer release card. ".repeat(20),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("article metadata plus a DNN main-content root identifies the release");

        assert_eq!(item.title.as_deref(), Some("Company update"));
        assert_eq!(
            item.payload["article_body_selector"],
            json!(".main_content")
        );
        assert!(
            item.body_html
                .as_deref()
                .is_some_and(|html| html.contains("DNN release body"))
        );
        assert!(
            item.body_html
                .as_deref()
                .is_some_and(|html| !html.contains("footer release card"))
        );
    }

    #[test]
    fn sanitized_body_scoring_does_not_let_embedded_css_shadow_the_article() {
        let url =
            Url::parse("https://example.com/blog/company-freight-update").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:type" content="article">
            <meta property="og:title" content="Company freight update">
            </head><body>
            <div class="field field--name-body field__item">
              <style>{}</style>
            </div>
            <h1>Company freight update</h1>
            <article>
              <section class="component-content">
                <div class="content__content-content"><p>{}</p></div>
              </section>
            </article>
            </body></html>"#,
            ".component-content table { font-size: 18px; margin: 1rem; }".repeat(30),
            "Substantive independently fetched Drupal article body. ".repeat(12),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("sanitized body scoring must reach the real article prose");

        assert_eq!(item.title.as_deref(), Some("Company freight update"));
        assert!(
            item.payload["sanitized_content_chars"]
                .as_u64()
                .is_some_and(|chars| chars >= 200)
        );
        assert!(
            item.body_html
                .as_deref()
                .is_some_and(|html| html.contains("Drupal article body"))
        );
        assert!(
            item.body_html
                .as_deref()
                .is_some_and(|html| !html.contains("font-size"))
        );
    }

    #[test]
    fn preserves_a_semantic_article_body_wrapped_in_an_aside() {
        let url =
            Url::parse("https://example.com/blog/a-sanity-company-update").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:type" content="article">
            <meta property="og:title" content="A Sanity company update">
            <meta property="article:published_time" content="2026-07-20T12:00:00Z">
            </head><body><main>
            <h1>A Sanity company update</h1>
            <aside id="blog-content" class="blog-rich-text w-richtext">
                <p>{}</p>
            </aside>
            </main></body></html>"#,
            "Substantive independently fetched CMS article body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("a selected semantic body must survive its chrome-like outer tag");

        assert_eq!(item.title.as_deref(), Some("A Sanity company update"));
        assert_eq!(item.payload["article_body_selector"], json!(".w-richtext"));
        assert!(
            item.body_html
                .as_deref()
                .is_some_and(|html| html.contains("Substantive independently fetched"))
        );
        assert!(
            item.payload["sanitized_content_chars"]
                .as_u64()
                .is_some_and(|chars| chars >= 200)
        );
    }

    #[test]
    fn accepts_listing_title_with_semantic_body_when_cms_omits_article_markup() {
        let url =
            Url::parse("https://example.com/blog/a-webflow-company-update").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head><title>Acme</title></head><body>
            <div class="heading-style-h2">A Webflow company update</div>
            <div class="text-rich-text w-richtext"><p>{}</p></div>
            </body></html>"#,
            "Substantive independently fetched CMS article body. ".repeat(10),
        );
        let item = extract_article_with_title_hint(
            &url,
            url.clone(),
            Some("text/html"),
            body.as_bytes(),
            200,
            Some("A Webflow company update"),
        )
        .expect("listing evidence plus a semantic body identifies the CMS article");
        assert_eq!(item.title.as_deref(), Some("A Webflow company update"));
        assert_eq!(item.payload["article_body_selector"], json!(".w-richtext"));
        assert_eq!(
            item.payload["article_signals"],
            json!(["article_like_path_with_listing_title_and_semantic_body"])
        );
    }

    #[test]
    fn accepts_a_listing_proven_resource_article_but_not_resource_pages_alone() {
        let url = Url::parse("https://example.com/resources/emr-data-migration-made-simple")
            .expect("resource article URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:type" content="website">
            <meta property="og:title" content="EMR Data Migration Made Simple">
            </head><body><main>
            <h1>EMR Data Migration Made Simple</h1>
            <div data-framer-name="Content" data-framer-component-type="RichTextContainer">
              <p>{}</p>
            </div>
            </main></body></html>"#,
            "Substantive independently fetched resource article body. ".repeat(10),
        );
        let item = extract_article_with_title_hint(
            &url,
            url.clone(),
            Some("text/html"),
            body.as_bytes(),
            200,
            Some("EMR Data Migration Made Simple"),
        )
        .expect("listing title plus semantic body identifies the resource article");
        assert_eq!(
            item.payload["article_signals"],
            json!(["article_like_path_with_listing_title_and_semantic_body"])
        );

        let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect_err("a resource detail page alone remains insufficient");
        assert!(matches!(
            error,
            ArticlePageError::MissingArticleSignal { .. }
        ));
        assert!(!is_listing_proven_article_like_path(
            &Url::parse("https://example.com/resources").expect("resource root")
        ));
        assert!(!is_listing_proven_article_like_path(
            &Url::parse("https://example.com/resources/contact").expect("utility child")
        ));
        assert!(is_listing_proven_article_like_path(
            &Url::parse("https://example.com/media/perspectives/stories/company-growth")
                .expect("nested perspective")
        ));
        assert!(is_listing_proven_article_like_path(
            &Url::parse("https://example.com/publications/company-research")
                .expect("publication detail")
        ));
        assert!(is_listing_proven_article_like_path(
            &Url::parse("https://example.com/research-and-press/company-appointment")
                .expect("research and press detail")
        ));
        assert!(is_listing_proven_article_like_path(
            &Url::parse("https://example.com/updates/1").expect("numeric update item")
        ));
        assert!(!is_listing_proven_article_like_path(
            &Url::parse("https://example.com/blog/2").expect("numeric blog page")
        ));
    }

    #[test]
    fn accepts_a_listing_proven_numeric_update_id_but_not_the_page_alone() {
        let url = Url::parse("https://example.com/updates/1").expect("numeric update URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:type" content="website">
            <meta property="og:title" content="Company extends its research contract">
            </head><body><main>
              <h1>Company extends its research contract</h1>
              <div class="content max-w-prose"><p>{}</p></div>
            </main></body></html>"#,
            "Substantive independently fetched numeric update body. ".repeat(10),
        );
        let item = extract_article_with_title_hint(
            &url,
            url.clone(),
            Some("text/html"),
            body.as_bytes(),
            200,
            Some("Company extends its research contract"),
        )
        .expect("listing evidence qualifies the numeric update item");
        assert_eq!(
            item.payload["article_signals"],
            json!(["article_like_path_with_listing_title_and_semantic_body"])
        );

        let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect_err("a numeric update page alone remains insufficient");
        assert!(matches!(
            error,
            ArticlePageError::MissingArticleSignal { .. }
        ));
    }

    #[test]
    fn accepts_listing_title_with_a_west_newsroom_release_body() {
        let url = Url::parse("https://news.example.com/2026-company-award").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:title" content="Company receives a global innovation award">
            </head><body><main id="content">
            <div id="wd_printable_content" class="fr-view">
                <div class="wd_newsfeed_releases-detail">
                    <div class="wd_body wd_news_body fr-view"><p>{}</p></div>
                </div>
            </div>
            </main></body></html>"#,
            "Substantive independently fetched newsroom release body. ".repeat(10),
        );
        let item = extract_article_with_title_hint(
            &url,
            url.clone(),
            Some("text/html"),
            body.as_bytes(),
            200,
            Some("Company receives a global innovation award"),
        )
        .expect("listing evidence plus a vendor newsroom body identifies the release");

        assert_eq!(
            item.title.as_deref(),
            Some("Company receives a global innovation award")
        );
        assert_eq!(
            item.payload["article_body_selector"],
            json!(".wd_news_body")
        );
    }

    #[test]
    fn accepts_url_backed_metadata_title_with_a_west_newsroom_release_body() {
        let url =
            Url::parse("https://media.example.com/2026-company-receives-a-global-innovation-award")
                .expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:title" content="Company receives a global innovation award">
            </head><body><main id="content">
            <div class="wd_newsfeed_releases-detail">
                <div class="wd_body wd_news_body fr-view"><p>{}</p></div>
            </div>
            </main></body></html>"#,
            "Substantive independently fetched newsroom release body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("editorial host, URL-backed title, and vendor body identify the release");

        assert_eq!(
            item.title.as_deref(),
            Some("Company receives a global innovation award")
        );
        assert_eq!(
            item.payload["article_signals"],
            json!(["article_like_path_with_metadata_title_and_semantic_body"])
        );
        assert_eq!(
            item.payload["article_body_selector"],
            json!(".wd_news_body")
        );
    }

    #[test]
    fn accepts_url_backed_metadata_title_with_a_bounded_news_content_body() {
        let url =
            Url::parse("https://example.com/News-And-Events/Company-ships-record-breaking-tanks")
                .expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:type" content="website">
            <meta property="og:title" content="Company ships record breaking tanks">
            </head><body><main>
            <section id="newsContent">
              <h2>Company ships record breaking tanks</h2>
              <h3>Chart Industries | May 7, 2025</h3>
              <p>{}</p>
            </section>
            </main></body></html>"#,
            "Substantive independently fetched corporate news article body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("article path, URL-backed metadata, and news body identify the item");

        assert_eq!(
            item.title.as_deref(),
            Some("Company ships record breaking tanks")
        );
        assert_eq!(
            item.payload["article_signals"],
            json!(["article_like_path_with_metadata_title_and_semantic_body"])
        );
        assert_eq!(item.payload["article_body_selector"], json!("#newsContent"));
        assert_eq!(
            item.published_at,
            Some("2025-05-07T00:00:00Z".parse().expect("byline date"))
        );
        assert_eq!(
            item.payload["published_at_source"],
            json!("article_page_leading_text")
        );
    }

    #[test]
    fn rejects_semantic_body_without_listing_or_page_article_evidence() {
        let url =
            Url::parse("https://example.com/blog/a-webflow-company-update").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head><title>Acme</title></head><body>
            <div class="heading-style-h2">A Webflow company update</div>
            <div class="text-rich-text w-richtext"><p>{}</p></div>
            </body></html>"#,
            "Substantive independently fetched CMS article body. ".repeat(10),
        );
        let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect_err("a semantic body without independent title evidence remains insufficient");
        assert!(matches!(
            error,
            ArticlePageError::MissingArticleSignal { .. }
        ));
    }

    #[test]
    fn aem_model_discovery_is_same_origin_and_prefers_the_page_model() {
        let article_url =
            Url::parse("https://example.com/en/blog/platform-update/").expect("article URL");
        let html = r#"<!doctype html><html><head>
            <link rel="preload" as="fetch" type="application/json"
                  href="/content/site/global.model.json">
            <link rel="preload" as="fetch" type="application/json"
                  href="/content/site/global/en/blog/platform-update.model.json">
            <link rel="preload" as="fetch" type="application/json"
                  href="https://unrelated.example/model.json">
            </head><body><div id="spa-root"></div></body></html>"#;

        let model_url = extract_aem_model_url(&article_url, html.as_bytes())
            .expect("parse model declarations")
            .expect("same-origin page model");

        assert_eq!(
            model_url.as_str(),
            "https://example.com/content/site/global/en/blog/platform-update.model.json"
        );
    }

    #[test]
    fn next_data_augmentation_requires_an_exact_terminal_slug() {
        let article_url =
            Url::parse("https://example.com/blog/current-company-update/").expect("article URL");
        let next_data = json!({
            "props": {
                "pageProps": {
                    "repo": {
                        "slug": "different-company-update",
                        "title": "Different company update",
                        "body_content": format!(
                            "<p>{}</p>",
                            "Substantive but unrelated embedded article content. ".repeat(10)
                        ),
                        "date_view": "2026-07-20"
                    }
                }
            }
        });
        let shell = format!(
            r#"<!doctype html><html><body><main><div class="loading"></div></main>
            <script id="__NEXT_DATA__" type="application/json">{next_data}</script>
            </body></html>"#
        );

        assert!(
            augment_html_with_next_data(shell.as_bytes(), &article_url, 200, 64 * 1024).is_none(),
            "rich data for a different slug must not become this URL's article"
        );

        let cross_origin_data = json!({
            "props": {
                "pageProps": {
                    "repo": {
                        "url": "https://unrelated.example/blog/current-company-update/",
                        "title": "Cross-origin company update",
                        "body_content": format!(
                            "<p>{}</p>",
                            "Substantive but cross-origin embedded article content. ".repeat(10)
                        )
                    }
                }
            }
        });
        let cross_origin_shell = format!(
            r#"<!doctype html><html><body>
            <script id="__NEXT_DATA__" type="application/json">{cross_origin_data}</script>
            </body></html>"#
        );
        assert!(
            augment_html_with_next_data(
                cross_origin_shell.as_bytes(),
                &article_url,
                200,
                64 * 1024
            )
            .is_none(),
            "an absolute URL identity must also remain on the fetched origin"
        );
    }

    #[test]
    fn next_data_augmentation_selects_only_the_slug_matched_array_object() {
        let article_url =
            Url::parse("https://example.com/blog/current-company-update/").expect("article URL");
        let next_data = json!({
            "props": {
                "pageProps": {
                    "posts": [
                        {
                            "slug": "unrelated-long-article",
                            "title": "Unrelated long article",
                            "body_content": format!(
                                "<p>{}</p>",
                                "Long unrelated listing-array content. ".repeat(30)
                            )
                        },
                        {
                            "slug": "current-company-update",
                            "title": "Current company update",
                            "body_content": format!(
                                "<p>{}</p>",
                                "Substantive slug-matched embedded article body. ".repeat(10)
                            ),
                            "date_view": "2026-07-20"
                        }
                    ]
                }
            }
        });
        let shell = format!(
            r#"<!doctype html><html><body><main><div class="loading"></div></main>
            <script id="__NEXT_DATA__" type="application/json">{next_data}</script>
            </body></html>"#
        );
        let augmented = augment_html_with_next_data(shell.as_bytes(), &article_url, 200, 64 * 1024)
            .expect("exact slug object");
        let item = extract_article(
            &article_url,
            article_url.clone(),
            Some("text/html"),
            &augmented.body,
            200,
        )
        .expect("generic extraction accepts the injected bounded article");

        assert_eq!(augmented.identity_field, "slug");
        assert_eq!(augmented.content_field, "body_content");
        assert_eq!(item.title.as_deref(), Some("Current company update"));
        assert_eq!(
            item.published_at,
            Some("2026-07-20T00:00:00Z".parse().expect("date"))
        );
        assert!(
            item.body_html
                .as_deref()
                .is_some_and(|body| body.contains("slug-matched embedded article body"))
        );
        assert!(
            item.body_html
                .as_deref()
                .is_some_and(|body| !body.contains("unrelated listing-array content"))
        );
    }

    #[test]
    fn sveltekit_data_augmentation_selects_only_the_exact_path_object() {
        let article_url =
            Url::parse("https://example.com/newsroom/current-company-update").expect("article URL");
        let shell = r#"<!doctype html><html><body>
            <main><div class="loading-fallback">Loading content...</div></main>
            <script>window.__sveltekit_fixture = true;</script>
            <script type="module" src="/_app/immutable/entry/start.fixture.js"></script>
            </body></html>"#;
        let data = json!({
            "type": "data",
            "nodes": [{
                "type": "data",
                "data": [
                    {"title": 1, "publishedDate": 2, "url": 3, "blocks": 4},
                    "Current company update",
                    1776686400000_i64,
                    "/newsroom/current-company-update",
                    [5],
                    {"component": 6},
                    {"options": 7},
                    {"text": 8},
                    format!(
                        "<p>{}</p><h2>What changed</h2>",
                        "Substantive exact-path SvelteKit article body. ".repeat(10)
                    ),
                    {"title": 10, "url": 11, "blocks": 12},
                    "Unrelated longer article",
                    "/newsroom/unrelated-longer-article",
                    [13],
                    {"text": 14},
                    format!(
                        "<p>{}</p>",
                        "Long unrelated SvelteKit listing data. ".repeat(30)
                    )
                ]
            }]
        });
        let data = serde_json::to_vec(&data).expect("serialize SvelteKit fixture");
        let augmented =
            augment_html_with_sveltekit_data(shell.as_bytes(), &data, &article_url, 200, 64 * 1024)
                .expect("exact path SvelteKit data object");
        let item = extract_article(
            &article_url,
            article_url.clone(),
            Some("text/html"),
            &augmented.body,
            200,
        )
        .expect("generic extraction accepts the injected SvelteKit article");

        assert_eq!(augmented.identity_field, "url");
        assert_eq!(augmented.title_field, "title");
        assert_eq!(augmented.content_field, "text");
        assert_eq!(
            augmented.published_at,
            Some("2026-04-20T12:00:00Z".parse().expect("date"))
        );
        assert_eq!(item.title.as_deref(), Some("Current company update"));
        assert!(
            item.body_html
                .as_deref()
                .is_some_and(|body| body.contains("exact-path SvelteKit article body"))
        );
        assert!(
            item.body_html
                .as_deref()
                .is_some_and(|body| !body.contains("unrelated SvelteKit listing data"))
        );
    }

    #[test]
    fn sveltekit_data_identity_rejects_cross_origin_and_other_paths() {
        let article_url =
            Url::parse("https://example.com/newsroom/current-company-update").expect("article URL");
        let content = format!(
            "<p>{}</p>",
            "Substantive but incorrectly scoped SvelteKit content. ".repeat(10)
        );
        for identity in [
            "https://unrelated.example/newsroom/current-company-update",
            "/newsroom/different-company-update",
        ] {
            let data = json!({
                "nodes": [{
                    "type": "data",
                    "data": [
                        {"title": 1, "url": 2, "content": 3},
                        "Incorrectly scoped article",
                        identity,
                        content
                    ]
                }]
            });
            assert!(
                augment_html_with_sveltekit_data(
                    b"<html><body></body></html>",
                    &serde_json::to_vec(&data).expect("serialize fixture"),
                    &article_url,
                    200,
                    64 * 1024,
                )
                .is_none(),
                "{identity:?} must not identify this article"
            );
        }
    }

    #[test]
    fn prefers_semantic_body_over_related_article_cards_with_strong_metadata() {
        let url = Url::parse("https://example.com/insights/company-update").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:type" content="article">
            <meta property="og:title" content="Company update">
            <meta property="article:published_time" content="2026-07-22T12:00:00Z">
            </head><body>
            <h1>Company update</h1>
            <article><h2>Related insight one</h2><p>Short related card.</p></article>
            <div class="rich-text"><p>{}</p></div>
            <article><h2>Related insight two</h2><p>Another short related card.</p></article>
            </body></html>"#,
            "Substantive independently fetched insight article body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("strong metadata plus a semantic body identifies the article");
        assert_eq!(item.title.as_deref(), Some("Company update"));
        assert_eq!(item.payload["article_element_count"], json!(2));
        assert_eq!(item.payload["article_body_selector"], json!(".rich-text"));
    }

    #[test]
    fn prefers_larger_semantic_body_over_one_unrelated_article_card() {
        let url = Url::parse("https://example.com/press/company-update").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:type" content="article">
            <meta property="og:title" content="Company update">
            </head><body>
            <article class="navigation-card"><h2>Marketplace</h2><p>Short navigation card.</p></article>
            <h1>Company update</h1>
            <section class="custom_code"><div class="code-wrap"><p>{}</p></div></section>
            </body></html>"#,
            "Substantive independently fetched company press-release body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("strong metadata should prefer the materially larger semantic body");
        assert_eq!(item.title.as_deref(), Some("Company update"));
        assert_eq!(item.payload["article_element_count"], json!(1));
        assert_eq!(
            item.payload["article_body_selector"],
            json!(".custom_code .code-wrap")
        );
    }

    #[test]
    fn accepts_css_module_single_post_body_after_article_signal_passes() {
        let url = Url::parse("https://example.com/blog/company-update").expect("article URL");
        let body = format!(
            r#"<!doctype html><html><head>
            <meta property="og:type" content="article">
            <title>Company update</title>
            </head><body>
            <h1>Company update</h1>
            <div class="BlogSingle_postContainer__abc SinglePost_singlePostContent__sqsHz">
              <p>{}</p>
            </div>
            </body></html>"#,
            "Substantive independently fetched company article body. ".repeat(10),
        );
        let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect("article signal plus a CSS-module body identifies the article");
        assert_eq!(item.title.as_deref(), Some("Company update"));
        assert_eq!(
            item.payload["article_body_selector"],
            json!("[class*='singlePostContent']")
        );
    }

    #[test]
    fn accepts_framework_article_body_roots_after_article_signal_passes() {
        let cases = [
            (
                "framer",
                r#"<div data-framer-name="Content" data-framer-component-type="RichTextContainer"><p>{body}</p></div>"#,
                "[data-framer-name='Content'][data-framer-component-type='RichTextContainer']",
            ),
            (
                "framer-blog",
                r#"<div data-framer-name="Blog"><div data-framer-component-type="RichTextContainer"><p>{body}</p></div></div>"#,
                "[data-framer-name='Blog'] [data-framer-component-type='RichTextContainer']",
            ),
            (
                "framer-nested-content",
                r#"<div data-framer-name="Content"><div data-framer-component-type="RichTextContainer"><p>{body}</p></div></div>"#,
                "[data-framer-name='Content'] [data-framer-component-type='RichTextContainer']",
            ),
            (
                "framer-lowercase-content",
                r#"<div data-framer-name="content" data-framer-component-type="RichTextContainer"><p>{body}</p></div>"#,
                "[data-framer-name='content'][data-framer-component-type='RichTextContainer']",
            ),
            (
                "framer-body-content",
                r#"<div data-framer-name="Body Content" data-framer-component-type="RichTextContainer"><p>{body}</p></div>"#,
                "[data-framer-name='Body Content'][data-framer-component-type='RichTextContainer']",
            ),
            (
                "framer-largest-rich-text",
                r#"<nav><div data-framer-component-type="RichTextContainer"><p>Navigation</p></div></nav><div data-framer-component-type="RichTextContainer"><p>{body}</p></div>"#,
                "[data-framer-component-type='RichTextContainer']",
            ),
            (
                "elementor-single-post",
                r#"<div data-elementor-type="single-post"><div class="elementor-widget-theme-post-content"><div class="elementor-widget-container"><p>{body}</p></div></div></div>"#,
                "[data-elementor-type='single-post'] .elementor-widget-theme-post-content .elementor-widget-container",
            ),
            (
                "elementor-wp-post",
                r#"<div data-elementor-type="wp-post"><div class="elementor-widget-text-editor"><div class="elementor-widget-container"><p>{body}</p></div></div></div>"#,
                "[data-elementor-type='wp-post'] .elementor-widget-text-editor .elementor-widget-container",
            ),
            (
                "gatsby-rich-text",
                r#"<div class="RichTextRenderer__BlogPostContent-sc-197msfm-8"><p>{body}</p></div>"#,
                "[class*='RichTextRenderer__BlogPostContent']",
            ),
            (
                "component-article-rich-text",
                r#"<div class="rc-ArticlePage"><div class="rc-RichText"><p>{body}</p></div></div>"#,
                "[class*='ArticlePage'] [class*='RichText']",
            ),
            (
                "hubspot-blog-post-content",
                r#"<div class="blog-post-content"><p>{body}</p></div>"#,
                ".blog-post-content",
            ),
            (
                "wysiwyg-single-blog",
                r#"<div class="single_blog clearfix wysiwyg-content"><p>{body}</p></div>"#,
                ".single_blog.wysiwyg-content",
            ),
            (
                "sitecore-field-content",
                r#"<div class="field-content"><p>{body}</p></div>"#,
                ".field-content",
            ),
            (
                "sitecore-rte-field",
                r#"<div class="rte_field"><div class="PraSection"><p>{body}</p></div></div>"#,
                ".rte_field",
            ),
            (
                "custom-code-module",
                r#"<section class="custom_code"><div class="code-wrap"><p>{body}</p></div></section>"#,
                ".custom_code .code-wrap",
            ),
            (
                "press-release-bem",
                r#"<div class="body-item company-press-release__body"><p>{body}</p></div>"#,
                "[class*='press-release__body']",
            ),
            (
                "rich-text-bem",
                r#"<div class="company-rich-text__body"><p>{body}</p></div>"#,
                "[class$='rich-text__body']",
            ),
            (
                "content-id",
                r#"<div id="content"><p>{body}</p></div>"#,
                "#content",
            ),
            (
                "hubspot-post-body",
                r#"<div class="post-body"><p>{body}</p></div>"#,
                ".post-body",
            ),
            (
                "hubspot-post-body-id",
                r#"<div id="hs_cos_wrapper_post_body"><p>{body}</p></div>"#,
                "#hs_cos_wrapper_post_body",
            ),
            (
                "bounded-content",
                r#"<div class="content mx-auto max-w-2xl"><p>{body}</p></div>"#,
                "[class~='content'][class*='max-w-']",
            ),
            (
                "tailwind-prose",
                r#"<div class="prose max-w-none"><p>{body}</p></div>"#,
                "[class~='prose']",
            ),
            (
                "chakra-container",
                r#"<div class="chakra-container css-generated"><p>{body}</p></div>"#,
                "[class~='chakra-container']",
            ),
            (
                "rich-text-container",
                r#"<div class="rich-text-container text-pretty"><p>{body}</p></div>"#,
                ".rich-text-container",
            ),
            (
                "react-blogpost-body",
                r#"<div class="Blogpost_body component-generated"><p>{body}</p></div>"#,
                "[class*='Blogpost_body']",
            ),
            (
                "divi-post-content",
                r#"<div class="et_pb_post_content"><p>{body}</p></div>"#,
                ".et_pb_post_content",
            ),
            (
                "joomla-article-body",
                r#"<div class="com-content-article__body"><p>{body}</p></div>"#,
                ".com-content-article__body",
            ),
            (
                "press-single-content",
                r#"<div class="press-single-main-content"><p>{body}</p></div>"#,
                ".press-single-main-content",
            ),
            (
                "press-release-content",
                r#"<div class="press-release"><p>{body}</p></div>"#,
                ".press-release",
            ),
            (
                "west-newsroom-release",
                r#"<div class="wd_body wd_news_body fr-view"><p>{body}</p></div>"#,
                ".wd_news_body",
            ),
            (
                "underscore-post-body",
                r#"<div class="clearfix post_body contain"><p>{body}</p></div>"#,
                ".post_body",
            ),
            (
                "post-content-section",
                r#"<div class="post-content-section"><p>{body}</p></div>"#,
                ".post-content-section",
            ),
            (
                "blog-content-wrapper",
                r#"<div class="blog_content_wrapper"><p>{body}</p></div>"#,
                ".blog_content_wrapper",
            ),
            (
                "aem-body-content",
                r#"<div class="body-content"><p>{body}</p></div>"#,
                ".body-content",
            ),
            (
                "commerce-content-asset-body",
                r#"<div class="content-asset-body-wrapper"><p>{body}</p></div>"#,
                ".content-asset-body-wrapper",
            ),
            (
                "aem-body-copy",
                r#"<div class="bodyCopyContainer"><p>{body}</p></div>"#,
                ".bodyCopyContainer",
            ),
            (
                "drupal-body-field",
                r#"<div class="field field--name-body"><div class="field__item"><p>{body}</p></div></div>"#,
                "[class~='field--name-body'] .field__item",
            ),
            (
                "drupal-inline-body-field",
                r#"<div class="field field--name-body field__item"><p>{body}</p></div>"#,
                "[class~='field--name-body'][class~='field__item']",
            ),
            (
                "static-content",
                r#"<div class="staticcontent"><p>{body}</p></div>"#,
                ".staticcontent",
            ),
            (
                "aem-article-content",
                r#"<div id="articleContent"><p>{body}</p></div>"#,
                "#articleContent",
            ),
            (
                "news-details",
                r#"<div class="news-details-info"><p>{body}</p></div>"#,
                "[class*='news-details-info']",
            ),
        ];

        for (slug, body_root, expected_selector) in cases {
            let url = Url::parse(&format!("https://example.com/news/{slug}-company-update"))
                .expect("article URL");
            let body_root = body_root.replace(
                "{body}",
                &"Substantive independently fetched CMS article body. ".repeat(10),
            );
            let body = format!(
                "<!doctype html><html><head><meta property=\"og:type\" content=\"article\"><title>Company update</title></head><body><h1>Company update</h1>{body_root}</body></html>"
            );
            let item = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
                .expect("article signal plus framework body root identifies an article");
            assert_eq!(item.title.as_deref(), Some("Company update"));
            assert_eq!(
                item.payload["article_body_selector"],
                json!(expected_selector)
            );
        }
    }

    #[test]
    fn rejects_framework_body_root_without_an_article_signal() {
        let url = Url::parse("https://example.com/product").expect("non-article URL");
        let body = format!(
            "<!doctype html><html><head><title>Product</title></head><body><div id=\"content\"><p>May 7, 2026</p><p>{}</p></div></body></html>",
            "Substantive product landing page content. ".repeat(10),
        );
        let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect_err("a generic body root alone must not identify an article");
        assert!(matches!(
            error,
            ArticlePageError::MissingArticleSignal { .. }
        ));
    }

    #[test]
    fn rejects_article_category_listing_even_with_h1() {
        let url = Url::parse("https://example.com/blog/category/engineering").expect("listing URL");
        let body = format!(
            "<!doctype html><html><head><title>Engineering</title></head><body><main><h1>Engineering</h1><p>{}</p></main></body></html>",
            "A long listing page is still not an individual article. ".repeat(10),
        );
        let error = extract_article(&url, url.clone(), Some("text/html"), body.as_bytes(), 200)
            .expect_err("category listing must be rejected");
        assert!(matches!(error, ArticlePageError::ObviousListingPath { .. }));
        assert_eq!(error.reason(), "obvious_listing_path");
    }

    #[tokio::test]
    async fn article_crawler_reports_quality_failures_without_silent_empty_output() {
        use axum::{Router, response::Html, routing::get};

        let app = Router::new().route(
            "/listing",
            get(|| async {
                Html(
                    "<!doctype html><html><head><title>Listing</title></head><body><main>Not an article</main></body></html>",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });
        let crawler = HtmlArticleCrawler::new(HtmlArticleCrawlerConfig {
            allow_private_networks: true,
            min_content_chars: 20,
            ..HtmlArticleCrawlerConfig::default()
        })
        .expect("crawler");
        let report = crawler
            .crawl_urls(&[Url::parse(&format!("http://{address}/listing")).expect("URL")])
            .await
            .expect("crawl report");
        assert!(report.items.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].reason, "missing_article_signal");
        assert!(!report.failures[0].retryable);
        task.abort();
    }

    #[tokio::test]
    async fn article_crawler_uses_configured_same_host_concurrency() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        use axum::{Router, extract::State, response::Html, routing::get};

        #[derive(Clone)]
        struct ConcurrencyState {
            active: Arc<AtomicUsize>,
            maximum: Arc<AtomicUsize>,
        }

        async fn article(State(state): State<ConcurrencyState>) -> Html<String> {
            let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
            state.maximum.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(40)).await;
            state.active.fetch_sub(1, Ordering::SeqCst);
            Html(format!(
                r#"<!doctype html><html><head><meta property="og:type" content="article"></head><body><article><h1>Company update</h1><p>{}</p></article></body></html>"#,
                "Substantive independently fetched company article body. ".repeat(8),
            ))
        }

        let state = ConcurrencyState {
            active: Arc::new(AtomicUsize::new(0)),
            maximum: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/news/one", get(article))
            .route("/news/two", get(article))
            .route("/news/three", get(article))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });
        let crawler = HtmlArticleCrawler::new(HtmlArticleCrawlerConfig {
            allow_private_networks: true,
            max_concurrency: 6,
            max_per_host_concurrency: 3,
            min_content_chars: 20,
            ..HtmlArticleCrawlerConfig::default()
        })
        .expect("crawler");
        let urls = ["one", "two", "three"]
            .into_iter()
            .map(|slug| Url::parse(&format!("http://{address}/news/{slug}")).expect("article URL"))
            .collect::<Vec<_>>();
        let report = crawler.crawl_urls(&urls).await.expect("crawl report");

        assert_eq!(report.items.len(), 3);
        assert_eq!(state.maximum.load(Ordering::SeqCst), 3);
        task.abort();
    }

    #[tokio::test]
    async fn article_crawler_preserves_candidate_priority_across_hosts() {
        use axum::{Router, response::Html, routing::get};

        async fn article(title: &'static str) -> Html<String> {
            Html(format!(
                r#"<!doctype html><html><head><meta property="og:type" content="article"><link rel="canonical" href="https://canonical.example/news/shared"></head><body><article><h1>{title}</h1><p>{}</p></article></body></html>"#,
                "Substantive independently fetched company article body. ".repeat(8),
            ))
        }

        let app = Router::new()
            .route(
                "/news/slow-first",
                get(|| async {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    article("First article").await
                }),
            )
            .route(
                "/news/fast-second",
                get(|| async { article("Second article").await }),
            );
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0")
            .await
            .expect("bind fixture");
        let port = listener.local_addr().expect("fixture address").port();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });

        let crawler = HtmlArticleCrawler::new(HtmlArticleCrawlerConfig {
            allow_private_networks: true,
            max_concurrency: 4,
            min_content_chars: 20,
            ..HtmlArticleCrawlerConfig::default()
        })
        .expect("crawler");
        let urls = vec![
            Url::parse(&format!("http://127.0.0.1:{port}/news/slow-first")).expect("first URL"),
            Url::parse(&format!("http://127.0.0.2:{port}/news/fast-second")).expect("second URL"),
        ];
        let report = crawler.crawl_urls(&urls).await.expect("crawl report");

        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].url, urls[0]);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].url, urls[1]);
        assert_eq!(report.failures[0].reason, "duplicate_canonical_url");
        task.abort();
    }

    #[tokio::test]
    async fn recipe_crawler_proves_listing_links_and_article_content() {
        use axum::{Router, response::Html, routing::get};

        async fn article(title: &'static str, published_at: Option<&'static str>) -> Html<String> {
            let date_metadata = published_at.map_or_else(String::new, |published_at| {
                format!(
                    r#"<script type="application/ld+json">{{"@type":"NewsArticle","datePublished":"{published_at}"}}</script>"#
                )
            });
            Html(format!(
                r#"<!doctype html><html><head><meta property="og:type" content="article"><meta property="og:title" content="{title}">{date_metadata}</head><body><article><h1>{title}</h1><p>{}</p></article></body></html>"#,
                "Substantive independently fetched company article body. ".repeat(8),
            ))
        }
        let app = Router::new()
            .route(
                "/news/",
                get(|| async {
                    Html(
                        r#"<!doctype html><html><body><main><a class="story" href="/news/first-launch">First product launch</a><a class="story" href="/news/second-update">Second company update</a></main></body></html>"#,
                    )
                }),
            )
            .route(
                "/news/first-launch",
                get(|| article("First product launch", Some("2020-01-02T00:00:00Z"))),
            )
            .route(
                "/news/second-update",
                get(|| article("Second company update", None)),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: Url::parse(&format!("http://{address}/news/")).expect("URL"),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "main a[href]".to_owned(),
            allowed_hosts: vec!["127.0.0.1".to_owned()],
            include_path_prefixes: vec!["/news/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy {
                baseline_discovered_items: 2,
                baseline_accepted_items: 2,
                min_discovered_items: 2,
                min_accepted_items: 2,
                min_acceptance_ratio_bps: 5_000,
                ..feed_core::RecipeCorrectnessPolicy::default()
            },
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };
        let crawler = HtmlRecipeCrawler::new(HtmlRecipeCrawlerConfig {
            allow_private_networks: true,
            min_content_chars: 20,
            ..HtmlRecipeCrawlerConfig::default()
        })
        .expect("crawler");
        let report = crawler.crawl(&recipe).await.expect("recipe crawl");
        assert!(report.correctness_passed());
        assert_eq!(report.discovered_url_count, 2);
        assert_eq!(report.accepted_item_count, 2);
        assert_eq!(report.distinct_title_count, 2);
        assert_eq!(report.acceptance_ratio_bps, 10_000);
        assert_eq!(report.dated_item_count, 1);
        assert!(!report.publication_date_coverage_complete);
        assert!(
            !report.content_stale,
            "an old dated item cannot prove staleness when another accepted item is undated"
        );
        assert!(!report.structure_fingerprint.is_empty());
        task.abort();
    }

    #[tokio::test]
    async fn recipe_crawler_cache_reuses_listing_across_scope_attempts() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        use axum::{Router, response::Html, routing::get};

        let listing_requests = Arc::new(AtomicUsize::new(0));
        let handler_requests = Arc::clone(&listing_requests);
        let app = Router::new().route(
            "/news/",
            get(move || {
                let handler_requests = Arc::clone(&handler_requests);
                async move {
                    handler_requests.fetch_add(1, Ordering::SeqCst);
                    Html("<!doctype html><html><body><main></main></body></html>")
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });
        let mut recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: Url::parse(&format!("http://{address}/news/")).expect("URL"),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "main a[href]".to_owned(),
            allowed_hosts: vec!["127.0.0.1".to_owned()],
            include_path_prefixes: vec!["/news/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };
        let crawler = HtmlRecipeCrawler::new(HtmlRecipeCrawlerConfig {
            allow_private_networks: true,
            ..HtmlRecipeCrawlerConfig::default()
        })
        .expect("crawler");
        let mut cache = HtmlRecipeCrawlCache::default();

        let narrow = crawler
            .crawl_with_cache(&recipe, &mut cache)
            .await
            .expect("narrow crawl");
        recipe.include_path_prefixes.clear();
        let broad = crawler
            .crawl_with_cache(&recipe, &mut cache)
            .await
            .expect("broad crawl");

        assert_eq!(narrow.discovered_url_count, 0);
        assert_eq!(broad.discovered_url_count, 0);
        assert_eq!(
            listing_requests.load(Ordering::SeqCst),
            1,
            "alternate recipe scopes should share one listing fetch"
        );
        task.abort();
    }

    #[tokio::test]
    async fn recipe_crawler_rejects_redirects_outside_the_recipe_path_scope() {
        use axum::{
            Router,
            response::{Html, Redirect},
            routing::get,
        };

        let app = Router::new()
            .route(
                "/news/",
                get(|| async {
                    Html(
                        r#"<!doctype html><html><body><main><a href="/news/redirected">Company product update</a></main></body></html>"#,
                    )
                }),
            )
            .route(
                "/news/redirected",
                get(|| async { Redirect::temporary("/outside/support-document") }),
            )
            .route(
                "/outside/support-document",
                get(|| async {
                    Html(format!(
                        r#"<!doctype html><html><head><meta property="og:type" content="article"></head><body><article><h1>Company product update</h1><p>{}</p></article></body></html>"#,
                        "Substantive independently fetched support content. ".repeat(8),
                    ))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: Url::parse(&format!("http://{address}/news/")).expect("URL"),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "main a[href]".to_owned(),
            allowed_hosts: vec!["127.0.0.1".to_owned()],
            include_path_prefixes: vec!["/news/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy {
                baseline_discovered_items: 1,
                baseline_accepted_items: 1,
                min_discovered_items: 1,
                min_accepted_items: 1,
                min_acceptance_ratio_bps: 5_000,
                ..feed_core::RecipeCorrectnessPolicy::default()
            },
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };
        let crawler = HtmlRecipeCrawler::new(HtmlRecipeCrawlerConfig {
            allow_private_networks: true,
            min_content_chars: 20,
            ..HtmlRecipeCrawlerConfig::default()
        })
        .expect("crawler");
        let report = crawler.crawl(&recipe).await.expect("recipe crawl");

        assert!(!report.correctness_passed());
        assert_eq!(report.discovered_url_count, 1);
        assert_eq!(report.accepted_item_count, 0);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].reason, "article_outside_recipe_scope");
        task.abort();
    }

    #[tokio::test]
    async fn recipe_crawler_expands_bounded_year_archive_collections() {
        use axum::{Router, response::Html, routing::get};

        async fn article(title: &'static str) -> Html<String> {
            Html(format!(
                r#"<!doctype html><html><head><title>{title}</title></head><body><div class="field-content"><span>May 7, 2026</span><strong>{title}</strong><p>{}</p></div></body></html>"#,
                "Substantive independently fetched company article body. ".repeat(8),
            ))
        }
        let app = Router::new()
            .route(
                "/news/",
                get(|| async {
                    Html(
                        r#"<!doctype html><html><body><main>
                        <div><h4>2026 Press Release</h4><a href="/news/2026">Read More</a></div>
                        <div><h4>2025 Press Releases</h4><a href="/news/2025">Read More</a></div>
                        </main></body></html>"#,
                    )
                }),
            )
            .route(
                "/news/2026",
                get(|| async {
                    Html(
                        r#"<!doctype html><html><body><main>
                        <div><h4>Current product launch</h4><a href="/news/2026/current-launch">Read More</a></div>
                        </main></body></html>"#,
                    )
                }),
            )
            .route(
                "/news/2025",
                get(|| async {
                    Html(
                        r#"<!doctype html><html><body><main>
                        <div><h4>Prior company update</h4><a href="/news/2025/prior-update">Read More</a></div>
                        </main></body></html>"#,
                    )
                }),
            )
            .route(
                "/news/2026/current-launch",
                get(|| article("Current product launch")),
            )
            .route(
                "/news/2025/prior-update",
                get(|| article("Prior company update")),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: Url::parse(&format!("http://{address}/news/")).expect("URL"),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "main a[href]".to_owned(),
            allowed_hosts: vec!["127.0.0.1".to_owned()],
            include_path_prefixes: vec!["/news/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy {
                baseline_discovered_items: 2,
                baseline_accepted_items: 2,
                min_discovered_items: 2,
                min_accepted_items: 2,
                min_acceptance_ratio_bps: 5_000,
                ..feed_core::RecipeCorrectnessPolicy::default()
            },
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };
        let crawler = HtmlRecipeCrawler::new(HtmlRecipeCrawlerConfig {
            allow_private_networks: true,
            min_content_chars: 20,
            ..HtmlRecipeCrawlerConfig::default()
        })
        .expect("crawler");
        let report = crawler.crawl(&recipe).await.expect("recipe crawl");

        assert!(report.correctness_passed());
        assert_eq!(report.discovered_url_count, 2);
        assert_eq!(report.accepted_item_count, 2);
        assert_eq!(report.distinct_title_count, 2);
        assert_eq!(
            report.latest_published_at,
            Some("2026-05-07T00:00:00Z".parse().expect("leading body date"))
        );
        assert_eq!(report.dated_item_count, 2);
        assert!(report.publication_date_coverage_complete);
        assert!(report.items.iter().all(|item| {
            item.payload["published_at_source"] == json!("article_page_leading_text")
        }));
        assert!(
            report
                .items
                .iter()
                .all(|item| !matches!(item.url.path(), "/news/2026" | "/news/2025"))
        );
        task.abort();
    }

    #[tokio::test]
    async fn recipe_crawler_reports_layout_yield_drift_as_incorrect() {
        use axum::{Router, response::Html, routing::get};

        let app = Router::new().route(
            "/news/",
            get(|| async {
                Html(
                    r#"<!doctype html><html><body><main><a href="/privacy">Privacy policy and legal terms</a></main></body></html>"#,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: Url::parse(&format!("http://{address}/news/")).expect("URL"),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "main a[href]".to_owned(),
            allowed_hosts: vec!["127.0.0.1".to_owned()],
            include_path_prefixes: vec!["/news/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };
        let crawler = HtmlRecipeCrawler::new(HtmlRecipeCrawlerConfig {
            allow_private_networks: true,
            min_content_chars: 20,
            ..HtmlRecipeCrawlerConfig::default()
        })
        .expect("crawler");
        let report = crawler.crawl(&recipe).await.expect("recipe crawl report");
        assert!(!report.correctness_passed());
        assert_eq!(report.discovered_url_count, 0);
        assert!(
            report
                .correctness_reasons
                .contains(&"discovered_items_below_minimum".to_owned())
        );
        assert!(
            report
                .correctness_reasons
                .contains(&"accepted_items_below_minimum".to_owned())
        );
        task.abort();
    }

    #[tokio::test]
    async fn recipe_crawler_rejects_low_title_diversity() {
        use axum::{Router, response::Html, routing::get};

        async fn repeated_page() -> Html<String> {
            Html(format!(
                r#"<!doctype html><html><head><meta property="og:type" content="article"></head><body><article><h1>Acme corporate updates</h1><p>{}</p></article></body></html>"#,
                "Substantive independently fetched company article body. ".repeat(8),
            ))
        }
        let app = Router::new()
            .route(
                "/news/",
                get(|| async {
                    Html(
                        r#"<!doctype html><html><body><main>
                        <a class="story" href="/news/one">Read more</a>
                        <a class="story" href="/news/two">Read more</a>
                        <a class="story" href="/news/three">Read more</a>
                        </main></body></html>"#,
                    )
                }),
            )
            .route("/news/one", get(repeated_page))
            .route("/news/two", get(repeated_page))
            .route("/news/three", get(repeated_page));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: Url::parse(&format!("http://{address}/news/")).expect("URL"),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a.story[href]".to_owned(),
            allowed_hosts: vec!["127.0.0.1".to_owned()],
            include_path_prefixes: vec!["/news/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };
        let crawler = HtmlRecipeCrawler::new(HtmlRecipeCrawlerConfig {
            allow_private_networks: true,
            min_content_chars: 20,
            ..HtmlRecipeCrawlerConfig::default()
        })
        .expect("crawler");
        let report = crawler.crawl(&recipe).await.expect("recipe crawl report");
        assert_eq!(report.accepted_item_count, 3);
        assert_eq!(report.distinct_title_count, 1);
        assert_eq!(report.distinct_content_count, 1);
        assert!(
            report
                .correctness_reasons
                .contains(&"title_diversity_below_minimum".to_owned())
        );
        assert!(
            report
                .correctness_reasons
                .contains(&"content_diversity_below_minimum".to_owned())
        );
        assert!(!report.correctness_passed());
        task.abort();
    }

    #[tokio::test]
    async fn recipe_crawler_rejects_distinct_urls_serving_the_same_catch_all_body() {
        use axum::{Router, response::Html, routing::get};

        async fn catch_all_page(title: &str) -> Html<String> {
            Html(format!(
                r#"<!doctype html><html><head>
                <meta property="og:type" content="article">
                <meta property="og:title" content="{title}">
                </head><body><article data-page="{title}"><p>{}</p></article></body></html>"#,
                "The same substantive homepage fallback body. ".repeat(8),
            ))
        }
        let app = Router::new()
            .route(
                "/news/",
                get(|| async {
                    Html(
                        r#"<!doctype html><html><body><main>
                        <a class="story" href="/news/one">First launch</a>
                        <a class="story" href="/news/two">Second update</a>
                        <a class="story" href="/news/three">Third announcement</a>
                        </main></body></html>"#,
                    )
                }),
            )
            .route("/news/one", get(|| catch_all_page("First launch")))
            .route("/news/two", get(|| catch_all_page("Second update")))
            .route("/news/three", get(|| catch_all_page("Third announcement")));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: Url::parse(&format!("http://{address}/news/")).expect("URL"),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a.story[href]".to_owned(),
            allowed_hosts: vec!["127.0.0.1".to_owned()],
            include_path_prefixes: vec!["/news/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };
        let crawler = HtmlRecipeCrawler::new(HtmlRecipeCrawlerConfig {
            allow_private_networks: true,
            min_content_chars: 20,
            ..HtmlRecipeCrawlerConfig::default()
        })
        .expect("crawler");
        let report = crawler.crawl(&recipe).await.expect("recipe crawl report");
        assert_eq!(report.accepted_item_count, 0);
        assert_eq!(report.distinct_title_count, 0);
        assert_eq!(report.distinct_content_count, 0);
        assert_eq!(report.failures.len(), 3);
        assert!(
            report
                .failures
                .iter()
                .all(|failure| failure.reason == "repeated_sanitized_content")
        );
        assert!(!report.correctness_passed());
        task.abort();
    }

    #[tokio::test]
    async fn article_crawler_extracts_slug_matched_next_data_json_body() {
        use axum::{Router, response::Html, routing::get};

        let next_data = json!({
            "props": {
                "pageProps": {
                    "repo": {
                        "slug": "embedded-company-update",
                        "title": "Embedded company update",
                        "meta_title": "Embedded company update | Acme",
                        "body_content": format!(
                            "<p>{}</p><h2>What changed</h2><p>{}</p>",
                            "First substantive embedded article section. ".repeat(6),
                            "Second substantive embedded article section. ".repeat(6)
                        ),
                        "date_view": "2026-07-20",
                        "publishedAt": "2025-01-01T12:00:00Z"
                    }
                }
            }
        });
        let shell = format!(
            r#"<!doctype html><html><head><title>Acme</title></head>
            <body><main><div class="loading-skeleton"></div></main>
            <script id="__NEXT_DATA__" type="application/json">{next_data}</script>
            </body></html>"#
        );
        let app = Router::new().route(
            "/blog/embedded-company-update/",
            get(move || {
                let shell = shell.clone();
                async move { Html(shell) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });
        let article_url =
            Url::parse(&format!("http://{address}/blog/embedded-company-update/")).expect("URL");
        let crawler = HtmlArticleCrawler::new(HtmlArticleCrawlerConfig {
            allow_private_networks: true,
            min_content_chars: 200,
            ..HtmlArticleCrawlerConfig::default()
        })
        .expect("article crawler");

        let report = crawler
            .crawl_urls(&[article_url])
            .await
            .expect("crawl Next.js shell article");

        assert!(report.failures.is_empty());
        assert_eq!(report.items.len(), 1);
        assert_eq!(
            report.items[0].payload["framework_fallback"],
            json!("next-data-json.v1")
        );
        assert_eq!(
            report.items[0].payload["framework_embedded_slug"],
            json!("embedded-company-update")
        );
        assert_eq!(
            report.items[0].payload["framework_embedded_identity_field"],
            json!("slug")
        );
        assert_eq!(
            report.items[0].payload["framework_embedded_content_field"],
            json!("body_content")
        );
        assert_eq!(
            report.items[0].payload["framework_embedded_published_at_field"],
            json!("date_view")
        );
        assert_eq!(
            report.items[0].payload["published_at_source"],
            json!("next_data_json")
        );
        assert_eq!(
            report.items[0].published_at,
            Some("2026-07-20T00:00:00Z".parse().expect("date"))
        );
        assert!(
            report.items[0]
                .body_html
                .as_deref()
                .is_some_and(|body| body.contains("First substantive embedded article section"))
        );
        task.abort();
    }

    #[tokio::test]
    async fn article_crawler_fetches_sveltekit_data_json_body() {
        use axum::{Json, Router, response::Html, routing::get};

        let shell = r#"<!doctype html><html><head>
            <meta property="og:type" content="article">
            <meta property="og:title" content="SvelteKit company update">
            <link rel="canonical" href="/newsroom/sveltekit-company-update">
            </head><body>
            <main><div class="loading-fallback">Loading content...</div></main>
            <script>window.__sveltekit_fixture = true;</script>
            <script type="module" src="/_app/immutable/entry/start.fixture.js"></script>
            </body></html>"#;
        let data = json!({
            "type": "data",
            "nodes": [{
                "type": "data",
                "data": [
                    {"content": 1},
                    {"data": 2},
                    {"title": 3, "publishedDate": 4, "url": 5, "blocks": 6},
                    "SvelteKit company update",
                    1776686400000_i64,
                    "/newsroom/sveltekit-company-update",
                    [7],
                    {"component": 8},
                    {"options": 9},
                    {"text": 10},
                    format!(
                        "<p>{}</p><h2>What changed</h2><p>{}</p>",
                        "First substantive SvelteKit article section. ".repeat(6),
                        "Second substantive SvelteKit article section. ".repeat(6)
                    )
                ]
            }]
        });
        let app = Router::new()
            .route(
                "/newsroom/sveltekit-company-update",
                get(move || async move { Html(shell) }),
            )
            .route(
                "/newsroom/sveltekit-company-update/__data.json",
                get(move || {
                    let data = data.clone();
                    async move { Json(data) }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });
        let article_url = Url::parse(&format!(
            "http://{address}/newsroom/sveltekit-company-update"
        ))
        .expect("URL");
        let crawler = HtmlArticleCrawler::new(HtmlArticleCrawlerConfig {
            allow_private_networks: true,
            min_content_chars: 200,
            ..HtmlArticleCrawlerConfig::default()
        })
        .expect("article crawler");

        let report = crawler
            .crawl_urls(&[article_url])
            .await
            .expect("crawl SvelteKit shell article");

        assert!(report.failures.is_empty());
        assert_eq!(report.items.len(), 1);
        assert_eq!(
            report.items[0].payload["framework_fallback"],
            json!("sveltekit-data-json.v1")
        );
        assert_eq!(
            report.items[0].payload["framework_embedded_identity_field"],
            json!("url")
        );
        assert_eq!(
            report.items[0].payload["framework_embedded_content_field"],
            json!("text")
        );
        assert_eq!(
            report.items[0].payload["framework_embedded_published_at_field"],
            json!("publishedDate")
        );
        assert_eq!(
            report.items[0].payload["published_at_source"],
            json!("sveltekit_data_json")
        );
        assert!(
            report.items[0]
                .body_html
                .as_deref()
                .is_some_and(|body| body.contains("First substantive SvelteKit article section"))
        );
        task.abort();
    }

    #[tokio::test]
    async fn article_crawler_fetches_declared_aem_model_json_body() {
        use axum::{Json, Router, response::Html, routing::get};

        let shell = r#"<!doctype html><html><head>
            <title>Acme launches analytical search</title>
            <link rel="canonical" href="/en/blog/analytical-search/">
            <link rel="preload" as="fetch" type="application/json"
                  href="/content/site/en/blog/analytical-search.model.json">
            <script type="application/ld+json">
              {"@context":"https://schema.org","@type":"BlogPosting",
               "headline":"Acme launches analytical search",
               "datePublished":"2026-07-20T12:00:00Z"}
            </script>
            </head><body><div id="spa-root"></div></body></html>"#;
        let model = json!({
            ":itemsOrder": ["root"],
            ":items": {
                "root": {
                    ":itemsOrder": ["container_main_content", "footer"],
                    ":items": {
                        "container_main_content": {
                            ":itemsOrder": ["blog_text_first", "blog_text_second"],
                            ":items": {
                                "blog_text_first": {
                                    ":type": "example/components/blog/blog-text",
                                    "richText": true,
                                    "text": format!(
                                        "<p>{}</p>",
                                        "First substantive AEM article section. ".repeat(6)
                                    )
                                },
                                "blog_text_second": {
                                    ":type": "example/components/blog/blog-text",
                                    "richText": true,
                                    "text": format!(
                                        "<p>{}</p>",
                                        "Second substantive AEM article section. ".repeat(6)
                                    )
                                }
                            }
                        },
                        "footer": {
                            ":itemsOrder": ["privacy_text"],
                            ":items": {
                                "privacy_text": {
                                    ":type": "example/components/text",
                                    "richText": true,
                                    "text": "<p>Unrelated privacy-form disclosure.</p>"
                                }
                            }
                        }
                    }
                }
            }
        });
        let app = Router::new()
            .route(
                "/en/blog/analytical-search/",
                get(move || async move { Html(shell) }),
            )
            .route(
                "/content/site/en/blog/analytical-search.model.json",
                get(move || {
                    let model = model.clone();
                    async move { Json(model) }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });
        let article_url =
            Url::parse(&format!("http://{address}/en/blog/analytical-search/")).expect("URL");
        let crawler = HtmlArticleCrawler::new(HtmlArticleCrawlerConfig {
            allow_private_networks: true,
            min_content_chars: 200,
            ..HtmlArticleCrawlerConfig::default()
        })
        .expect("article crawler");

        let report = crawler
            .crawl_urls(&[article_url])
            .await
            .expect("crawl AEM article");

        assert!(report.failures.is_empty());
        assert_eq!(report.items.len(), 1);
        assert_eq!(
            report.items[0].payload["framework_fallback"],
            json!("aem-model-json.v1")
        );
        assert_eq!(
            report.items[0].payload["article_body_selector"],
            json!("[itemprop='articleBody']")
        );
        assert!(
            report.items[0]
                .body_html
                .as_deref()
                .is_some_and(|body| body.contains("First substantive AEM article section"))
        );
        assert!(
            report.items[0]
                .body_html
                .as_deref()
                .is_some_and(|body| !body.contains("privacy-form disclosure"))
        );
        task.abort();
    }

    #[tokio::test]
    async fn recipe_crawler_rejects_publication_page_returned_as_article() {
        use axum::{Router, response::Html, routing::get};

        let listing = r#"<!doctype html><html><body><main><a class="story" href="/news/current">Newsroom home</a></main></body></html>"#;
        let article_like_listing = format!(
            r#"<!doctype html><html><head><meta property="og:type" content="article"><meta property="og:title" content="Acme newsroom hub"><link rel="canonical" href="/news/"></head><body><article><h1>Acme newsroom hub</h1><p>{}</p></article></body></html>"#,
            "Substantive listing page body that must never be published as an article. ".repeat(8),
        );
        let app = Router::new()
            .route("/news/", get(move || async move { Html(listing) }))
            .route(
                "/news/current",
                get(move || {
                    let article_like_listing = article_like_listing.clone();
                    async move { Html(article_like_listing) }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });
        let recipe = CompanyNewsRecipeSpec {
            schema_version: feed_core::COMPANY_NEWS_RECIPE_SCHEMA_VERSION.to_owned(),
            publication_url: Url::parse(&format!("http://{address}/news/")).expect("URL"),
            render_mode: RecipeRenderMode::Http,
            article_link_selector: "a.story[href]".to_owned(),
            allowed_hosts: vec!["127.0.0.1".to_owned()],
            include_path_prefixes: vec!["/news/".to_owned()],
            exclude_path_prefixes: Vec::new(),
            max_links: 10,
            freshness: feed_core::RecipeFreshnessPolicy::default(),
            correctness: feed_core::RecipeCorrectnessPolicy::default(),
            item_scope: feed_core::RecipeItemScope::CompanyIdentity,
            evidence_article_urls: Vec::new(),
        };
        let crawler = HtmlRecipeCrawler::new(HtmlRecipeCrawlerConfig {
            allow_private_networks: true,
            min_content_chars: 20,
            ..HtmlRecipeCrawlerConfig::default()
        })
        .expect("crawler");
        let report = crawler.crawl(&recipe).await.expect("recipe crawl report");
        assert_eq!(report.discovered_url_count, 1);
        assert_eq!(report.accepted_item_count, 0);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].reason, "obvious_listing_path");
        assert!(!report.failures[0].retryable);
        assert!(!report.correctness_passed());
        task.abort();
    }
}
