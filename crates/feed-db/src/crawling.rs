use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use feed_core::{ProcessedCrawlItem, Source};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{Database, DatabaseError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CrawlPersistSummary {
    pub item_count: i32,
    pub new_item_count: i32,
    pub normalized_item_count: i32,
    pub failed_item_count: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipeArtifactFailure {
    pub url: String,
    pub reason: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FeedItemQualityQuarantine {
    pub feed_item_id: Uuid,
    pub reason: String,
    pub policy: String,
    pub reversible: bool,
    pub metadata: Value,
}

impl Database {
    pub async fn enqueue_due_crawl_jobs(&self, now: DateTime<Utc>) -> Result<u64, DatabaseError> {
        sqlx::query(
            r#"
            UPDATE company_news_recipe_state AS state
            SET freshness_status = 'overdue'
            FROM company_news_recipes AS recipe
            WHERE
                recipe.id = state.recipe_id
                AND recipe.status = 'active'
                AND state.freshness_status <> 'content_stale'
                AND COALESCE(state.last_correct_at, recipe.verified_at, recipe.created_at)
                    + (
                        (recipe.spec #>> '{freshness,crawl_interval_seconds}')::bigint
                        * INTERVAL '1 second'
                    ) <= $1
            "#,
        )
        .bind(now)
        .execute(self.pool())
        .await?;
        let result = sqlx::query(
            r#"
            INSERT INTO jobs (
                job_type,
                job_key,
                status,
                run_after,
                source_id,
                company_id,
                payload
            )
            SELECT
                'crawl_source',
                'source:' || source.id::text,
                'pending',
                $1,
                source.id,
                source.company_id,
                jsonb_build_object('source_id', source.id)
            FROM sources AS source
            LEFT JOIN source_state AS state ON state.source_id = source.id
            WHERE
                source.status = 'approved'
                AND (
                    source.kind IN ('rss', 'atom')
                    OR (
                        source.kind IN ('html', 'browser')
                        AND EXISTS (
                            SELECT 1
                            FROM company_news_recipes AS recipe
                            LEFT JOIN company_news_recipe_state AS recipe_state
                                ON recipe_state.recipe_id = recipe.id
                            WHERE
                                recipe.source_id = source.id
                                AND recipe.status = 'active'
                                AND COALESCE(recipe_state.rebuild_required, false) = false
                        )
                    )
                )
                AND (
                    COALESCE(state.last_attempt_at, state.last_success_at) IS NULL
                    OR COALESCE(state.last_attempt_at, state.last_success_at)
                        + (source.freshness_slo_seconds::bigint * INTERVAL '1 second') <= $1
                )
                AND (state.backoff_until IS NULL OR state.backoff_until <= $1)
                AND NOT EXISTS (
                    SELECT 1
                    FROM jobs AS active_job
                    WHERE
                        active_job.job_type = 'crawl_source'
                        AND active_job.job_key = 'source:' || source.id::text
                        AND active_job.status IN ('pending', 'running')
                )
            ON CONFLICT (job_type, job_key)
                WHERE status IN ('pending', 'running')
            DO NOTHING
            "#,
        )
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn begin_crawl_run(
        &self,
        source_id: Uuid,
        job_id: Uuid,
    ) -> Result<Uuid, DatabaseError> {
        let mut transaction = self.pool().begin().await?;
        // A worker can disappear after creating an attempt but before it closes
        // the audit rows. When the job is claimed again, preserve that attempt
        // as cancelled instead of leaving an impossible, permanently-running
        // crawl (or reusing its recipe-run row for the new attempt).
        sqlx::query(
            r#"
            UPDATE company_news_recipe_runs
            SET
                status = 'cancelled',
                finished_at = CURRENT_TIMESTAMP,
                error = 'superseded by a newer crawl attempt for the same job',
                metadata = metadata || jsonb_build_object(
                    'cancellation_reason',
                    'superseded by a newer crawl attempt for the same job'
                )
            WHERE job_id = $1 AND status = 'running'
            "#,
        )
        .bind(job_id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            UPDATE crawl_runs
            SET
                status = 'cancelled',
                finished_at = CURRENT_TIMESTAMP,
                error = 'superseded by a newer crawl attempt for the same job',
                metadata = metadata || jsonb_build_object(
                    'cancellation_reason',
                    'superseded by a newer crawl attempt for the same job'
                )
            WHERE job_id = $1 AND status = 'running'
            "#,
        )
        .bind(job_id)
        .execute(&mut *transaction)
        .await?;
        let run_id = sqlx::query_scalar(
            r#"
            INSERT INTO crawl_runs (source_id, job_id, status)
            VALUES ($1, $2, 'running')
            RETURNING id
            "#,
        )
        .bind(source_id)
        .bind(job_id)
        .fetch_one(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO source_state (source_id, last_attempt_at)
            VALUES ($1, CURRENT_TIMESTAMP)
            ON CONFLICT (source_id) DO UPDATE
            SET last_attempt_at = CURRENT_TIMESTAMP
            "#,
        )
        .bind(source_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(run_id)
    }

    pub async fn quarantine_recipe_artifact_failures(
        &self,
        source: &Source,
        recipe_id: Uuid,
        recipe_run_id: Uuid,
        failures: &[RecipeArtifactFailure],
    ) -> Result<u64, DatabaseError> {
        let candidates = failures
            .iter()
            .filter(|failure| {
                !failure.retryable && is_runtime_recipe_quarantine_reason(&failure.reason)
            })
            .map(|failure| (failure.url.clone(), failure.reason.clone()))
            .collect::<BTreeMap<_, _>>();
        if candidates.is_empty() {
            return Ok(0);
        }
        let (urls, reasons): (Vec<_>, Vec<_>) = candidates.into_iter().unzip();
        let mut transaction = self.pool().begin().await?;
        let matches = sqlx::query_as::<
            _,
            (
                Uuid,
                Option<Uuid>,
                String,
                String,
                String,
                Option<DateTime<Utc>>,
                String,
                String,
            ),
        >(
            r#"
            SELECT DISTINCT ON (item.id)
                item.id,
                item.raw_crawl_item_id,
                item.url,
                item.canonical_url,
                item.title,
                item.published_at,
                candidate.url,
                candidate.reason
            FROM feed_items AS item
            JOIN unnest($2::text[], $3::text[]) AS candidate(url, reason) ON
                public_url_identity_key(item.canonical_url)
                    = public_url_identity_key(candidate.url)
                OR public_url_identity_key(item.url)
                    = public_url_identity_key(candidate.url)
                OR public_url_identity_key(item.external_id)
                    = public_url_identity_key(candidate.url)
            WHERE item.source_id = $1 AND NOT item.is_private
            ORDER BY item.id, candidate.url, candidate.reason
            "#,
        )
        .bind(source.id)
        .bind(&urls)
        .bind(&reasons)
        .fetch_all(&mut *transaction)
        .await?;
        let detected_at = Utc::now();
        let mut quarantined_count = 0_u64;
        for (
            feed_item_id,
            raw_crawl_item_id,
            item_url,
            canonical_url,
            title,
            published_at,
            trigger_url,
            reason,
        ) in matches
        {
            let policy = match reason.as_str() {
                "article_not_company_scoped" => "company-scope-relevance.v4",
                "cross_company_item_not_explicitly_scoped" => "cross-company-item-scope.v1",
                _ => "recipe-runtime-artifact.v1",
            };
            let quarantine = json!({
                "state": "quarantined",
                "reason": reason,
                "original_published_at": published_at,
                "reversible": true,
                "policy": policy,
                "recipe_id": recipe_id,
                "recipe_run_id": recipe_run_id,
                "trigger_url": trigger_url,
                "quarantined_at": detected_at,
            });
            let update = sqlx::query(
                r#"
                UPDATE feed_items
                SET
                    is_private = true,
                    content_processing = jsonb_set(
                        content_processing,
                        '{quality_quarantine}',
                        $2,
                        true
                    )
                WHERE id = $1 AND NOT is_private
                "#,
            )
            .bind(feed_item_id)
            .bind(&quarantine)
            .execute(&mut *transaction)
            .await?;
            if update.rows_affected() != 1 {
                continue;
            }
            if let Some(raw_crawl_item_id) = raw_crawl_item_id {
                sqlx::query(
                    r#"
                    UPDATE raw_crawl_items
                    SET
                        processing_status = 'skipped',
                        normalization_error =
                            'quality quarantine: ' || $2 || ' (' || $3 || ')',
                        normalized_feed_item_id = NULL
                    WHERE id = $1
                    "#,
                )
                .bind(raw_crawl_item_id)
                .bind(&reason)
                .bind(policy)
                .execute(&mut *transaction)
                .await?;
            }
            sqlx::query(
                r#"
                INSERT INTO event_log (event_type, company_id, source_id, payload)
                VALUES ('feed_item.quality_quarantined', $1, $2, $3)
                "#,
            )
            .bind(source.company_id)
            .bind(source.id)
            .bind(json!({
                "feed_item_id": feed_item_id,
                "raw_crawl_item_id": raw_crawl_item_id,
                "url": item_url,
                "canonical_url": canonical_url,
                "title": title,
                "published_at": published_at,
                "reason": reason,
                "reversible": true,
                "policy": policy,
                "recipe_id": recipe_id,
                "recipe_run_id": recipe_run_id,
                "trigger_url": trigger_url,
                "detected_at": detected_at,
                "trigger": "runtime_recipe_crawl",
            }))
            .execute(&mut *transaction)
            .await?;
            quarantined_count += 1;
        }
        transaction.commit().await?;
        Ok(quarantined_count)
    }

    pub async fn quarantine_feed_items_for_quality(
        &self,
        quarantines: &[FeedItemQualityQuarantine],
    ) -> Result<u64, DatabaseError> {
        let mut candidates = BTreeMap::<Uuid, FeedItemQualityQuarantine>::new();
        for quarantine in quarantines {
            if quarantine.reason.trim().is_empty() || quarantine.policy.trim().is_empty() {
                return Err(DatabaseError::InvalidState(
                    "feed item quality quarantine reason and policy cannot be blank".to_owned(),
                ));
            }
            candidates
                .entry(quarantine.feed_item_id)
                .or_insert_with(|| quarantine.clone());
        }
        if candidates.is_empty() {
            return Ok(0);
        }

        let mut transaction = self.pool().begin().await?;
        let detected_at = Utc::now();
        let mut quarantined_count = 0_u64;
        for quarantine in candidates.into_values() {
            let item = sqlx::query_as::<
                _,
                (
                    Option<Uuid>,
                    Uuid,
                    Uuid,
                    String,
                    String,
                    String,
                    Option<DateTime<Utc>>,
                ),
            >(
                r#"
                SELECT
                    raw_crawl_item_id,
                    company_id,
                    source_id,
                    url,
                    canonical_url,
                    title,
                    published_at
                FROM feed_items
                WHERE id = $1 AND NOT is_private
                FOR UPDATE
                "#,
            )
            .bind(quarantine.feed_item_id)
            .fetch_optional(&mut *transaction)
            .await?;
            let Some((
                raw_crawl_item_id,
                company_id,
                source_id,
                item_url,
                canonical_url,
                title,
                published_at,
            )) = item
            else {
                continue;
            };

            let quality_quarantine = json!({
                "state": "quarantined",
                "reason": quarantine.reason,
                "original_published_at": published_at,
                "reversible": quarantine.reversible,
                "policy": quarantine.policy,
                "metadata": quarantine.metadata,
                "quarantined_at": detected_at,
            });
            let update = sqlx::query(
                r#"
                UPDATE feed_items
                SET
                    is_private = true,
                    content_processing = jsonb_set(
                        content_processing,
                        '{quality_quarantine}',
                        $2,
                        true
                    )
                WHERE id = $1 AND NOT is_private
                "#,
            )
            .bind(quarantine.feed_item_id)
            .bind(&quality_quarantine)
            .execute(&mut *transaction)
            .await?;
            if update.rows_affected() != 1 {
                continue;
            }

            if let Some(raw_crawl_item_id) = raw_crawl_item_id {
                sqlx::query(
                    r#"
                    UPDATE raw_crawl_items
                    SET
                        processing_status = 'skipped',
                        normalization_error =
                            'quality quarantine: ' || $2 || ' (' || $3 || ')',
                        normalized_feed_item_id = NULL
                    WHERE id = $1
                    "#,
                )
                .bind(raw_crawl_item_id)
                .bind(&quarantine.reason)
                .bind(&quarantine.policy)
                .execute(&mut *transaction)
                .await?;
            }

            sqlx::query(
                r#"
                INSERT INTO event_log (event_type, company_id, source_id, payload)
                VALUES ('feed_item.quality_quarantined', $1, $2, $3)
                "#,
            )
            .bind(company_id)
            .bind(source_id)
            .bind(json!({
                "feed_item_id": quarantine.feed_item_id,
                "raw_crawl_item_id": raw_crawl_item_id,
                "url": item_url,
                "canonical_url": canonical_url,
                "title": title,
                "published_at": published_at,
                "reason": quarantine.reason,
                "reversible": quarantine.reversible,
                "policy": quarantine.policy,
                "metadata": quarantine.metadata,
                "detected_at": detected_at,
                "trigger": "cross_company_item_scope_audit",
            }))
            .execute(&mut *transaction)
            .await?;
            quarantined_count += 1;
        }
        transaction.commit().await?;
        Ok(quarantined_count)
    }

    pub async fn complete_crawl_run(
        &self,
        run_id: Uuid,
        source: &Source,
        fetched_at: DateTime<Utc>,
        items: &[ProcessedCrawlItem],
        metadata: Value,
    ) -> Result<CrawlPersistSummary, DatabaseError> {
        let mut transaction = self.pool().begin().await?;
        let mut summary = CrawlPersistSummary {
            item_count: i32::try_from(items.len())
                .map_err(|_| DatabaseError::Invariant("crawl item count exceeds i32".to_owned()))?,
            new_item_count: 0,
            normalized_item_count: 0,
            failed_item_count: 0,
        };

        sqlx::query(
            r#"
            UPDATE sources
            SET kind = $2
            WHERE id = $1 AND kind <> $2
            "#,
        )
        .bind(source.id)
        .bind(source.kind.as_str())
        .execute(&mut *transaction)
        .await?;

        for processed in items {
            let raw = &processed.raw;
            let raw_payload = serde_json::to_value(raw)?;
            let (processing_status, normalization_error) = match &processed.normalized {
                Ok(_) => ("pending", None),
                Err(error) => ("failed", Some(error.as_str())),
            };
            let raw_id = sqlx::query_scalar::<_, Uuid>(
                r#"
                INSERT INTO raw_crawl_items (
                    crawl_run_id,
                    source_id,
                    source_item_key,
                    external_id,
                    fetched_url,
                    canonical_url,
                    payload,
                    fetched_at,
                    processing_status,
                    normalization_attempt_count,
                    normalization_error
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 1, $10)
                ON CONFLICT (crawl_run_id, source_item_key) DO UPDATE
                SET
                    payload = EXCLUDED.payload,
                    processing_status = EXCLUDED.processing_status,
                    normalization_attempt_count =
                        raw_crawl_items.normalization_attempt_count + 1,
                    normalization_error = EXCLUDED.normalization_error
                RETURNING id
                "#,
            )
            .bind(run_id)
            .bind(source.id)
            .bind(&raw.source_item_key)
            .bind(&raw.external_id)
            .bind(raw.url.as_str())
            .bind(raw.canonical_url.as_ref().map(|url| url.as_str()))
            .bind(raw_payload)
            .bind(fetched_at)
            .bind(processing_status)
            .bind(normalization_error)
            .fetch_one(&mut *transaction)
            .await?;

            let Ok(normalized) = &processed.normalized else {
                summary.failed_item_count += 1;
                continue;
            };
            let inserted_id = sqlx::query_scalar::<_, Uuid>(
                r#"
                INSERT INTO feed_items (
                    company_id,
                    source_id,
                    raw_crawl_item_id,
                    external_id,
                    url,
                    canonical_url,
                    title,
                    summary,
                    body_text,
                    body_html,
                    body_markdown,
                    published_at,
                    fetched_at,
                    content_hash,
                    source_kind,
                    raw,
                    normalized,
                    content_processing
                )
                VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9,
                    $10, $11, $12, $13, $14, $15, $16, $17, $18
                )
                ON CONFLICT DO NOTHING
                RETURNING id
                "#,
            )
            .bind(source.company_id)
            .bind(source.id)
            .bind(raw_id)
            .bind(&normalized.external_id)
            .bind(normalized.url.as_str())
            .bind(normalized.canonical_url.as_str())
            .bind(&normalized.title)
            .bind(&normalized.summary)
            .bind(&normalized.body_text)
            .bind(&normalized.body_html)
            .bind(&normalized.body_markdown)
            .bind(normalized.published_at)
            .bind(normalized.fetched_at)
            .bind(&normalized.content_hash)
            .bind(normalized.source_kind.as_str())
            .bind(&normalized.raw)
            .bind(&normalized.normalized)
            .bind(&normalized.content_processing)
            .fetch_optional(&mut *transaction)
            .await?;

            let feed_item_id = if let Some(feed_item_id) = inserted_id {
                summary.new_item_count += 1;
                feed_item_id
            } else {
                let (existing_id, existing_is_private, existing_content_processing) =
                    sqlx::query_as::<_, (Uuid, bool, Value)>(
                        r#"
                    SELECT id, is_private, content_processing
                    FROM feed_items
                    WHERE
                        source_id = $1
                        AND (
                            external_id = $2
                            OR canonical_url = $3
                            OR public_url_identity_key(url)
                                = public_url_identity_key($4)
                        )
                    -- A canonicalization policy can intentionally collapse a
                    -- legacy external-ID row onto an identity already owned by
                    -- another row. A public row that already owns the fetched
                    -- URL identity takes precedence; otherwise prefer the
                    -- canonical owner. This keeps CMS origin-canonical drift
                    -- from creating a second public item for one article.
                    ORDER BY
                        (
                            NOT is_private
                            AND public_url_identity_key(url)
                                = public_url_identity_key($4)
                        ) DESC,
                        (canonical_url = $3) DESC,
                        (external_id = $2) DESC,
                        created_at
                    LIMIT 1
                    FOR UPDATE
                    "#,
                    )
                    .bind(source.id)
                    .bind(&normalized.external_id)
                    .bind(normalized.canonical_url.as_str())
                    .bind(normalized.url.as_str())
                    .fetch_optional(&mut *transaction)
                    .await?
                    .ok_or_else(|| {
                        DatabaseError::Invariant(format!(
                            "feed item conflict for source {} could not be identified",
                            source.id
                        ))
                    })?;
                let preserve_hydrated_content =
                    existing_content_processing.get("content_crawl").is_some();
                let released_quarantine = existing_is_private
                    .then(|| releasable_quality_quarantine_reason(&existing_content_processing))
                    .flatten()
                    .map(|(policy, reason)| (policy.to_owned(), reason.to_owned()));
                sqlx::query(
                    r#"
                    UPDATE feed_items
                    SET
                        raw_crawl_item_id = $2,
                        external_id = CASE
                            WHEN EXISTS (
                                SELECT 1
                                FROM feed_items AS external_owner
                                WHERE
                                    external_owner.source_id =
                                        feed_items.source_id
                                    AND external_owner.external_id = $3
                                    AND external_owner.id <> feed_items.id
                            )
                            THEN feed_items.external_id
                            ELSE $3
                        END,
                        url = $4,
                        canonical_url = CASE
                            WHEN EXISTS (
                                SELECT 1
                                FROM feed_items AS canonical_owner
                                WHERE
                                    canonical_owner.source_id =
                                        feed_items.source_id
                                    AND canonical_owner.canonical_url = $5
                                    AND canonical_owner.id <> feed_items.id
                            )
                            THEN feed_items.canonical_url
                            ELSE $5
                        END,
                        title = $6,
                        summary = CASE
                            WHEN $19 THEN summary
                            ELSE $7
                        END,
                        body_text = CASE
                            WHEN $19 THEN body_text
                            ELSE $8
                        END,
                        body_html = CASE
                            WHEN $19 THEN body_html
                            ELSE $9
                        END,
                        body_markdown = CASE
                            WHEN $19 THEN body_markdown
                            ELSE $10
                        END,
                        published_at = $11,
                        fetched_at = CASE
                            WHEN $19 THEN fetched_at
                            ELSE $12
                        END,
                        content_hash = CASE
                            WHEN $19 THEN content_hash
                            ELSE $13
                        END,
                        source_kind = $14,
                        raw = $15,
                        normalized = $16,
                        content_processing =
                            $17
                            || CASE
                                WHEN $19
                                THEN jsonb_build_object(
                                    'content_crawl',
                                    content_processing -> 'content_crawl'
                                )
                                ELSE '{}'::jsonb
                            END
                            || CASE
                                WHEN
                                    is_private
                                    AND NOT $18
                                    AND content_processing ? 'quality_quarantine'
                                THEN jsonb_build_object(
                                    'quality_quarantine',
                                    content_processing -> 'quality_quarantine'
                                )
                                ELSE '{}'::jsonb
                            END,
                        is_private = CASE WHEN $18 THEN false ELSE is_private END
                    WHERE id = $1
                    "#,
                )
                .bind(existing_id)
                .bind(raw_id)
                .bind(&normalized.external_id)
                .bind(normalized.url.as_str())
                .bind(normalized.canonical_url.as_str())
                .bind(&normalized.title)
                .bind(&normalized.summary)
                .bind(&normalized.body_text)
                .bind(&normalized.body_html)
                .bind(&normalized.body_markdown)
                .bind(normalized.published_at)
                .bind(normalized.fetched_at)
                .bind(&normalized.content_hash)
                .bind(normalized.source_kind.as_str())
                .bind(&normalized.raw)
                .bind(&normalized.normalized)
                .bind(&normalized.content_processing)
                .bind(released_quarantine.is_some())
                .bind(preserve_hydrated_content)
                .execute(&mut *transaction)
                .await?;
                if let Some((policy, reason)) = released_quarantine {
                    sqlx::query(
                        r#"
                        INSERT INTO event_log (event_type, company_id, source_id, payload)
                        VALUES ('feed_item.quality_released', $1, $2, $3)
                        "#,
                    )
                    .bind(source.company_id)
                    .bind(source.id)
                    .bind(json!({
                        "feed_item_id": existing_id,
                        "raw_crawl_item_id": raw_id,
                        "previous_reason": reason,
                        "policy": policy,
                        "released_by": "successful_normalization",
                    }))
                    .execute(&mut *transaction)
                    .await?;
                }
                existing_id
            };

            sqlx::query(
                r#"
                UPDATE raw_crawl_items
                SET
                    processing_status = 'normalized',
                    normalization_error = NULL,
                    normalized_feed_item_id = $2
                WHERE id = $1
                "#,
            )
            .bind(raw_id)
            .bind(feed_item_id)
            .execute(&mut *transaction)
            .await?;
            summary.normalized_item_count += 1;
        }

        let result = sqlx::query(
            r#"
            UPDATE crawl_runs
            SET
                status = 'completed',
                finished_at = CURRENT_TIMESTAMP,
                item_count = $2,
                new_item_count = $3,
                metadata = $4
            WHERE id = $1 AND status = 'running'
            "#,
        )
        .bind(run_id)
        .bind(summary.item_count)
        .bind(summary.new_item_count)
        .bind(metadata)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            return Err(DatabaseError::NotFound {
                entity: "running crawl run",
                id: run_id,
            });
        }
        sqlx::query(
            r#"
            INSERT INTO source_state (
                source_id,
                last_attempt_at,
                last_success_at,
                consecutive_failures,
                backoff_until,
                consecutive_zero_runs,
                total_successful_runs,
                total_items,
                last_nonzero_at,
                cursor
            )
            VALUES (
                $1,
                CURRENT_TIMESTAMP,
                CURRENT_TIMESTAMP,
                0,
                NULL,
                CASE WHEN $2 = 0 THEN 1 ELSE 0 END,
                1,
                $2,
                CASE WHEN $2 > 0 THEN CURRENT_TIMESTAMP ELSE NULL END,
                $3
            )
            ON CONFLICT (source_id) DO UPDATE
            SET
                last_attempt_at = CURRENT_TIMESTAMP,
                last_success_at = CURRENT_TIMESTAMP,
                last_error = NULL,
                consecutive_failures = 0,
                backoff_until = NULL,
                consecutive_zero_runs = CASE
                    WHEN $2 = 0 THEN source_state.consecutive_zero_runs + 1
                    ELSE 0
                END,
                total_successful_runs = source_state.total_successful_runs + 1,
                total_items = source_state.total_items + $2,
                last_nonzero_at = CASE
                    WHEN $2 > 0 THEN CURRENT_TIMESTAMP
                    ELSE source_state.last_nonzero_at
                END,
                cursor = $3
            "#,
        )
        .bind(source.id)
        .bind(summary.item_count)
        .bind(json!({ "last_fetched_at": fetched_at }))
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO event_log (event_type, company_id, source_id, payload)
            VALUES ('crawl.completed', $1, $2, $3)
            "#,
        )
        .bind(source.company_id)
        .bind(source.id)
        .bind(json!({
            "run_id": run_id,
            "item_count": summary.item_count,
            "new_item_count": summary.new_item_count,
            "normalized_item_count": summary.normalized_item_count,
            "failed_item_count": summary.failed_item_count,
        }))
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;
        Ok(summary)
    }

    pub async fn cancel_running_crawl_runs_for_job(
        &self,
        job_id: Uuid,
        lease_token: Uuid,
        reason: &str,
    ) -> Result<u64, DatabaseError> {
        let mut transaction = self.pool().begin().await?;
        let recipe_runs = sqlx::query(
            r#"
            UPDATE company_news_recipe_runs
            SET
                status = 'cancelled',
                finished_at = CURRENT_TIMESTAMP,
                error = $3,
                metadata = metadata || jsonb_build_object(
                    'cancellation_reason',
                    $3
                )
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
            "#,
        )
        .bind(job_id)
        .bind(lease_token)
        .bind(reason)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        let crawl_runs = sqlx::query(
            r#"
            UPDATE crawl_runs
            SET
                status = 'cancelled',
                finished_at = CURRENT_TIMESTAMP,
                error = $3,
                metadata = metadata || jsonb_build_object(
                    'cancellation_reason',
                    $3
                )
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
            "#,
        )
        .bind(job_id)
        .bind(lease_token)
        .bind(reason)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        transaction.commit().await?;
        Ok(recipe_runs + crawl_runs)
    }

    pub async fn fail_crawl_run(
        &self,
        run_id: Uuid,
        source: &Source,
        error: &str,
    ) -> Result<(), DatabaseError> {
        let mut transaction = self.pool().begin().await?;
        let result = sqlx::query(
            r#"
            UPDATE crawl_runs
            SET
                status = 'failed',
                finished_at = CURRENT_TIMESTAMP,
                error = $2
            WHERE id = $1 AND status = 'running'
            "#,
        )
        .bind(run_id)
        .bind(error)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            return Err(DatabaseError::NotFound {
                entity: "running crawl run",
                id: run_id,
            });
        }
        sqlx::query(
            r#"
            INSERT INTO source_state (
                source_id,
                last_attempt_at,
                last_error,
                consecutive_failures,
                backoff_until
            )
            VALUES (
                $1,
                CURRENT_TIMESTAMP,
                $2,
                1,
                CURRENT_TIMESTAMP + INTERVAL '2 minutes'
            )
            ON CONFLICT (source_id) DO UPDATE
            SET
                last_attempt_at = CURRENT_TIMESTAMP,
                last_error = $2,
                backoff_until = CURRENT_TIMESTAMP
                    + (
                        LEAST(
                            3600,
                            120 * power(2, LEAST(source_state.consecutive_failures, 5))::integer
                        )
                        * INTERVAL '1 second'
                    ),
                consecutive_failures = source_state.consecutive_failures + 1
            "#,
        )
        .bind(source.id)
        .bind(error)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO event_log (event_type, company_id, source_id, payload)
            VALUES ('crawl.failed', $1, $2, $3)
            "#,
        )
        .bind(source.company_id)
        .bind(source.id)
        .bind(json!({ "run_id": run_id, "error": error }))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }
}

fn releasable_quality_quarantine_reason(content_processing: &Value) -> Option<(&str, &str)> {
    let quarantine = content_processing.get("quality_quarantine")?;
    let policy = quarantine.get("policy")?.as_str()?;
    if let Some(version) = policy.strip_prefix("recipe-listing-artifact.v") {
        if version.is_empty() || !version.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
    } else if let Some(version) = policy.strip_prefix("recipe-content-diversity.v") {
        if version.is_empty() || !version.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
    } else if let Some(version) = policy.strip_prefix("cross-company-item-scope.v") {
        if version.is_empty() || !version.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
    } else if !matches!(
        policy,
        "recipe-runtime-artifact.v1" | "shared-direct-scope.v1" | "shared-direct-scope.v2"
    ) {
        return None;
    }
    Some((policy, quarantine.get("reason").and_then(Value::as_str)?))
}

fn is_replay_safe_recipe_artifact_reason(reason: &str) -> bool {
    matches!(
        reason,
        "article_outside_dominant_editorial_namespace"
            | "article_outside_recipe_scope"
            | "double_encoded_utf8_url_alias"
            | "generic_listing_title"
            | "high_link_density_collection"
            | "multiple_article_collection"
            | "obvious_listing_path"
            | "year_archive_collection"
    )
}

fn is_runtime_recipe_quarantine_reason(reason: &str) -> bool {
    matches!(
        reason,
        "article_not_company_scoped" | "cross_company_item_not_explicitly_scoped"
    ) || is_replay_safe_recipe_artifact_reason(reason)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        is_replay_safe_recipe_artifact_reason, is_runtime_recipe_quarantine_reason,
        releasable_quality_quarantine_reason,
    };

    #[test]
    fn identifies_only_replay_safe_quality_quarantines() {
        assert_eq!(
            releasable_quality_quarantine_reason(&json!({
                "quality_quarantine": {
                    "policy": "recipe-listing-artifact.v1",
                    "reason": "generic_listing_title"
                }
            })),
            Some(("recipe-listing-artifact.v1", "generic_listing_title"))
        );
        assert_eq!(
            releasable_quality_quarantine_reason(&json!({
                "quality_quarantine": {
                    "policy": "recipe-listing-artifact.v2",
                    "reason": "list_collection_path"
                }
            })),
            Some(("recipe-listing-artifact.v2", "list_collection_path"))
        );
        assert_eq!(
            releasable_quality_quarantine_reason(&json!({
                "quality_quarantine": {
                    "policy": "recipe-runtime-artifact.v1",
                    "reason": "multiple_article_collection"
                }
            })),
            Some(("recipe-runtime-artifact.v1", "multiple_article_collection"))
        );
        assert_eq!(
            releasable_quality_quarantine_reason(&json!({
                "quality_quarantine": {
                    "policy": "shared-direct-scope.v1",
                    "reason": "shared_host_manual_item_requires_revalidation"
                }
            })),
            Some((
                "shared-direct-scope.v1",
                "shared_host_manual_item_requires_revalidation"
            ))
        );
        assert_eq!(
            releasable_quality_quarantine_reason(&json!({
                "quality_quarantine": {
                    "policy": "cross-company-item-scope.v1",
                    "reason": "cross_company_item_not_explicitly_scoped"
                }
            })),
            Some((
                "cross-company-item-scope.v1",
                "cross_company_item_not_explicitly_scoped"
            ))
        );
        assert_eq!(
            releasable_quality_quarantine_reason(&json!({
                "quality_quarantine": {
                    "policy": "recipe-content-diversity.v2",
                    "reason": "repeated_sanitized_content"
                }
            })),
            Some(("recipe-content-diversity.v2", "repeated_sanitized_content"))
        );
        assert_eq!(
            releasable_quality_quarantine_reason(&json!({
                "quality_quarantine": {
                    "policy": "shared-direct-scope.v2",
                    "reason": "shared_host_manual_item_requires_revalidation"
                }
            })),
            Some((
                "shared-direct-scope.v2",
                "shared_host_manual_item_requires_revalidation"
            ))
        );
        assert_eq!(
            releasable_quality_quarantine_reason(&json!({
                "quality_quarantine": {
                    "policy": "recipe-content-diversity.v1",
                    "reason": "content_diversity_below_minimum"
                }
            })),
            Some((
                "recipe-content-diversity.v1",
                "content_diversity_below_minimum"
            ))
        );
        assert_eq!(
            releasable_quality_quarantine_reason(&json!({
                "quality_quarantine": {
                    "policy": "another-policy.v1",
                    "reason": "manual"
                }
            })),
            None
        );
    }

    #[test]
    fn limits_runtime_quarantine_to_deterministic_listing_failures() {
        for reason in [
            "article_outside_dominant_editorial_namespace",
            "article_outside_recipe_scope",
            "double_encoded_utf8_url_alias",
            "generic_listing_title",
            "high_link_density_collection",
            "multiple_article_collection",
            "obvious_listing_path",
            "year_archive_collection",
        ] {
            assert!(is_replay_safe_recipe_artifact_reason(reason), "{reason}");
        }
        assert!(is_runtime_recipe_quarantine_reason(
            "article_not_company_scoped"
        ));
        assert!(is_runtime_recipe_quarantine_reason(
            "cross_company_item_not_explicitly_scoped"
        ));
        assert!(!is_replay_safe_recipe_artifact_reason(
            "article_not_company_scoped"
        ));
        for reason in [
            "http_status",
            "insufficient_content",
            "missing_article_body",
            "missing_article_signal",
            "request_failed",
        ] {
            assert!(!is_replay_safe_recipe_artifact_reason(reason), "{reason}");
        }
    }
}
