use std::str::FromStr;

use chrono::{DateTime, Utc};
use feed_core::{
    CrawlRun, DiscoveryRun, FeedItem, FeedItemSummary, RunStatus, SourceHealth, SourceKind,
};
use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;
use url::Url;
use uuid::Uuid;

use crate::{Database, DatabaseError};

#[derive(Debug, FromRow)]
struct FeedItemRow {
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
}

#[derive(Debug, FromRow)]
struct FeedItemSummaryRow {
    #[sqlx(flatten)]
    item: FeedItemRow,
    company_key: String,
    company_name: String,
    source_key: String,
}

#[derive(Clone, Debug, Default)]
pub struct FeedItemSummaryFilter {
    pub company_id: Option<Uuid>,
    pub source_id: Option<Uuid>,
    pub source_kind: Option<SourceKind>,
    pub search: Option<String>,
    pub include_future: bool,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, FromRow, Serialize)]
pub struct ApprovedFeedItemCompanyClaim {
    pub company_id: Uuid,
    pub company_name: String,
    pub matched_item_count: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, FromRow, Serialize)]
pub struct PublicFeedItemCompanyClaim {
    pub candidate_url: String,
    pub feed_item_id: Uuid,
    pub company_id: Uuid,
    pub company_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedItemSignatureCandidate {
    pub identity_url: String,
    pub title: String,
    pub published_at: Option<DateTime<Utc>>,
}

impl FeedItemRow {
    fn into_domain(self) -> Result<FeedItem, DatabaseError> {
        Ok(FeedItem {
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
        })
    }
}

impl FeedItemSummaryRow {
    fn into_domain(self) -> Result<FeedItemSummary, DatabaseError> {
        Ok(FeedItemSummary {
            item: self.item.into_domain()?,
            company_key: self.company_key,
            company_name: self.company_name,
            source_key: self.source_key,
        })
    }
}

#[derive(Debug, FromRow)]
struct CrawlRunRow {
    id: Uuid,
    source_id: Uuid,
    job_id: Option<Uuid>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    status: String,
    item_count: i32,
    new_item_count: i32,
    error: Option<String>,
    metadata: Value,
}

impl CrawlRunRow {
    fn into_domain(self) -> Result<CrawlRun, DatabaseError> {
        Ok(CrawlRun {
            id: self.id,
            source_id: self.source_id,
            job_id: self.job_id,
            started_at: self.started_at,
            finished_at: self.finished_at,
            status: RunStatus::from_str(&self.status)?,
            item_count: self.item_count,
            new_item_count: self.new_item_count,
            error: self.error,
            metadata: self.metadata,
        })
    }
}

#[derive(Debug, FromRow)]
struct DiscoveryRunRow {
    id: Uuid,
    company_id: Uuid,
    job_id: Option<Uuid>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    status: String,
    candidate_count: i32,
    error: Option<String>,
    metadata: Value,
}

impl DiscoveryRunRow {
    fn into_domain(self) -> Result<DiscoveryRun, DatabaseError> {
        Ok(DiscoveryRun {
            id: self.id,
            company_id: self.company_id,
            job_id: self.job_id,
            started_at: self.started_at,
            finished_at: self.finished_at,
            status: RunStatus::from_str(&self.status)?,
            candidate_count: self.candidate_count,
            error: self.error,
            metadata: self.metadata,
        })
    }
}

impl Database {
    pub async fn get_feed_item(&self, item_id: Uuid) -> Result<Option<FeedItem>, DatabaseError> {
        let row = sqlx::query_as::<_, FeedItemRow>(
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
                item.updated_at
            FROM feed_items AS item
            JOIN sources AS source ON source.id = item.source_id
            WHERE
                item.id = $1
                AND NOT item.is_private
                AND (
                    item.published_at IS NULL
                    OR item.published_at <= CURRENT_TIMESTAMP
                )
                AND source.status = 'approved'
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
            "#,
        )
        .bind(item_id)
        .fetch_optional(self.pool())
        .await?;
        row.map(FeedItemRow::into_domain).transpose()
    }

    pub async fn list_feed_items(
        &self,
        company_id: Option<Uuid>,
        source_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FeedItem>, DatabaseError> {
        let rows = sqlx::query_as::<_, FeedItemRow>(
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
                item.updated_at
            FROM feed_items AS item
            JOIN sources AS source ON source.id = item.source_id
            WHERE
                NOT item.is_private
                AND (
                    item.published_at IS NULL
                    OR item.published_at <= CURRENT_TIMESTAMP
                )
                AND source.status = 'approved'
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
                AND ($1::uuid IS NULL OR item.company_id = $1)
                AND ($2::uuid IS NULL OR item.source_id = $2)
            ORDER BY item.published_at DESC NULLS LAST, item.fetched_at DESC, item.id
            LIMIT $3 OFFSET $4
            "#,
        )
        .bind(company_id)
        .bind(source_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(FeedItemRow::into_domain).collect()
    }

    pub async fn count_feed_items(
        &self,
        company_id: Option<Uuid>,
        source_id: Option<Uuid>,
    ) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM feed_items AS item
            JOIN sources AS source ON source.id = item.source_id
            WHERE
                NOT item.is_private
                AND (
                    item.published_at IS NULL
                    OR item.published_at <= CURRENT_TIMESTAMP
                )
                AND source.status = 'approved'
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
                AND ($1::uuid IS NULL OR item.company_id = $1)
                AND ($2::uuid IS NULL OR item.source_id = $2)
            "#,
        )
        .bind(company_id)
        .bind(source_id)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn list_approved_feed_item_url_matches(
        &self,
        company_id: Uuid,
        candidate_urls: &[String],
    ) -> Result<Vec<String>, DatabaseError> {
        if candidate_urls.is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_scalar(
            r#"
            SELECT DISTINCT candidate.url
            FROM unnest($2::text[]) AS candidate(url)
            JOIN feed_items AS item ON
                item.company_id = $1
                AND (
                    public_url_identity_key(item.canonical_url)
                        = public_url_identity_key(candidate.url)
                    OR public_url_identity_key(item.url)
                        = public_url_identity_key(candidate.url)
                    OR public_url_identity_key(item.external_id)
                        = public_url_identity_key(candidate.url)
                )
            JOIN sources AS source ON source.id = item.source_id
            WHERE
                source.status = 'approved'
                AND source.kind IN ('rss', 'atom')
                AND NOT item.is_private
            ORDER BY candidate.url
            "#,
        )
        .bind(company_id)
        .bind(candidate_urls)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn list_approved_feed_item_signature_matches(
        &self,
        company_id: Uuid,
        candidates: &[FeedItemSignatureCandidate],
    ) -> Result<Vec<String>, DatabaseError> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let identity_urls = candidates
            .iter()
            .map(|candidate| candidate.identity_url.clone())
            .collect::<Vec<_>>();
        let titles = candidates
            .iter()
            .map(|candidate| candidate.title.clone())
            .collect::<Vec<_>>();
        let published_at = candidates
            .iter()
            .map(|candidate| candidate.published_at)
            .collect::<Vec<_>>();
        Ok(sqlx::query_scalar(
            r#"
            WITH candidate AS (
                SELECT identity_url, title, published_at
                FROM unnest(
                    $2::text[],
                    $3::text[],
                    $4::timestamptz[]
                ) AS input(identity_url, title, published_at)
            )
            SELECT DISTINCT candidate.identity_url
            FROM candidate
            JOIN feed_items AS item ON
                item.company_id = $1
                AND lower(btrim(regexp_replace(
                    item.title,
                    '[[:space:]]+',
                    ' ',
                    'g'
                ))) = lower(btrim(regexp_replace(
                    candidate.title,
                    '[[:space:]]+',
                    ' ',
                    'g'
                )))
                AND (
                    (
                        item.published_at IS NULL
                        AND candidate.published_at IS NULL
                    )
                    OR (
                        item.published_at IS NOT NULL
                        AND candidate.published_at IS NOT NULL
                        AND (item.published_at AT TIME ZONE 'UTC')::date
                            = (candidate.published_at AT TIME ZONE 'UTC')::date
                    )
                )
            JOIN sources AS source ON source.id = item.source_id
            WHERE
                source.status = 'approved'
                AND source.kind IN ('rss', 'atom')
                AND NOT item.is_private
            ORDER BY candidate.identity_url
            "#,
        )
        .bind(company_id)
        .bind(identity_urls)
        .bind(titles)
        .bind(published_at)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn list_approved_feed_item_company_claims(
        &self,
        candidate_urls: &[String],
    ) -> Result<Vec<ApprovedFeedItemCompanyClaim>, DatabaseError> {
        if candidate_urls.is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_as::<_, ApprovedFeedItemCompanyClaim>(
            r#"
            SELECT
                item.company_id,
                company.name AS company_name,
                count(DISTINCT candidate.url) AS matched_item_count
            FROM unnest($1::text[]) AS candidate(url)
            JOIN feed_items AS item ON
                public_url_identity_key(item.canonical_url)
                    = public_url_identity_key(candidate.url)
                OR public_url_identity_key(item.url)
                    = public_url_identity_key(candidate.url)
                OR public_url_identity_key(item.external_id)
                    = public_url_identity_key(candidate.url)
            JOIN sources AS source ON source.id = item.source_id
            JOIN companies AS company ON company.id = item.company_id
            WHERE
                source.status = 'approved'
                AND source.kind IN ('rss', 'atom')
                AND NOT item.is_private
            GROUP BY item.company_id, company.name
            ORDER BY matched_item_count DESC, company.name, item.company_id
            "#,
        )
        .bind(candidate_urls)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn list_public_feed_item_company_claims(
        &self,
        candidate_urls: &[String],
    ) -> Result<Vec<PublicFeedItemCompanyClaim>, DatabaseError> {
        if candidate_urls.is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_as::<_, PublicFeedItemCompanyClaim>(
            r#"
            SELECT DISTINCT
                candidate.url AS candidate_url,
                item.id AS feed_item_id,
                item.company_id,
                company.name AS company_name
            FROM unnest($1::text[]) AS candidate(url)
            JOIN feed_items AS item ON
                public_url_identity_key(item.canonical_url)
                    = public_url_identity_key(candidate.url)
                OR public_url_identity_key(item.url)
                    = public_url_identity_key(candidate.url)
                OR public_url_identity_key(item.external_id)
                    = public_url_identity_key(candidate.url)
            JOIN sources AS source ON source.id = item.source_id
            JOIN companies AS company ON company.id = item.company_id
            WHERE
                NOT item.is_private
                AND source.status = 'approved'
                AND (
                    source.kind IN ('rss', 'atom')
                    OR EXISTS (
                        SELECT 1
                        FROM company_news_recipes AS active_recipe
                        LEFT JOIN company_news_recipe_state AS active_recipe_state
                            ON active_recipe_state.recipe_id = active_recipe.id
                        WHERE active_recipe.source_id = source.id
                          AND active_recipe.status = 'active'
                          AND NOT COALESCE(
                              active_recipe_state.rebuild_required,
                              false
                          )
                    )
                )
            ORDER BY
                candidate.url,
                company.name,
                item.company_id,
                item.id
            "#,
        )
        .bind(candidate_urls)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn list_public_cross_company_feed_item_ids(
        &self,
    ) -> Result<Vec<Uuid>, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            WITH public_item AS (
                SELECT
                    item.id,
                    item.company_id,
                    public_url_identity_key(item.canonical_url) AS url_identity
                FROM feed_items AS item
                JOIN sources AS source ON source.id = item.source_id
                WHERE
                    NOT item.is_private
                    AND source.status = 'approved'
                    AND (
                        source.kind IN ('rss', 'atom')
                        OR EXISTS (
                            SELECT 1
                            FROM company_news_recipes AS active_recipe
                            LEFT JOIN company_news_recipe_state
                                AS active_recipe_state
                                ON active_recipe_state.recipe_id =
                                    active_recipe.id
                            WHERE active_recipe.source_id = source.id
                              AND active_recipe.status = 'active'
                              AND NOT COALESCE(
                                  active_recipe_state.rebuild_required,
                                  false
                              )
                        )
                    )
            ),
            collision AS (
                SELECT url_identity
                FROM public_item
                GROUP BY url_identity
                HAVING count(DISTINCT company_id) > 1
            )
            SELECT item.id
            FROM public_item AS item
            JOIN collision USING (url_identity)
            ORDER BY item.id
            "#,
        )
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn list_active_recipe_item_url_matches(
        &self,
        company_id: Uuid,
        candidate_urls: &[String],
    ) -> Result<Vec<String>, DatabaseError> {
        if candidate_urls.is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_scalar(
            r#"
            SELECT DISTINCT candidate.url
            FROM unnest($2::text[]) AS candidate(url)
            JOIN feed_items AS item ON
                item.company_id = $1
                AND (
                    public_url_identity_key(item.canonical_url)
                        = public_url_identity_key(candidate.url)
                    OR public_url_identity_key(item.url)
                        = public_url_identity_key(candidate.url)
                    OR public_url_identity_key(item.external_id)
                        = public_url_identity_key(candidate.url)
                )
            JOIN company_news_recipes AS recipe ON recipe.source_id = item.source_id
            JOIN sources AS source ON source.id = item.source_id
            LEFT JOIN company_news_recipe_state AS recipe_state
                ON recipe_state.recipe_id = recipe.id
            WHERE
                recipe.status = 'active'
                AND source.status = 'approved'
                AND NOT item.is_private
                AND NOT COALESCE(recipe_state.rebuild_required, false)
            ORDER BY candidate.url
            "#,
        )
        .bind(company_id)
        .bind(candidate_urls)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn list_active_recipe_item_signature_matches(
        &self,
        company_id: Uuid,
        candidates: &[FeedItemSignatureCandidate],
    ) -> Result<Vec<String>, DatabaseError> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let identity_urls = candidates
            .iter()
            .map(|candidate| candidate.identity_url.clone())
            .collect::<Vec<_>>();
        let titles = candidates
            .iter()
            .map(|candidate| candidate.title.clone())
            .collect::<Vec<_>>();
        let published_at = candidates
            .iter()
            .map(|candidate| candidate.published_at)
            .collect::<Vec<_>>();
        Ok(sqlx::query_scalar(
            r#"
            WITH candidate AS (
                SELECT identity_url, title, published_at
                FROM unnest(
                    $2::text[],
                    $3::text[],
                    $4::timestamptz[]
                ) AS input(identity_url, title, published_at)
            )
            SELECT DISTINCT candidate.identity_url
            FROM candidate
            JOIN feed_items AS item ON
                item.company_id = $1
                AND lower(btrim(regexp_replace(
                    item.title,
                    '[[:space:]]+',
                    ' ',
                    'g'
                ))) = lower(btrim(regexp_replace(
                    candidate.title,
                    '[[:space:]]+',
                    ' ',
                    'g'
                )))
                AND (
                    (
                        item.published_at IS NULL
                        AND candidate.published_at IS NULL
                    )
                    OR (
                        item.published_at IS NOT NULL
                        AND candidate.published_at IS NOT NULL
                        AND (item.published_at AT TIME ZONE 'UTC')::date
                            = (candidate.published_at AT TIME ZONE 'UTC')::date
                    )
                )
            JOIN company_news_recipes AS recipe ON recipe.source_id = item.source_id
            JOIN sources AS source ON source.id = item.source_id
            LEFT JOIN company_news_recipe_state AS recipe_state
                ON recipe_state.recipe_id = recipe.id
            WHERE
                recipe.status = 'active'
                AND source.status = 'approved'
                AND NOT item.is_private
                AND NOT COALESCE(recipe_state.rebuild_required, false)
            ORDER BY candidate.identity_url
            "#,
        )
        .bind(company_id)
        .bind(identity_urls)
        .bind(titles)
        .bind(published_at)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn list_active_recipe_ids_fully_covered_by_item_urls(
        &self,
        company_id: Uuid,
        candidate_urls: &[String],
    ) -> Result<Vec<Uuid>, DatabaseError> {
        if candidate_urls.is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_scalar(
            r#"
            WITH candidate AS (
                SELECT DISTINCT public_url_identity_key(url) AS identity
                FROM unnest($2::text[]) AS input(url)
            ),
            active_item AS (
                SELECT
                    recipe.id AS recipe_id,
                    public_url_identity_key(item.canonical_url)
                        AS canonical_identity,
                    EXISTS (
                        SELECT 1
                        FROM candidate
                        WHERE
                            candidate.identity
                                = public_url_identity_key(item.canonical_url)
                            OR candidate.identity
                                = public_url_identity_key(item.url)
                            OR candidate.identity
                                = public_url_identity_key(item.external_id)
                    ) AS is_covered
                FROM company_news_recipes AS recipe
                JOIN sources AS source ON source.id = recipe.source_id
                JOIN feed_items AS item ON item.source_id = recipe.source_id
                LEFT JOIN company_news_recipe_state AS recipe_state
                    ON recipe_state.recipe_id = recipe.id
                WHERE
                    recipe.company_id = $1
                    AND recipe.status = 'active'
                    AND source.status = 'approved'
                    AND NOT item.is_private
                    AND NOT COALESCE(recipe_state.rebuild_required, false)
            )
            SELECT recipe_id
            FROM active_item
            GROUP BY recipe_id
            HAVING
                count(DISTINCT canonical_identity) > 0
                AND count(DISTINCT canonical_identity)
                    = count(DISTINCT canonical_identity)
                        FILTER (WHERE is_covered)
            ORDER BY recipe_id
            "#,
        )
        .bind(company_id)
        .bind(candidate_urls)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn list_preferred_active_recipe_item_url_matches(
        &self,
        company_id: Uuid,
        current_recipe_id: Uuid,
        candidate_urls: &[String],
    ) -> Result<Vec<String>, DatabaseError> {
        if candidate_urls.is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_scalar(
            r#"
            WITH active_recipe_counts AS (
                SELECT
                    recipe.id AS recipe_id,
                    count(DISTINCT public_url_identity_key(item.canonical_url))
                        FILTER (WHERE NOT item.is_private) AS public_item_count
                FROM company_news_recipes AS recipe
                JOIN sources AS source ON source.id = recipe.source_id
                LEFT JOIN company_news_recipe_state AS state
                    ON state.recipe_id = recipe.id
                LEFT JOIN feed_items AS item ON item.source_id = recipe.source_id
                WHERE
                    recipe.company_id = $1
                    AND recipe.status = 'active'
                    AND source.status = 'approved'
                    AND NOT COALESCE(state.rebuild_required, false)
                GROUP BY recipe.id
            )
            SELECT DISTINCT candidate.url
            FROM unnest($3::text[]) AS candidate(url)
            JOIN feed_items AS item ON
                item.company_id = $1
                AND (
                    public_url_identity_key(item.canonical_url)
                        = public_url_identity_key(candidate.url)
                    OR public_url_identity_key(item.url)
                        = public_url_identity_key(candidate.url)
                    OR public_url_identity_key(item.external_id)
                        = public_url_identity_key(candidate.url)
                )
            JOIN company_news_recipes AS other_recipe
                ON other_recipe.source_id = item.source_id
            JOIN sources AS other_source ON other_source.id = item.source_id
            LEFT JOIN company_news_recipe_state AS other_state
                ON other_state.recipe_id = other_recipe.id
            JOIN company_news_recipes AS current_recipe ON current_recipe.id = $2
            JOIN active_recipe_counts AS other_count
                ON other_count.recipe_id = other_recipe.id
            JOIN active_recipe_counts AS current_count
                ON current_count.recipe_id = current_recipe.id
            WHERE
                other_recipe.company_id = $1
                AND other_recipe.status = 'active'
                AND other_source.status = 'approved'
                AND NOT item.is_private
                AND NOT COALESCE(other_state.rebuild_required, false)
                AND other_recipe.id <> current_recipe.id
                AND (
                    other_count.public_item_count > current_count.public_item_count
                    OR (
                        other_count.public_item_count = current_count.public_item_count
                        AND (
                            COALESCE(other_recipe.verified_at, other_recipe.created_at),
                            other_recipe.id
                        ) < (
                            COALESCE(current_recipe.verified_at, current_recipe.created_at),
                            current_recipe.id
                        )
                    )
                )
            ORDER BY candidate.url
            "#,
        )
        .bind(company_id)
        .bind(current_recipe_id)
        .bind(candidate_urls)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn list_preferred_active_recipe_item_signature_matches(
        &self,
        company_id: Uuid,
        current_recipe_id: Uuid,
        candidates: &[FeedItemSignatureCandidate],
    ) -> Result<Vec<String>, DatabaseError> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let identity_urls = candidates
            .iter()
            .map(|candidate| candidate.identity_url.clone())
            .collect::<Vec<_>>();
        let titles = candidates
            .iter()
            .map(|candidate| candidate.title.clone())
            .collect::<Vec<_>>();
        let published_at = candidates
            .iter()
            .map(|candidate| candidate.published_at)
            .collect::<Vec<_>>();
        Ok(sqlx::query_scalar(
            r#"
            WITH active_recipe_counts AS (
                SELECT
                    recipe.id AS recipe_id,
                    count(DISTINCT public_url_identity_key(item.canonical_url))
                        FILTER (WHERE NOT item.is_private) AS public_item_count
                FROM company_news_recipes AS recipe
                JOIN sources AS source ON source.id = recipe.source_id
                LEFT JOIN company_news_recipe_state AS state
                    ON state.recipe_id = recipe.id
                LEFT JOIN feed_items AS item ON item.source_id = recipe.source_id
                WHERE
                    recipe.company_id = $1
                    AND recipe.status = 'active'
                    AND source.status = 'approved'
                    AND NOT COALESCE(state.rebuild_required, false)
                GROUP BY recipe.id
            ),
            candidate AS (
                SELECT identity_url, title, published_at
                FROM unnest(
                    $3::text[],
                    $4::text[],
                    $5::timestamptz[]
                ) AS input(identity_url, title, published_at)
            )
            SELECT DISTINCT candidate.identity_url
            FROM candidate
            JOIN feed_items AS item ON
                item.company_id = $1
                AND lower(btrim(regexp_replace(
                    item.title,
                    '[[:space:]]+',
                    ' ',
                    'g'
                ))) = lower(btrim(regexp_replace(
                    candidate.title,
                    '[[:space:]]+',
                    ' ',
                    'g'
                )))
                AND (
                    (
                        item.published_at IS NULL
                        AND candidate.published_at IS NULL
                    )
                    OR (
                        item.published_at IS NOT NULL
                        AND candidate.published_at IS NOT NULL
                        AND (item.published_at AT TIME ZONE 'UTC')::date
                            = (candidate.published_at AT TIME ZONE 'UTC')::date
                    )
                )
            JOIN company_news_recipes AS other_recipe
                ON other_recipe.source_id = item.source_id
            JOIN sources AS other_source ON other_source.id = item.source_id
            LEFT JOIN company_news_recipe_state AS other_state
                ON other_state.recipe_id = other_recipe.id
            JOIN company_news_recipes AS current_recipe ON current_recipe.id = $2
            JOIN active_recipe_counts AS other_count
                ON other_count.recipe_id = other_recipe.id
            JOIN active_recipe_counts AS current_count
                ON current_count.recipe_id = current_recipe.id
            WHERE
                other_recipe.company_id = $1
                AND other_recipe.status = 'active'
                AND other_source.status = 'approved'
                AND NOT item.is_private
                AND NOT COALESCE(other_state.rebuild_required, false)
                AND other_recipe.id <> current_recipe.id
                AND (
                    other_count.public_item_count > current_count.public_item_count
                    OR (
                        other_count.public_item_count = current_count.public_item_count
                        AND (
                            COALESCE(other_recipe.verified_at, other_recipe.created_at),
                            other_recipe.id
                        ) < (
                            COALESCE(current_recipe.verified_at, current_recipe.created_at),
                            current_recipe.id
                        )
                    )
                )
            ORDER BY candidate.identity_url
            "#,
        )
        .bind(company_id)
        .bind(current_recipe_id)
        .bind(identity_urls)
        .bind(titles)
        .bind(published_at)
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn list_feed_item_summaries(
        &self,
        filter: &FeedItemSummaryFilter,
    ) -> Result<Vec<FeedItemSummary>, DatabaseError> {
        let rows = sqlx::query_as::<_, FeedItemSummaryRow>(
            r#"
            WITH canonical_ranked_items AS (
                SELECT
                    item.id,
                    item.company_id,
                    item.canonical_url,
                    item.title,
                    item.published_at,
                    item.fetched_at,
                    item.source_kind,
                    item.created_at,
                    row_number() OVER (
                        PARTITION BY
                            item.company_id,
                            public_url_identity_key(item.canonical_url)
                        ORDER BY
                            CASE source.kind
                                WHEN 'rss' THEN 0
                                WHEN 'atom' THEN 1
                                WHEN 'html' THEN 2
                                ELSE 3
                            END,
                            item.fetched_at DESC,
                            item.id
                    ) AS canonical_duplicate_rank
                FROM feed_items AS item
                JOIN companies AS company ON company.id = item.company_id
                JOIN sources AS source ON source.id = item.source_id
                WHERE
                    NOT item.is_private
                    AND source.status = 'approved'
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
                    AND ($1::uuid IS NULL OR item.company_id = $1)
                    AND ($2::uuid IS NULL OR item.source_id = $2)
                    AND ($3::text IS NULL OR item.source_kind = $3)
                    AND (
                        $4::boolean
                        OR item.published_at IS NULL
                        OR item.published_at <= CURRENT_TIMESTAMP
                    )
                    AND (
                        $5::text IS NULL
                        OR company.name ILIKE '%' || $5 || '%'
                        OR item.title ILIKE '%' || $5 || '%'
                        OR item.summary ILIKE '%' || $5 || '%'
                    )
            ),
            ranked_items AS (
                SELECT
                    canonical_item.*,
                    row_number() OVER (
                        PARTITION BY
                            canonical_item.company_id,
                            CASE
                                WHEN canonical_item.published_at IS NULL
                                THEN
                                    'url:'
                                    || public_url_identity_key(
                                        canonical_item.canonical_url
                                    )
                                ELSE
                                    'dated-title:'
                                    || canonical_item.published_at::date::text
                                    || ':'
                                    || lower(
                                        regexp_replace(
                                            btrim(canonical_item.title),
                                            '\s+',
                                            ' ',
                                            'g'
                                        )
                                    )
                            END
                        ORDER BY
                            CASE canonical_item.source_kind
                                WHEN 'rss' THEN 0
                                WHEN 'atom' THEN 1
                                WHEN 'html' THEN 2
                                ELSE 3
                            END,
                            canonical_item.fetched_at DESC,
                            canonical_item.id
                    ) AS duplicate_rank
                FROM canonical_ranked_items AS canonical_item
                WHERE canonical_item.canonical_duplicate_rank = 1
            ),
            selected_item_ids AS (
                SELECT
                    id,
                    COALESCE(published_at, created_at) AS sort_at,
                    created_at
                FROM ranked_items
                WHERE duplicate_rank = 1
                ORDER BY
                    COALESCE(published_at, created_at) DESC,
                    created_at DESC,
                    id
                LIMIT $6 OFFSET $7
            )
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
                company_key,
                company.name AS company_name,
                source.source_id AS source_key
            FROM selected_item_ids AS selected_item
            JOIN feed_items AS item ON item.id = selected_item.id
            JOIN companies AS company ON company.id = item.company_id
            JOIN sources AS source ON source.id = item.source_id
            ORDER BY
                selected_item.sort_at DESC,
                selected_item.created_at DESC,
                selected_item.id
            "#,
        )
        .bind(filter.company_id)
        .bind(filter.source_id)
        .bind(filter.source_kind.map(SourceKind::as_str))
        .bind(filter.include_future)
        .bind(filter.search.as_deref())
        .bind(filter.limit)
        .bind(filter.offset)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(FeedItemSummaryRow::into_domain)
            .collect()
    }

    pub async fn count_feed_item_summaries(
        &self,
        filter: &FeedItemSummaryFilter,
    ) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            WITH canonical_ranked_items AS (
                SELECT
                    item.id,
                    item.company_id,
                    item.canonical_url,
                    item.title,
                    item.published_at,
                    row_number() OVER (
                        PARTITION BY
                            item.company_id,
                            public_url_identity_key(item.canonical_url)
                        ORDER BY
                            CASE source.kind
                                WHEN 'rss' THEN 0
                                WHEN 'atom' THEN 1
                                WHEN 'html' THEN 2
                                ELSE 3
                            END,
                            item.fetched_at DESC,
                            item.id
                    ) AS canonical_duplicate_rank
                FROM feed_items AS item
                JOIN companies AS company ON company.id = item.company_id
                JOIN sources AS source ON source.id = item.source_id
                WHERE
                    NOT item.is_private
                    AND source.status = 'approved'
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
                    AND ($1::uuid IS NULL OR item.company_id = $1)
                    AND ($2::uuid IS NULL OR item.source_id = $2)
                    AND ($3::text IS NULL OR item.source_kind = $3)
                    AND (
                        $4::boolean
                        OR item.published_at IS NULL
                        OR item.published_at <= CURRENT_TIMESTAMP
                    )
                    AND (
                        $5::text IS NULL
                        OR company.name ILIKE '%' || $5 || '%'
                        OR item.title ILIKE '%' || $5 || '%'
                        OR item.summary ILIKE '%' || $5 || '%'
                    )
            )
            SELECT count(*)
            FROM (
                SELECT
                    item.company_id,
                    CASE
                        WHEN item.published_at IS NULL
                        THEN
                            'url:'
                            || public_url_identity_key(item.canonical_url)
                        ELSE
                            'dated-title:'
                            || item.published_at::date::text
                            || ':'
                            || lower(
                                regexp_replace(
                                    btrim(item.title),
                                    '\s+',
                                    ' ',
                                    'g'
                                )
                            )
                    END AS story_identity
                FROM canonical_ranked_items AS item
                WHERE item.canonical_duplicate_rank = 1
                GROUP BY
                    item.company_id,
                    story_identity
            ) AS unique_items
            "#,
        )
        .bind(filter.company_id)
        .bind(filter.source_id)
        .bind(filter.source_kind.map(SourceKind::as_str))
        .bind(filter.include_future)
        .bind(filter.search.as_deref())
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn list_source_health(
        &self,
        source_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SourceHealth>, DatabaseError> {
        Ok(sqlx::query_as::<_, SourceHealthRow>(
            r#"
            SELECT
                source_id,
                last_attempt_at,
                last_success_at,
                last_error,
                consecutive_failures,
                backoff_until,
                consecutive_zero_runs,
                total_successful_runs,
                total_items,
                last_nonzero_at,
                updated_at
            FROM source_state
            WHERE $1::uuid IS NULL OR source_id = $1
            ORDER BY updated_at DESC, source_id
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(source_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(SourceHealthRow::into_domain)
        .collect())
    }

    pub async fn count_source_health(&self, source_id: Option<Uuid>) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM source_state
            WHERE $1::uuid IS NULL OR source_id = $1
            "#,
        )
        .bind(source_id)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn list_crawl_runs(
        &self,
        source_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<CrawlRun>, DatabaseError> {
        let rows = sqlx::query_as::<_, CrawlRunRow>(
            r#"
            SELECT
                id,
                source_id,
                job_id,
                started_at,
                finished_at,
                status,
                item_count,
                new_item_count,
                error,
                metadata
            FROM crawl_runs
            WHERE $1::uuid IS NULL OR source_id = $1
            ORDER BY started_at DESC, id
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(source_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(CrawlRunRow::into_domain).collect()
    }

    pub async fn count_crawl_runs(&self, source_id: Option<Uuid>) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM crawl_runs
            WHERE $1::uuid IS NULL OR source_id = $1
            "#,
        )
        .bind(source_id)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn list_discovery_runs(
        &self,
        company_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<DiscoveryRun>, DatabaseError> {
        let rows = sqlx::query_as::<_, DiscoveryRunRow>(
            r#"
            SELECT
                id,
                company_id,
                job_id,
                started_at,
                finished_at,
                status,
                candidate_count,
                error,
                metadata
            FROM discovery_runs
            WHERE $1::uuid IS NULL OR company_id = $1
            ORDER BY started_at DESC, id
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(company_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(DiscoveryRunRow::into_domain).collect()
    }

    pub async fn count_discovery_runs(
        &self,
        company_id: Option<Uuid>,
    ) -> Result<i64, DatabaseError> {
        Ok(sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM discovery_runs
            WHERE $1::uuid IS NULL OR company_id = $1
            "#,
        )
        .bind(company_id)
        .fetch_one(self.pool())
        .await?)
    }
}

#[derive(Debug, FromRow)]
struct SourceHealthRow {
    source_id: Uuid,
    last_attempt_at: Option<DateTime<Utc>>,
    last_success_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    consecutive_failures: i32,
    backoff_until: Option<DateTime<Utc>>,
    consecutive_zero_runs: i32,
    total_successful_runs: i64,
    total_items: i64,
    last_nonzero_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
}

impl SourceHealthRow {
    fn into_domain(self) -> SourceHealth {
        SourceHealth {
            source_id: self.source_id,
            last_attempt_at: self.last_attempt_at,
            last_success_at: self.last_success_at,
            last_error: self.last_error,
            consecutive_failures: self.consecutive_failures,
            backoff_until: self.backoff_until,
            consecutive_zero_runs: self.consecutive_zero_runs,
            total_successful_runs: self.total_successful_runs,
            total_items: self.total_items,
            last_nonzero_at: self.last_nonzero_at,
            updated_at: self.updated_at,
        }
    }
}
