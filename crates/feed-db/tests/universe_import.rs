#![cfg(feature = "postgres-tests")]

use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use feed_db::{Database, UniverseImportOptions, UniverseWaveOptions};
use feed_universe::{COMPANY_UNIVERSE_SCHEMA_VERSION, ValidatedUniverse};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn imports_idempotently_and_releases_only_an_explicit_wave() {
    let database_url =
        std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required for this test");
    let database = Database::connect(&database_url, 5)
        .await
        .expect("connect to test database");
    database.migrate().await.expect("run migrations");

    let suffix = &Uuid::new_v4().simple().to_string()[..10];
    let company_key_low = format!("universe-low-{suffix}");
    let company_key_high = format!("universe-high-{suffix}");
    let ticker_high = format!("U{suffix}B").to_ascii_uppercase();
    let input = serde_json::to_vec(&json!({
        "schema_version": COMPANY_UNIVERSE_SCHEMA_VERSION,
        "source": {
            "name": "postgres-test",
            "revision": suffix,
            "generated_at": Utc::now(),
            "metadata": {"selection_policy": "fixture"}
        },
        "companies": [
            {
                "source_company_id": format!("private-{suffix}"),
                "company_key": company_key_low,
                "name": "Universe Low Corp",
                "aliases": ["Universe Low"],
                "ownership_status": "private",
                "lifecycle_status": "active",
                "listings": [],
                "market_cap": 10000000,
                "country": "United States",
                "ipo_year": 2020,
                "sector": "Technology",
                "industry": "Software",
                "identifiers": {},
                "homepage_url": null,
                "investor_relations_url": null,
                "newsroom_url": null,
                "blog_url": null,
                "hints": [],
                "metadata": {}
            },
            {
                "source_company_id": format!("public-{suffix}"),
                "company_key": company_key_high,
                "name": "Universe High Corp",
                "aliases": [],
                "ownership_status": "public",
                "lifecycle_status": "active",
                "listings": [{
                    "ticker": ticker_high,
                    "exchange": "NYSE",
                    "is_primary": true,
                    "metadata": {}
                }],
                "market_cap": 20000000,
                "country": "United States",
                "ipo_year": 2010,
                "sector": "Industrials",
                "industry": "Machinery",
                "identifiers": {},
                "homepage_url": null,
                "investor_relations_url": null,
                "newsroom_url": null,
                "blog_url": null,
                "hints": [],
                "metadata": {}
            }
        ]
    }))
    .expect("serialize universe");
    let universe = ValidatedUniverse::parse(&input).expect("validate universe");
    let options = UniverseImportOptions {
        activate_new: false,
        discovery_cadence_seconds: 604_800,
    };

    let first = database
        .import_company_universe(&universe, options)
        .await
        .expect("import universe");
    assert_eq!(first.inserted_rows, 2);
    assert_eq!(first.updated_rows, 0);
    assert_eq!(first.unchanged_rows, 0);
    assert!(!first.idempotent_replay);

    let replay = database
        .import_company_universe(&universe, options)
        .await
        .expect("replay universe");
    assert_eq!(replay.import_run_id, first.import_run_id);
    assert!(replay.idempotent_replay);

    let staged: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM companies WHERE company_key = ANY($1) AND NOT discovery_enabled",
    )
    .bind(vec![company_key_low.clone(), company_key_high.clone()])
    .fetch_one(database.pool())
    .await
    .expect("count staged companies");
    assert_eq!(staged, 2);

    let private_listing_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM company_listings WHERE company_id = (SELECT id FROM companies WHERE company_key = $1)",
    )
    .bind(&company_key_low)
    .fetch_one(database.pool())
    .await
    .expect("count private company listings");
    assert_eq!(private_listing_count, 0);

    sqlx::query("UPDATE companies SET name = $1, name_source = 'operator' WHERE company_key = $2")
        .bind("Operator Curated Low")
        .bind(&company_key_low)
        .execute(database.pool())
        .await
        .expect("curate one imported company name");
    let mut refresh_document: serde_json::Value =
        serde_json::from_slice(&input).expect("parse universe for refresh");
    refresh_document["source"]["revision"] = json!(format!("{suffix}-refresh"));
    refresh_document["companies"][0]["name"] = json!("Universe Low Renamed");
    refresh_document["companies"][1]["name"] = json!("Universe High Renamed");
    let refresh_input = serde_json::to_vec(&refresh_document).expect("serialize universe refresh");
    let refresh_universe =
        ValidatedUniverse::parse(&refresh_input).expect("validate universe refresh");
    let refresh = database
        .import_company_universe(&refresh_universe, options)
        .await
        .expect("refresh universe");
    assert_eq!(refresh.updated_rows, 2);
    let names: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT company_key, name, name_source FROM companies WHERE company_key = ANY($1) ORDER BY company_key",
    )
    .bind(vec![company_key_low.clone(), company_key_high.clone()])
    .fetch_all(database.pool())
    .await
    .expect("load refreshed names");
    assert_eq!(
        names,
        vec![
            (
                company_key_high.clone(),
                "Universe High Renamed".to_owned(),
                "universe".to_owned()
            ),
            (
                company_key_low.clone(),
                "Operator Curated Low".to_owned(),
                "operator".to_owned()
            )
        ]
    );

    database
        .enqueue_due_discovery_jobs(Utc::now(), 100)
        .await
        .expect("schedule due discovery jobs");
    let jobs_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE company_id IN (SELECT id FROM companies WHERE company_key = ANY($1))")
            .bind(vec![company_key_low.clone(), company_key_high.clone()])
            .fetch_one(database.pool())
            .await
            .expect("count pre-activation jobs");
    assert_eq!(jobs_before, 0);

    let wave = database
        .activate_company_wave(UniverseWaveOptions {
            import_run_id: Some(first.import_run_id),
            limit: 1,
            start_at: Utc::now() - ChronoDuration::seconds(1),
            spacing: Duration::ZERO,
        })
        .await
        .expect("activate wave");
    assert_eq!(wave.activated.len(), 1);
    assert_eq!(wave.activated[0].company_key, company_key_high);

    database
        .enqueue_due_discovery_jobs(Utc::now(), 100)
        .await
        .expect("schedule activated company");
    let selected_jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE company_id = $1 AND job_type = 'discover_company'",
    )
    .bind(wave.activated[0].company_id)
    .fetch_one(database.pool())
    .await
    .expect("count activated jobs");
    assert_eq!(selected_jobs, 1);

    let remaining_staged: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM companies WHERE company_key = ANY($1) AND NOT discovery_enabled",
    )
    .bind(vec![company_key_low.clone(), company_key_high.clone()])
    .fetch_one(database.pool())
    .await
    .expect("count remaining staged companies");
    assert_eq!(remaining_staged, 1);

    let remaining_company_id: Uuid =
        sqlx::query_scalar("SELECT id FROM companies WHERE company_key = $1")
            .bind(&company_key_low)
            .fetch_one(database.pool())
            .await
            .expect("load remaining staged company");
    sqlx::query(
        r#"
        INSERT INTO jobs (job_type, job_key, status, run_after, company_id, payload)
        VALUES ('discover_company', $1, 'pending', now(), $2, jsonb_build_object('company_id', $2))
        "#,
    )
    .bind(format!("queue-cap-test:{suffix}"))
    .bind(remaining_company_id)
    .execute(database.pool())
    .await
    .expect("insert a second active discovery job");
    let capped = database
        .enqueue_due_discovery_jobs(Utc::now(), 1)
        .await
        .expect("an over-target queue must not use a negative SQL limit");
    assert_eq!(capped, 0);

    let mut cleanup = database.pool().begin().await.expect("begin cleanup");
    sqlx::query("DELETE FROM company_import_runs WHERE id = ANY($1)")
        .bind(vec![first.import_run_id, refresh.import_run_id])
        .execute(&mut *cleanup)
        .await
        .expect("delete import run");
    sqlx::query("DELETE FROM companies WHERE company_key = ANY($1)")
        .bind(vec![company_key_low, company_key_high])
        .execute(&mut *cleanup)
        .await
        .expect("delete imported companies");
    cleanup.commit().await.expect("commit cleanup");
    database.close().await;
}
