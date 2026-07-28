use std::time::Instant;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{Method, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use feed_core::{
    CandidateDecisionMode, CandidateDecisionRecord, CandidateReviewItem, CandidateStatus,
    CandidateValidationRun, CandidateValidationStatus, Company, CompanyNewsExtractionRun,
    CompanyNewsRecipe, CompanyNewsRecipeCoverage, CompanyNewsRecipeRun, CrawlRun, DiscoveryRun,
    ExportRun, ExportTarget, FeedItem, JobSpec, JobType, RecipeStatus, ReviewDashboard, Source,
    SourceCandidate, SourceHealth, SourceHealthSummary, SourceKind, SourceStatus,
};
use feed_db::{ContentCrawlCoverage, Database, DatabaseError, FeedItemSummaryFilter};
use serde::{Deserialize, Serialize};
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};
use tracing::error;
use uuid::Uuid;

const DEFAULT_PAGE_LIMIT: u32 = 50;
const MAX_PAGE_LIMIT: u32 = 200;
const COMPANY_PROFILE_RESOURCE_LIMIT: i64 = 200;

#[derive(Clone)]
pub struct ApiState {
    database: Database,
    service_name: &'static str,
    started_at: Instant,
    job_runner_enabled: bool,
    supported_job_types: Vec<JobType>,
}

impl ApiState {
    pub fn new(
        database: Database,
        service_name: &'static str,
        job_runner_enabled: bool,
        supported_job_types: Vec<JobType>,
    ) -> Self {
        Self {
            database,
            service_name,
            started_at: Instant::now(),
            job_runner_enabled,
            supported_job_types,
        }
    }
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(readiness))
        .route("/api/v1/health", get(health))
        .route("/api/v1/companies", get(list_companies))
        .route(
            "/api/v1/companies/{company_key}/profile",
            get(get_company_profile),
        )
        .route("/api/v1/companies/{company_key}", get(get_company))
        .route("/api/v1/source-candidates", get(list_source_candidates))
        .route(
            "/api/v1/source-candidates/{candidate_id}/validate",
            post(validate_source_candidate),
        )
        .route(
            "/api/v1/source-candidates/{candidate_id}/activate",
            post(activate_source_candidate),
        )
        .route(
            "/api/v1/source-candidates/{candidate_id}/reject",
            post(reject_source_candidate),
        )
        .route(
            "/api/v1/source-candidates/batch",
            post(batch_source_candidate_action),
        )
        .route("/api/v1/review/dashboard", get(get_review_dashboard))
        .route("/api/v1/review/candidates", get(list_review_candidates))
        .route("/api/v1/review/sources", get(list_review_source_health))
        .route(
            "/api/v1/candidate-validation-runs",
            get(list_candidate_validation_runs),
        )
        .route("/api/v1/candidate-decisions", get(list_candidate_decisions))
        .route("/api/v1/sources", get(list_sources))
        .route("/api/v1/feed-items", get(list_feed_items))
        .route("/api/v1/feed-items/{item_id}", get(get_feed_item))
        .route("/api/v1/news-items", get(list_news_items))
        .route("/api/v1/source-health", get(list_source_health))
        .route("/api/v1/crawl-runs", get(list_crawl_runs))
        .route(
            "/api/v1/company-news-extraction-runs",
            get(list_company_news_extraction_runs),
        )
        .route(
            "/api/v1/company-news-recipes",
            get(list_company_news_recipes),
        )
        .route(
            "/api/v1/company-news-recipe-runs",
            get(list_company_news_recipe_runs),
        )
        .route(
            "/api/v1/company-news-recipe-coverage",
            get(get_company_news_recipe_coverage),
        )
        .route(
            "/api/v1/content-crawl-coverage",
            get(get_content_crawl_coverage),
        )
        .route("/api/v1/discovery-runs", get(list_discovery_runs))
        .route("/api/v1/export-targets", get(list_export_targets))
        .route("/api/v1/export-runs", get(list_export_runs))
        .route("/news", get(news_dashboard))
        .route("/review", get(review_dashboard))
        .with_state(state)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([Method::GET]),
        )
        .layer(TraceLayer::new_for_http())
}

async fn list_companies(
    State(state): State<ApiState>,
    Query(query): Query<PaginationQuery>,
) -> Result<Json<Page<Company>>, ApiError> {
    let pagination = query.resolve();
    let (items, total) = tokio::try_join!(
        state
            .database
            .list_companies(pagination.limit, pagination.offset),
        state.database.count_companies(),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn get_company(
    State(state): State<ApiState>,
    Path(company_key): Path<String>,
) -> Result<Json<Company>, ApiError> {
    state
        .database
        .get_company_by_key(&company_key)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("company {company_key} was not found")))
}

async fn get_company_profile(
    State(state): State<ApiState>,
    Path(company_key): Path<String>,
) -> Result<Json<CompanyProfile>, ApiError> {
    let company = state
        .database
        .get_company_by_key(&company_key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("company {company_key} was not found")))?;
    let company_id = company.id;
    let (
        source_candidates,
        source_candidate_total,
        approved_sources,
        approved_source_total,
        discovery_runs,
        news_extraction_runs,
        news_recipes,
        news_recipe_total,
    ) = tokio::try_join!(
        state.database.list_source_candidates(
            Some(company_id),
            Some(CandidateStatus::New),
            COMPANY_PROFILE_RESOURCE_LIMIT,
            0,
        ),
        state
            .database
            .count_source_candidates(Some(company_id), Some(CandidateStatus::New)),
        state.database.list_sources(
            Some(company_id),
            Some(SourceStatus::Approved),
            COMPANY_PROFILE_RESOURCE_LIMIT,
            0,
        ),
        state
            .database
            .count_sources(Some(company_id), Some(SourceStatus::Approved)),
        state.database.list_discovery_runs(Some(company_id), 1, 0),
        state
            .database
            .list_company_news_extraction_runs(Some(company_id), 1, 0),
        state.database.list_company_news_recipes(
            Some(company_id),
            None,
            COMPANY_PROFILE_RESOURCE_LIMIT,
            0,
        ),
        state
            .database
            .count_company_news_recipes(Some(company_id), None),
    )?;

    Ok(Json(CompanyProfile {
        company,
        latest_discovery_run: discovery_runs.into_iter().next(),
        latest_news_extraction_run: news_extraction_runs.into_iter().next(),
        source_candidates,
        source_candidate_total,
        approved_sources,
        approved_source_total,
        news_recipes,
        news_recipe_total,
    }))
}

async fn list_source_candidates(
    State(state): State<ApiState>,
    Query(query): Query<CandidateQuery>,
) -> Result<Json<Page<SourceCandidate>>, ApiError> {
    let pagination = resolve_embedded_pagination(query.limit, query.offset);
    let (items, total) = tokio::try_join!(
        state.database.list_source_candidates(
            query.company_id,
            query.status,
            pagination.limit,
            pagination.offset,
        ),
        state
            .database
            .count_source_candidates(query.company_id, query.status),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn get_review_dashboard(
    State(state): State<ApiState>,
) -> Result<Json<ReviewDashboard>, ApiError> {
    Ok(Json(state.database.get_review_dashboard().await?))
}

async fn list_review_candidates(
    State(state): State<ApiState>,
    Query(query): Query<ReviewCandidateQuery>,
) -> Result<Json<Page<CandidateReviewItem>>, ApiError> {
    let pagination = resolve_embedded_pagination(query.limit, query.offset);
    let candidate_status = query.status.or(Some(CandidateStatus::New));
    let feed_only = !query.include_html.unwrap_or(false);
    let (items, total) = tokio::try_join!(
        state.database.list_review_candidates(
            candidate_status,
            query.validation_status,
            query.kind,
            feed_only,
            pagination.limit,
            pagination.offset,
        ),
        state.database.count_review_candidates(
            candidate_status,
            query.validation_status,
            query.kind,
            feed_only,
        ),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn list_review_source_health(
    State(state): State<ApiState>,
    Query(query): Query<PaginationQuery>,
) -> Result<Json<Page<SourceHealthSummary>>, ApiError> {
    let pagination = query.resolve();
    let (items, total) = tokio::try_join!(
        state
            .database
            .list_review_source_health(pagination.limit, pagination.offset),
        state
            .database
            .count_sources(None, Some(SourceStatus::Approved)),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn list_candidate_validation_runs(
    State(state): State<ApiState>,
    Query(query): Query<CandidateResourceQuery>,
) -> Result<Json<Page<CandidateValidationRun>>, ApiError> {
    let pagination = resolve_embedded_pagination(query.limit, query.offset);
    let (items, total) = tokio::try_join!(
        state.database.list_candidate_validation_runs(
            query.candidate_id,
            pagination.limit,
            pagination.offset,
        ),
        state
            .database
            .count_candidate_validation_runs(query.candidate_id),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn list_candidate_decisions(
    State(state): State<ApiState>,
    Query(query): Query<CandidateResourceQuery>,
) -> Result<Json<Page<CandidateDecisionRecord>>, ApiError> {
    let pagination = resolve_embedded_pagination(query.limit, query.offset);
    let (items, total) = tokio::try_join!(
        state.database.list_candidate_decisions(
            query.candidate_id,
            pagination.limit,
            pagination.offset,
        ),
        state.database.count_candidate_decisions(query.candidate_id),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn validate_source_candidate(
    State(state): State<ApiState>,
    Path(candidate_id): Path<Uuid>,
) -> Result<(StatusCode, Json<feed_core::Job>), ApiError> {
    let job = state
        .database
        .enqueue_candidate_validation(candidate_id, Utc::now(), i16::MAX)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn activate_source_candidate(
    State(state): State<ApiState>,
    Path(candidate_id): Path<Uuid>,
    Json(request): Json<DecisionRequest>,
) -> Result<Json<Source>, ApiError> {
    validate_decision_request(&request)?;
    let source = activate_candidate(&state.database, candidate_id, &request).await?;
    Ok(Json(source))
}

async fn reject_source_candidate(
    State(state): State<ApiState>,
    Path(candidate_id): Path<Uuid>,
    Json(request): Json<DecisionRequest>,
) -> Result<StatusCode, ApiError> {
    validate_decision_request(&request)?;
    state
        .database
        .reject_source_candidate_with_decision(
            candidate_id,
            CandidateDecisionMode::Operator,
            &request.actor,
            &request.reason,
            serde_json::json!({ "origin": "review_api" }),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn batch_source_candidate_action(
    State(state): State<ApiState>,
    Json(request): Json<BatchCandidateActionRequest>,
) -> Result<Json<BatchCandidateActionResponse>, ApiError> {
    if request.candidate_ids.is_empty() || request.candidate_ids.len() > 100 {
        return Err(ApiError::BadRequest(
            "candidate_ids must contain between 1 and 100 IDs".to_owned(),
        ));
    }
    if request.actor.trim().is_empty() {
        return Err(ApiError::BadRequest("actor cannot be empty".to_owned()));
    }
    if request.action != BatchAction::Validate && request.reason.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "reason is required for activation or rejection".to_owned(),
        ));
    }

    let mut results = Vec::with_capacity(request.candidate_ids.len());
    for candidate_id in request.candidate_ids {
        let result = match request.action {
            BatchAction::Validate => state
                .database
                .enqueue_candidate_validation(candidate_id, Utc::now(), i16::MAX)
                .await
                .map(|_| (true, "validation_queued".to_owned())),
            BatchAction::Activate => {
                let decision = DecisionRequest {
                    actor: request.actor.clone(),
                    reason: request.reason.clone(),
                    public_export_allowed: request.public_export_allowed,
                    freshness_slo_seconds: request.freshness_slo_seconds,
                };
                activate_candidate(&state.database, candidate_id, &decision)
                    .await
                    .map(|source| (true, format!("activated:{}", source.source_id)))
            }
            BatchAction::Reject => state
                .database
                .reject_source_candidate_with_decision(
                    candidate_id,
                    CandidateDecisionMode::Operator,
                    &request.actor,
                    &request.reason,
                    serde_json::json!({ "origin": "review_api_batch" }),
                )
                .await
                .map(|()| (true, "rejected".to_owned())),
        };
        results.push(match result {
            Ok((success, outcome)) => BatchCandidateActionResult {
                candidate_id,
                success,
                outcome,
            },
            Err(error) => BatchCandidateActionResult {
                candidate_id,
                success: false,
                outcome: error.to_string(),
            },
        });
    }

    Ok(Json(BatchCandidateActionResponse { results }))
}

async fn activate_candidate(
    database: &Database,
    candidate_id: Uuid,
    request: &DecisionRequest,
) -> Result<Source, DatabaseError> {
    let candidate =
        database
            .get_source_candidate(candidate_id)
            .await?
            .ok_or(DatabaseError::NotFound {
                entity: "source candidate",
                id: candidate_id,
            })?;
    if !matches!(candidate.candidate_kind, SourceKind::Rss | SourceKind::Atom) {
        return Err(DatabaseError::InvalidState(
            "only RSS/Atom candidates can be activated".to_owned(),
        ));
    }
    let company =
        database
            .get_company(candidate.company_id)
            .await?
            .ok_or(DatabaseError::NotFound {
                entity: "company",
                id: candidate.company_id,
            })?;
    let source_id = default_source_id(&company.company_key, candidate.candidate_kind, candidate.id);
    let source = database
        .accept_source_candidate_with_decision(
            candidate.id,
            &source_id,
            request.freshness_slo_seconds.unwrap_or(3_600),
            request.public_export_allowed.unwrap_or(false),
            CandidateDecisionMode::Operator,
            &request.actor,
            &request.reason,
            serde_json::json!({ "origin": "review_api" }),
        )
        .await?;
    let mut job = JobSpec::new(
        JobType::CrawlSource,
        format!("source:{}", source.id),
        Utc::now(),
    );
    job.company_id = Some(source.company_id);
    job.source_id = Some(source.id);
    job.priority = i16::MAX / 2;
    job.payload = serde_json::json!({ "source_id": source.id });
    database.enqueue_job(&job).await?;
    Ok(source)
}

fn validate_decision_request(request: &DecisionRequest) -> Result<(), ApiError> {
    if request.actor.trim().is_empty() {
        return Err(ApiError::BadRequest("actor cannot be empty".to_owned()));
    }
    if request.reason.trim().is_empty() {
        return Err(ApiError::BadRequest("reason cannot be empty".to_owned()));
    }
    if request
        .freshness_slo_seconds
        .is_some_and(|value| value <= 0)
    {
        return Err(ApiError::BadRequest(
            "freshness_slo_seconds must be positive".to_owned(),
        ));
    }
    Ok(())
}

fn default_source_id(company_key: &str, kind: SourceKind, candidate_id: Uuid) -> String {
    let suffix = candidate_id.simple().to_string();
    format!("{company_key}-{}-{}", kind.as_str(), &suffix[..12])
}

async fn list_sources(
    State(state): State<ApiState>,
    Query(query): Query<SourceQuery>,
) -> Result<Json<Page<Source>>, ApiError> {
    let pagination = resolve_embedded_pagination(query.limit, query.offset);
    let (items, total) = tokio::try_join!(
        state.database.list_sources(
            query.company_id,
            query.status,
            pagination.limit,
            pagination.offset,
        ),
        state.database.count_sources(query.company_id, query.status),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn list_feed_items(
    State(state): State<ApiState>,
    Query(query): Query<FeedItemQuery>,
) -> Result<Json<Page<FeedItem>>, ApiError> {
    let pagination = resolve_embedded_pagination(query.limit, query.offset);
    let (items, total) = tokio::try_join!(
        state.database.list_feed_items(
            query.company_id,
            query.source_id,
            pagination.limit,
            pagination.offset,
        ),
        state
            .database
            .count_feed_items(query.company_id, query.source_id),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn list_news_items(
    State(state): State<ApiState>,
    Query(query): Query<NewsItemQuery>,
) -> Result<Response, ApiError> {
    let pagination = resolve_embedded_pagination(query.limit, query.offset);
    let search = query
        .q
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if search
        .as_ref()
        .is_some_and(|value| value.chars().count() > 200)
    {
        return Err(ApiError::BadRequest(
            "news search must not exceed 200 characters".to_owned(),
        ));
    }
    let filter = FeedItemSummaryFilter {
        company_id: query.company_id,
        source_id: query.source_id,
        source_kind: query.source_kind,
        search,
        include_future: query.include_future.unwrap_or(false),
        limit: pagination.limit,
        offset: pagination.offset,
    };
    let (items, total) = tokio::try_join!(
        state.database.list_feed_item_summaries(&filter),
        state.database.count_feed_item_summaries(&filter),
    )?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(Page::new(items, total, pagination)),
    )
        .into_response())
}

async fn get_feed_item(
    State(state): State<ApiState>,
    Path(item_id): Path<Uuid>,
) -> Result<Json<FeedItem>, ApiError> {
    state
        .database
        .get_feed_item(item_id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("feed item {item_id} was not found")))
}

async fn list_source_health(
    State(state): State<ApiState>,
    Query(query): Query<SourceHealthQuery>,
) -> Result<Json<Page<SourceHealth>>, ApiError> {
    let pagination = resolve_embedded_pagination(query.limit, query.offset);
    let (items, total) = tokio::try_join!(
        state
            .database
            .list_source_health(query.source_id, pagination.limit, pagination.offset,),
        state.database.count_source_health(query.source_id),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn list_crawl_runs(
    State(state): State<ApiState>,
    Query(query): Query<SourceHealthQuery>,
) -> Result<Json<Page<CrawlRun>>, ApiError> {
    let pagination = resolve_embedded_pagination(query.limit, query.offset);
    let (items, total) = tokio::try_join!(
        state
            .database
            .list_crawl_runs(query.source_id, pagination.limit, pagination.offset),
        state.database.count_crawl_runs(query.source_id),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn list_discovery_runs(
    State(state): State<ApiState>,
    Query(query): Query<DiscoveryRunQuery>,
) -> Result<Json<Page<DiscoveryRun>>, ApiError> {
    let pagination = resolve_embedded_pagination(query.limit, query.offset);
    let (items, total) = tokio::try_join!(
        state
            .database
            .list_discovery_runs(query.company_id, pagination.limit, pagination.offset,),
        state.database.count_discovery_runs(query.company_id),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn list_company_news_extraction_runs(
    State(state): State<ApiState>,
    Query(query): Query<DiscoveryRunQuery>,
) -> Result<Json<Page<CompanyNewsExtractionRun>>, ApiError> {
    let pagination = resolve_embedded_pagination(query.limit, query.offset);
    let (items, total) = tokio::try_join!(
        state.database.list_company_news_extraction_runs(
            query.company_id,
            pagination.limit,
            pagination.offset,
        ),
        state
            .database
            .count_company_news_extraction_runs(query.company_id),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn list_company_news_recipes(
    State(state): State<ApiState>,
    Query(query): Query<RecipeQuery>,
) -> Result<Json<Page<CompanyNewsRecipe>>, ApiError> {
    let pagination = resolve_embedded_pagination(query.limit, query.offset);
    let (items, total) = tokio::try_join!(
        state.database.list_company_news_recipes(
            query.company_id,
            query.status,
            pagination.limit,
            pagination.offset,
        ),
        state
            .database
            .count_company_news_recipes(query.company_id, query.status),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn list_company_news_recipe_runs(
    State(state): State<ApiState>,
    Query(query): Query<RecipeRunQuery>,
) -> Result<Json<Page<CompanyNewsRecipeRun>>, ApiError> {
    let pagination = resolve_embedded_pagination(query.limit, query.offset);
    let (items, total) = tokio::try_join!(
        state.database.list_company_news_recipe_runs(
            query.recipe_id,
            pagination.limit,
            pagination.offset,
        ),
        state
            .database
            .count_company_news_recipe_runs(query.recipe_id),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn get_company_news_recipe_coverage(
    State(state): State<ApiState>,
) -> Result<Json<CompanyNewsRecipeCoverage>, ApiError> {
    Ok(Json(
        state.database.get_company_news_recipe_coverage().await?,
    ))
}

#[derive(Debug, Deserialize)]
struct ContentCrawlCoverageQuery {
    #[serde(default = "default_content_crawl_min_chars")]
    min_content_chars: i32,
}

fn default_content_crawl_min_chars() -> i32 {
    200
}

async fn get_content_crawl_coverage(
    State(state): State<ApiState>,
    Query(query): Query<ContentCrawlCoverageQuery>,
) -> Result<Json<ContentCrawlCoverage>, ApiError> {
    if query.min_content_chars <= 0 {
        return Err(ApiError::BadRequest(
            "min_content_chars must be positive".to_owned(),
        ));
    }
    Ok(Json(
        state
            .database
            .content_crawl_coverage(query.min_content_chars)
            .await?,
    ))
}

async fn list_export_targets(
    State(state): State<ApiState>,
    Query(query): Query<PaginationQuery>,
) -> Result<Json<Page<ExportTarget>>, ApiError> {
    let pagination = query.resolve();
    let (items, total) = tokio::try_join!(
        state
            .database
            .list_export_targets(pagination.limit, pagination.offset),
        state.database.count_export_targets(),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn list_export_runs(
    State(state): State<ApiState>,
    Query(query): Query<ExportRunQuery>,
) -> Result<Json<Page<ExportRun>>, ApiError> {
    let pagination = resolve_embedded_pagination(query.limit, query.offset);
    let (items, total) = tokio::try_join!(
        state.database.list_export_runs(
            query.export_target_id,
            pagination.limit,
            pagination.offset,
        ),
        state.database.count_export_runs(query.export_target_id),
    )?;
    Ok(Json(Page::new(items, total, pagination)))
}

async fn review_dashboard() -> Html<&'static str> {
    Html(REVIEW_DASHBOARD_HTML)
}

async fn news_dashboard() -> Html<&'static str> {
    Html(NEWS_DASHBOARD_HTML)
}

async fn health(State(state): State<ApiState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: state.service_name,
        version: env!("CARGO_PKG_VERSION"),
        uptime_seconds: state.started_at.elapsed().as_secs(),
        job_runner_enabled: state.job_runner_enabled,
        supported_job_types: state
            .supported_job_types
            .iter()
            .map(|job_type| job_type.as_str())
            .collect(),
    })
}

async fn readiness(State(state): State<ApiState>) -> Response {
    match state.database.ping().await {
        Ok(()) => (
            StatusCode::OK,
            Json(ReadinessResponse {
                status: "ready",
                database: "reachable",
            }),
        )
            .into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadinessResponse {
                status: "not_ready",
                database: "unreachable",
            }),
        )
            .into_response(),
    }
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
    version: &'static str,
    uptime_seconds: u64,
    job_runner_enabled: bool,
    supported_job_types: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct ReadinessResponse {
    status: &'static str,
    database: &'static str,
}

#[derive(Debug, Serialize)]
struct CompanyProfile {
    company: Company,
    latest_discovery_run: Option<DiscoveryRun>,
    latest_news_extraction_run: Option<CompanyNewsExtractionRun>,
    source_candidates: Vec<SourceCandidate>,
    source_candidate_total: i64,
    approved_sources: Vec<Source>,
    approved_source_total: i64,
    news_recipes: Vec<CompanyNewsRecipe>,
    news_recipe_total: i64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct PaginationQuery {
    limit: Option<u32>,
    offset: Option<u64>,
}

impl PaginationQuery {
    fn resolve(self) -> Pagination {
        Pagination {
            limit: i64::from(self.limit.unwrap_or(DEFAULT_PAGE_LIMIT).min(MAX_PAGE_LIMIT)),
            offset: i64::try_from(self.offset.unwrap_or_default()).unwrap_or(i64::MAX),
        }
    }
}

fn resolve_embedded_pagination(limit: Option<u32>, offset: Option<u64>) -> Pagination {
    PaginationQuery { limit, offset }.resolve()
}

#[derive(Clone, Copy, Debug)]
struct Pagination {
    limit: i64,
    offset: i64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct CandidateQuery {
    company_id: Option<Uuid>,
    status: Option<CandidateStatus>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct ReviewCandidateQuery {
    status: Option<CandidateStatus>,
    validation_status: Option<CandidateValidationStatus>,
    kind: Option<SourceKind>,
    include_html: Option<bool>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct CandidateResourceQuery {
    candidate_id: Option<Uuid>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
struct DecisionRequest {
    actor: String,
    reason: String,
    public_export_allowed: Option<bool>,
    freshness_slo_seconds: Option<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BatchAction {
    Validate,
    Activate,
    Reject,
}

#[derive(Clone, Debug, Deserialize)]
struct BatchCandidateActionRequest {
    candidate_ids: Vec<Uuid>,
    action: BatchAction,
    actor: String,
    #[serde(default)]
    reason: String,
    public_export_allowed: Option<bool>,
    freshness_slo_seconds: Option<i32>,
}

#[derive(Debug, Serialize)]
struct BatchCandidateActionResponse {
    results: Vec<BatchCandidateActionResult>,
}

#[derive(Debug, Serialize)]
struct BatchCandidateActionResult {
    candidate_id: Uuid,
    success: bool,
    outcome: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct SourceQuery {
    company_id: Option<Uuid>,
    status: Option<SourceStatus>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct FeedItemQuery {
    company_id: Option<Uuid>,
    source_id: Option<Uuid>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct NewsItemQuery {
    company_id: Option<Uuid>,
    source_id: Option<Uuid>,
    source_kind: Option<SourceKind>,
    q: Option<String>,
    include_future: Option<bool>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct SourceHealthQuery {
    source_id: Option<Uuid>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct DiscoveryRunQuery {
    company_id: Option<Uuid>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct RecipeQuery {
    company_id: Option<Uuid>,
    status: Option<RecipeStatus>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct RecipeRunQuery {
    recipe_id: Option<Uuid>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct ExportRunQuery {
    export_target_id: Option<Uuid>,
    limit: Option<u32>,
    offset: Option<u64>,
}

#[derive(Debug, Serialize)]
struct Page<T> {
    items: Vec<T>,
    total: i64,
    limit: i64,
    offset: i64,
}

impl<T> Page<T> {
    fn new(items: Vec<T>, total: i64, pagination: Pagination) -> Self {
        Self {
            items,
            total,
            limit: pagination.limit,
            offset: pagination.offset,
        }
    }
}

#[derive(Debug)]
enum ApiError {
    Database(DatabaseError),
    BadRequest(String),
    NotFound(String),
}

impl From<DatabaseError> for ApiError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(message) => (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "bad_request",
                    message: &message,
                }),
            )
                .into_response(),
            Self::Database(error) => {
                error!(%error, "API database operation failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: "internal_error",
                        message: "the request could not be completed",
                    }),
                )
                    .into_response()
            }
            Self::NotFound(message) => (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "not_found",
                    message: &message,
                }),
            )
                .into_response(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ErrorResponse<'a> {
    error: &'static str,
    message: &'a str,
}

const NEWS_DASHBOARD_HTML: &str = include_str!("../../../docs/news-viewer.html");

const REVIEW_DASHBOARD_HTML: &str = r####"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Company Source Review</title>
  <style>
    :root {
      color-scheme: light;
      --ink: #162019;
      --muted: #617067;
      --paper: #f5f3eb;
      --panel: #fffdf7;
      --line: #d9d7cc;
      --green: #16633d;
      --orange: #aa541a;
      --red: #a33131;
      --blue: #245a8d;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      background: var(--paper);
      color: var(--ink);
      font: 14px/1.45 ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    }
    header, main { width: min(1480px, calc(100% - 32px)); margin: 0 auto; }
    header { padding: 30px 0 18px; }
    h1 { margin: 0; font: 700 30px/1.1 ui-serif, Georgia, serif; }
    h2 { margin: 28px 0 6px; font-size: 22px; }
    header p { color: var(--muted); margin: 8px 0 0; }
    .metrics {
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
      gap: 10px;
      margin-bottom: 16px;
    }
    .metric, .controls, .table-wrap {
      background: var(--panel);
      border: 1px solid var(--line);
      border-radius: 12px;
    }
    .metric { padding: 14px; }
    .metric strong { display: block; font-size: 24px; }
    .metric span { color: var(--muted); }
    .controls {
      display: flex;
      flex-wrap: wrap;
      gap: 10px;
      padding: 12px;
      margin-bottom: 16px;
      align-items: end;
    }
    label { color: var(--muted); font-size: 12px; }
    label > input, label > select { display: block; margin-top: 4px; }
    input, select, button {
      border: 1px solid var(--line);
      border-radius: 8px;
      background: white;
      min-height: 36px;
      padding: 7px 10px;
      color: var(--ink);
      font: inherit;
    }
    input[type="text"] { min-width: 220px; }
    input[type="checkbox"] { min-height: auto; }
    button { cursor: pointer; font-weight: 650; }
    button:hover { border-color: var(--blue); }
    button.primary { color: white; background: var(--green); border-color: var(--green); }
    button.danger { color: white; background: var(--red); border-color: var(--red); }
    .table-wrap { overflow-x: auto; margin-bottom: 28px; }
    table { width: 100%; border-collapse: collapse; min-width: 1080px; }
    th, td { text-align: left; padding: 10px; border-bottom: 1px solid var(--line); vertical-align: top; }
    th { position: sticky; top: 0; background: #ece9df; font-size: 12px; color: var(--muted); }
    tr:last-child td { border-bottom: 0; }
    .url { max-width: 380px; overflow-wrap: anywhere; }
    .url a { color: var(--blue); }
    .badge { display: inline-block; border-radius: 999px; padding: 2px 8px; background: #e5e4dc; }
    .badge.valid { color: var(--green); background: #dcecdf; }
    .badge.needs_review { color: var(--orange); background: #f5e5d2; }
    .badge.invalid, .badge.failed { color: var(--red); background: #f2dada; }
    .actions { display: flex; flex-wrap: wrap; gap: 5px; }
    .actions button { min-height: 30px; padding: 4px 8px; font-size: 12px; }
    #status { color: var(--muted); min-width: 160px; }
    .section-note { color: var(--muted); margin: 0 0 10px; }
    @media (max-width: 720px) {
      header, main { width: min(100% - 18px, 1480px); }
      h1 { font-size: 25px; }
      .controls { align-items: stretch; }
      label, label > input, label > select, .controls > button { width: 100%; }
    }
  </style>
</head>
<body>
  <header>
    <h1>Company Source Review</h1>
    <p>AI-assisted discoveries activate provisionally when they contain usable feed items. Disable a source when its company association or content is wrong.</p>
  </header>
  <main>
    <section id="metrics" class="metrics"></section>
    <h2>Candidate review</h2>
    <section class="controls">
      <label>Validation
        <select id="validation-filter">
          <option value="">All / unvalidated</option>
          <option value="needs_review">Needs review</option>
          <option value="invalid">Invalid</option>
          <option value="failed">Failed</option>
          <option value="running">Running</option>
          <option value="valid">Valid</option>
        </select>
      </label>
      <label>Actor <input id="actor" type="text" value="operator"></label>
      <label>Decision reason <input id="reason" type="text" placeholder="Why this decision is correct"></label>
      <label>Public export <input id="public-export" type="checkbox"></label>
      <button id="refresh">Refresh</button>
      <button id="batch-validate">Validate selected</button>
      <button id="batch-activate" class="primary">Activate selected</button>
      <button id="batch-reject" class="danger">Reject selected</button>
      <span id="status">Loading…</span>
    </section>
    <section class="table-wrap">
      <table>
        <thead>
          <tr>
            <th><input id="select-all" type="checkbox" aria-label="Select all"></th>
            <th>Company</th>
            <th>Candidate</th>
            <th>Kind</th>
            <th>Confidence</th>
            <th>Validation</th>
            <th>Items</th>
            <th>Latest item</th>
            <th>Evidence / reason</th>
            <th>Actions</th>
          </tr>
        </thead>
        <tbody id="rows"></tbody>
      </table>
    </section>
    <h2>Activated source health</h2>
    <p id="source-status" class="section-note">Loading…</p>
    <section class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Company</th>
            <th>Source</th>
            <th>Basis</th>
            <th>Health</th>
            <th>Last success</th>
            <th>Stored items</th>
            <th>Latest article</th>
            <th>Failures</th>
            <th>Public export</th>
            <th>Actions</th>
          </tr>
        </thead>
        <tbody id="source-rows"></tbody>
      </table>
    </section>
  </main>
  <script>
    const selected = new Set();
    const metrics = [
      ["total_companies", "Companies"],
      ["companies_with_feed_candidates", "Companies with feed candidates"],
      ["companies_with_activated_sources", "Companies with active sources"],
      ["companies_with_healthy_sources", "Companies healthy"],
      ["new_feed_candidates", "New feed candidates"],
      ["unvalidated_feed_candidates", "Unvalidated"],
      ["validation_pending", "Queued"],
      ["validation_running", "Running"],
      ["needs_review_candidates", "Needs review"],
      ["activated_sources", "Activated sources"],
      ["healthy_sources", "Healthy sources"],
      ["failing_sources", "Failing sources"],
      ["sources_awaiting_first_crawl", "Awaiting first crawl"],
      ["normalized_items", "Normalized items"]
    ];

    async function request(path, options = {}) {
      const response = await fetch(path, {
        headers: { "content-type": "application/json" },
        ...options
      });
      if (!response.ok) {
        const body = await response.json().catch(() => ({}));
        throw new Error(body.message || `${response.status} ${response.statusText}`);
      }
      return response.status === 204 ? null : response.json();
    }

    function text(tag, value, className) {
      const element = document.createElement(tag);
      element.textContent = value ?? "";
      if (className) element.className = className;
      return element;
    }

    function renderMetrics(data) {
      const root = document.getElementById("metrics");
      root.replaceChildren();
      for (const [key, label] of metrics) {
        const card = text("article", "", "metric");
        card.append(text("strong", Number(data[key] || 0).toLocaleString()));
        card.append(text("span", label));
        root.append(card);
      }
    }

    function actionButton(label, action, candidateId, className = "") {
      const button = text("button", label, className);
      button.addEventListener("click", () => runSingle(action, candidateId));
      return button;
    }

    function renderRows(items) {
      const root = document.getElementById("rows");
      root.replaceChildren();
      selected.clear();
      document.getElementById("select-all").checked = false;
      for (const item of items) {
        const candidate = item.candidate;
        const validation = item.latest_validation;
        const row = document.createElement("tr");
        const selectCell = document.createElement("td");
        const checkbox = document.createElement("input");
        checkbox.type = "checkbox";
        checkbox.addEventListener("change", () => {
          checkbox.checked ? selected.add(candidate.id) : selected.delete(candidate.id);
        });
        selectCell.append(checkbox);
        row.append(selectCell);
        row.append(text("td", item.company_name));

        const urlCell = text("td", "", "url");
        const link = text("a", candidate.candidate_url);
        link.href = candidate.candidate_url;
        link.target = "_blank";
        link.rel = "noopener noreferrer";
        urlCell.append(link);
        row.append(urlCell);
        row.append(text("td", candidate.candidate_kind.toUpperCase()));
        row.append(text("td", `${Math.round(candidate.confidence * 100)}%`));

        const validationCell = document.createElement("td");
        validationCell.append(text(
          "span",
          validation?.status || "unvalidated",
          `badge ${validation?.status || ""}`
        ));
        row.append(validationCell);
        row.append(text("td", validation ? `${validation.titled_item_count}/${validation.item_count}` : "—"));
        row.append(text("td", validation?.latest_item_at ? new Date(validation.latest_item_at).toLocaleDateString() : "—"));
        row.append(text("td", validation?.policy_reasons?.join(", ") || "Awaiting validation"));

        const actions = text("td", "", "actions");
        actions.append(actionButton("Validate", "validate", candidate.id));
        actions.append(actionButton("Activate", "activate", candidate.id, "primary"));
        actions.append(actionButton("Reject", "reject", candidate.id, "danger"));
        row.append(actions);
        root.append(row);
      }
    }

    function renderSourceRows(items) {
      const root = document.getElementById("source-rows");
      root.replaceChildren();
      for (const item of items) {
        const source = item.source;
        const health = item.health;
        let healthLabel = "awaiting crawl";
        let healthClass = "needs_review";
        if (health?.consecutive_failures > 0) {
          healthLabel = "failing";
          healthClass = "failed";
        } else if (health?.last_success_at) {
          healthLabel = health.consecutive_zero_runs > 0 ? "empty" : "healthy";
          healthClass = health.consecutive_zero_runs > 0 ? "needs_review" : "valid";
        }

        const row = document.createElement("tr");
        row.append(text("td", item.company_name));
        const sourceCell = text("td", "", "url");
        const link = text("a", source.url);
        link.href = source.url;
        link.target = "_blank";
        link.rel = "noopener noreferrer";
        sourceCell.append(link);
        sourceCell.append(text("div", source.source_id));
        row.append(sourceCell);
        const activation = source.metadata?.activation;
        const provisional = activation?.metadata?.provisional === true;
        const basisLabel = provisional
          ? "AI-assisted / provisional"
          : activation?.decision_mode === "operator"
            ? "operator"
            : "verified";
        const basisCell = document.createElement("td");
        basisCell.append(text(
          "span",
          basisLabel,
          `badge ${provisional ? "needs_review" : "valid"}`
        ));
        row.append(basisCell);
        const healthCell = document.createElement("td");
        healthCell.append(text("span", healthLabel, `badge ${healthClass}`));
        row.append(healthCell);
        row.append(text("td", health?.last_success_at ? new Date(health.last_success_at).toLocaleString() : "—"));
        row.append(text("td", Number(item.stored_item_count).toLocaleString()));
        row.append(text("td", item.latest_item_at ? new Date(item.latest_item_at).toLocaleDateString() : "—"));
        row.append(text("td", health?.consecutive_failures || 0));
        row.append(text("td", source.public_export_allowed ? "yes" : "no"));
        const actions = text("td", "", "actions");
        if (source.metadata?.accepted_candidate_id) {
          actions.append(actionButton(
            "Wrong / disable",
            "reject",
            source.metadata.accepted_candidate_id,
            "danger"
          ));
        }
        row.append(actions);
        root.append(row);
      }
    }

    function decisionBody() {
      return {
        actor: document.getElementById("actor").value.trim(),
        reason: document.getElementById("reason").value.trim(),
        public_export_allowed: document.getElementById("public-export").checked,
        freshness_slo_seconds: 3600
      };
    }

    async function runSingle(action, candidateId) {
      const status = document.getElementById("status");
      try {
        status.textContent = `${action}…`;
        const options = { method: "POST" };
        if (action !== "validate") options.body = JSON.stringify(decisionBody());
        await request(`/api/v1/source-candidates/${candidateId}/${action}`, options);
        await load();
      } catch (error) {
        status.textContent = error.message;
      }
    }

    async function runBatch(action) {
      const status = document.getElementById("status");
      if (selected.size === 0) {
        status.textContent = "Select at least one candidate.";
        return;
      }
      const decision = decisionBody();
      try {
        status.textContent = `${action} ${selected.size}…`;
        const result = await request("/api/v1/source-candidates/batch", {
          method: "POST",
          body: JSON.stringify({
            candidate_ids: [...selected],
            action,
            ...decision
          })
        });
        const failures = result.results.filter(item => !item.success).length;
        status.textContent = failures ? `${failures} actions failed` : "Batch complete";
        await load();
      } catch (error) {
        status.textContent = error.message;
      }
    }

    async function load() {
      const status = document.getElementById("status");
      status.textContent = "Loading…";
      try {
        const validation = document.getElementById("validation-filter").value;
        const query = new URLSearchParams({ status: "new", limit: "100" });
        if (validation) query.set("validation_status", validation);
        const [dashboard, candidates, sources] = await Promise.all([
          request("/api/v1/review/dashboard"),
          request(`/api/v1/review/candidates?${query}`),
          request("/api/v1/review/sources?limit=100")
        ]);
        renderMetrics(dashboard);
        renderRows(candidates.items);
        renderSourceRows(sources.items);
        status.textContent = `${candidates.items.length} of ${candidates.total} candidates`;
        document.getElementById("source-status").textContent =
          `${sources.items.length} of ${sources.total} activated sources`;
      } catch (error) {
        status.textContent = error.message;
      }
    }

    document.getElementById("refresh").addEventListener("click", load);
    document.getElementById("validation-filter").addEventListener("change", load);
    document.getElementById("batch-validate").addEventListener("click", () => runBatch("validate"));
    document.getElementById("batch-activate").addEventListener("click", () => runBatch("activate"));
    document.getElementById("batch-reject").addEventListener("click", () => runBatch("reject"));
    document.getElementById("select-all").addEventListener("change", event => {
      document.querySelectorAll("#rows input[type=checkbox]").forEach(checkbox => {
        checkbox.checked = event.target.checked;
        checkbox.dispatchEvent(new Event("change"));
      });
    });
    load();
  </script>
</body>
</html>
"####;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn api_state_records_registered_handlers() {
        let options = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://unused:unused@localhost/unused")
            .expect("valid test URL");
        let state = ApiState::new(
            Database::from_pool(options),
            "test-service",
            true,
            vec![JobType::DiscoverCompany],
        );

        assert_eq!(state.service_name, "test-service");
        assert_eq!(state.supported_job_types, vec![JobType::DiscoverCompany]);
    }
}
