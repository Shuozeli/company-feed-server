#![cfg(feature = "postgres-tests")]

use chrono::Utc;
use feed_core::{
    JobSpec, JobType, NormalizedFeedItem, ProcessedCrawlItem, RawCrawlItem, SourceKind,
};
use feed_db::Database;
use serde_json::json;
use url::Url;
use uuid::Uuid;

#[tokio::test]
async fn successful_recrawl_releases_only_named_listing_quarantine() {
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
        VALUES ($1, 'Quarantine Release Fixture', 'private', 'active', 3600)
        RETURNING id
        "#,
    )
    .bind(format!("quarantine-release-{}", &suffix[..10]))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture company");
    let source_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (source_id, company_id, kind, url, status)
        VALUES ($1, $2, 'html', $3, 'approved')
        RETURNING id
        "#,
    )
    .bind(format!("quarantine-release-source-{}", &suffix[..10]))
    .bind(company_id)
    .bind(format!("https://{suffix}.example.test/news/"))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture source");
    let source = database
        .get_source(source_id)
        .await
        .expect("load fixture source")
        .expect("fixture source exists");

    let mut job_spec = JobSpec::new(
        JobType::CrawlSource,
        format!("quarantine-release:{suffix}"),
        Utc::now(),
    );
    job_spec.company_id = Some(company_id);
    job_spec.source_id = Some(source_id);
    let job = database
        .enqueue_job(&job_spec)
        .await
        .expect("enqueue fixture crawl");
    let canonical_url =
        Url::parse(&format!("https://{suffix}.example.test/news/update")).expect("fixture URL");
    let protected_url =
        Url::parse(&format!("https://{suffix}.example.test/legal/cookies")).expect("fixture URL");

    let first_run = database
        .begin_crawl_run(source_id, job.id)
        .await
        .expect("begin initial crawl");
    database
        .complete_crawl_run(
            first_run,
            &source,
            Utc::now(),
            &[
                processed_item(
                    &canonical_url,
                    "Press Releases",
                    &format!("sha256:first:{suffix}"),
                ),
                processed_item(
                    &protected_url,
                    "Cookies Policy",
                    &format!("sha256:protected-first:{suffix}"),
                ),
            ],
            json!({}),
        )
        .await
        .expect("persist initial item");

    let feed_item_id: Uuid = sqlx::query_scalar(
        r#"
        UPDATE feed_items
        SET
            is_private = true,
            content_processing = jsonb_build_object(
                'quality_quarantine',
                jsonb_build_object(
                    'policy', 'recipe-listing-artifact.v2',
                    'reason', 'generic_listing_title'
                )
            )
        WHERE source_id = $1 AND canonical_url = $2
        RETURNING id
        "#,
    )
    .bind(source_id)
    .bind(canonical_url.as_str())
    .fetch_one(database.pool())
    .await
    .expect("quarantine fixture item");
    let protected_feed_item_id: Uuid = sqlx::query_scalar(
        r#"
        UPDATE feed_items
        SET
            is_private = true,
            content_processing = jsonb_build_object(
                'quality_quarantine',
                jsonb_build_object(
                    'policy', 'non-editorial-utility-item.v1',
                    'reason', 'non_editorial_utility_item'
                )
            )
        WHERE source_id = $1 AND canonical_url = $2
        RETURNING id
        "#,
    )
    .bind(source_id)
    .bind(protected_url.as_str())
    .fetch_one(database.pool())
    .await
    .expect("quarantine protected fixture item");

    let second_run = database
        .begin_crawl_run(source_id, job.id)
        .await
        .expect("begin corrected crawl");
    database
        .complete_crawl_run(
            second_run,
            &source,
            Utc::now(),
            &[
                processed_item(
                    &canonical_url,
                    "Acme launches a corrected product",
                    &format!("sha256:corrected:{suffix}"),
                ),
                processed_item(
                    &protected_url,
                    "Cookies Policy opens in new window",
                    &format!("sha256:protected-corrected:{suffix}"),
                ),
            ],
            json!({}),
        )
        .await
        .expect("persist corrected item");

    let (is_private, title): (bool, String) =
        sqlx::query_as("SELECT is_private, title FROM feed_items WHERE id = $1")
            .bind(feed_item_id)
            .fetch_one(database.pool())
            .await
            .expect("read corrected item");
    assert!(!is_private);
    assert_eq!(title, "Acme launches a corrected product");
    let (protected_is_private, protected_title, protected_policy): (bool, String, String) =
        sqlx::query_as(
            r#"
            SELECT
                is_private,
                title,
                content_processing -> 'quality_quarantine' ->> 'policy'
            FROM feed_items
            WHERE id = $1
            "#,
        )
        .bind(protected_feed_item_id)
        .fetch_one(database.pool())
        .await
        .expect("read protected item");
    assert!(protected_is_private);
    assert_eq!(protected_title, "Cookies Policy opens in new window");
    assert_eq!(protected_policy, "non-editorial-utility-item.v1");
    let release_policies: Vec<String> = sqlx::query_scalar::<_, String>(
        r#"
        SELECT payload ->> 'policy'
        FROM event_log
        WHERE event_type = 'feed_item.quality_released'
          AND payload ->> 'feed_item_id' = $1
        "#,
    )
    .bind(feed_item_id.to_string())
    .fetch_all(database.pool())
    .await
    .expect("list release event policies");
    assert_eq!(release_policies, vec!["recipe-listing-artifact.v2"]);

    sqlx::query("DELETE FROM event_log WHERE company_id = $1 OR source_id = $2")
        .bind(company_id)
        .bind(source_id)
        .execute(database.pool())
        .await
        .expect("delete fixture audit events");
    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(database.pool())
        .await
        .expect("delete quarantine release fixture");
}

#[tokio::test]
async fn canonical_owner_wins_when_legacy_external_ids_collapse() {
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
        VALUES ($1, 'Canonical Collapse Fixture', 'private', 'active', 3600)
        RETURNING id
        "#,
    )
    .bind(format!("canonical-collapse-{}", &suffix[..10]))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture company");
    let source_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (source_id, company_id, kind, url, status)
        VALUES ($1, $2, 'html', $3, 'approved')
        RETURNING id
        "#,
    )
    .bind(format!("canonical-collapse-source-{}", &suffix[..10]))
    .bind(company_id)
    .bind(format!("https://{suffix}.example.test/news"))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture source");
    let source = database
        .get_source(source_id)
        .await
        .expect("load fixture source")
        .expect("fixture source exists");
    let mut job_spec = JobSpec::new(
        JobType::CrawlSource,
        format!("canonical-collapse:{suffix}"),
        Utc::now(),
    );
    job_spec.company_id = Some(company_id);
    job_spec.source_id = Some(source_id);
    let job = database
        .enqueue_job(&job_spec)
        .await
        .expect("enqueue fixture crawl");

    let canonical_url =
        Url::parse(&format!("https://{suffix}.example.test/news")).expect("canonical URL");
    let first_variant =
        Url::parse(&format!("{canonical_url}?category=1")).expect("first variant URL");
    let second_variant =
        Url::parse(&format!("{canonical_url}?category=2")).expect("second variant URL");
    let initial_run = database
        .begin_crawl_run(source_id, job.id)
        .await
        .expect("begin legacy crawl");
    database
        .complete_crawl_run(
            initial_run,
            &source,
            Utc::now(),
            &[
                processed_item(
                    &first_variant,
                    "First legacy filter",
                    &format!("sha256:first:{suffix}"),
                ),
                processed_item(
                    &second_variant,
                    "Second legacy filter",
                    &format!("sha256:second:{suffix}"),
                ),
            ],
            json!({}),
        )
        .await
        .expect("persist legacy query identities");
    sqlx::query(
        r#"
        UPDATE feed_items
        SET
            is_private = true,
            content_processing = jsonb_build_object(
                'quality_quarantine',
                jsonb_build_object(
                    'policy', 'recipe-listing-artifact.v15',
                    'reason', 'taxonomy_filter_query'
                )
            )
        WHERE source_id = $1
        "#,
    )
    .bind(source_id)
    .execute(database.pool())
    .await
    .expect("quarantine legacy variants");

    let corrected_run = database
        .begin_crawl_run(source_id, job.id)
        .await
        .expect("begin corrected crawl");
    let summary = database
        .complete_crawl_run(
            corrected_run,
            &source,
            Utc::now(),
            &[
                processed_item_with_canonical(
                    &first_variant,
                    &canonical_url,
                    "Corrected canonical article",
                    &format!("sha256:corrected-first:{suffix}"),
                ),
                processed_item_with_canonical(
                    &second_variant,
                    &canonical_url,
                    "Corrected canonical article",
                    &format!("sha256:corrected-second:{suffix}"),
                ),
            ],
            json!({}),
        )
        .await
        .expect("converge legacy external IDs without a unique-key collision");
    assert_eq!(summary.normalized_item_count, 2);
    assert_eq!(summary.new_item_count, 0);

    let (total_items, public_items, canonical_owners): (i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT
            count(*),
            count(*) FILTER (WHERE NOT is_private),
            count(*) FILTER (WHERE canonical_url = $2)
        FROM feed_items
        WHERE source_id = $1
        "#,
    )
    .bind(source_id)
    .bind(canonical_url.as_str())
    .fetch_one(database.pool())
    .await
    .expect("inspect converged identities");
    assert_eq!(total_items, 2);
    assert_eq!(public_items, 1);
    assert_eq!(canonical_owners, 1);

    sqlx::query("DELETE FROM event_log WHERE company_id = $1 OR source_id = $2")
        .bind(company_id)
        .bind(source_id)
        .execute(database.pool())
        .await
        .expect("delete fixture audit events");
    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(database.pool())
        .await
        .expect("delete canonical collapse fixture");
}

#[tokio::test]
async fn public_url_identity_survives_cms_canonical_host_drift() {
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
        VALUES ($1, 'Canonical Host Drift Fixture', 'private', 'active', 3600)
        RETURNING id
        "#,
    )
    .bind(format!("canonical-host-drift-{}", &suffix[..10]))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture company");
    let source_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (source_id, company_id, kind, url, status)
        VALUES ($1, $2, 'html', $3, 'approved')
        RETURNING id
        "#,
    )
    .bind(format!("canonical-host-drift-source-{}", &suffix[..10]))
    .bind(company_id)
    .bind(format!("https://{suffix}.example.test/news"))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture source");
    let source = database
        .get_source(source_id)
        .await
        .expect("load fixture source")
        .expect("fixture source exists");
    let mut job_spec = JobSpec::new(
        JobType::CrawlSource,
        format!("canonical-host-drift:{suffix}"),
        Utc::now(),
    );
    job_spec.company_id = Some(company_id);
    job_spec.source_id = Some(source_id);
    let job = database
        .enqueue_job(&job_spec)
        .await
        .expect("enqueue fixture crawl");

    let public_url = Url::parse(&format!(
        "https://{suffix}.example.test/news/product-launch"
    ))
    .expect("public URL");
    let origin_url = Url::parse(&format!(
        "https://origin-{suffix}.example.test/news/product-launch"
    ))
    .expect("origin canonical URL");
    let first_run = database
        .begin_crawl_run(source_id, job.id)
        .await
        .expect("begin origin-canonical crawl");
    database
        .complete_crawl_run(
            first_run,
            &source,
            Utc::now(),
            &[processed_item_with_identity(
                &public_url,
                &origin_url,
                &origin_url,
                "Product launch from origin canonical",
                &format!("sha256:origin:{suffix}"),
            )],
            json!({}),
        )
        .await
        .expect("persist origin-canonical item");

    let second_run = database
        .begin_crawl_run(source_id, job.id)
        .await
        .expect("begin public-canonical crawl");
    let summary = database
        .complete_crawl_run(
            second_run,
            &source,
            Utc::now(),
            &[processed_item_with_identity(
                &public_url,
                &public_url,
                &public_url,
                "Product launch from public canonical",
                &format!("sha256:public:{suffix}"),
            )],
            json!({}),
        )
        .await
        .expect("converge canonical-host drift by public URL identity");
    assert_eq!(summary.normalized_item_count, 1);
    assert_eq!(summary.new_item_count, 0);

    let (total_items, public_items, title): (i64, i64, String) = sqlx::query_as(
        r#"
        SELECT
            count(*),
            count(*) FILTER (WHERE NOT is_private),
            max(title)
        FROM feed_items
        WHERE source_id = $1
        "#,
    )
    .bind(source_id)
    .fetch_one(database.pool())
    .await
    .expect("inspect converged public URL identity");
    assert_eq!(total_items, 1);
    assert_eq!(public_items, 1);
    assert_eq!(title, "Product launch from public canonical");

    sqlx::query("DELETE FROM event_log WHERE company_id = $1 OR source_id = $2")
        .bind(company_id)
        .bind(source_id)
        .execute(database.pool())
        .await
        .expect("delete fixture audit events");
    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(database.pool())
        .await
        .expect("delete canonical host drift fixture");
}

#[tokio::test]
async fn failed_source_attempt_waits_for_its_crawl_interval_before_rescheduling() {
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
        VALUES ($1, 'Failed Crawl Cadence Fixture', 'private', 'active', 3600)
        RETURNING id
        "#,
    )
    .bind(format!("failed-crawl-cadence-{}", &suffix[..10]))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture company");
    let source_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (
            source_id, company_id, kind, url, status, freshness_slo_seconds
        )
        VALUES ($1, $2, 'rss', $3, 'approved', 3600)
        RETURNING id
        "#,
    )
    .bind(format!("failed-crawl-cadence-source-{}", &suffix[..10]))
    .bind(company_id)
    .bind(format!("https://{suffix}.example.test/feed.xml"))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture source");
    let now = Utc::now();
    sqlx::query(
        r#"
        INSERT INTO source_state (source_id, last_attempt_at, last_success_at)
        VALUES (
            $1,
            $2,
            $2 - INTERVAL '2 hours'
        )
        "#,
    )
    .bind(source_id)
    .bind(now)
    .execute(database.pool())
    .await
    .expect("insert recent failed source attempt");

    database
        .enqueue_due_crawl_jobs(now)
        .await
        .expect("schedule due crawl jobs");
    let premature_jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE source_id = $1")
        .bind(source_id)
        .fetch_one(database.pool())
        .await
        .expect("count premature crawl jobs");
    assert_eq!(premature_jobs, 0);

    sqlx::query(
        r#"
        UPDATE source_state
        SET last_attempt_at = $2 - INTERVAL '2 hours'
        WHERE source_id = $1
        "#,
    )
    .bind(source_id)
    .bind(now)
    .execute(database.pool())
    .await
    .expect("age failed source attempt");
    database
        .enqueue_due_crawl_jobs(now)
        .await
        .expect("schedule aged failed source");
    let scheduled_jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE source_id = $1")
        .bind(source_id)
        .fetch_one(database.pool())
        .await
        .expect("count scheduled crawl jobs");
    assert_eq!(scheduled_jobs, 1);

    sqlx::query("DELETE FROM jobs WHERE source_id = $1")
        .bind(source_id)
        .execute(database.pool())
        .await
        .expect("delete fixture crawl job");
    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(database.pool())
        .await
        .expect("delete failed crawl cadence fixture");
}

fn processed_item(url: &Url, title: &str, content_hash: &str) -> ProcessedCrawlItem {
    let fetched_at = Utc::now();
    ProcessedCrawlItem {
        raw: RawCrawlItem {
            source_item_key: url.as_str().to_owned(),
            external_id: Some(url.as_str().to_owned()),
            url: url.clone(),
            canonical_url: Some(url.clone()),
            title: Some(title.to_owned()),
            summary_html: None,
            body_html: Some("<p>Substantive corrected fixture body.</p>".to_owned()),
            published_at: None,
            payload: json!({}),
        },
        normalized: Ok(NormalizedFeedItem {
            external_id: url.as_str().to_owned(),
            url: url.clone(),
            canonical_url: url.clone(),
            title: title.to_owned(),
            summary: String::new(),
            body_text: "Substantive corrected fixture body.".to_owned(),
            body_html: "<p>Substantive corrected fixture body.</p>".to_owned(),
            body_markdown: "Substantive corrected fixture body.".to_owned(),
            published_at: None,
            fetched_at,
            content_hash: content_hash.to_owned(),
            source_kind: SourceKind::Html,
            raw: json!({}),
            normalized: json!({}),
            content_processing: json!({ "contract": "test.v1" }),
        }),
    }
}

fn processed_item_with_canonical(
    url: &Url,
    canonical_url: &Url,
    title: &str,
    content_hash: &str,
) -> ProcessedCrawlItem {
    let mut processed = processed_item(url, title, content_hash);
    let normalized = processed
        .normalized
        .as_mut()
        .expect("fixture normalization succeeds");
    normalized.canonical_url = canonical_url.clone();
    processed
}

fn processed_item_with_identity(
    url: &Url,
    external_id: &Url,
    canonical_url: &Url,
    title: &str,
    content_hash: &str,
) -> ProcessedCrawlItem {
    let mut processed = processed_item(url, title, content_hash);
    processed.raw.external_id = Some(external_id.as_str().to_owned());
    processed.raw.canonical_url = Some(canonical_url.clone());
    let normalized = processed
        .normalized
        .as_mut()
        .expect("fixture normalization succeeds");
    normalized.external_id = external_id.as_str().to_owned();
    normalized.canonical_url = canonical_url.clone();
    processed
}
