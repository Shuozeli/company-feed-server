use std::{path::PathBuf, str::FromStr};

use chrono::{DateTime, Utc};
use feed_core::{
    ExportFormat, ExportLayout, ExportRun, ExportTarget, ExportableFeedItem, ExportedItem,
    FeedItem, RunStatus, SourceKind,
};
use serde_json::{Value, json};
use sqlx::FromRow;
use url::Url;
use uuid::Uuid;

use crate::{Database, DatabaseError};

#[derive(Debug, FromRow)]
struct ExportTargetRow {
    id: Uuid,
    target_id: String,
    repo_url: String,
    local_path: String,
    branch: String,
    format: String,
    layout: String,
    cadence_seconds: i32,
    enabled: bool,
    push_enabled: bool,
    metadata: Value,
    last_scheduled_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ExportTargetRow {
    fn into_domain(self) -> Result<ExportTarget, DatabaseError> {
        Ok(ExportTarget {
            id: self.id,
            target_id: self.target_id,
            repo_url: self.repo_url,
            local_path: PathBuf::from(self.local_path),
            branch: self.branch,
            format: ExportFormat::from_str(&self.format)?,
            layout: ExportLayout::from_str(&self.layout)?,
            cadence_seconds: self.cadence_seconds,
            enabled: self.enabled,
            push_enabled: self.push_enabled,
            metadata: self.metadata,
            last_scheduled_at: self.last_scheduled_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(Debug, FromRow)]
struct ExportRunRow {
    id: Uuid,
    export_target_id: Uuid,
    job_id: Option<Uuid>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    status: String,
    item_count: i32,
    commit_sha: Option<String>,
    pushed: bool,
    error: Option<String>,
    metadata: Value,
}

impl ExportRunRow {
    fn into_domain(self) -> Result<ExportRun, DatabaseError> {
        Ok(ExportRun {
            id: self.id,
            export_target_id: self.export_target_id,
            job_id: self.job_id,
            started_at: self.started_at,
            finished_at: self.finished_at,
            status: RunStatus::from_str(&self.status)?,
            item_count: self.item_count,
            commit_sha: self.commit_sha,
            pushed: self.pushed,
            error: self.error,
            metadata: self.metadata,
        })
    }
}

#[derive(Debug, FromRow)]
struct ExportableFeedItemRow {
    id: Uuid,
    company_id: Uuid,
    source_id: Uuid,
    external_id: String,
    url: String,
    canonical_url: String,
    title: String,
    summary: String,
    body_text: String,
    body_html: String,
    body_markdown: String,
    published_at: Option<DateTime<Utc>>,
    fetched_at: DateTime<Utc>,
    content_hash: String,
    source_kind: String,
    content_processing: Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    company_key: String,
    company_name: String,
    source_key: String,
    previous_exported_path: Option<String>,
    previous_content_hash: Option<String>,
}

impl ExportableFeedItemRow {
    fn into_domain(self) -> Result<ExportableFeedItem, DatabaseError> {
        Ok(ExportableFeedItem {
            item: FeedItem {
                id: self.id,
                company_id: self.company_id,
                source_id: self.source_id,
                external_id: self.external_id,
                url: Url::parse(&self.url)?,
                canonical_url: Url::parse(&self.canonical_url)?,
                title: self.title,
                summary: self.summary,
                body_text: self.body_text,
                body_html: self.body_html,
                body_markdown: self.body_markdown,
                published_at: self.published_at,
                fetched_at: self.fetched_at,
                content_hash: self.content_hash,
                source_kind: SourceKind::from_str(&self.source_kind)?,
                content_processing: self.content_processing,
                created_at: self.created_at,
                updated_at: self.updated_at,
            },
            company_key: self.company_key,
            company_name: self.company_name,
            source_key: self.source_key,
            previous_exported_path: self.previous_exported_path.map(PathBuf::from),
            previous_content_hash: self.previous_content_hash,
        })
    }
}

impl Database {
    pub async fn get_export_target(
        &self,
        export_target_id: Uuid,
    ) -> Result<Option<ExportTarget>, DatabaseError> {
        sqlx::query_as::<_, ExportTargetRow>(
            r#"
            SELECT
                id, target_id, repo_url, local_path, branch, format, layout,
                cadence_seconds, enabled, push_enabled, metadata,
                last_scheduled_at, created_at, updated_at
            FROM export_targets
            WHERE id = $1
            "#,
        )
        .bind(export_target_id)
        .fetch_optional(self.pool())
        .await?
        .map(ExportTargetRow::into_domain)
        .transpose()
    }

    pub async fn get_export_target_by_key(
        &self,
        target_id: &str,
    ) -> Result<Option<ExportTarget>, DatabaseError> {
        sqlx::query_as::<_, ExportTargetRow>(
            r#"
            SELECT
                id, target_id, repo_url, local_path, branch, format, layout,
                cadence_seconds, enabled, push_enabled, metadata,
                last_scheduled_at, created_at, updated_at
            FROM export_targets
            WHERE target_id = $1
            "#,
        )
        .bind(target_id)
        .fetch_optional(self.pool())
        .await?
        .map(ExportTargetRow::into_domain)
        .transpose()
    }

    pub async fn list_export_targets(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ExportTarget>, DatabaseError> {
        sqlx::query_as::<_, ExportTargetRow>(
            r#"
            SELECT
                id, target_id, repo_url, local_path, branch, format, layout,
                cadence_seconds, enabled, push_enabled, metadata,
                last_scheduled_at, created_at, updated_at
            FROM export_targets
            ORDER BY target_id
            LIMIT $1 OFFSET $2
            "#,
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(ExportTargetRow::into_domain)
        .collect()
    }

    pub async fn count_export_targets(&self) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar("SELECT count(*) FROM export_targets")
            .fetch_one(self.pool())
            .await?)
    }

    pub async fn enqueue_due_export_jobs(&self, now: DateTime<Utc>) -> Result<u64, DatabaseError> {
        let result = sqlx::query(
            r#"
            WITH due_targets AS (
                SELECT target.id
                FROM export_targets AS target
                WHERE
                    target.enabled
                    AND (
                        target.last_scheduled_at IS NULL
                        OR target.last_scheduled_at
                            + (target.cadence_seconds::bigint * INTERVAL '1 second') <= $1
                    )
                    AND NOT EXISTS (
                        SELECT 1
                        FROM jobs AS active_job
                        WHERE
                            active_job.job_type = 'export_target'
                            AND active_job.job_key = 'target:' || target.id::text
                            AND active_job.status IN ('pending', 'running')
                    )
                FOR UPDATE OF target SKIP LOCKED
            ),
            inserted_jobs AS (
                INSERT INTO jobs (
                    job_type,
                    job_key,
                    status,
                    run_after,
                    export_target_id,
                    payload
                )
                SELECT
                    'export_target',
                    'target:' || target.id::text,
                    'pending',
                    $1,
                    target.id,
                    jsonb_build_object('export_target_id', target.id)
                FROM due_targets AS target
                ON CONFLICT (job_type, job_key)
                    WHERE status IN ('pending', 'running')
                DO NOTHING
                RETURNING export_target_id
            )
            UPDATE export_targets
            SET last_scheduled_at = $1
            WHERE id IN (SELECT export_target_id FROM inserted_jobs)
            "#,
        )
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn begin_export_run(
        &self,
        export_target_id: Uuid,
        job_id: Uuid,
    ) -> Result<Uuid, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            INSERT INTO export_runs (export_target_id, job_id, status)
            VALUES ($1, $2, 'running')
            RETURNING id
            "#,
        )
        .bind(export_target_id)
        .bind(job_id)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn list_exportable_feed_items(
        &self,
        export_target_id: Uuid,
    ) -> Result<Vec<ExportableFeedItem>, DatabaseError> {
        sqlx::query_as::<_, ExportableFeedItemRow>(
            r#"
            SELECT
                item.id,
                item.company_id,
                item.source_id,
                item.external_id,
                item.url,
                item.canonical_url,
                item.title,
                item.summary,
                item.body_text,
                item.body_html,
                item.body_markdown,
                item.published_at,
                item.fetched_at,
                item.content_hash,
                item.source_kind,
                item.content_processing,
                item.created_at,
                item.updated_at,
                company.company_key AS company_key,
                company.name AS company_name,
                source.source_id AS source_key,
                exported.exported_path AS previous_exported_path,
                exported.exported_content_hash AS previous_content_hash
            FROM feed_items AS item
            JOIN companies AS company ON company.id = item.company_id
            JOIN sources AS source ON source.id = item.source_id
            LEFT JOIN exported_items AS exported
                ON exported.target_id = $1
                AND exported.feed_item_id = item.id
            WHERE
                NOT item.is_private
                AND (
                    item.published_at IS NULL
                    OR item.published_at <= CURRENT_TIMESTAMP
                )
                AND source.status = 'approved'
                AND source.public_export_allowed
                AND source.kind IN ('rss', 'atom', 'html', 'browser')
                AND (
                    source.kind IN ('rss', 'atom')
                    OR EXISTS (
                        SELECT 1
                        FROM company_news_recipes AS active_recipe
                        LEFT JOIN company_news_recipe_state AS active_recipe_state
                            ON active_recipe_state.recipe_id = active_recipe.id
                        WHERE active_recipe.source_id = source.id
                          AND active_recipe.status = 'active'
                          AND NOT COALESCE(active_recipe_state.rebuild_required, false)
                    )
                )
            ORDER BY
                item.published_at ASC NULLS LAST,
                item.fetched_at ASC,
                item.id
            "#,
        )
        .bind(export_target_id)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(ExportableFeedItemRow::into_domain)
        .collect()
    }

    pub async fn complete_export_run(
        &self,
        run_id: Uuid,
        records: &[ExportedItem],
        commit_sha: Option<&str>,
        pushed: bool,
        metadata: Value,
    ) -> Result<(), DatabaseError> {
        let item_count = i32::try_from(records.len())
            .map_err(|_| DatabaseError::Invariant("export item count exceeds i32".to_owned()))?;
        let mut transaction = self.pool().begin().await?;
        let target_id: Uuid =
            sqlx::query_scalar("SELECT export_target_id FROM export_runs WHERE id = $1 FOR UPDATE")
                .bind(run_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or(DatabaseError::NotFound {
                    entity: "export run",
                    id: run_id,
                })?;

        let result = sqlx::query(
            r#"
            UPDATE export_runs
            SET
                status = 'completed',
                finished_at = CURRENT_TIMESTAMP,
                item_count = $2,
                commit_sha = $3,
                pushed = $4,
                metadata = $5
            WHERE id = $1 AND status = 'running'
            "#,
        )
        .bind(run_id)
        .bind(item_count)
        .bind(commit_sha)
        .bind(pushed)
        .bind(metadata)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            return Err(DatabaseError::InvalidState(format!(
                "export run {run_id} is not running"
            )));
        }

        for record in records {
            let exported_path = record.exported_path.to_str().ok_or_else(|| {
                DatabaseError::Invariant(format!(
                    "exported path for feed item {} is not valid UTF-8",
                    record.feed_item_id
                ))
            })?;
            sqlx::query(
                r#"
                INSERT INTO exported_items (
                    target_id,
                    feed_item_id,
                    exported_path,
                    exported_content_hash,
                    exported_commit,
                    exported_at
                )
                VALUES ($1, $2, $3, $4, $5, CURRENT_TIMESTAMP)
                ON CONFLICT (target_id, feed_item_id) DO UPDATE
                SET
                    exported_path = EXCLUDED.exported_path,
                    exported_content_hash = EXCLUDED.exported_content_hash,
                    exported_commit = EXCLUDED.exported_commit,
                    exported_at = EXCLUDED.exported_at
                "#,
            )
            .bind(target_id)
            .bind(record.feed_item_id)
            .bind(exported_path)
            .bind(&record.exported_content_hash)
            .bind(commit_sha)
            .execute(&mut *transaction)
            .await?;
        }

        sqlx::query(
            r#"
            INSERT INTO event_log (event_type, payload)
            VALUES ('export.completed', $1)
            "#,
        )
        .bind(json!({
            "run_id": run_id,
            "export_target_id": target_id,
            "item_count": item_count,
            "commit_sha": commit_sha,
            "pushed": pushed,
        }))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn fail_export_run(&self, run_id: Uuid, error: &str) -> Result<(), DatabaseError> {
        let result = sqlx::query(
            r#"
            UPDATE export_runs
            SET
                status = 'failed',
                finished_at = CURRENT_TIMESTAMP,
                error = $2
            WHERE id = $1 AND status = 'running'
            "#,
        )
        .bind(run_id)
        .bind(error)
        .execute(self.pool())
        .await?;
        if result.rows_affected() != 1 {
            return Err(DatabaseError::NotFound {
                entity: "running export run",
                id: run_id,
            });
        }
        Ok(())
    }

    pub async fn list_export_runs(
        &self,
        export_target_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ExportRun>, DatabaseError> {
        let rows = sqlx::query_as::<_, ExportRunRow>(
            r#"
            SELECT
                id,
                export_target_id,
                job_id,
                started_at,
                finished_at,
                status,
                item_count,
                commit_sha,
                pushed,
                error,
                metadata
            FROM export_runs
            WHERE $1::uuid IS NULL OR export_target_id = $1
            ORDER BY started_at DESC, id
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(export_target_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(ExportRunRow::into_domain).collect()
    }

    pub async fn count_export_runs(
        &self,
        export_target_id: Option<Uuid>,
    ) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM export_runs
            WHERE $1::uuid IS NULL OR export_target_id = $1
            "#,
        )
        .bind(export_target_id)
        .fetch_one(self.pool())
        .await?)
    }
}
