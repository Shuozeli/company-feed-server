use std::time::Duration;

use chrono::{DateTime, Utc};
use feed_core::NormalizedFeedItem;
use serde::Serialize;
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

use crate::{Database, DatabaseError};

pub const CONTENT_EXTRACTION_VERSION: &str = "generic-public-article.v1";
const CONTENT_CRAWL_CLAIM_LOCK_KEY: i64 = 0x4346_434F_4E54_454E;

#[derive(Clone, Debug, PartialEq)]
pub struct ContentCrawlCandidate {
    pub attempt_id: Uuid,
    pub feed_item_id: Uuid,
    pub source_id: Uuid,
    pub url: Url,
    pub title_hint: String,
    pub published_at_hint: Option<DateTime<Utc>>,
    pub attempt_count: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContentCrawlSuccess {
    pub attempt_id: Uuid,
    pub feed_item_id: Uuid,
    pub requested_url: Url,
    pub normalized: NormalizedFeedItem,
    pub extraction_metadata: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentCrawlFailure {
    pub attempt_id: Uuid,
    pub feed_item_id: Uuid,
    pub requested_url: Url,
    pub reason: String,
    pub retryable: bool,
    pub error: String,
    pub http_status: Option<i32>,
    pub next_attempt_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ContentCrawlCoverage {
    pub eligible_items: i64,
    pub never_attempted: i64,
    pub running: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub permanent_failure: i64,
    pub with_body: i64,
    pub with_substantive_body: i64,
}

impl Database {
    pub async fn enqueue_due_content_crawl_job(
        &self,
        now: DateTime<Utc>,
        refresh_before: DateTime<Utc>,
        job_concurrency: i32,
    ) -> Result<u64, DatabaseError> {
        let result = sqlx::query(
            r#"
            INSERT INTO jobs (
                job_type,
                job_key,
                status,
                run_after,
                payload
            )
            SELECT
                'crawl_content',
                'content-crawl:' || gen_random_uuid()::text,
                'pending',
                $1,
                jsonb_build_object(
                    'contract', 'article-content-crawl.v1',
                    'scheduled_at', $1
                )
            WHERE
                EXISTS (
                    SELECT 1
                    FROM feed_items AS item
                    JOIN sources AS source ON source.id = item.source_id
                    LEFT JOIN content_crawl_state AS state
                        ON state.feed_item_id = item.id
                    WHERE
                        NOT item.is_private
                        AND source.status = 'approved'
                        AND (
                            item.published_at IS NULL
                            OR item.published_at <= $1
                        )
                        AND (
                            source.kind IN ('rss', 'atom')
                            OR EXISTS (
                                SELECT 1
                                FROM company_news_recipes AS recipe
                                LEFT JOIN company_news_recipe_state AS recipe_state
                                    ON recipe_state.recipe_id = recipe.id
                                WHERE
                                    recipe.source_id = source.id
                                    AND recipe.status = 'active'
                                    AND NOT COALESCE(
                                        recipe_state.rebuild_required,
                                        false
                                    )
                            )
                        )
                        AND (
                            state.feed_item_id IS NULL
                            OR (
                                state.status IN ('pending', 'failed')
                                AND (
                                    state.next_attempt_at IS NULL
                                    OR state.next_attempt_at <= $1
                                )
                            )
                            OR (
                                state.status = 'succeeded'
                                AND state.last_success_at <= $2
                            )
                        )
                )
                AND (
                    SELECT count(*)
                    FROM jobs AS active
                    WHERE
                        active.job_type = 'crawl_content'
                        AND active.status IN ('pending', 'running')
                ) < $3
            ON CONFLICT (job_type, job_key)
                WHERE status IN ('pending', 'running')
            DO NOTHING
            "#,
        )
        .bind(now)
        .bind(refresh_before)
        .bind(job_concurrency)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn begin_content_crawl_batch(
        &self,
        job_id: Uuid,
        now: DateTime<Utc>,
        refresh_before: DateTime<Utc>,
        batch_size: i64,
    ) -> Result<Vec<ContentCrawlCandidate>, DatabaseError> {
        let mut transaction = self.pool().begin().await?;
        // Concurrent runners must see content_crawl_state rows committed by
        // the preceding selector. Row locks alone are insufficient because a
        // READ COMMITTED statement can identify absent-state rows before a
        // competing transaction inserts their state records.
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(CONTENT_CRAWL_CLAIM_LOCK_KEY)
            .execute(&mut *transaction)
            .await?;
        let superseded = sqlx::query_scalar::<_, Uuid>(
            r#"
            UPDATE content_crawl_attempts
            SET
                status = 'cancelled',
                finished_at = $2,
                retryable = true,
                error_reason = 'superseded_attempt',
                error = 'superseded by a newer attempt for the same job'
            WHERE job_id = $1 AND status = 'running'
            RETURNING feed_item_id
            "#,
        )
        .bind(job_id)
        .bind(now)
        .fetch_all(&mut *transaction)
        .await?;
        if !superseded.is_empty() {
            sqlx::query(
                r#"
                UPDATE content_crawl_state
                SET
                    status = 'failed',
                    next_attempt_at = $2,
                    last_error_reason = 'superseded_attempt',
                    last_error =
                        'superseded by a newer attempt for the same job'
                WHERE feed_item_id = ANY($1)
                  AND status = 'running'
                "#,
            )
            .bind(&superseded)
            .bind(now)
            .execute(&mut *transaction)
            .await?;
        }

        let rows = sqlx::query_as::<_, (Uuid, Uuid, String, String, Option<DateTime<Utc>>)>(
            r#"
            SELECT
                item.id,
                item.source_id,
                item.url,
                item.title,
                item.published_at
            FROM feed_items AS item
            JOIN sources AS source ON source.id = item.source_id
            LEFT JOIN content_crawl_state AS state
                ON state.feed_item_id = item.id
            WHERE
                NOT item.is_private
                AND source.status = 'approved'
                AND (
                    item.published_at IS NULL
                    OR item.published_at <= $1
                )
                AND (
                    source.kind IN ('rss', 'atom')
                    OR EXISTS (
                        SELECT 1
                        FROM company_news_recipes AS recipe
                        LEFT JOIN company_news_recipe_state AS recipe_state
                            ON recipe_state.recipe_id = recipe.id
                        WHERE
                            recipe.source_id = source.id
                            AND recipe.status = 'active'
                            AND NOT COALESCE(
                                recipe_state.rebuild_required,
                                false
                            )
                    )
                )
                AND (
                    state.feed_item_id IS NULL
                    OR (
                        state.status IN ('pending', 'failed')
                        AND (
                            state.next_attempt_at IS NULL
                            OR state.next_attempt_at <= $1
                        )
                    )
                    OR (
                        state.status = 'succeeded'
                        AND state.last_success_at <= $2
                    )
                )
            ORDER BY
                CASE
                    WHEN state.feed_item_id IS NULL THEN 0
                    WHEN state.status = 'failed' THEN 1
                    ELSE 2
                END,
                COALESCE(item.published_at, item.created_at) DESC,
                item.id
            LIMIT $3
            FOR UPDATE OF item SKIP LOCKED
            "#,
        )
        .bind(now)
        .bind(refresh_before)
        .bind(batch_size)
        .fetch_all(&mut *transaction)
        .await?;

        let mut candidates = Vec::with_capacity(rows.len());
        for (feed_item_id, source_id, url, title_hint, published_at_hint) in rows {
            let attempt_id = sqlx::query_scalar::<_, Uuid>(
                r#"
                INSERT INTO content_crawl_attempts (
                    feed_item_id,
                    job_id,
                    status,
                    requested_url,
                    started_at,
                    metadata
                )
                VALUES (
                    $1,
                    $2,
                    'running',
                    $3,
                    $4,
                    jsonb_build_object(
                        'contract',
                        'article-content-crawl.v1'
                    )
                )
                RETURNING id
                "#,
            )
            .bind(feed_item_id)
            .bind(job_id)
            .bind(&url)
            .bind(now)
            .fetch_one(&mut *transaction)
            .await?;
            let attempt_count = sqlx::query_scalar::<_, i32>(
                r#"
                INSERT INTO content_crawl_state (
                    feed_item_id,
                    status,
                    last_attempt_at,
                    next_attempt_at,
                    attempt_count,
                    consecutive_failures
                )
                VALUES ($1, 'running', $2, NULL, 1, 0)
                ON CONFLICT (feed_item_id) DO UPDATE
                SET
                    status = 'running',
                    last_attempt_at = EXCLUDED.last_attempt_at,
                    next_attempt_at = NULL,
                    attempt_count =
                        content_crawl_state.attempt_count + 1,
                    last_error_reason = NULL,
                    last_error = NULL,
                    last_http_status = NULL
                RETURNING attempt_count
                "#,
            )
            .bind(feed_item_id)
            .bind(now)
            .fetch_one(&mut *transaction)
            .await?;
            candidates.push(ContentCrawlCandidate {
                attempt_id,
                feed_item_id,
                source_id,
                url: Url::parse(&url)?,
                title_hint,
                published_at_hint,
                attempt_count,
            });
        }

        transaction.commit().await?;
        Ok(candidates)
    }

    pub async fn complete_content_crawl_success(
        &self,
        success: &ContentCrawlSuccess,
        refresh_after: Duration,
    ) -> Result<(), DatabaseError> {
        let refresh_after = chrono::Duration::from_std(refresh_after)
            .map_err(|_| DatabaseError::InvalidDuration(refresh_after))?;
        let completed_at = success.normalized.fetched_at;
        let next_attempt_at = completed_at + refresh_after;
        let content_chars =
            i32::try_from(success.normalized.body_text.chars().count()).map_err(|_| {
                DatabaseError::Invariant("article content length exceeds i32".to_owned())
            })?;
        let crawl_metadata = json!({
            "contract": "article-content-crawl.v1",
            "extraction_version": CONTENT_EXTRACTION_VERSION,
            "attempt_id": success.attempt_id,
            "requested_url": success.requested_url,
            "final_url": success.normalized.url,
            "canonical_url": success.normalized.canonical_url,
            "fetched_at": completed_at,
            "content_chars": content_chars,
            "extraction": success.extraction_metadata,
        });
        let mut transaction = self.pool().begin().await?;
        let attempt = sqlx::query(
            r#"
            UPDATE content_crawl_attempts
            SET
                status = 'succeeded',
                finished_at = $3,
                final_url = $4,
                canonical_url = $5,
                retryable = false,
                error_reason = NULL,
                error = NULL,
                http_status = NULL,
                content_chars = $6,
                content_hash = $7,
                metadata = metadata || $8
            WHERE id = $1
              AND feed_item_id = $2
              AND status = 'running'
            "#,
        )
        .bind(success.attempt_id)
        .bind(success.feed_item_id)
        .bind(completed_at)
        .bind(success.normalized.url.as_str())
        .bind(success.normalized.canonical_url.as_str())
        .bind(content_chars)
        .bind(&success.normalized.content_hash)
        .bind(&crawl_metadata)
        .execute(&mut *transaction)
        .await?;
        if attempt.rows_affected() != 1 {
            return Err(DatabaseError::NotFound {
                entity: "running content crawl attempt",
                id: success.attempt_id,
            });
        }

        let item = sqlx::query(
            r#"
            UPDATE feed_items
            SET
                summary = CASE
                    WHEN btrim($2) = '' THEN summary
                    ELSE $2
                END,
                body_text = CASE WHEN btrim($3) = '' THEN body_text ELSE $3 END,
                body_html = CASE WHEN btrim($3) = '' THEN body_html ELSE $4 END,
                body_markdown = CASE WHEN btrim($3) = '' THEN body_markdown ELSE $5 END,
                published_at = COALESCE(published_at, $6),
                fetched_at = $7,
                content_hash = CASE WHEN btrim($3) = '' THEN content_hash ELSE $8 END,
                content_processing =
                    content_processing
                    || $9
                    || jsonb_build_object(
                        'content_crawl',
                        $10
                    )
            WHERE id = $1 AND NOT is_private
            "#,
        )
        .bind(success.feed_item_id)
        .bind(&success.normalized.summary)
        .bind(&success.normalized.body_text)
        .bind(&success.normalized.body_html)
        .bind(&success.normalized.body_markdown)
        .bind(success.normalized.published_at)
        .bind(completed_at)
        .bind(&success.normalized.content_hash)
        .bind(&success.normalized.content_processing)
        .bind(&crawl_metadata)
        .execute(&mut *transaction)
        .await?;
        if item.rows_affected() != 1 {
            return Err(DatabaseError::NotFound {
                entity: "public feed item",
                id: success.feed_item_id,
            });
        }

        let state = sqlx::query(
            r#"
            UPDATE content_crawl_state
            SET
                status = 'succeeded',
                last_success_at = $2,
                next_attempt_at = $3,
                consecutive_failures = 0,
                last_error_reason = NULL,
                last_error = NULL,
                last_http_status = NULL,
                content_chars = $4,
                extraction_version = $5
            WHERE feed_item_id = $1 AND status = 'running'
            "#,
        )
        .bind(success.feed_item_id)
        .bind(completed_at)
        .bind(next_attempt_at)
        .bind(content_chars)
        .bind(CONTENT_EXTRACTION_VERSION)
        .execute(&mut *transaction)
        .await?;
        if state.rows_affected() != 1 {
            return Err(DatabaseError::NotFound {
                entity: "running content crawl state",
                id: success.feed_item_id,
            });
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn complete_content_crawl_failure(
        &self,
        failure: &ContentCrawlFailure,
    ) -> Result<(), DatabaseError> {
        let completed_at = Utc::now();
        let mut transaction = self.pool().begin().await?;
        let attempt = sqlx::query(
            r#"
            UPDATE content_crawl_attempts
            SET
                status = 'failed',
                finished_at = $3,
                retryable = $4,
                error_reason = $5,
                error = $6,
                http_status = $7
            WHERE id = $1
              AND feed_item_id = $2
              AND status = 'running'
            "#,
        )
        .bind(failure.attempt_id)
        .bind(failure.feed_item_id)
        .bind(completed_at)
        .bind(failure.retryable)
        .bind(&failure.reason)
        .bind(&failure.error)
        .bind(failure.http_status)
        .execute(&mut *transaction)
        .await?;
        if attempt.rows_affected() != 1 {
            return Err(DatabaseError::NotFound {
                entity: "running content crawl attempt",
                id: failure.attempt_id,
            });
        }
        let state = sqlx::query(
            r#"
            UPDATE content_crawl_state
            SET
                status = CASE
                    WHEN $2::timestamptz IS NULL
                    THEN 'permanent_failure'
                    ELSE 'failed'
                END,
                next_attempt_at = $2,
                consecutive_failures = consecutive_failures + 1,
                last_error_reason = $3,
                last_error = $4,
                last_http_status = $5
            WHERE feed_item_id = $1 AND status = 'running'
            "#,
        )
        .bind(failure.feed_item_id)
        .bind(failure.next_attempt_at)
        .bind(&failure.reason)
        .bind(&failure.error)
        .bind(failure.http_status)
        .execute(&mut *transaction)
        .await?;
        if state.rows_affected() != 1 {
            return Err(DatabaseError::NotFound {
                entity: "running content crawl state",
                id: failure.feed_item_id,
            });
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn cancel_running_content_crawl_attempts_for_job(
        &self,
        job_id: Uuid,
        lease_token: Uuid,
        reason: &str,
    ) -> Result<u64, DatabaseError> {
        let mut transaction = self.pool().begin().await?;
        let cancelled_at = Utc::now();
        let feed_item_ids = sqlx::query_scalar::<_, Uuid>(
            r#"
            UPDATE content_crawl_attempts
            SET
                status = 'cancelled',
                finished_at = $3,
                retryable = true,
                error_reason = 'job_cancelled',
                error = $4
            WHERE
                job_id = $1
                AND status = 'running'
                AND EXISTS (
                    SELECT 1
                    FROM jobs
                    WHERE
                        id = $1
                        AND lease_token = $2
                        AND status = 'running'
                )
            RETURNING feed_item_id
            "#,
        )
        .bind(job_id)
        .bind(lease_token)
        .bind(cancelled_at)
        .bind(reason)
        .fetch_all(&mut *transaction)
        .await?;
        if !feed_item_ids.is_empty() {
            sqlx::query(
                r#"
                UPDATE content_crawl_state
                SET
                    status = 'failed',
                    next_attempt_at = $2,
                    consecutive_failures = consecutive_failures + 1,
                    last_error_reason = 'job_cancelled',
                    last_error = $3
                WHERE feed_item_id = ANY($1)
                  AND status = 'running'
                "#,
            )
            .bind(&feed_item_ids)
            .bind(cancelled_at)
            .bind(reason)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(feed_item_ids.len() as u64)
    }

    pub async fn content_crawl_coverage(
        &self,
        min_substantive_chars: i32,
    ) -> Result<ContentCrawlCoverage, DatabaseError> {
        let row = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64, i64, i64)>(
            r#"
            WITH eligible AS (
                SELECT item.id, item.body_text
                FROM feed_items AS item
                JOIN sources AS source ON source.id = item.source_id
                WHERE
                    NOT item.is_private
                    AND source.status = 'approved'
                    AND (
                        item.published_at IS NULL
                        OR item.published_at <= CURRENT_TIMESTAMP
                    )
                    AND (
                        source.kind IN ('rss', 'atom')
                        OR EXISTS (
                            SELECT 1
                            FROM company_news_recipes AS recipe
                            LEFT JOIN company_news_recipe_state AS recipe_state
                                ON recipe_state.recipe_id = recipe.id
                            WHERE
                                recipe.source_id = source.id
                                AND recipe.status = 'active'
                                AND NOT COALESCE(
                                    recipe_state.rebuild_required,
                                    false
                                )
                        )
                    )
            )
            SELECT
                count(*),
                count(*) FILTER (WHERE state.feed_item_id IS NULL),
                count(*) FILTER (WHERE state.status = 'running'),
                count(*) FILTER (WHERE state.status = 'succeeded'),
                count(*) FILTER (WHERE state.status = 'failed'),
                count(*) FILTER (WHERE state.status = 'permanent_failure'),
                count(*) FILTER (
                    WHERE NULLIF(btrim(eligible.body_text), '') IS NOT NULL
                ),
                count(*) FILTER (
                    WHERE length(btrim(eligible.body_text)) >= $1
                )
            FROM eligible
            LEFT JOIN content_crawl_state AS state
                ON state.feed_item_id = eligible.id
            "#,
        )
        .bind(min_substantive_chars)
        .fetch_one(self.pool())
        .await?;
        Ok(ContentCrawlCoverage {
            eligible_items: row.0,
            never_attempted: row.1,
            running: row.2,
            succeeded: row.3,
            failed: row.4,
            permanent_failure: row.5,
            with_body: row.6,
            with_substantive_body: row.7,
        })
    }
}
