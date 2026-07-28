#![cfg(feature = "postgres-tests")]

use chrono::{Duration, Utc};
use feed_core::{SourceKind, SourceStatus};
use feed_db::{Database, FeedItemSignatureCandidate, FeedItemSummaryFilter};
use uuid::Uuid;

async fn acquire_test_lock(database: &Database) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut transaction = database
        .pool()
        .begin()
        .await
        .expect("begin feed-overlap test lock transaction");
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('feed_overlap_tests'))")
        .execute(&mut *transaction)
        .await
        .expect("acquire feed-overlap test lock");
    transaction
}

#[tokio::test]
async fn news_summaries_do_not_resurface_undated_items_after_recrawl() {
    let database_url =
        std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required for this test");
    let database = Database::connect(&database_url, 5)
        .await
        .expect("connect to test Postgres");
    database.ensure_schema().await.expect("ensure schema");
    let _test_lock = acquire_test_lock(&database).await;

    let suffix = Uuid::new_v4().simple().to_string();
    let company_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO companies (
            company_key, name, ownership_status, lifecycle_status,
            discovery_cadence_seconds
        )
        VALUES ($1, 'News Ordering Fixture', 'private', 'active', 3600)
        RETURNING id
        "#,
    )
    .bind(format!("news-ordering-{}", &suffix[..10]))
    .fetch_one(database.pool())
    .await
    .expect("insert ordering fixture company");
    let source_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (source_id, company_id, kind, url, status)
        VALUES ($1, $2, 'rss', $3, 'approved')
        RETURNING id
        "#,
    )
    .bind(format!("news-ordering-rss-{}", &suffix[..10]))
    .bind(company_id)
    .bind(format!("https://{suffix}.example.test/feed.xml"))
    .fetch_one(database.pool())
    .await
    .expect("insert ordering fixture source");

    let now = Utc::now();
    for (index, title, published_at, fetched_at, created_at) in [
        (
            0,
            "Recently published article",
            Some(now - Duration::hours(2)),
            now - Duration::hours(1),
            now - Duration::days(20),
        ),
        (
            1,
            "Newly discovered undated article",
            None,
            now - Duration::hours(12),
            now - Duration::days(1),
        ),
        (
            2,
            "Old undated article fetched again",
            None,
            now,
            now - Duration::days(10),
        ),
        (
            3,
            "Future scheduled article",
            Some(now + Duration::hours(2)),
            now,
            now,
        ),
    ] {
        let url = format!("https://{suffix}.example.test/news/{index}");
        sqlx::query(
            r#"
            INSERT INTO feed_items (
                company_id, source_id, external_id, url, canonical_url,
                title, published_at, fetched_at, content_hash, source_kind,
                created_at, updated_at
            )
            VALUES ($1, $2, $3, $3, $3, $4, $5, $6, $7, 'rss', $8, $8)
            "#,
        )
        .bind(company_id)
        .bind(source_id)
        .bind(url)
        .bind(title)
        .bind(published_at)
        .bind(fetched_at)
        .bind(format!("sha256:ordering:{index}:{suffix}"))
        .bind(created_at)
        .execute(database.pool())
        .await
        .expect("insert ordering fixture item");
    }

    let summaries = database
        .list_feed_item_summaries(&FeedItemSummaryFilter {
            company_id: Some(company_id),
            limit: 10,
            ..FeedItemSummaryFilter::default()
        })
        .await
        .expect("list ordered news summaries");
    assert_eq!(
        summaries
            .iter()
            .map(|summary| summary.item.title.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Recently published article",
            "Newly discovered undated article",
            "Old undated article fetched again",
        ]
    );
    let future_summaries = database
        .list_feed_item_summaries(&FeedItemSummaryFilter {
            company_id: Some(company_id),
            include_future: true,
            limit: 10,
            ..FeedItemSummaryFilter::default()
        })
        .await
        .expect("list ordered news summaries including future items");
    assert_eq!(future_summaries.len(), 4);
    assert_eq!(future_summaries[0].item.title, "Future scheduled article");

    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(database.pool())
        .await
        .expect("clean up ordering fixture company");
}

#[tokio::test]
async fn news_summaries_collapse_dated_cross_property_story_mirrors() {
    let database_url =
        std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required for this test");
    let database = Database::connect(&database_url, 5)
        .await
        .expect("connect to test Postgres");
    database.ensure_schema().await.expect("ensure schema");
    let _test_lock = acquire_test_lock(&database).await;

    let suffix = Uuid::new_v4().simple().to_string();
    let company_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO companies (
            company_key, name, ownership_status, lifecycle_status,
            discovery_cadence_seconds
        )
        VALUES ($1, 'Story Mirror Fixture', 'private', 'active', 3600)
        RETURNING id
        "#,
    )
    .bind(format!("story-mirror-{}", &suffix[..10]))
    .fetch_one(database.pool())
    .await
    .expect("insert story mirror company");
    let rss_source_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (source_id, company_id, kind, url, status)
        VALUES ($1, $2, 'rss', $3, 'approved')
        RETURNING id
        "#,
    )
    .bind(format!("story-mirror-rss-{}", &suffix[..10]))
    .bind(company_id)
    .bind(format!("https://{suffix}.example.test/feed.xml"))
    .fetch_one(database.pool())
    .await
    .expect("insert story mirror RSS source");
    let html_source_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (source_id, company_id, kind, url, status)
        VALUES ($1, $2, 'html', $3, 'approved')
        RETURNING id
        "#,
    )
    .bind(format!("story-mirror-html-{}", &suffix[..10]))
    .bind(company_id)
    .bind(format!("https://news.{suffix}.example.test/"))
    .fetch_one(database.pool())
    .await
    .expect("insert story mirror HTML source");
    let recipe_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO company_news_recipes (
            recipe_key, company_id, source_id, version, status,
            schema_version, spec, content_hash, verified_at
        )
        VALUES (
            $1, $2, $3, 1, 'active', 'company-news-recipe.v1',
            jsonb_build_object('publication_url', $4::text), $5, CURRENT_TIMESTAMP
        )
        RETURNING id
        "#,
    )
    .bind(format!("story-mirror-recipe-{}", &suffix[..10]))
    .bind(company_id)
    .bind(html_source_id)
    .bind(format!("https://news.{suffix}.example.test/"))
    .bind(format!("sha256:story-mirror-recipe:{suffix}"))
    .fetch_one(database.pool())
    .await
    .expect("insert active story mirror recipe");
    sqlx::query(
        r#"
        INSERT INTO company_news_recipe_state (
            recipe_id, freshness_status, correctness_status, rebuild_required
        )
        VALUES ($1, 'fresh', 'passing', false)
        "#,
    )
    .bind(recipe_id)
    .execute(database.pool())
    .await
    .expect("insert story mirror recipe state");

    let first_day = Utc::now() - Duration::days(2);
    let second_day = first_day + Duration::days(1);
    for (index, source_id, source_kind, title, published_at) in [
        (
            0,
            rss_source_id,
            "rss",
            "Acme launches its next-generation platform",
            Some(first_day),
        ),
        (
            1,
            html_source_id,
            "html",
            "  ACME   LAUNCHES ITS NEXT-GENERATION PLATFORM  ",
            Some(first_day + Duration::hours(4)),
        ),
        (
            2,
            html_source_id,
            "html",
            "Acme launches its next-generation platform",
            Some(second_day),
        ),
        (
            3,
            html_source_id,
            "html",
            "Recurring undated company update",
            None,
        ),
        (
            4,
            html_source_id,
            "html",
            "Recurring undated company update",
            None,
        ),
    ] {
        let url = format!("https://news.{suffix}.example.test/article-{index}");
        sqlx::query(
            r#"
            INSERT INTO feed_items (
                company_id, source_id, external_id, url, canonical_url,
                title, published_at, fetched_at, content_hash, source_kind
            )
            VALUES ($1, $2, $3, $3, $3, $4, $5, CURRENT_TIMESTAMP, $6, $7)
            "#,
        )
        .bind(company_id)
        .bind(source_id)
        .bind(url)
        .bind(title)
        .bind(published_at)
        .bind(format!("sha256:story-mirror:{index}:{suffix}"))
        .bind(source_kind)
        .execute(database.pool())
        .await
        .expect("insert story mirror item");
    }

    let filter = FeedItemSummaryFilter {
        company_id: Some(company_id),
        include_future: true,
        limit: 10,
        ..FeedItemSummaryFilter::default()
    };
    let summaries = database
        .list_feed_item_summaries(&filter)
        .await
        .expect("list story-mirror summaries");

    assert_eq!(summaries.len(), 4);
    assert_eq!(
        database
            .count_feed_item_summaries(&filter)
            .await
            .expect("count story-mirror summaries"),
        4
    );
    let first_day_story = summaries
        .iter()
        .find(|summary| summary.item.source_id == rss_source_id)
        .expect("find first-day mirrored story");
    assert_eq!(first_day_story.item.source_id, rss_source_id);
    assert_eq!(
        first_day_story
            .item
            .published_at
            .map(|value| value.date_naive()),
        Some(first_day.date_naive())
    );
    assert_eq!(
        summaries
            .iter()
            .filter(|summary| summary.item.published_at.is_none())
            .count(),
        2
    );

    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(database.pool())
        .await
        .expect("clean up story mirror company");
}

#[tokio::test]
async fn approved_rss_and_atom_items_are_the_only_overlap_matches() {
    let database_url =
        std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required for this test");
    let database = Database::connect(&database_url, 5)
        .await
        .expect("connect to test Postgres");
    database.ensure_schema().await.expect("ensure schema");
    let _test_lock = acquire_test_lock(&database).await;
    let coverage_before = database
        .get_company_news_recipe_coverage()
        .await
        .expect("read baseline recipe coverage");

    let suffix = Uuid::new_v4().simple().to_string();
    let company_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO companies (
            company_key, name, ownership_status, lifecycle_status,
            discovery_cadence_seconds
        )
        VALUES ($1, 'Feed Overlap Fixture', 'private', 'active', 3600)
        RETURNING id
        "#,
    )
    .bind(format!("feed-overlap-{}", &suffix[..10]))
    .fetch_one(database.pool())
    .await
    .expect("insert fixture company");
    let collision_name = format!("Collision Alias {}", &suffix[..8]);
    let alias_owner_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO companies (
            company_key, name, ownership_status, lifecycle_status,
            discovery_cadence_seconds
        )
        VALUES ($1, $2, 'private', 'active', 3600)
        RETURNING id
        "#,
    )
    .bind(format!("alias-owner-{}", &suffix[..10]))
    .bind(&collision_name)
    .fetch_one(database.pool())
    .await
    .expect("insert alias owner company");
    assert_eq!(
        database
            .list_aliases_colliding_with_company_names(
                company_id,
                &["Unique Alias".to_owned(), collision_name.clone()],
            )
            .await
            .expect("find alias collision"),
        vec![collision_name]
    );
    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(alias_owner_id)
        .execute(database.pool())
        .await
        .expect("clean up alias owner company");

    let rss_source_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (source_id, company_id, kind, url, status)
        VALUES ($1, $2, 'rss', $3, 'approved')
        RETURNING id
        "#,
    )
    .bind(format!("feed-overlap-rss-{}", &suffix[..10]))
    .bind(company_id)
    .bind(format!("https://{suffix}.example.test/feed.xml"))
    .fetch_one(database.pool())
    .await
    .expect("insert RSS source");
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
    .bind(rss_source_id)
    .execute(database.pool())
    .await
    .expect("mark RSS source healthy");
    let html_source_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (source_id, company_id, kind, url, status)
        VALUES ($1, $2, 'html', $3, 'approved')
        RETURNING id
        "#,
    )
    .bind(format!("feed-overlap-html-{}", &suffix[..10]))
    .bind(company_id)
    .bind(format!("https://{suffix}.example.test/blog/"))
    .fetch_one(database.pool())
    .await
    .expect("insert HTML source");
    let publication_url = format!("https://{suffix}.example.test/en-us/blog/?campaign=recipe");

    let rss_source = database
        .get_source(rss_source_id)
        .await
        .expect("load RSS source")
        .expect("RSS source exists");
    let source_claims = database
        .list_approved_feed_source_company_claims(&rss_source.url)
        .await
        .expect("list approved feed source claims");
    assert_eq!(source_claims.len(), 1);
    assert_eq!(source_claims[0].source_id, rss_source_id);
    assert_eq!(source_claims[0].company_id, company_id);
    assert_eq!(source_claims[0].company_name, "Feed Overlap Fixture");

    let coverage_with_fixture = database
        .get_company_news_recipe_coverage()
        .await
        .expect("read recipe coverage with feed fixture");
    assert_eq!(
        coverage_with_fixture.eligible_companies,
        coverage_before.eligible_companies + 1
    );
    assert_eq!(
        coverage_with_fixture.companies_with_approved_feed,
        coverage_before.companies_with_approved_feed + 1
    );
    assert_eq!(
        coverage_with_fixture.companies_with_healthy_feed,
        coverage_before.companies_with_healthy_feed + 1
    );
    assert_eq!(
        coverage_with_fixture.companies_covered_by_feed_or_recipe,
        coverage_before.companies_covered_by_feed_or_recipe + 1
    );
    assert_eq!(
        coverage_with_fixture.companies_missing_recipe,
        coverage_before.companies_missing_recipe + 1
    );
    assert_eq!(
        coverage_with_fixture.companies_missing_feed_or_recipe,
        coverage_before.companies_missing_feed_or_recipe
    );
    assert_eq!(
        coverage_with_fixture.companies_without_completed_build,
        coverage_before.companies_without_completed_build + 1
    );
    assert_eq!(
        coverage_with_fixture.companies_uncovered_awaiting_completed_build,
        coverage_before.companies_uncovered_awaiting_completed_build
    );

    let html_recipe_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO company_news_recipes (
            recipe_key, company_id, source_id, version, status,
            schema_version, spec, content_hash, verified_at
        )
        VALUES (
            $1, $2, $3, 1, 'active', 'company-news-recipe.v1',
            jsonb_build_object('publication_url', $4::text), $5, $6
        )
        RETURNING id
        "#,
    )
    .bind(format!("feed-overlap-recipe-{}", &suffix[..10]))
    .bind(company_id)
    .bind(html_source_id)
    .bind(&publication_url)
    .bind(format!("sha256:recipe:{suffix}"))
    .bind(Utc::now())
    .fetch_one(database.pool())
    .await
    .expect("insert active HTML recipe");
    sqlx::query(
        r#"
        INSERT INTO company_news_recipe_state (
            recipe_id, freshness_status, correctness_status, rebuild_required
        )
        VALUES ($1, 'fresh', 'passing', false)
        "#,
    )
    .bind(html_recipe_id)
    .execute(database.pool())
    .await
    .expect("insert active HTML recipe state");

    let claims = database
        .list_active_company_news_publication_claims(&format!(
            "http://www.{suffix}.example.test/blog#latest"
        ))
        .await
        .expect("list active publication claims");
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].recipe_id, html_recipe_id);
    assert_eq!(claims[0].company_id, company_id);
    assert_eq!(claims[0].company_name, "Feed Overlap Fixture");

    let rss_url = format!("https://{suffix}.example.test/blog/rss-article/");
    let rss_alias_url = format!("http://www.{suffix}.example.test/blog/rss-article");
    let html_url = format!("https://{suffix}.example.test/blog/html-article/");
    for (source_id, source_kind, url) in [
        (rss_source_id, "rss", rss_url.as_str()),
        (html_source_id, "html", html_url.as_str()),
    ] {
        sqlx::query(
            r#"
            INSERT INTO feed_items (
                company_id, source_id, external_id, url, canonical_url,
                title, fetched_at, content_hash, source_kind
            )
            VALUES ($1, $2, $3, $3, $3, 'Overlap fixture article', $4, $5, $6)
            "#,
        )
        .bind(company_id)
        .bind(source_id)
        .bind(url)
        .bind(Utc::now())
        .bind(format!("sha256:{source_kind}:{suffix}"))
        .bind(source_kind)
        .execute(database.pool())
        .await
        .expect("insert fixture feed item");
    }
    sqlx::query(
        r#"
        INSERT INTO feed_items (
            company_id, source_id, external_id, url, canonical_url,
            title, fetched_at, content_hash, source_kind
        )
        VALUES ($1, $2, $3, $3, $4, 'Duplicate HTML article', $5, $6, 'html')
        "#,
    )
    .bind(company_id)
    .bind(html_source_id)
    .bind(format!("{rss_url}?html-copy=1"))
    .bind(&rss_alias_url)
    .bind(Utc::now())
    .bind(format!("sha256:html-duplicate:{suffix}"))
    .execute(database.pool())
    .await
    .expect("insert duplicate HTML item");

    let matches = database
        .list_approved_feed_item_url_matches(
            company_id,
            &[
                rss_alias_url.clone(),
                html_url.clone(),
                "https://unmatched.test/".to_owned(),
            ],
        )
        .await
        .expect("match approved feed items");
    assert_eq!(matches, vec![rss_alias_url]);

    let signature_alias_url = format!("https://mirror.{suffix}.example.test/news/same-release");
    let signature_candidate = FeedItemSignatureCandidate {
        identity_url: signature_alias_url.clone(),
        title: "  Overlap   fixture article  ".to_owned(),
        published_at: None,
    };
    assert_eq!(
        database
            .list_approved_feed_item_signature_matches(
                company_id,
                std::slice::from_ref(&signature_candidate),
            )
            .await
            .expect("match approved feed title/date signature"),
        vec![signature_alias_url.clone()]
    );
    assert_eq!(
        database
            .list_active_recipe_item_signature_matches(
                company_id,
                std::slice::from_ref(&signature_candidate),
            )
            .await
            .expect("match active recipe title/date signature"),
        vec![signature_alias_url.clone()]
    );
    assert!(
        database
            .list_approved_feed_item_signature_matches(
                company_id,
                &[FeedItemSignatureCandidate {
                    published_at: Some(Utc::now()),
                    ..signature_candidate.clone()
                }],
            )
            .await
            .expect("reject a signature with a different publication date")
            .is_empty()
    );

    let company_claims = database
        .list_approved_feed_item_company_claims(&[rss_url.clone(), html_url.clone()])
        .await
        .expect("group approved feed item claims by company");
    assert_eq!(company_claims.len(), 1);
    assert_eq!(company_claims[0].company_id, company_id);
    assert_eq!(company_claims[0].company_name, "Feed Overlap Fixture");
    assert_eq!(company_claims[0].matched_item_count, 1);

    let mut active_recipe_matches = database
        .list_active_recipe_item_url_matches(
            company_id,
            &[
                matches[0].clone(),
                html_url.clone(),
                "https://unmatched.test/".to_owned(),
            ],
        )
        .await
        .expect("match active recipe items");
    active_recipe_matches.sort();
    let mut expected_recipe_matches = vec![matches[0].clone(), html_url.clone()];
    expected_recipe_matches.sort();
    assert_eq!(active_recipe_matches, expected_recipe_matches);
    assert_eq!(
        database
            .list_active_recipe_ids_fully_covered_by_item_urls(company_id, &active_recipe_matches,)
            .await
            .expect("find fully covered active recipe"),
        vec![html_recipe_id]
    );
    assert!(
        database
            .list_preferred_active_recipe_item_url_matches(
                company_id,
                html_recipe_id,
                &active_recipe_matches,
            )
            .await
            .expect("match preferred active recipe items")
            .is_empty()
    );

    let broader_source_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO sources (source_id, company_id, kind, url, status)
        VALUES ($1, $2, 'html', $3, 'approved')
        RETURNING id
        "#,
    )
    .bind(format!("feed-overlap-broader-{}", &suffix[..10]))
    .bind(company_id)
    .bind(format!("https://{suffix}.example.test/news/"))
    .fetch_one(database.pool())
    .await
    .expect("insert broader HTML source");
    let broader_recipe_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO company_news_recipes (
            recipe_key, company_id, source_id, version, status,
            schema_version, spec, content_hash, verified_at
        )
        VALUES ($1, $2, $3, 1, 'active', 'company-news-recipe.v1', '{}', $4, $5)
        RETURNING id
        "#,
    )
    .bind(format!("feed-overlap-broader-recipe-{}", &suffix[..10]))
    .bind(company_id)
    .bind(broader_source_id)
    .bind(format!("sha256:broader-recipe:{suffix}"))
    .bind(Utc::now())
    .fetch_one(database.pool())
    .await
    .expect("insert broader active HTML recipe");
    sqlx::query(
        r#"
        INSERT INTO company_news_recipe_state (
            recipe_id, freshness_status, correctness_status, rebuild_required
        )
        VALUES ($1, 'fresh', 'passing', false)
        "#,
    )
    .bind(broader_recipe_id)
    .execute(database.pool())
    .await
    .expect("insert broader recipe state");
    let broader_urls = [
        html_url.clone(),
        format!("https://{suffix}.example.test/news/broader-one/"),
        format!("https://{suffix}.example.test/news/broader-two/"),
    ];
    for (index, url) in broader_urls.iter().enumerate() {
        sqlx::query(
            r#"
            INSERT INTO feed_items (
                company_id, source_id, external_id, url, canonical_url,
                title, fetched_at, content_hash, source_kind
            )
            VALUES ($1, $2, $3, $3, $3, 'Broader recipe article', $4, $5, 'html')
            "#,
        )
        .bind(company_id)
        .bind(broader_source_id)
        .bind(url)
        .bind(Utc::now())
        .bind(format!("sha256:broader:{index}:{suffix}"))
        .execute(database.pool())
        .await
        .expect("insert broader recipe item");
    }
    assert_eq!(
        database
            .list_preferred_active_recipe_item_url_matches(
                company_id,
                html_recipe_id,
                std::slice::from_ref(&html_url),
            )
            .await
            .expect("prefer broader active recipe"),
        vec![html_url.clone()]
    );
    assert!(
        database
            .list_preferred_active_recipe_item_url_matches(
                company_id,
                broader_recipe_id,
                std::slice::from_ref(&html_url),
            )
            .await
            .expect("do not prefer narrower active recipe")
            .is_empty()
    );
    assert!(
        database
            .list_active_recipe_ids_fully_covered_by_item_urls(
                company_id,
                std::slice::from_ref(&html_url),
            )
            .await
            .expect("reject partial active recipe coverage")
            .is_empty()
    );

    let private_url = format!("https://{suffix}.example.test/news/private-artifact/");
    sqlx::query(
        r#"
        INSERT INTO feed_items (
            company_id, source_id, external_id, url, canonical_url,
            title, fetched_at, content_hash, source_kind, is_private
        )
        VALUES ($1, $2, $3, $3, $3, 'Private artifact', $4, $5, 'html', true)
        "#,
    )
    .bind(company_id)
    .bind(broader_source_id)
    .bind(&private_url)
    .bind(Utc::now())
    .bind(format!("sha256:private:{suffix}"))
    .execute(database.pool())
    .await
    .expect("insert private recipe item");
    assert!(
        database
            .list_active_recipe_item_url_matches(company_id, &[private_url])
            .await
            .expect("exclude private overlap items")
            .is_empty()
    );

    let filter = FeedItemSummaryFilter {
        company_id: Some(company_id),
        include_future: true,
        limit: 10,
        ..FeedItemSummaryFilter::default()
    };
    let summaries = database
        .list_feed_item_summaries(&filter)
        .await
        .expect("list deduplicated summaries");
    assert_eq!(summaries.len(), 4);
    assert_eq!(
        database
            .count_feed_item_summaries(&filter)
            .await
            .expect("count deduplicated summaries"),
        4
    );
    let preferred = summaries
        .iter()
        .find(|summary| summary.item.canonical_url.as_str() == rss_url)
        .expect("find shared canonical URL");
    assert_eq!(preferred.item.source_kind, SourceKind::Rss);

    let needs_recipe_before = database
        .count_companies_needing_news_recipes(true)
        .await
        .expect("count recipe rebuild candidates before content staleness");
    let coverage_before_content_staleness = database
        .get_company_news_recipe_coverage()
        .await
        .expect("read coverage before content staleness");
    assert!(
        !database
            .list_company_ids_needing_news_recipes(true, 10_000, 0)
            .await
            .expect("list recipe rebuild candidates before content staleness")
            .contains(&company_id)
    );
    sqlx::query(
        r#"
        UPDATE company_news_recipe_state
        SET freshness_status = 'content_stale'
        WHERE recipe_id IN ($1, $2)
        "#,
    )
    .bind(html_recipe_id)
    .bind(broader_recipe_id)
    .execute(database.pool())
    .await
    .expect("mark fixture recipes content-stale");
    let coverage_after_content_staleness = database
        .get_company_news_recipe_coverage()
        .await
        .expect("read coverage after content staleness");
    assert_eq!(
        coverage_after_content_staleness.companies_with_active_recipe,
        coverage_before_content_staleness.companies_with_active_recipe - 1
    );
    assert_eq!(
        coverage_after_content_staleness.active_recipes,
        coverage_before_content_staleness.active_recipes - 2
    );
    assert_eq!(
        coverage_after_content_staleness.companies_covered_by_feed_or_recipe,
        coverage_before_content_staleness.companies_covered_by_feed_or_recipe,
        "the healthy RSS fixture continues to cover the company"
    );
    assert!(
        database
            .list_company_ids_needing_news_recipes(true, 10_000, 0)
            .await
            .expect("list recipe rebuild candidates after content staleness")
            .contains(&company_id)
    );
    assert_eq!(
        database
            .count_companies_needing_news_recipes(true)
            .await
            .expect("count recipe rebuild candidates after content staleness"),
        needs_recipe_before + 1
    );

    sqlx::query(
        r#"
        UPDATE company_news_recipes
        SET status = 'stale',
            stale_at = CURRENT_TIMESTAMP,
            stale_reason = 'fixture invalidation'
        WHERE id = $1
        "#,
    )
    .bind(html_recipe_id)
    .execute(database.pool())
    .await
    .expect("stale the narrow fixture recipe");
    assert!(
        database
            .retire_active_company_news_recipe_for_ownership(
                broader_recipe_id,
                "publication_owned_by_different_company",
                serde_json::json!({"fixture": "cross_company_feed_claim"}),
            )
            .await
            .expect("retire the broader fixture recipe for ownership")
    );
    assert_eq!(
        database
            .get_source(broader_source_id)
            .await
            .expect("load retired ownership source")
            .expect("retired ownership source exists")
            .status,
        SourceStatus::Disabled
    );
    let ownership_quarantine_count: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM feed_items
        WHERE source_id = $1
          AND is_private
          AND content_processing #>> '{quality_quarantine,policy}'
              = 'cross-company-feed-ownership.v1'
        "#,
    )
    .bind(broader_source_id)
    .fetch_one(database.pool())
    .await
    .expect("count ownership-quarantined items");
    assert_eq!(ownership_quarantine_count, broader_urls.len() as i64);

    let public_items = database
        .list_feed_items(Some(company_id), None, 20, 0)
        .await
        .expect("list public items after recipe invalidation");
    assert_eq!(public_items.len(), 1);
    assert_eq!(public_items[0].source_id, rss_source_id);
    assert_eq!(
        database
            .count_feed_items(Some(company_id), None)
            .await
            .expect("count public items after recipe invalidation"),
        1
    );
    let summaries = database
        .list_feed_item_summaries(&filter)
        .await
        .expect("list summaries after recipe invalidation");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].item.source_id, rss_source_id);
    assert_eq!(
        database
            .count_feed_item_summaries(&filter)
            .await
            .expect("count summaries after recipe invalidation"),
        1
    );

    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(database.pool())
        .await
        .expect("clean up fixture company");
}
