#![cfg(feature = "postgres-tests")]

use chrono::Utc;
use feed_db::Database;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn approved_public_export_scope_deduplicates_and_prefers_hydrated_content() {
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
            discovery_cadence_seconds, metadata
        )
        VALUES ($1, $2, 'private', 'active', 3600, $3)
        RETURNING id
        "#,
    )
    .bind(format!("export-selection-{}", &suffix[..12]))
    .bind(format!("Export Selection Fixture {suffix}"))
    .bind(json!({"universe": {"sector": "Consumer Cyclical"}}))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture company");

    let first_source: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (
            source_id, company_id, kind, url, status, public_export_allowed
        )
        VALUES ($1, $2, 'rss', $3, 'approved', false)
        RETURNING id
        "#,
    )
    .bind(format!("export-selection-rss-{}", &suffix[..12]))
    .bind(company_id)
    .bind(format!("https://{suffix}.example.test/feed.xml"))
    .fetch_one(database.pool())
    .await
    .expect("insert RSS source");
    let second_source: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (
            source_id, company_id, kind, url, status, public_export_allowed
        )
        VALUES ($1, $2, 'atom', $3, 'approved', false)
        RETURNING id
        "#,
    )
    .bind(format!("export-selection-atom-{}", &suffix[..12]))
    .bind(company_id)
    .bind(format!("https://{suffix}.example.test/atom.xml"))
    .fetch_one(database.pool())
    .await
    .expect("insert Atom source");

    let published_at = Utc::now() - chrono::Duration::days(1);
    let first_item: Uuid = insert_item(
        &database,
        company_id,
        first_source,
        "rss",
        &format!("https://www.{suffix}.example.test/news/launch"),
        "Short excerpt",
        published_at,
        &suffix,
    )
    .await;
    let hydrated_body = "Independently hydrated article body. ".repeat(20);
    let second_item: Uuid = insert_item(
        &database,
        company_id,
        second_source,
        "atom",
        &format!("https://{suffix}.example.test/news/launch/"),
        &hydrated_body,
        published_at,
        &suffix,
    )
    .await;
    sqlx::query(
        r#"
        INSERT INTO content_crawl_state (
            feed_item_id, status, last_attempt_at, last_success_at,
            next_attempt_at, attempt_count, consecutive_failures,
            content_chars, extraction_version
        )
        VALUES (
            $1, 'succeeded', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP,
            CURRENT_TIMESTAMP + INTERVAL '30 days', 1, 0, $2,
            'generic-public-article.v1'
        )
        "#,
    )
    .bind(second_item)
    .bind(i32::try_from(hydrated_body.len()).expect("fixture length fits"))
    .execute(database.pool())
    .await
    .expect("mark second item hydrated");

    let approved_public_target: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO export_targets (
            target_id, repo_url, local_path, branch, format, layout,
            cadence_seconds, enabled, push_enabled, metadata
        )
        VALUES (
            $1, 'https://example.test/archive.git', '/tmp/archive', 'main',
            'markdown_json', 'by_company_date', 3600, true, false, $2
        )
        RETURNING id
        "#,
    )
    .bind(format!("approved-public-{}", &suffix[..12]))
    .bind(json!({"publication_scope": "approved_public"}))
    .fetch_one(database.pool())
    .await
    .expect("insert approved-public target");
    let explicit_target: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO export_targets (
            target_id, repo_url, local_path, branch, format, layout,
            cadence_seconds, enabled, push_enabled, metadata
        )
        VALUES (
            $1, 'https://example.test/explicit.git', '/tmp/explicit', 'main',
            'markdown_json', 'by_company_date', 3600, true, false, '{}'
        )
        RETURNING id
        "#,
    )
    .bind(format!("explicit-only-{}", &suffix[..12]))
    .fetch_one(database.pool())
    .await
    .expect("insert explicit-only target");

    let selected = database
        .list_exportable_feed_items(approved_public_target)
        .await
        .expect("select approved public archive records");
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].item.id, second_item);
    assert_eq!(selected[0].item.body_text, hydrated_body);
    assert!(
        selected[0]
            .company_category_key
            .starts_with("consumer-cyclical-")
    );
    assert_eq!(selected[0].company_category_name, "Consumer Cyclical");
    assert_ne!(selected[0].item.id, first_item);

    let explicit = database
        .list_exportable_feed_items(explicit_target)
        .await
        .expect("select explicit archive records");
    assert!(explicit.is_empty());

    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(database.pool())
        .await
        .expect("delete export selection fixture company");
    sqlx::query("DELETE FROM export_targets WHERE id = ANY($1::uuid[])")
        .bind([approved_public_target, explicit_target])
        .execute(database.pool())
        .await
        .expect("delete export selection fixture targets");
}

#[allow(clippy::too_many_arguments)]
async fn insert_item(
    database: &Database,
    company_id: Uuid,
    source_id: Uuid,
    source_kind: &str,
    url: &str,
    body_text: &str,
    published_at: chrono::DateTime<Utc>,
    suffix: &str,
) -> Uuid {
    sqlx::query_scalar(
        r#"
        INSERT INTO feed_items (
            company_id, source_id, external_id, url, canonical_url,
            title, summary, body_text, body_html, body_markdown,
            published_at, fetched_at, content_hash, source_kind
        )
        VALUES (
            $1, $2, $3, $3, $3,
            'Same launch title', '', $4, $5, $4,
            $6, CURRENT_TIMESTAMP, $7, $8
        )
        RETURNING id
        "#,
    )
    .bind(company_id)
    .bind(source_id)
    .bind(url)
    .bind(body_text)
    .bind(format!("<p>{body_text}</p>"))
    .bind(published_at)
    .bind(format!("sha256:{suffix}:{source_id}"))
    .bind(source_kind)
    .fetch_one(database.pool())
    .await
    .expect("insert export selection item")
}
