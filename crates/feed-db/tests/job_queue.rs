#![cfg(feature = "postgres-tests")]

use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use feed_core::{DiscoveredSource, JobSpec, JobStatus, JobType, SourceKind};
use feed_db::{Database, JobFailureOutcome};
use tokio::sync::Mutex;
use url::Url;
use uuid::Uuid;

static JOB_QUEUE_TEST_LOCK: Mutex<()> = Mutex::const_new(());

#[tokio::test]
async fn transient_news_retry_selection_uses_latest_attempt_and_feed_policy() {
    let _guard = JOB_QUEUE_TEST_LOCK.lock().await;

    let Some(database_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL is not set; skipping Postgres integration test");
        return;
    };
    let database = Database::connect(&database_url, 5)
        .await
        .expect("connect to test Postgres");
    database.migrate().await.expect("run migrations");

    let suffix = Uuid::new_v4().simple().to_string();
    let retry_after = Utc::now() + ChronoDuration::days(1);
    let window_start = retry_after - ChronoDuration::days(31);
    let window_end = retry_after + ChronoDuration::minutes(1);
    let mut company_ids = Vec::new();
    for (index, label) in ["recipe", "feed", "latest", "permanent", "failed"]
        .into_iter()
        .enumerate()
    {
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
        .bind(format!(
            "transient-retry-{label}-{}",
            &suffix[index..index + 10]
        ))
        .bind(format!("Transient Retry {label} {suffix}"))
        .fetch_one(database.pool())
        .await
        .expect("insert transient retry company");
        company_ids.push(company_id);
    }
    let [
        recipe_company,
        feed_company,
        latest_company,
        permanent_company,
        failed_company,
    ] = company_ids.as_slice()
    else {
        panic!("five company fixtures");
    };

    let feed_source_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (source_id, company_id, kind, url, status)
        VALUES ($1, $2, 'rss', $3, 'approved')
        RETURNING id
        "#,
    )
    .bind(format!("transient-retry-feed-{}", &suffix[..10]))
    .bind(feed_company)
    .bind(format!("https://{suffix}.example.test/feed.xml"))
    .fetch_one(database.pool())
    .await
    .expect("insert approved feed fixture");
    sqlx::query(
        r#"
        INSERT INTO source_state (
            source_id, last_attempt_at, last_success_at,
            consecutive_failures, consecutive_zero_runs,
            total_successful_runs
        )
        VALUES ($1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, 0, 0, 1)
        "#,
    )
    .bind(feed_source_id)
    .execute(database.pool())
    .await
    .expect("mark approved feed fixture healthy");

    let attempts = [
        (
            *recipe_company,
            "completed",
            serde_json::json!({
                "recipe_builds": [{
                    "outcome": "crawl_failed",
                    "retryable": true,
                    "error": "request timed out"
                }]
            }),
            retry_after + ChronoDuration::seconds(1),
        ),
        (
            *feed_company,
            "completed",
            serde_json::json!({
                "failures": [{
                    "url": "https://example.test/article",
                    "retryable": true,
                    "reason": "request"
                }]
            }),
            retry_after + ChronoDuration::seconds(2),
        ),
        (
            *latest_company,
            "completed",
            serde_json::json!({
                "recipe_builds": [{
                    "outcome": "correctness_failed",
                    "failure_diagnostics": {"retryable_failure_count": 2}
                }]
            }),
            retry_after + ChronoDuration::seconds(3),
        ),
        (
            *latest_company,
            "completed",
            serde_json::json!({"outcome": "completed", "failures": []}),
            retry_after + ChronoDuration::seconds(4),
        ),
        (
            *permanent_company,
            "completed",
            serde_json::json!({
                "recipe_builds": [{
                    "outcome": "crawl_failed",
                    "retryable": false,
                    "error": "HTTP 404 Not Found"
                }]
            }),
            retry_after + ChronoDuration::seconds(5),
        ),
        (
            *failed_company,
            "failed",
            serde_json::json!({"stage": "url_suggestion", "retryable": true}),
            retry_after + ChronoDuration::seconds(6),
        ),
    ];
    for (company_id, status, metadata, started_at) in attempts {
        sqlx::query(
            r#"
            INSERT INTO company_news_extraction_runs (
                company_id, window_start, window_end, started_at, finished_at,
                status, metadata
            )
            VALUES ($1, $2, $3, $4, $4, $5, $6)
            "#,
        )
        .bind(company_id)
        .bind(window_start)
        .bind(window_end)
        .bind(started_at)
        .bind(status)
        .bind(metadata)
        .execute(database.pool())
        .await
        .expect("insert extraction attempt fixture");
    }

    let including_feeds = database
        .list_company_ids_needing_transient_news_retry(retry_after, true, 100, 0)
        .await
        .expect("list transient retries including feeds");
    assert_eq!(
        database
            .count_companies_needing_transient_news_retry(retry_after, true)
            .await
            .expect("count transient retries including feeds"),
        3
    );
    assert!(including_feeds.contains(recipe_company));
    assert!(including_feeds.contains(feed_company));
    assert!(including_feeds.contains(failed_company));
    assert!(!including_feeds.contains(latest_company));
    assert!(!including_feeds.contains(permanent_company));

    let excluding_feeds = database
        .list_company_ids_needing_transient_news_retry(retry_after, false, 100, 0)
        .await
        .expect("list transient retries excluding feeds");
    assert_eq!(excluding_feeds.len(), 2);
    assert!(excluding_feeds.contains(recipe_company));
    assert!(excluding_feeds.contains(failed_company));
    assert!(!excluding_feeds.contains(feed_company));

    let mut active_job = JobSpec::new(
        JobType::ExtractCompanyNews,
        format!("transient-retry-active-{suffix}"),
        Utc::now(),
    );
    active_job.company_id = Some(*recipe_company);
    let active_job = database
        .enqueue_job(&active_job)
        .await
        .expect("enqueue active retry fixture");
    let while_active = database
        .list_company_ids_needing_transient_news_retry(retry_after, true, 100, 0)
        .await
        .expect("list transient retries while one company has an active job");
    assert!(!while_active.contains(recipe_company));
    assert!(while_active.contains(feed_company));
    assert!(while_active.contains(failed_company));

    sqlx::query("DELETE FROM jobs WHERE id = $1")
        .bind(active_job.id)
        .execute(database.pool())
        .await
        .expect("delete active job fixture");
    sqlx::query("DELETE FROM companies WHERE id = ANY($1::uuid[])")
        .bind(&company_ids)
        .execute(database.pool())
        .await
        .expect("delete transient retry companies");
}

#[tokio::test]
async fn unhealthy_approved_feeds_do_not_suppress_recipe_recovery() {
    let _guard = JOB_QUEUE_TEST_LOCK.lock().await;

    let Some(database_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL is not set; skipping Postgres integration test");
        return;
    };
    let database = Database::connect(&database_url, 5)
        .await
        .expect("connect to test Postgres");
    database.migrate().await.expect("run migrations");

    let coverage_before = database
        .get_company_news_recipe_coverage()
        .await
        .expect("read baseline recipe coverage");
    let suffix = Uuid::new_v4().simple().to_string();
    let mut company_ids = Vec::new();
    let mut source_ids = Vec::new();
    for (index, label) in ["healthy", "failing", "empty"].into_iter().enumerate() {
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
        .bind(format!(
            "feed-health-{label}-{}",
            &suffix[index..index + 10]
        ))
        .bind(format!("Feed Health {label} {suffix}"))
        .fetch_one(database.pool())
        .await
        .expect("insert feed-health company");
        let source_id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO sources (source_id, company_id, kind, url, status)
            VALUES ($1, $2, 'rss', $3, 'approved')
            RETURNING id
            "#,
        )
        .bind(format!(
            "feed-health-{label}-{}",
            &suffix[index..index + 10]
        ))
        .bind(company_id)
        .bind(format!("https://{label}.{suffix}.example.test/feed.xml"))
        .fetch_one(database.pool())
        .await
        .expect("insert feed-health source");
        company_ids.push(company_id);
        source_ids.push(source_id);
    }
    let [healthy_company, failing_company, empty_company] = company_ids.as_slice() else {
        panic!("three feed-health companies");
    };
    let [healthy_source, failing_source, empty_source] = source_ids.as_slice() else {
        panic!("three feed-health sources");
    };

    sqlx::query(
        r#"
        INSERT INTO source_state (
            source_id, last_attempt_at, last_success_at, last_error,
            consecutive_failures, consecutive_zero_runs,
            total_successful_runs, total_items, last_nonzero_at
        )
        VALUES
            ($1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, NULL, 0, 0, 1, 10, CURRENT_TIMESTAMP),
            ($2, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP - INTERVAL '1 day',
             'terminal fixture failure', 5, 0, 1, 10, CURRENT_TIMESTAMP - INTERVAL '1 day'),
            ($3, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, NULL, 0, 3, 3, 10,
             CURRENT_TIMESTAMP - INTERVAL '1 day')
        "#,
    )
    .bind(healthy_source)
    .bind(failing_source)
    .bind(empty_source)
    .execute(database.pool())
    .await
    .expect("insert feed-health state");

    for company_id in &company_ids {
        assert!(
            database
                .company_has_approved_feed(*company_id)
                .await
                .expect("check approved feed inventory")
        );
    }
    assert!(
        database
            .company_has_healthy_approved_feed(*healthy_company)
            .await
            .expect("check healthy approved feed")
    );
    for company_id in [failing_company, empty_company] {
        assert!(
            !database
                .company_has_healthy_approved_feed(*company_id)
                .await
                .expect("check unhealthy approved feed")
        );
    }

    let recovery_companies = database
        .list_company_ids_needing_news_recipes(false, 10_000, 0)
        .await
        .expect("list companies needing fallback recipes");
    assert!(!recovery_companies.contains(healthy_company));
    assert!(recovery_companies.contains(failing_company));
    assert!(recovery_companies.contains(empty_company));

    let coverage_after = database
        .get_company_news_recipe_coverage()
        .await
        .expect("read feed-health recipe coverage");
    assert_eq!(
        coverage_after.eligible_companies,
        coverage_before.eligible_companies + 3
    );
    assert_eq!(
        coverage_after.companies_with_approved_feed,
        coverage_before.companies_with_approved_feed + 3
    );
    assert_eq!(
        coverage_after.companies_with_healthy_feed,
        coverage_before.companies_with_healthy_feed + 1
    );
    assert_eq!(
        coverage_after.companies_covered_by_feed_or_recipe,
        coverage_before.companies_covered_by_feed_or_recipe + 1
    );
    assert_eq!(
        coverage_after.companies_missing_feed_or_recipe,
        coverage_before.companies_missing_feed_or_recipe + 2
    );

    sqlx::query("DELETE FROM companies WHERE id = ANY($1::uuid[])")
        .bind(&company_ids)
        .execute(database.pool())
        .await
        .expect("delete feed-health companies");
}

#[tokio::test]
async fn company_news_job_claims_are_database_serialized() {
    let _guard = JOB_QUEUE_TEST_LOCK.lock().await;

    let Some(database_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL is not set; skipping Postgres integration test");
        return;
    };
    let database = Database::connect(&database_url, 5)
        .await
        .expect("connect to test Postgres");
    database.migrate().await.expect("run migrations");

    let suffix = Uuid::new_v4();
    let first = database
        .enqueue_job(&JobSpec::new(
            JobType::ExtractCompanyNews,
            format!("sequential-company-news:{suffix}:one"),
            Utc::now(),
        ))
        .await
        .expect("enqueue first company-news job");
    let second = database
        .enqueue_job(&JobSpec::new(
            JobType::ExtractCompanyNews,
            format!("sequential-company-news:{suffix}:two"),
            Utc::now(),
        ))
        .await
        .expect("enqueue second company-news job");

    let first_claim = database
        .claim_job(
            "company-news-worker-one",
            Duration::from_secs(30),
            &[JobType::ExtractCompanyNews],
            1,
        )
        .await
        .expect("claim first company-news job")
        .expect("first company-news job is due");
    assert_eq!(first_claim.job.id, first.id);

    let overlapping_claim = database
        .claim_job(
            "company-news-worker-two",
            Duration::from_secs(30),
            &[JobType::ExtractCompanyNews],
            1,
        )
        .await
        .expect("attempt overlapping company-news claim");
    assert!(
        overlapping_claim.is_none(),
        "a second worker must not claim while one company-news job is running"
    );

    assert!(
        database
            .complete_job(first_claim.job.id, first_claim.lease_token)
            .await
            .expect("complete first company-news job")
    );
    let second_claim = database
        .claim_job(
            "company-news-worker-two",
            Duration::from_secs(30),
            &[JobType::ExtractCompanyNews],
            1,
        )
        .await
        .expect("claim second company-news job")
        .expect("second job becomes claimable after completion");
    assert_eq!(second_claim.job.id, second.id);
    assert!(
        database
            .complete_job(second_claim.job.id, second_claim.lease_token)
            .await
            .expect("complete second company-news job")
    );

    sqlx::query("DELETE FROM jobs WHERE id = ANY($1::uuid[])")
        .bind(vec![first.id, second.id])
        .execute(database.pool())
        .await
        .expect("delete sequential claim fixtures");
}

#[tokio::test]
async fn company_news_job_claims_honor_configured_pipeline_width() {
    let _guard = JOB_QUEUE_TEST_LOCK.lock().await;

    let Some(database_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL is not set; skipping Postgres integration test");
        return;
    };
    let database = Database::connect(&database_url, 5)
        .await
        .expect("connect to test Postgres");
    database.migrate().await.expect("run migrations");

    let suffix = Uuid::new_v4();
    let mut jobs = Vec::new();
    for lane in 1..=3 {
        jobs.push(
            database
                .enqueue_job(&JobSpec::new(
                    JobType::ExtractCompanyNews,
                    format!("pipelined-company-news:{suffix}:{lane}"),
                    Utc::now(),
                ))
                .await
                .expect("enqueue company-news job"),
        );
    }

    let first_claim = database
        .claim_job(
            "company-news-pipeline-one",
            Duration::from_secs(30),
            &[JobType::ExtractCompanyNews],
            2,
        )
        .await
        .expect("claim first pipeline job")
        .expect("first pipeline job is due");
    let second_claim = database
        .claim_job(
            "company-news-pipeline-two",
            Duration::from_secs(30),
            &[JobType::ExtractCompanyNews],
            2,
        )
        .await
        .expect("claim second pipeline job")
        .expect("second pipeline lane is available");
    assert_ne!(first_claim.job.id, second_claim.job.id);

    assert!(
        database
            .claim_job(
                "company-news-pipeline-three",
                Duration::from_secs(30),
                &[JobType::ExtractCompanyNews],
                2,
            )
            .await
            .expect("attempt claim beyond pipeline width")
            .is_none(),
        "a third job must wait while both configured lanes are occupied"
    );

    assert!(
        database
            .complete_job(first_claim.job.id, first_claim.lease_token)
            .await
            .expect("complete first pipeline job")
    );
    let third_claim = database
        .claim_job(
            "company-news-pipeline-three",
            Duration::from_secs(30),
            &[JobType::ExtractCompanyNews],
            2,
        )
        .await
        .expect("claim third pipeline job")
        .expect("completed lane becomes available");
    assert!(
        database
            .complete_job(second_claim.job.id, second_claim.lease_token)
            .await
            .expect("complete second pipeline job")
    );
    assert!(
        database
            .complete_job(third_claim.job.id, third_claim.lease_token)
            .await
            .expect("complete third pipeline job")
    );

    let job_ids = jobs.into_iter().map(|job| job.id).collect::<Vec<_>>();
    sqlx::query("DELETE FROM jobs WHERE id = ANY($1::uuid[])")
        .bind(job_ids)
        .execute(database.pool())
        .await
        .expect("delete pipeline claim fixtures");
}

#[tokio::test]
async fn recipe_seeded_feed_validation_can_replace_html_but_stops_after_a_feed() {
    let _guard = JOB_QUEUE_TEST_LOCK.lock().await;

    let Some(database_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL is not set; skipping Postgres integration test");
        return;
    };
    let database = Database::connect(&database_url, 5)
        .await
        .expect("connect to test Postgres");
    database.migrate().await.expect("run migrations");

    let suffix = Uuid::new_v4().simple().to_string();
    let company_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO companies (
            company_key, name, ownership_status, lifecycle_status,
            discovery_cadence_seconds
        )
        VALUES ($1, 'Recipe Seed Validation Fixture', 'private', 'active', 3600)
        RETURNING id
        "#,
    )
    .bind(format!("recipe-seed-validation-{}", &suffix[..10]))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture company");
    sqlx::query(
        r#"
        INSERT INTO sources (source_id, company_id, kind, url, status)
        VALUES ($1, $2, 'html', $3, 'approved')
        "#,
    )
    .bind(format!("recipe-seed-html-{}", &suffix[..10]))
    .bind(company_id)
    .bind(format!("https://{suffix}.example.test/blog"))
    .execute(database.pool())
    .await
    .expect("insert approved HTML fallback");

    let mut discovery_job = JobSpec::new(
        JobType::DiscoverCompany,
        format!("recipe-seed-discovery:{suffix}:one"),
        Utc::now(),
    );
    discovery_job.company_id = Some(company_id);
    discovery_job.payload = serde_json::json!({
        "seed_origin": "company_news_recipe_builder"
    });
    let discovery_job = database
        .enqueue_job(&discovery_job)
        .await
        .expect("enqueue seeded discovery");
    let discovery_run = database
        .begin_discovery_run(company_id, discovery_job.id)
        .await
        .expect("begin seeded discovery");
    let first_feed_url =
        Url::parse(&format!("https://{suffix}.example.test/feed")).expect("first feed URL");
    database
        .complete_discovery_run(
            discovery_run,
            &[DiscoveredSource {
                candidate_url: first_feed_url.clone(),
                candidate_kind: SourceKind::Rss,
                confidence: 0.96,
                evidence: serde_json::json!({"fixture": true}),
            }],
            serde_json::json!({"fixture": true}),
        )
        .await
        .expect("complete seeded discovery");
    let first_candidate_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM source_candidates WHERE company_id = $1 AND candidate_url = $2",
    )
    .bind(company_id)
    .bind(first_feed_url.as_str())
    .fetch_one(database.pool())
    .await
    .expect("find first feed candidate");

    database
        .enqueue_unvalidated_candidate_jobs(Utc::now(), 100)
        .await
        .expect("enqueue seeded feed validation despite HTML coverage");
    let first_validation_queued: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM jobs
            WHERE job_type = 'validate_candidate'
              AND candidate_id = $1
              AND status = 'pending'
        )
        "#,
    )
    .bind(first_candidate_id)
    .fetch_one(database.pool())
    .await
    .expect("check first validation job");
    assert!(first_validation_queued);

    let approved_feed_source_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (source_id, company_id, kind, url, status)
        VALUES ($1, $2, 'rss', $3, 'approved')
        RETURNING id
        "#,
    )
    .bind(format!("recipe-seed-rss-{}", &suffix[..10]))
    .bind(company_id)
    .bind(first_feed_url.as_str())
    .fetch_one(database.pool())
    .await
    .expect("insert approved feed");
    sqlx::query(
        r#"
        INSERT INTO source_state (
            source_id, last_attempt_at, last_success_at,
            consecutive_failures, consecutive_zero_runs,
            total_successful_runs
        )
        VALUES ($1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, 0, 0, 1)
        "#,
    )
    .bind(approved_feed_source_id)
    .execute(database.pool())
    .await
    .expect("mark approved feed healthy");
    sqlx::query(
        r#"
        UPDATE source_candidates
        SET status = 'accepted', accepted_source_id = $2
        WHERE id = $1
        "#,
    )
    .bind(first_candidate_id)
    .bind(approved_feed_source_id)
    .execute(database.pool())
    .await
    .expect("accept first feed candidate");

    let mut second_discovery_job = JobSpec::new(
        JobType::DiscoverCompany,
        format!("recipe-seed-discovery:{suffix}:two"),
        Utc::now(),
    );
    second_discovery_job.company_id = Some(company_id);
    second_discovery_job.payload = serde_json::json!({
        "seed_origin": "company_news_recipe_builder"
    });
    let second_discovery_job = database
        .enqueue_job(&second_discovery_job)
        .await
        .expect("enqueue second seeded discovery");
    let second_discovery_run = database
        .begin_discovery_run(company_id, second_discovery_job.id)
        .await
        .expect("begin second seeded discovery");
    let second_feed_url =
        Url::parse(&format!("https://{suffix}.example.test/atom.xml")).expect("second feed URL");
    database
        .complete_discovery_run(
            second_discovery_run,
            &[DiscoveredSource {
                candidate_url: second_feed_url.clone(),
                candidate_kind: SourceKind::Atom,
                confidence: 0.95,
                evidence: serde_json::json!({"fixture": true}),
            }],
            serde_json::json!({"fixture": true}),
        )
        .await
        .expect("complete second seeded discovery");
    let second_candidate_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM source_candidates WHERE company_id = $1 AND candidate_url = $2",
    )
    .bind(company_id)
    .bind(second_feed_url.as_str())
    .fetch_one(database.pool())
    .await
    .expect("find second feed candidate");

    database
        .enqueue_unvalidated_candidate_jobs(Utc::now(), 100)
        .await
        .expect("run validation refill after feed approval");
    let second_validation_queued: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM jobs
            WHERE job_type = 'validate_candidate'
              AND candidate_id = $1
              AND status IN ('pending', 'running')
        )
        "#,
    )
    .bind(second_candidate_id)
    .fetch_one(database.pool())
    .await
    .expect("check second validation job");
    assert!(!second_validation_queued);

    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(database.pool())
        .await
        .expect("delete recipe seed fixture");
}

#[tokio::test]
async fn automatic_scope_reconsideration_never_reopens_operator_rejections() {
    let _guard = JOB_QUEUE_TEST_LOCK.lock().await;

    let Some(database_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL is not set; skipping Postgres integration test");
        return;
    };
    let database = Database::connect(&database_url, 5)
        .await
        .expect("connect to test Postgres");
    database.migrate().await.expect("run migrations");

    let suffix = Uuid::new_v4().simple().to_string();
    let mut fixtures = Vec::new();
    for (index, decision_mode) in ["automatic", "operator"].into_iter().enumerate() {
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
        .bind(format!(
            "scope-reconsider-{decision_mode}-{}",
            &suffix[index..index + 10]
        ))
        .bind(format!("Scope Reconsider {decision_mode} {suffix}"))
        .fetch_one(database.pool())
        .await
        .expect("insert scope reconsider company");
        let candidate_id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO source_candidates (
                company_id, candidate_url, candidate_kind, confidence,
                evidence, status
            )
            VALUES ($1, $2, 'rss', 0.97, $3, 'rejected')
            RETURNING id
            "#,
        )
        .bind(company_id)
        .bind(format!(
            "https://{decision_mode}-{suffix}.example.test/feed.xml"
        ))
        .bind(serde_json::json!({
            "external_web_adapter": {
                "roles": ["blog"],
                "rank_score": 0.9,
            }
        }))
        .fetch_one(database.pool())
        .await
        .expect("insert rejected candidate");
        sqlx::query(
            r#"
            INSERT INTO candidate_validation_runs (
                candidate_id, finished_at, status, detected_kind,
                item_count, titled_item_count, metadata
            )
            VALUES ($1, CURRENT_TIMESTAMP, 'invalid', 'rss', 10, 10, $2)
            "#,
        )
        .bind(candidate_id)
        .bind(serde_json::json!({
            "feed": {
                "feed_title": format!("Scope Reconsider {decision_mode}"),
            },
            "policy": {
                "company_scope_passed": false,
                "adapter_recommended": true,
                "has_usable_items": true,
                "sitemap_source": false,
                "non_editorial_item_scope": false,
                "publication_host_excluded": false,
                "redundant_with_approved_feed": false,
            }
        }))
        .execute(database.pool())
        .await
        .expect("insert invalid validation run");
        sqlx::query(
            r#"
            INSERT INTO candidate_decisions (
                candidate_id, decision, decision_mode, actor, reason
            )
            VALUES ($1, 'rejected', $2, 'fixture', 'scope fixture rejection')
            "#,
        )
        .bind(candidate_id)
        .bind(decision_mode)
        .execute(database.pool())
        .await
        .expect("insert candidate rejection decision");
        fixtures.push((company_id, candidate_id, decision_mode));
    }

    assert_eq!(
        database
            .reconsider_automatically_rejected_scope_candidates(Utc::now(), 10)
            .await
            .expect("reconsider automatic scope candidates"),
        1
    );

    for (_, candidate_id, decision_mode) in &fixtures {
        let status: String =
            sqlx::query_scalar("SELECT status FROM source_candidates WHERE id = $1")
                .bind(candidate_id)
                .fetch_one(database.pool())
                .await
                .expect("read reconsidered candidate status");
        let has_job: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM jobs
                WHERE
                    candidate_id = $1
                    AND job_type = 'validate_candidate'
                    AND status = 'pending'
            )
            "#,
        )
        .bind(candidate_id)
        .fetch_one(database.pool())
        .await
        .expect("read reconsideration job");
        if *decision_mode == "automatic" {
            assert_eq!(status, "new");
            assert!(has_job);
        } else {
            assert_eq!(status, "rejected");
            assert!(!has_job);
        }
    }

    let automatic_candidate = fixtures[0].1;
    let event_recorded: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM event_log
            WHERE
                event_type = 'source_candidate.reopened_for_validation'
                AND payload->>'candidate_id' = $1
        )
        "#,
    )
    .bind(automatic_candidate.to_string())
    .fetch_one(database.pool())
    .await
    .expect("read reconsideration event");
    assert!(event_recorded);

    let company_ids = fixtures
        .iter()
        .map(|(company_id, _, _)| *company_id)
        .collect::<Vec<_>>();
    sqlx::query("DELETE FROM companies WHERE id = ANY($1::uuid[])")
        .bind(company_ids)
        .execute(database.pool())
        .await
        .expect("delete scope reconsider fixtures");
}

#[tokio::test]
async fn job_lifecycle_is_idempotent_and_lease_fenced() {
    let _guard = JOB_QUEUE_TEST_LOCK.lock().await;

    let Some(database_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL is not set; skipping Postgres integration test");
        return;
    };
    let database = Database::connect(&database_url, 5)
        .await
        .expect("connect to test Postgres");
    database.migrate().await.expect("run migrations");

    let key = format!("integration:{}", Uuid::new_v4());
    let mut spec = JobSpec::new(JobType::DiscoverCompany, &key, Utc::now());
    spec.max_attempts = 3;

    let first = database.enqueue_job(&spec).await.expect("enqueue job");
    let duplicate = database
        .enqueue_job(&spec)
        .await
        .expect("deduplicate active job");
    assert_eq!(first.id, duplicate.id);

    let claimed = database
        .claim_job(
            "integration-worker",
            Duration::from_secs(30),
            &[JobType::DiscoverCompany],
            1,
        )
        .await
        .expect("claim job")
        .expect("job is due");
    assert_eq!(claimed.job.id, first.id);
    assert_eq!(claimed.job.attempt_count, 1);

    assert!(
        database
            .heartbeat_job(claimed.job.id, claimed.lease_token, Duration::from_secs(30))
            .await
            .expect("heartbeat")
    );
    assert!(
        !database
            .complete_job(claimed.job.id, Uuid::new_v4())
            .await
            .expect("fenced completion")
    );

    let retry_at = Utc::now() - ChronoDuration::seconds(1);
    match database
        .record_job_failure(
            claimed.job.id,
            claimed.lease_token,
            "temporary failure",
            Some(retry_at),
        )
        .await
        .expect("record retryable failure")
    {
        JobFailureOutcome::Retrying { run_after } => {
            assert!(run_after >= retry_at);
        }
        JobFailureOutcome::Failed => panic!("expected retryable outcome"),
    }

    let retried = database
        .claim_job(
            "integration-worker",
            Duration::from_secs(30),
            &[JobType::DiscoverCompany],
            1,
        )
        .await
        .expect("reclaim retry")
        .expect("retry is due");
    assert_eq!(retried.job.id, first.id);
    assert_eq!(retried.job.attempt_count, 2);
    assert!(
        database
            .complete_job(retried.job.id, retried.lease_token)
            .await
            .expect("complete retry")
    );
    assert_eq!(
        database
            .get_job(first.id)
            .await
            .expect("load completed job")
            .expect("job exists")
            .status,
        JobStatus::Completed
    );

    let replacement = database
        .enqueue_job(&spec)
        .await
        .expect("enqueue next logical occurrence");
    assert_ne!(replacement.id, first.id);

    sqlx::query("DELETE FROM jobs WHERE job_key = $1")
        .bind(&key)
        .execute(database.pool())
        .await
        .expect("clean up integration jobs");
}

#[tokio::test]
async fn retried_crawl_attempts_close_abandoned_crawl_and_recipe_runs() {
    let _guard = JOB_QUEUE_TEST_LOCK.lock().await;

    let Some(database_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL is not set; skipping Postgres integration test");
        return;
    };
    let database = Database::connect(&database_url, 5)
        .await
        .expect("connect to test Postgres");
    database.migrate().await.expect("run migrations");

    let suffix = Uuid::new_v4().simple().to_string();
    let company_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO companies (
            company_key, name, ownership_status, lifecycle_status,
            discovery_cadence_seconds
        )
        VALUES ($1, 'Crawl Retry Fixture', 'private', 'active', 3600)
        RETURNING id
        "#,
    )
    .bind(format!("crawl-retry-{}", &suffix[..10]))
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
    .bind(format!("crawl-retry-source-{}", &suffix[..10]))
    .bind(company_id)
    .bind(format!("https://{suffix}.example.test/news/"))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture source");
    let recipe_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO company_news_recipes (
            recipe_key, company_id, source_id, version, status,
            schema_version, spec, content_hash, verified_at
        )
        VALUES ($1, $2, $3, 1, 'active', 'company-news-recipe.v1', '{}', $4, $5)
        RETURNING id
        "#,
    )
    .bind(format!("crawl-retry-recipe-{}", &suffix[..10]))
    .bind(company_id)
    .bind(source_id)
    .bind(format!("sha256:crawl-retry:{suffix}"))
    .bind(Utc::now())
    .fetch_one(database.pool())
    .await
    .expect("insert fixture recipe");

    let mut spec = JobSpec::new(
        JobType::CrawlSource,
        format!("crawl-retry:{suffix}"),
        Utc::now(),
    );
    spec.company_id = Some(company_id);
    spec.source_id = Some(source_id);
    let job = database
        .enqueue_job(&spec)
        .await
        .expect("enqueue crawl job");

    let first_crawl_run = database
        .begin_crawl_run(source_id, job.id)
        .await
        .expect("begin first crawl attempt");
    let first_recipe_run = database
        .begin_company_news_recipe_run(recipe_id, source_id, job.id, first_crawl_run)
        .await
        .expect("begin first recipe attempt");
    let second_crawl_run = database
        .begin_crawl_run(source_id, job.id)
        .await
        .expect("begin replacement crawl attempt");

    let first_statuses: (String, String) = sqlx::query_as(
        r#"
        SELECT
            (SELECT status FROM crawl_runs WHERE id = $1),
            (SELECT status FROM company_news_recipe_runs WHERE id = $2)
        "#,
    )
    .bind(first_crawl_run)
    .bind(first_recipe_run)
    .fetch_one(database.pool())
    .await
    .expect("read abandoned attempt statuses");
    assert_eq!(
        first_statuses,
        ("cancelled".to_owned(), "cancelled".to_owned())
    );

    let second_recipe_run = database
        .begin_company_news_recipe_run(recipe_id, source_id, job.id, second_crawl_run)
        .await
        .expect("begin replacement recipe attempt");
    assert_eq!(
        database
            .cancel_running_crawl_runs_for_job(job.id, "test shutdown")
            .await
            .expect("cancel active crawl attempt"),
        2
    );
    let second_statuses: (String, String) = sqlx::query_as(
        r#"
        SELECT
            (SELECT status FROM crawl_runs WHERE id = $1),
            (SELECT status FROM company_news_recipe_runs WHERE id = $2)
        "#,
    )
    .bind(second_crawl_run)
    .bind(second_recipe_run)
    .fetch_one(database.pool())
    .await
    .expect("read cancelled replacement attempt statuses");
    assert_eq!(
        second_statuses,
        ("cancelled".to_owned(), "cancelled".to_owned())
    );

    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(database.pool())
        .await
        .expect("delete crawl retry fixture");
}

#[tokio::test]
async fn expired_final_attempt_is_failed_instead_of_reclaimed() {
    let _guard = JOB_QUEUE_TEST_LOCK.lock().await;

    let Some(database_url) = std::env::var("TEST_DATABASE_URL").ok() else {
        eprintln!("TEST_DATABASE_URL is not set; skipping Postgres integration test");
        return;
    };
    let database = Database::connect(&database_url, 5)
        .await
        .expect("connect to test Postgres");
    database.migrate().await.expect("run migrations");

    let key = format!("integration-expired:{}", Uuid::new_v4());
    let mut spec = JobSpec::new(JobType::DiscoverCompany, &key, Utc::now());
    spec.max_attempts = 1;
    let job = database.enqueue_job(&spec).await.expect("enqueue job");
    let claimed = database
        .claim_job(
            "expiring-worker",
            Duration::from_secs(30),
            &[JobType::DiscoverCompany],
            1,
        )
        .await
        .expect("claim job")
        .expect("job is due");

    sqlx::query(
        "UPDATE jobs SET lease_until = CURRENT_TIMESTAMP - INTERVAL '1 second' WHERE id = $1",
    )
    .bind(claimed.job.id)
    .execute(database.pool())
    .await
    .expect("expire lease");

    assert!(
        database
            .claim_job(
                "replacement-worker",
                Duration::from_secs(30),
                &[JobType::DiscoverCompany],
                1,
            )
            .await
            .expect("run expired-job reaper")
            .is_none()
    );
    assert_eq!(
        database
            .get_job(job.id)
            .await
            .expect("load exhausted job")
            .expect("job exists")
            .status,
        JobStatus::Failed
    );

    sqlx::query("DELETE FROM jobs WHERE job_key = $1")
        .bind(&key)
        .execute(database.pool())
        .await
        .expect("clean up integration job");
}
