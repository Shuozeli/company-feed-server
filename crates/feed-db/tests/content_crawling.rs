#![cfg(feature = "postgres-tests")]

use std::{collections::HashSet, time::Duration};

use chrono::{Duration as ChronoDuration, Utc};
use feed_core::{JobSpec, JobType, NormalizedFeedItem, SourceKind};
use feed_db::{ContentCrawlFailure, ContentCrawlSuccess, Database};
use serde_json::json;
use tokio::sync::Mutex;
use url::Url;
use uuid::Uuid;

static CONTENT_CRAWL_TEST_LOCK: Mutex<()> = Mutex::const_new(());

#[tokio::test]
async fn content_crawl_state_tracks_success_refresh_and_retry() {
    let _guard = CONTENT_CRAWL_TEST_LOCK.lock().await;
    let Some(database_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL is not set; skipping Postgres integration test");
        return;
    };
    let database = Database::connect(&database_url, 5)
        .await
        .expect("connect to test Postgres");
    database.ensure_schema().await.expect("ensure schema");

    let suffix = Uuid::new_v4().simple().to_string();
    let company_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO companies (
            company_key, name, ownership_status, lifecycle_status,
            discovery_cadence_seconds
        )
        VALUES ($1, $2, 'private', 'active', 3600)
        RETURNING id
        "#,
    )
    .bind(format!("content-crawl-{}", &suffix[..12]))
    .bind(format!("Content Crawl Fixture {suffix}"))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture company");
    let article_url =
        Url::parse(&format!("https://{suffix}.example.test/news/article")).expect("fixture URL");
    let source_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (source_id, company_id, kind, url, status)
        VALUES ($1, $2, 'rss', $3, 'approved')
        RETURNING id
        "#,
    )
    .bind(format!("content-crawl-source-{}", &suffix[..12]))
    .bind(company_id)
    .bind(format!("https://{suffix}.example.test/feed.xml"))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture source");
    let observed_at = Utc::now() - ChronoDuration::minutes(5);
    let feed_item_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO feed_items (
            company_id, source_id, external_id, url, canonical_url,
            title, summary, body_text, body_html, body_markdown,
            published_at, fetched_at, content_hash, source_kind
        )
        VALUES (
            $1, $2, $3, $3, $3,
            'Feed title', 'Feed excerpt', 'Feed excerpt', '<p>Feed excerpt</p>',
            'Feed excerpt', $4, $4, $5, 'rss'
        )
        RETURNING id
        "#,
    )
    .bind(company_id)
    .bind(source_id)
    .bind(article_url.as_str())
    .bind(observed_at)
    .bind(format!("sha256:feed:{suffix}"))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture feed item");

    let first_job = content_job(&database, company_id, &suffix, "first").await;
    let first_started_at = Utc::now();
    let first = database
        .begin_content_crawl_batch(
            first_job,
            first_started_at,
            first_started_at - ChronoDuration::days(30),
            200,
            10,
        )
        .await
        .expect("begin first content batch");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].feed_item_id, feed_item_id);
    assert_eq!(first[0].attempt_count, 1);

    let body = "Independent article page body. ".repeat(20);
    let completed_at = Utc::now();
    database
        .complete_content_crawl_success(
            &ContentCrawlSuccess {
                attempt_id: first[0].attempt_id,
                feed_item_id,
                requested_url: article_url.clone(),
                normalized: NormalizedFeedItem {
                    external_id: article_url.as_str().to_owned(),
                    url: article_url.clone(),
                    canonical_url: article_url.clone(),
                    title: "Independent article title".to_owned(),
                    summary: "Independent summary".to_owned(),
                    body_text: body.clone(),
                    body_html: format!("<p>{body}</p>"),
                    body_markdown: body.clone(),
                    published_at: Some(observed_at),
                    fetched_at: completed_at,
                    content_hash: format!("sha256:article:{suffix}"),
                    source_kind: SourceKind::Rss,
                    raw: json!({}),
                    normalized: json!({}),
                    content_processing: json!({"contract": "test-content.v1"}),
                },
                extraction_metadata: json!({"selector": "article"}),
            },
            Duration::from_secs(3600),
        )
        .await
        .expect("complete content crawl success");

    let (stored_body, state_status, content_chars, extraction_version): (
        String,
        String,
        i32,
        String,
    ) = sqlx::query_as(
        r#"
        SELECT
            item.body_text,
            state.status,
            state.content_chars,
            state.extraction_version
        FROM feed_items AS item
        JOIN content_crawl_state AS state ON state.feed_item_id = item.id
        WHERE item.id = $1
        "#,
    )
    .bind(feed_item_id)
    .fetch_one(database.pool())
    .await
    .expect("read hydrated item");
    assert_eq!(stored_body, body);
    assert_eq!(state_status, "succeeded");
    assert_eq!(content_chars as usize, stored_body.chars().count());
    assert_eq!(extraction_version, "generic-public-article.v1");

    let not_due_job = content_job(&database, company_id, &suffix, "not-due").await;
    let not_due = database
        .begin_content_crawl_batch(
            not_due_job,
            completed_at + ChronoDuration::minutes(30),
            completed_at - ChronoDuration::minutes(30),
            200,
            10,
        )
        .await
        .expect("check refresh window");
    assert!(not_due.is_empty());

    let retry_job = content_job(&database, company_id, &suffix, "retry").await;
    let retry_started_at = completed_at + ChronoDuration::hours(2);
    let retry = database
        .begin_content_crawl_batch(
            retry_job,
            retry_started_at,
            completed_at + ChronoDuration::hours(1),
            200,
            10,
        )
        .await
        .expect("begin refresh attempt");
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].attempt_count, 2);
    let next_attempt_at = retry_started_at + ChronoDuration::minutes(10);
    database
        .complete_content_crawl_failure(&ContentCrawlFailure {
            attempt_id: retry[0].attempt_id,
            feed_item_id,
            requested_url: article_url,
            reason: "request".to_owned(),
            retryable: true,
            error: "temporary fixture failure".to_owned(),
            http_status: None,
            next_attempt_at: Some(next_attempt_at),
        })
        .await
        .expect("record retryable content failure");

    let (status, retry_at): (String, chrono::DateTime<Utc>) = sqlx::query_as(
        "SELECT status, next_attempt_at FROM content_crawl_state WHERE feed_item_id = $1",
    )
    .bind(feed_item_id)
    .fetch_one(database.pool())
    .await
    .expect("read retry state");
    assert_eq!(status, "failed");
    assert_eq!(
        retry_at.timestamp_micros(),
        next_attempt_at.timestamp_micros()
    );

    let coverage = database
        .content_crawl_coverage(200)
        .await
        .expect("read content coverage");
    assert!(coverage.eligible_items >= 1);
    assert!(coverage.failed >= 1);
    assert!(coverage.with_substantive_body >= 1);

    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(database.pool())
        .await
        .expect("delete content crawl fixture");
}

#[tokio::test]
async fn concurrent_content_batches_claim_disjoint_items() {
    let _guard = CONTENT_CRAWL_TEST_LOCK.lock().await;
    let Some(database_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL is not set; skipping Postgres integration test");
        return;
    };
    let database = Database::connect(&database_url, 8)
        .await
        .expect("connect to test Postgres");
    database.ensure_schema().await.expect("ensure schema");

    let suffix = Uuid::new_v4().simple().to_string();
    let company_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO companies (
            company_key, name, ownership_status, lifecycle_status,
            discovery_cadence_seconds
        )
        VALUES ($1, $2, 'private', 'active', 3600)
        RETURNING id
        "#,
    )
    .bind(format!("content-claim-{}", &suffix[..12]))
    .bind(format!("Content Claim Fixture {suffix}"))
    .fetch_one(database.pool())
    .await
    .expect("insert claim fixture company");
    let source_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (source_id, company_id, kind, url, status)
        VALUES ($1, $2, 'rss', $3, 'approved')
        RETURNING id
        "#,
    )
    .bind(format!("content-claim-source-{}", &suffix[..12]))
    .bind(company_id)
    .bind(format!("https://{suffix}.example.test/feed.xml"))
    .fetch_one(database.pool())
    .await
    .expect("insert claim fixture source");
    for index in 0..4 {
        let url = format!("https://{suffix}.example.test/news/{index}");
        sqlx::query(
            r#"
            INSERT INTO feed_items (
                company_id, source_id, external_id, url, canonical_url,
                title, fetched_at, content_hash, source_kind
            )
            VALUES ($1, $2, $3, $3, $3, $4, CURRENT_TIMESTAMP, $5, 'rss')
            "#,
        )
        .bind(company_id)
        .bind(source_id)
        .bind(url)
        .bind(format!("Claim fixture {index}"))
        .bind(format!("sha256:claim:{suffix}:{index}"))
        .execute(database.pool())
        .await
        .expect("insert claim fixture item");
    }

    let first_job = content_job(&database, company_id, &suffix, "claim-first").await;
    let second_job = content_job(&database, company_id, &suffix, "claim-second").await;
    let now = Utc::now();
    let (first, second) = tokio::join!(
        database.begin_content_crawl_batch(first_job, now, now - ChronoDuration::days(30), 200, 2,),
        database
            .begin_content_crawl_batch(second_job, now, now - ChronoDuration::days(30), 200, 2,),
    );
    let first = first.expect("claim first batch");
    let second = second.expect("claim second batch");
    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 2);
    let first_ids = first
        .iter()
        .map(|candidate| candidate.feed_item_id)
        .collect::<HashSet<_>>();
    let second_ids = second
        .iter()
        .map(|candidate| candidate.feed_item_id)
        .collect::<HashSet<_>>();
    assert!(first_ids.is_disjoint(&second_ids));

    database
        .cancel_running_content_crawl_attempts_for_job(first_job, "fixture cleanup")
        .await
        .expect("cancel first fixture batch");
    database
        .cancel_running_content_crawl_attempts_for_job(second_job, "fixture cleanup")
        .await
        .expect("cancel second fixture batch");
    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(database.pool())
        .await
        .expect("delete claim fixture");
}

async fn content_job(database: &Database, company_id: Uuid, suffix: &str, label: &str) -> Uuid {
    let mut spec = JobSpec::new(
        JobType::CrawlContent,
        format!("content-crawl-test:{suffix}:{label}"),
        Utc::now(),
    );
    spec.company_id = Some(company_id);
    database
        .enqueue_job(&spec)
        .await
        .expect("enqueue content crawl fixture")
        .id
}
