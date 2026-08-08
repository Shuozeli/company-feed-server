#![cfg(feature = "postgres-tests")]

use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode, header},
    response::Html,
    routing::{get, post},
};
use feed_api::{ApiState, router as api_router};
use feed_core::{
    CandidateDecision, CandidateStatus, JobSpec, JobType, SourceKind, SourceStatus,
    WebDiscoveryAdapterMode,
};
use feed_crawler::{
    HtmlArticleCrawler, HtmlArticleCrawlerConfig, HtmlRecipeCrawler, HtmlRecipeCrawlerConfig,
    RssAtomCrawler, RssAtomCrawlerConfig,
};
use feed_db::Database;
use feed_discovery::{DiscoveryClient, DiscoveryConfig};
use feed_jobs::{
    CandidateValidationJobHandler, CandidateValidationPolicy, CompanyNewsExtractionJobHandler,
    CrawlJobHandler, DiscoveryJobHandler, ExportJobHandler, WebAdapterIntegration,
};
use feed_scheduler::{JobRunOutcome, JobRunner, JobRunnerConfig};
use feed_web_adapter::{
    COMPANY_NEWS_EXTRACTION_SCHEMA_VERSION, CompanyNewsArticleCandidate,
    CompanyNewsExtractionAdapterClient, CompanyNewsExtractionAdapterConfig,
    CompanyNewsExtractionRequest, CompanyNewsExtractionResponse, CompanyNewsPublicationCandidate,
    SuggestedResourceKind, WEB_DISCOVERY_SCHEMA_VERSION, WebDiscoveryAdapterClient,
    WebDiscoveryAdapterConfig, WebDiscoveryCandidate, WebDiscoveryRequest, WebDiscoveryResponse,
    WebPropertyRole,
};
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

const RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/">
  <channel>
    <title>Acme News</title>
    <link>https://example.com/news</link>
    <description>Acme public company news</description>
    <item>
      <guid>acme-launch-1</guid>
      <title>Acme launches a safer widget</title>
      <link>https://example.com/news/widget?utm_source=fixture</link>
      <description><![CDATA[<p>A concise launch summary.</p>]]></description>
      <content:encoded><![CDATA[
        <article><p>The widget is now generally available.</p><script>alert('unsafe')</script></article>
      ]]></content:encoded>
      <pubDate>Sun, 19 Jul 2026 12:00:00 GMT</pubDate>
    </item>
  </channel>
</rss>"#;

static PRODUCT_FLOW_TEST_LOCK: Mutex<()> = Mutex::const_new(());

#[tokio::test]
async fn public_feed_becomes_api_item_and_git_archive() {
    let _guard = PRODUCT_FLOW_TEST_LOCK.lock().await;

    let Some(database_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL is not set; skipping product flow integration test");
        return;
    };

    let fixture_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind feed fixture");
    let fixture_address = fixture_listener.local_addr().expect("fixture address");
    let feed_url = format!("http://{fixture_address}/feed");
    let homepage_feed_url = feed_url.clone();
    let fixture = Router::new()
        .route(
            "/",
            get(move || {
                let feed_url = homepage_feed_url.clone();
                async move {
                    Html(format!(
                        r#"<html><head><link rel="alternate" type="application/rss+xml" href="{feed_url}"></head><body>Acme</body></html>"#
                    ))
                }
            }),
        )
        .route(
            "/feed",
            get(|| async { ([("content-type", "application/rss+xml")], RSS) }),
        );
    let fixture_task = tokio::spawn(async move {
        axum::serve(fixture_listener, fixture)
            .await
            .expect("serve feed fixture");
    });

    let database = Database::connect(&database_url, 8)
        .await
        .expect("connect to Postgres");
    database.ensure_schema().await.expect("ensure schema");
    let unique = Uuid::new_v4().simple().to_string();
    let company_key = format!("e2e-{}", &unique[..8]);
    let company_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO companies (
            company_key, name, ownership_status, lifecycle_status,
            homepage_url, discovery_cadence_seconds
        )
        VALUES ($1, 'End-to-End Fixture Company', 'private', 'active', $2, 3600)
        RETURNING id
        "#,
    )
    .bind(&company_key)
    .bind(format!("http://{fixture_address}/"))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture company");

    let mut discovery_job = JobSpec::new(
        JobType::DiscoverCompany,
        format!("e2e-discovery-{unique}"),
        chrono::Utc::now(),
    );
    discovery_job.company_id = Some(company_id);
    discovery_job.priority = i16::MAX;
    let discovery_job = database
        .enqueue_job(&discovery_job)
        .await
        .expect("enqueue discovery");
    let discovery_handler = Arc::new(DiscoveryJobHandler::new(
        database.clone(),
        DiscoveryClient::new(DiscoveryConfig {
            probe_common_paths: false,
            allow_private_networks: true,
            request_timeout: Duration::from_secs(5),
            ..DiscoveryConfig::default()
        })
        .expect("discovery client"),
    ));
    let discovery_runner = runner(database.clone(), "e2e-discovery", discovery_handler);
    assert_eq!(
        discovery_runner
            .run_once(CancellationToken::new())
            .await
            .expect("run discovery"),
        JobRunOutcome::Completed {
            job_id: discovery_job.id
        }
    );

    let candidates = database
        .list_source_candidates(Some(company_id), Some(CandidateStatus::New), 20, 0)
        .await
        .expect("list candidates");
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.candidate_url.as_str() == feed_url)
        .expect("discovered fixture feed");
    let mut validation_job = JobSpec::new(
        JobType::ValidateCandidate,
        format!("e2e-validation-{unique}"),
        chrono::Utc::now(),
    );
    validation_job.company_id = Some(company_id);
    validation_job.candidate_id = Some(candidate.id);
    validation_job.priority = i16::MAX;
    let validation_job = database
        .enqueue_job(&validation_job)
        .await
        .expect("enqueue candidate validation");
    let validation_handler = Arc::new(CandidateValidationJobHandler::new(
        database.clone(),
        RssAtomCrawler::new(RssAtomCrawlerConfig {
            request_timeout: Duration::from_secs(5),
            allow_private_networks: true,
            ..RssAtomCrawlerConfig::default()
        })
        .expect("validation crawler"),
        CandidateValidationPolicy {
            auto_activate: true,
            public_export_allowed: true,
            activation_policy: feed_core::ValidationActivationPolicy::Strict,
            max_item_age_days: 730,
            freshness_slo_seconds: 3_600,
        },
    ));
    let validation_runner = runner(database.clone(), "e2e-validation", validation_handler);
    assert_eq!(
        validation_runner
            .run_once(CancellationToken::new())
            .await
            .expect("run validation"),
        JobRunOutcome::Completed {
            job_id: validation_job.id
        }
    );
    let candidate = database
        .get_source_candidate(candidate.id)
        .await
        .expect("reload candidate")
        .expect("candidate exists");
    assert_eq!(candidate.status, CandidateStatus::Accepted);
    let source = database
        .get_source(
            candidate
                .accepted_source_id
                .expect("validation activated source"),
        )
        .await
        .expect("load activated source")
        .expect("activated source exists");
    assert_eq!(source.status, SourceStatus::Approved);
    sqlx::query("UPDATE sources SET kind = 'atom' WHERE id = $1")
        .bind(source.id)
        .execute(database.pool())
        .await
        .expect("simulate a misdeclared feed kind");

    let mut crawl_job = JobSpec::new(
        JobType::CrawlSource,
        format!("e2e-crawl-{unique}"),
        chrono::Utc::now(),
    );
    crawl_job.company_id = Some(company_id);
    crawl_job.source_id = Some(source.id);
    crawl_job.priority = i16::MAX;
    let crawl_job = database
        .enqueue_job(&crawl_job)
        .await
        .expect("enqueue crawl");
    let crawl_handler = Arc::new(CrawlJobHandler::new(
        database.clone(),
        RssAtomCrawler::new(RssAtomCrawlerConfig {
            request_timeout: Duration::from_secs(5),
            allow_private_networks: true,
            ..RssAtomCrawlerConfig::default()
        })
        .expect("crawler"),
        HtmlRecipeCrawler::new(HtmlRecipeCrawlerConfig {
            allow_private_networks: true,
            ..HtmlRecipeCrawlerConfig::default()
        })
        .expect("recipe crawler"),
    ));
    let crawl_runner = runner(database.clone(), "e2e-crawl", crawl_handler);
    assert_eq!(
        crawl_runner
            .run_once(CancellationToken::new())
            .await
            .expect("run crawl"),
        JobRunOutcome::Completed {
            job_id: crawl_job.id
        }
    );

    let items = database
        .list_feed_items(Some(company_id), Some(source.id), 20, 0)
        .await
        .expect("list normalized items");
    let corrected_source = database
        .get_source(source.id)
        .await
        .expect("load corrected source")
        .expect("source exists");
    assert_eq!(corrected_source.kind, SourceKind::Rss);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source_kind, SourceKind::Rss);
    assert_eq!(items[0].title, "Acme launches a safer widget");
    assert!(!items[0].body_html.contains("<script"));
    assert_eq!(
        items[0].canonical_url,
        Url::parse("https://example.com/news/widget").expect("canonical URL")
    );
    let raw_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM raw_crawl_items WHERE source_id = $1")
            .bind(source.id)
            .fetch_one(database.pool())
            .await
            .expect("count raw items");
    assert_eq!(raw_count, 1);

    let api_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind API fixture");
    let api_address = api_listener.local_addr().expect("API address");
    let api = api_router(ApiState::new(
        database.clone(),
        "product-flow-test",
        false,
        Vec::new(),
    ));
    let api_task = tokio::spawn(async move {
        axum::serve(api_listener, api)
            .await
            .expect("serve API fixture");
    });
    let api_body = reqwest::get(format!(
        "http://{api_address}/api/v1/feed-items?company_id={company_id}&limit=5&offset=0"
    ))
    .await
    .expect("request feed API")
    .error_for_status()
    .expect("successful feed API")
    .bytes()
    .await
    .expect("feed API body");
    let api_response: serde_json::Value = serde_json::from_slice(&api_body).expect("feed API JSON");
    assert_eq!(api_response["total"], 1);
    assert_eq!(
        api_response["items"][0]["title"],
        "Acme launches a safer widget"
    );

    let export_directory = TempDir::new().expect("temporary export directory");
    let archive_path = export_directory.path().join("archive");
    let target_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO export_targets (
            target_id,
            repo_url,
            local_path,
            branch,
            format,
            layout,
            cadence_seconds,
            enabled,
            push_enabled
        )
        VALUES ($1, $2, $3, 'main', 'markdown_json', 'by_company_date', 3600, true, false)
        RETURNING id
        "#,
    )
    .bind(format!("e2e-{unique}"))
    .bind(
        export_directory
            .path()
            .join("unused.git")
            .display()
            .to_string(),
    )
    .bind(archive_path.display().to_string())
    .fetch_one(database.pool())
    .await
    .expect("insert export target");
    let mut export_job = JobSpec::new(
        JobType::ExportTarget,
        format!("e2e-export-{unique}"),
        chrono::Utc::now(),
    );
    export_job.export_target_id = Some(target_id);
    export_job.priority = i16::MAX;
    let export_job = database
        .enqueue_job(&export_job)
        .await
        .expect("enqueue export");
    let export_runner = runner(
        database.clone(),
        "e2e-export",
        Arc::new(ExportJobHandler::new(database.clone())),
    );
    assert_eq!(
        export_runner
            .run_once(CancellationToken::new())
            .await
            .expect("run export"),
        JobRunOutcome::Completed {
            job_id: export_job.id
        }
    );
    assert!(archive_path.join(".git").is_dir());
    assert!(archive_path.join("HEAD.json").is_file());
    assert!(
        archive_path
            .join("index/v1/current/manifest.json")
            .is_file()
    );
    let fixture_exported: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM exported_items
            WHERE target_id = $1 AND feed_item_id = $2
        )",
    )
    .bind(target_id)
    .bind(items[0].id)
    .fetch_one(database.pool())
    .await
    .expect("check fixture export");
    assert!(fixture_exported);
    let export_runs = database
        .list_export_runs(Some(target_id), 10, 0)
        .await
        .expect("list export runs");
    assert_eq!(export_runs.len(), 1);
    assert!(export_runs[0].commit_sha.is_some());
    let export_runs_body = reqwest::get(format!(
        "http://{api_address}/api/v1/export-runs?export_target_id={target_id}&limit=5&offset=0"
    ))
    .await
    .expect("request export runs API")
    .error_for_status()
    .expect("successful export runs API")
    .bytes()
    .await
    .expect("export runs API body");
    let export_runs_response: serde_json::Value =
        serde_json::from_slice(&export_runs_body).expect("export runs API JSON");
    assert_eq!(export_runs_response["total"], 1);
    assert_eq!(
        export_runs_response["items"][0]["export_target_id"],
        target_id.to_string()
    );

    database
        .reject_source_candidate(candidate.id)
        .await
        .expect("disable an accepted candidate");
    let disabled_source = database
        .get_source(source.id)
        .await
        .expect("load disabled source")
        .expect("disabled source exists");
    assert_eq!(disabled_source.status, SourceStatus::Disabled);
    assert!(
        database
            .list_feed_items(Some(company_id), Some(source.id), 20, 0)
            .await
            .expect("list items after source disable")
            .is_empty()
    );
    let decisions = database
        .list_candidate_decisions(Some(candidate.id), 10, 0)
        .await
        .expect("list candidate decisions");
    assert_eq!(decisions[0].decision, CandidateDecision::Rejected);
    assert_eq!(decisions[0].source_id, Some(source.id));

    api_task.abort();
    fixture_task.abort();
    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(database.pool())
        .await
        .expect("delete fixture company");
    sqlx::query("DELETE FROM export_targets WHERE id = $1")
        .bind(target_id)
        .execute(database.pool())
        .await
        .expect("delete export target");
    database.close().await;
}

#[tokio::test]
async fn private_adapter_suggestions_are_publicly_validated_before_storage() {
    let _guard = PRODUCT_FLOW_TEST_LOCK.lock().await;

    let Some(database_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL is not set; skipping web adapter integration test");
        return;
    };

    let site_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind site fixture");
    let site_address = site_listener.local_addr().expect("site address");
    let newsroom_url = format!("http://{site_address}/newsroom");
    let feed_url = format!("http://{site_address}/feed.xml");
    let site = Router::new().route(
        "/newsroom",
        get(|| async {
            Html(
                r#"<html><head><link rel="alternate" type="application/rss+xml" href="/feed.xml"></head><body>Example newsroom</body></html>"#,
            )
        }),
    );
    let site_task = tokio::spawn(async move {
        axum::serve(site_listener, site)
            .await
            .expect("serve site fixture");
    });

    let adapter_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind adapter fixture");
    let adapter_address = adapter_listener.local_addr().expect("adapter address");
    let adapter_newsroom_url = Url::parse(&newsroom_url).expect("newsroom URL");
    let adapter = Router::new().route(
        "/v1/discover",
        post(
            move |headers: HeaderMap, Json(request): Json<WebDiscoveryRequest>| {
                let newsroom_url = adapter_newsroom_url.clone();
                async move {
                    assert_eq!(
                        headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok()),
                        Some("Bearer fixture-token")
                    );
                    assert_eq!(request.company.name, "Web Adapter Fixture Company");
                    assert!(request.company.aliases.is_empty());
                    Json(WebDiscoveryResponse {
                        schema_version: WEB_DISCOVERY_SCHEMA_VERSION.to_owned(),
                        request_id: request.request_id,
                        candidates: vec![WebDiscoveryCandidate {
                            url: newsroom_url,
                            role: WebPropertyRole::Newsroom,
                            suggested_kind: SuggestedResourceKind::Html,
                            rank_score: 0.55,
                        }],
                        adapter_trace_id: Some("fixture-adapter-trace".to_owned()),
                    })
                }
            },
        ),
    );
    let adapter_task = tokio::spawn(async move {
        axum::serve(adapter_listener, adapter)
            .await
            .expect("serve adapter fixture");
    });

    let database = Database::connect(&database_url, 8)
        .await
        .expect("connect to Postgres");
    database.ensure_schema().await.expect("ensure schema");
    let unique = Uuid::new_v4().simple().to_string();
    let company_key = format!("web-adapter-{}", &unique[..8]);
    let company_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO companies (
            company_key, name, ownership_status, lifecycle_status,
            discovery_cadence_seconds
        )
        VALUES ($1, 'Web Adapter Fixture Company', 'private', 'active', 3600)
        RETURNING id
        "#,
    )
    .bind(&company_key)
    .fetch_one(database.pool())
    .await
    .expect("insert fixture company");
    let mut job = JobSpec::new(
        JobType::DiscoverCompany,
        format!("web-adapter-discovery-{unique}"),
        chrono::Utc::now(),
    );
    job.company_id = Some(company_id);
    let job = database.enqueue_job(&job).await.expect("enqueue discovery");
    let adapter_client = WebDiscoveryAdapterClient::new(WebDiscoveryAdapterConfig {
        base_url: Url::parse(&format!("http://{adapter_address}/")).expect("adapter URL"),
        bearer_token: Some("fixture-token".to_owned()),
        request_timeout: Duration::from_secs(5),
        max_response_bytes: 64 * 1024,
        max_candidates: 20,
    })
    .expect("adapter client");
    let integration =
        WebAdapterIntegration::new(adapter_client, WebDiscoveryAdapterMode::Fallback, 20)
            .expect("adapter integration");
    let handler = Arc::new(
        DiscoveryJobHandler::new(
            database.clone(),
            DiscoveryClient::new(DiscoveryConfig {
                probe_common_paths: false,
                allow_private_networks: true,
                request_timeout: Duration::from_secs(5),
                ..DiscoveryConfig::default()
            })
            .expect("discovery client"),
        )
        .with_web_adapter(Some(integration)),
    );
    let discovery_runner = runner(database.clone(), "web-adapter-e2e", handler);
    assert_eq!(
        discovery_runner
            .run_once(CancellationToken::new())
            .await
            .expect("run discovery"),
        JobRunOutcome::Completed { job_id: job.id }
    );

    let candidates = database
        .list_source_candidates(Some(company_id), Some(CandidateStatus::New), 20, 0)
        .await
        .expect("list candidates");
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.candidate_url.as_str() == feed_url)
        .expect("publicly parsed feed candidate");
    assert_eq!(candidate.candidate_kind, SourceKind::Rss);
    assert_eq!(
        candidate.evidence["observations"][0]["external_web_adapter"]["roles"][0],
        "newsroom"
    );
    let runs = database
        .list_discovery_runs(Some(company_id), 10, 0)
        .await
        .expect("list discovery runs");
    assert_eq!(
        runs[0].metadata["web_adapter"]["adapter_trace_id"],
        "fixture-adapter-trace"
    );

    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(database.pool())
        .await
        .expect("delete fixture company");
    database.close().await;
    adapter_task.abort();
    site_task.abort();
}

#[tokio::test]
async fn manual_news_import_fetches_and_persists_cited_public_articles() {
    let _guard = PRODUCT_FLOW_TEST_LOCK.lock().await;

    let Some(database_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL is not set; skipping manual news import integration test");
        return;
    };

    let published_at = chrono::Utc::now() - chrono::Duration::days(2);
    let site_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind article fixture");
    let site_address = site_listener.local_addr().expect("site address");
    let article_url = format!("http://{site_address}/news/manual-import-launch");
    let unavailable_article_url = format!("http://{site_address}/wire/unavailable-citation");
    let publication_url = format!("http://{site_address}/news/");
    let mirror_publication_url = format!("http://{site_address}/updates/");
    let canonical_url = article_url.clone();
    let article_html = format!(
        r#"<!doctype html><html><head>
        <meta property="og:type" content="article">
        <meta property="og:title" content="Manual import launch">
        <meta property="article:published_time" content="{}">
        <meta name="description" content="A verified fixture article.">
        <link rel="canonical" href="{}">
        </head><body><article><h1>Manual import launch</h1>
        <p>{}</p><script>notPersisted()</script></article></body></html>"#,
        published_at.to_rfc3339(),
        canonical_url,
        "This is independently fetched public article content. ".repeat(12),
    );
    let listing_state = Arc::new(AtomicU8::new(0));
    let fixture_listing_state = listing_state.clone();
    let site = Router::new()
        .route(
            "/news/",
            get(move || {
                let state = fixture_listing_state.load(Ordering::SeqCst);
                async move {
                    match state {
                        0 => (
                            StatusCode::OK,
                            Html(
                            r#"<!doctype html><html><body><main><a class="story" href="/news/manual-import-launch">Manual import launch</a></main></body></html>"#,
                            ),
                        ),
                        1 => (
                            StatusCode::OK,
                            Html(
                            r#"<!doctype html><html><body><main><a href="/privacy">Privacy policy and legal information</a></main></body></html>"#,
                            ),
                        ),
                        _ => (
                            StatusCode::TOO_MANY_REQUESTS,
                            Html("rate limited; retry later"),
                        ),
                    }
                }
            }),
        )
        .route(
            "/news/manual-import-launch",
            get(move || {
                let article_html = article_html.clone();
                async move { Html(article_html) }
            }),
        )
        .route(
            "/updates/",
            get(|| async {
                Html(
                    r#"<!doctype html><html><body><main><a class="story" href="/news/manual-import-launch">Manual import launch</a></main></body></html>"#,
                )
            }),
        )
        .route(
            "/wire/unavailable-citation",
            get(|| async {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Html("temporarily unavailable"),
                )
            }),
        );
    let site_task = tokio::spawn(async move {
        axum::serve(site_listener, site)
            .await
            .expect("serve article fixture");
    });

    let adapter_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind news adapter fixture");
    let adapter_address = adapter_listener.local_addr().expect("adapter address");
    let adapter_article_url = Url::parse(&article_url).expect("article URL");
    let adapter_unavailable_article_url =
        Url::parse(&unavailable_article_url).expect("unavailable article URL");
    let adapter_publication_url = Url::parse(&publication_url).expect("publication URL");
    let adapter_mirror_publication_url =
        Url::parse(&mirror_publication_url).expect("mirror publication URL");
    let adapter_article_state = Arc::new(AtomicU8::new(0));
    let fixture_adapter_article_state = adapter_article_state.clone();
    let adapter = Router::new().route(
        "/v1/extract-news",
        post(
            move |headers: HeaderMap, Json(request): Json<CompanyNewsExtractionRequest>| {
                let article_url = if fixture_adapter_article_state.load(Ordering::SeqCst) == 0 {
                    adapter_article_url.clone()
                } else {
                    adapter_unavailable_article_url.clone()
                };
                let publication_url = adapter_publication_url.clone();
                let mirror_publication_url = adapter_mirror_publication_url.clone();
                async move {
                    assert_eq!(
                        headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok()),
                        Some("Bearer fixture-token")
                    );
                    assert_eq!(request.company.name, "Manual News Fixture Company");
                    assert!(
                        request
                            .company
                            .aliases
                            .contains(&"Manual Fixture".to_owned())
                    );
                    Json(CompanyNewsExtractionResponse {
                        schema_version: COMPANY_NEWS_EXTRACTION_SCHEMA_VERSION.to_owned(),
                        request_id: request.request_id,
                        publications: vec![
                            CompanyNewsPublicationCandidate {
                                url: publication_url,
                                rank_score: 0.9,
                            },
                            CompanyNewsPublicationCandidate {
                                url: mirror_publication_url,
                                rank_score: 0.7,
                            },
                        ],
                        articles: vec![CompanyNewsArticleCandidate {
                            url: article_url,
                            rank_score: 0.8,
                        }],
                        adapter_trace_id: Some("manual-fixture-trace".to_owned()),
                    })
                }
            },
        ),
    );
    let adapter_task = tokio::spawn(async move {
        axum::serve(adapter_listener, adapter)
            .await
            .expect("serve news adapter fixture");
    });

    let database = Database::connect(&database_url, 8)
        .await
        .expect("connect to Postgres");
    database.ensure_schema().await.expect("ensure schema");
    let unique = Uuid::new_v4().simple().to_string();
    let company_key = format!("manual-news-{}", &unique[..8]);
    let company_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO companies (
            company_key, name, aliases, ownership_status, lifecycle_status,
            discovery_cadence_seconds
        )
        VALUES ($1, 'Manual News Fixture Company', '["Manual Fixture"]', 'private', 'active', 3600)
        RETURNING id
        "#,
    )
    .bind(&company_key)
    .fetch_one(database.pool())
    .await
    .expect("insert manual fixture company");
    let window_end = chrono::Utc::now();
    let window_start = window_end - chrono::Duration::days(31);
    let mut job = JobSpec::new(
        JobType::ExtractCompanyNews,
        format!("manual-news-e2e-{unique}"),
        chrono::Utc::now(),
    );
    job.company_id = Some(company_id);
    job.priority = i16::MAX;
    job.payload = serde_json::json!({
        "schema_version": "company-news-extraction-job.v1",
        "window_start": window_start,
        "window_end": window_end,
        "max_articles": 10,
    });
    let job = database
        .enqueue_job(&job)
        .await
        .expect("enqueue manual news import");
    let handler = Arc::new(CompanyNewsExtractionJobHandler::new(
        database.clone(),
        CompanyNewsExtractionAdapterClient::new(CompanyNewsExtractionAdapterConfig {
            base_url: Url::parse(&format!("http://{adapter_address}/")).expect("adapter base URL"),
            bearer_token: Some("fixture-token".to_owned()),
            request_timeout: Duration::from_secs(5),
            max_response_bytes: 64 * 1024,
            max_articles: 10,
        })
        .expect("news adapter client"),
        HtmlArticleCrawler::new(HtmlArticleCrawlerConfig {
            request_timeout: Duration::from_secs(5),
            max_articles: 10,
            max_concurrency: 2,
            min_content_chars: 200,
            allow_private_networks: true,
            ..HtmlArticleCrawlerConfig::default()
        })
        .expect("article crawler"),
        HtmlRecipeCrawler::new(HtmlRecipeCrawlerConfig {
            request_timeout: Duration::from_secs(5),
            max_articles: 10,
            max_concurrency: 2,
            min_content_chars: 200,
            allow_private_networks: true,
            ..HtmlRecipeCrawlerConfig::default()
        })
        .expect("recipe crawler"),
        10,
        30 * 24 * 60 * 60,
        false,
    ));
    let news_runner = runner(database.clone(), "manual-news-e2e", handler);
    assert_eq!(
        news_runner
            .run_once(CancellationToken::new())
            .await
            .expect("run manual news import"),
        JobRunOutcome::Completed { job_id: job.id }
    );

    // Direct evidence remains in the audit store, while the public item list
    // waits for the independently validated recipe source to crawl it.
    let item: (Uuid, String, String, String, String) = sqlx::query_as(
        r#"
        SELECT source_id, title, body_text, body_html, source_kind
        FROM feed_items
        WHERE company_id = $1
        "#,
    )
    .bind(company_id)
    .fetch_one(database.pool())
    .await
    .expect("read independently fetched audit item");
    assert_eq!(item.1, "Manual import launch");
    assert!(item.2.chars().count() >= 200);
    assert!(!item.3.contains("script"));
    assert_eq!(item.4, SourceKind::Html.as_str());
    let raw_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM raw_crawl_items WHERE source_id = $1")
            .bind(item.0)
            .fetch_one(database.pool())
            .await
            .expect("count raw extracted items");
    assert_eq!(raw_count, 1);
    let runs = database
        .list_company_news_extraction_runs(Some(company_id), 10, 0)
        .await
        .expect("list extraction runs");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].suggested_url_count, 1);
    assert_eq!(runs[0].accepted_url_count, 1);
    assert_eq!(runs[0].normalized_item_count, 1);
    assert_eq!(runs[0].new_item_count, 1);
    assert_eq!(runs[0].metadata["adapter_trace_id"], "manual-fixture-trace");
    assert_eq!(
        runs[0].metadata["adapter_request_id"],
        runs[0].id.to_string()
    );
    assert_ne!(runs[0].metadata["adapter_request_id"], job.id.to_string());
    assert_eq!(runs[0].metadata["activated_recipe_count"], 1);
    assert_eq!(runs[0].metadata["seeded_discovery_seed_count"], 2);
    let seeded_discovery_payload: serde_json::Value = sqlx::query_scalar(
        r#"
        SELECT payload
        FROM jobs
        WHERE company_id = $1
          AND job_type = 'discover_company'
          AND payload->>'seed_origin' = 'company_news_recipe_builder'
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(company_id)
    .fetch_one(database.pool())
    .await
    .expect("find recipe-seeded discovery handoff");
    assert_eq!(
        seeded_discovery_payload["origin_run_id"],
        runs[0].id.to_string()
    );
    assert_eq!(
        seeded_discovery_payload["seeds"].as_array().map(Vec::len),
        Some(2)
    );
    let recipes = database
        .list_company_news_recipes(
            Some(company_id),
            Some(feed_core::RecipeStatus::Active),
            10,
            0,
        )
        .await
        .expect("list active crawl recipes");
    assert_eq!(recipes.len(), 1);
    assert_eq!(recipes[0].spec.publication_url.as_str(), publication_url);
    assert_eq!(recipes[0].spec.correctness.baseline_accepted_items, 1);
    assert_eq!(recipes[0].health.correctness_status, "passing");
    let mut mirror_spec = recipes[0].spec.clone();
    mirror_spec.publication_url =
        Url::parse(&mirror_publication_url).expect("mirror publication URL");
    let mirror_source = database
        .get_or_create_company_news_source(
            company_id,
            &format!("{company_key}:mirror"),
            &mirror_spec.publication_url,
            30 * 24 * 60 * 60,
            false,
            runs[0].id,
        )
        .await
        .expect("create pre-existing mirror source");
    database
        .activate_company_news_recipe(
            &format!("{company_key}:mirror:recipe"),
            company_id,
            mirror_source.id,
            &mirror_spec,
            "fixture-mirror-content-hash",
            Some(runs[0].id),
            chrono::Utc::now(),
            Some(published_at),
            true,
            Some("fixture-mirror-structure"),
            serde_json::json!({"fixture": "pre_existing_listing_mirror"}),
        )
        .await
        .expect("activate pre-existing mirror recipe");
    assert_eq!(
        database
            .count_company_news_recipes(Some(company_id), Some(feed_core::RecipeStatus::Active))
            .await
            .expect("count active recipes with fixture mirror"),
        2
    );

    let homepage_url = format!("http://{site_address}/");
    let investor_relations_url = format!("http://127.0.0.2:{}/investors/", site_address.port());
    sqlx::query(
        r#"
        UPDATE companies
        SET homepage_url = $2, investor_relations_url = $3
        WHERE id = $1
        "#,
    )
    .bind(company_id)
    .bind(&homepage_url)
    .bind(&investor_relations_url)
    .execute(database.pool())
    .await
    .expect("add verified cross-domain company profile");
    let mut rebuild_job = JobSpec::new(
        JobType::ExtractCompanyNews,
        format!("manual-news-rebuild-e2e-{unique}"),
        chrono::Utc::now(),
    );
    rebuild_job.company_id = Some(company_id);
    rebuild_job.payload = serde_json::json!({
        "schema_version": "company-news-extraction-job.v1",
        "window_start": window_start,
        "window_end": window_end,
        "max_articles": 10,
        "include_covered": true,
    });
    let rebuild_job = database
        .enqueue_job(&rebuild_job)
        .await
        .expect("enqueue recipe rebuild");
    assert_eq!(
        news_runner
            .run_once(CancellationToken::new())
            .await
            .expect("run recipe rebuild"),
        JobRunOutcome::Completed {
            job_id: rebuild_job.id
        }
    );
    let recipes = database
        .list_company_news_recipes(
            Some(company_id),
            Some(feed_core::RecipeStatus::Active),
            10,
            0,
        )
        .await
        .expect("list rebuilt active crawl recipe");
    assert_eq!(recipes.len(), 1);
    assert_eq!(recipes[0].version, 2);
    assert_eq!(
        recipes[0].spec.allowed_hosts,
        vec!["127.0.0.1".to_owned(), "127.0.0.2".to_owned()]
    );
    let superseded_recipe_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM company_news_recipes WHERE company_id = $1 AND status = 'superseded'",
    )
    .bind(company_id)
    .fetch_one(database.pool())
    .await
    .expect("count superseded recipe versions");
    assert_eq!(
        superseded_recipe_count, 2,
        "the prior version and the now-redundant listing mirror are both superseded"
    );
    let mirror_supersession_event_count: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM event_log
        WHERE company_id = $1
          AND event_type = 'company_news.recipe_superseded'
          AND payload ->> 'reason' = 'overlaps_selected_recipe_items'
        "#,
    )
    .bind(company_id)
    .fetch_one(database.pool())
    .await
    .expect("count automatic mirror supersession events");
    assert_eq!(mirror_supersession_event_count, 1);

    adapter_article_state.store(1, Ordering::SeqCst);
    let mut fallback_job = JobSpec::new(
        JobType::ExtractCompanyNews,
        format!("manual-news-publication-fallback-e2e-{unique}"),
        chrono::Utc::now(),
    );
    fallback_job.company_id = Some(company_id);
    fallback_job.payload = serde_json::json!({
        "schema_version": "company-news-extraction-job.v1",
        "window_start": window_start,
        "window_end": window_end,
        "max_articles": 10,
        "include_covered": true,
    });
    let fallback_job = database
        .enqueue_job(&fallback_job)
        .await
        .expect("enqueue publication fallback rebuild");
    assert_eq!(
        news_runner
            .run_once(CancellationToken::new())
            .await
            .expect("run publication fallback rebuild"),
        JobRunOutcome::Completed {
            job_id: fallback_job.id
        }
    );
    let fallback_runs = database
        .list_company_news_extraction_runs(Some(company_id), 10, 0)
        .await
        .expect("list publication fallback extraction runs");
    assert_eq!(
        fallback_runs[0].metadata["continued_after_transient_evidence_failure"],
        true
    );
    assert_eq!(fallback_runs[0].suggested_url_count, 1);
    assert_eq!(fallback_runs[0].accepted_url_count, 0);
    assert_eq!(fallback_runs[0].rejected_url_count, 1);
    assert_eq!(fallback_runs[0].metadata["activated_recipe_count"], 1);
    let recipes = database
        .list_company_news_recipes(
            Some(company_id),
            Some(feed_core::RecipeStatus::Active),
            10,
            0,
        )
        .await
        .expect("list publication fallback active recipe");
    assert_eq!(recipes.len(), 1);
    assert_eq!(recipes[0].version, 3);
    let superseded_recipe_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM company_news_recipes WHERE company_id = $1 AND status = 'superseded'",
    )
    .bind(company_id)
    .fetch_one(database.pool())
    .await
    .expect("count publication fallback superseded recipe versions");
    assert_eq!(superseded_recipe_count, 3);

    let crawl_handler = Arc::new(CrawlJobHandler::new(
        database.clone(),
        RssAtomCrawler::new(RssAtomCrawlerConfig {
            request_timeout: Duration::from_secs(5),
            allow_private_networks: true,
            ..RssAtomCrawlerConfig::default()
        })
        .expect("feed crawler"),
        HtmlRecipeCrawler::new(HtmlRecipeCrawlerConfig {
            request_timeout: Duration::from_secs(5),
            max_articles: 10,
            max_concurrency: 2,
            min_content_chars: 200,
            allow_private_networks: true,
            ..HtmlRecipeCrawlerConfig::default()
        })
        .expect("scheduled recipe crawler"),
    ));
    let crawl_runner = runner(database.clone(), "manual-news-recipe-e2e", crawl_handler);
    assert!(matches!(
        crawl_runner
            .run_once(CancellationToken::new())
            .await
            .expect("run activated recipe"),
        JobRunOutcome::Completed { .. }
    ));
    let recipe_runs = database
        .list_company_news_recipe_runs(Some(recipes[0].id), 10, 0)
        .await
        .expect("list recipe runs");
    assert_eq!(recipe_runs.len(), 1);
    assert_eq!(recipe_runs[0].status, feed_core::RecipeRunStatus::Passed);
    assert_eq!(recipe_runs[0].accepted_item_count, 1);

    listing_state.store(2, Ordering::SeqCst);
    let mut transient_job = JobSpec::new(
        JobType::CrawlSource,
        format!("source:{}", recipes[0].source_id),
        chrono::Utc::now(),
    );
    transient_job.company_id = Some(company_id);
    transient_job.source_id = Some(recipes[0].source_id);
    let transient_job = database
        .enqueue_job(&transient_job)
        .await
        .expect("enqueue transient crawl");
    assert!(matches!(
        crawl_runner
            .run_once(CancellationToken::new())
            .await
            .expect("run rate-limited crawl"),
        JobRunOutcome::RetryScheduled { job_id, .. } if job_id == transient_job.id
    ));
    let recipe_after_transient = database
        .get_active_company_news_recipe_for_source(recipes[0].source_id)
        .await
        .expect("query active recipe after transient failure")
        .expect("transient failure must not stale recipe");
    assert_eq!(recipe_after_transient.health.consecutive_failures, 0);
    assert_eq!(recipe_after_transient.health.consecutive_empty_runs, 0);
    assert_eq!(
        recipe_after_transient
            .health
            .consecutive_correctness_failures,
        0
    );
    assert_eq!(recipe_after_transient.health.correctness_status, "passing");
    assert_eq!(recipe_after_transient.health.freshness_status, "fresh");
    let transient_runs = database
        .list_company_news_recipe_runs(Some(recipes[0].id), 10, 0)
        .await
        .expect("list transient recipe runs");
    assert_eq!(transient_runs[0].status, feed_core::RecipeRunStatus::Failed);
    assert_eq!(transient_runs[0].metadata["transient_error"], true);

    listing_state.store(0, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        crawl_runner
            .run_once(CancellationToken::new())
            .await
            .expect("recover rate-limited crawl"),
        JobRunOutcome::Completed {
            job_id: transient_job.id
        }
    );
    let recipe_after_recovery = database
        .get_active_company_news_recipe_for_source(recipes[0].source_id)
        .await
        .expect("query active recipe after transient recovery")
        .expect("recipe remains active after transient recovery");
    assert_eq!(recipe_after_recovery.health.consecutive_failures, 0);
    assert_eq!(recipe_after_recovery.health.correctness_status, "passing");
    assert_eq!(recipe_after_recovery.health.freshness_status, "fresh");

    let raw_before_drift: i64 =
        sqlx::query_scalar("SELECT count(*) FROM raw_crawl_items WHERE source_id = $1")
            .bind(recipes[0].source_id)
            .fetch_one(database.pool())
            .await
            .expect("count recipe raw items before drift");
    listing_state.store(1, Ordering::SeqCst);
    for attempt in 0..3 {
        let mut drift_job = JobSpec::new(
            JobType::CrawlSource,
            format!("source:{}", recipes[0].source_id),
            chrono::Utc::now(),
        );
        drift_job.company_id = Some(company_id);
        drift_job.source_id = Some(recipes[0].source_id);
        let drift_job = database
            .enqueue_job(&drift_job)
            .await
            .expect("enqueue drift crawl");
        assert_eq!(
            crawl_runner
                .run_once(CancellationToken::new())
                .await
                .expect("run drift crawl"),
            JobRunOutcome::Completed {
                job_id: drift_job.id
            },
            "drift attempt {attempt}"
        );
    }
    assert!(
        database
            .get_active_company_news_recipe_for_source(recipes[0].source_id)
            .await
            .expect("query active recipe after drift")
            .is_none()
    );
    let stale_recipes = database
        .list_company_news_recipes(
            Some(company_id),
            Some(feed_core::RecipeStatus::Stale),
            10,
            0,
        )
        .await
        .expect("list stale recipes");
    assert_eq!(stale_recipes.len(), 1);
    assert!(stale_recipes[0].health.rebuild_required);
    assert_eq!(stale_recipes[0].health.consecutive_empty_runs, 3);
    let raw_after_drift: i64 =
        sqlx::query_scalar("SELECT count(*) FROM raw_crawl_items WHERE source_id = $1")
            .bind(recipes[0].source_id)
            .fetch_one(database.pool())
            .await
            .expect("count recipe raw items after drift");
    assert_eq!(raw_after_drift, raw_before_drift);

    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(database.pool())
        .await
        .expect("delete manual fixture company");
    database.close().await;
    adapter_task.abort();
    site_task.abort();
}

fn runner<H>(database: Database, worker_id: &str, handler: Arc<H>) -> JobRunner<H>
where
    H: feed_scheduler::JobHandler,
{
    JobRunner::new(
        database,
        worker_id,
        handler,
        JobRunnerConfig {
            lease_duration: Duration::from_secs(30),
            heartbeat_interval: Duration::from_secs(5),
            poll_interval: Duration::from_millis(10),
            retry_base: Duration::from_millis(10),
            retry_max: Duration::from_secs(1),
            max_company_news_in_flight: 1,
        },
    )
    .expect("job runner")
}
