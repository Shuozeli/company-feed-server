#!/usr/bin/env bash
set -euo pipefail

readonly SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly DEFAULT_WORKDIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
readonly WORKDIR="${WORKDIR:-$DEFAULT_WORKDIR}"
readonly DATABASE_URL="${DATABASE_URL:-postgresql://company_feed:company_feed@localhost:55432/company_feed}"
readonly CAMPAIGN_START="${CAMPAIGN_START:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}"
readonly INITIAL_RETRY_AFTER="${INITIAL_RETRY_AFTER:-$CAMPAIGN_START}"
readonly QUIET_CHECKS_REQUIRED="${QUIET_CHECKS_REQUIRED:-6}"
readonly QUIET_CHECK_INTERVAL_SECONDS="${QUIET_CHECK_INTERVAL_SECONDS:-30}"
readonly CAMPAIGN_LIMIT="${CAMPAIGN_LIMIT:-10000}"
readonly CAMPAIGN_SPACING_SECONDS="${CAMPAIGN_SPACING_SECONDS:-1}"
readonly LOOKBACK_DAYS="${LOOKBACK_DAYS:-93}"
readonly MAX_ARTICLES="${MAX_ARTICLES:-20}"
readonly SUPERVISOR_LOCK_FILE="${SUPERVISOR_LOCK_FILE:-/tmp/company-feed-recipe-campaign-supervisor.lock}"

exec 9>"$SUPERVISOR_LOCK_FILE"
if ! flock -n 9; then
  printf '%s supervisor_already_running lock=%s\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$SUPERVISOR_LOCK_FILE" >&2
  exit 1
fi

timestamp() {
  date -u +"%Y-%m-%dT%H:%M:%SZ"
}

active_extraction_jobs() {
  psql "$DATABASE_URL" -X -At -c \
    "SELECT count(*)
     FROM jobs
     WHERE job_type='extract_company_news'
       AND status IN ('pending','running');"
}

active_activation_crawls() {
  psql "$DATABASE_URL" -X -At -c \
    "SELECT count(*)
     FROM jobs
     WHERE job_type='crawl_source'
       AND created_at >= timestamptz '$CAMPAIGN_START'
       AND payload->>'trigger'='recipe_activation'
       AND status IN ('pending','running');"
}

wait_for_extraction_quiet() {
  local phase="$1"
  local quiet_checks=0
  while (( quiet_checks < QUIET_CHECKS_REQUIRED )); do
    local active
    active="$(active_extraction_jobs)"
    if [[ "$active" == "0" ]]; then
      quiet_checks=$((quiet_checks + 1))
    else
      quiet_checks=0
    fi
    printf '%s supervisor_phase=%s active=%s quiet_checks=%s\n' \
      "$(timestamp)" "$phase" "$active" "$quiet_checks"
    sleep "$QUIET_CHECK_INTERVAL_SECONDS"
  done
}

wait_for_activation_quiet() {
  local quiet_checks=0
  while (( quiet_checks < QUIET_CHECKS_REQUIRED )); do
    local active
    active="$(active_activation_crawls)"
    if [[ "$active" == "0" ]]; then
      quiet_checks=$((quiet_checks + 1))
    else
      quiet_checks=0
    fi
    printf '%s supervisor_phase=activation_crawl_drain active=%s quiet_checks=%s\n' \
      "$(timestamp)" "$active" "$quiet_checks"
    sleep "$QUIET_CHECK_INTERVAL_SECONDS"
  done
}

queue_transient_retry() {
  local retry_after="$1"
  local include_covered="$2"
  local include_covered_args=()
  if [[ "$include_covered" == "true" ]]; then
    include_covered_args+=(--include-covered)
  fi
  (
    cd "$WORKDIR"
    docker compose exec -T server feed-admin news-import \
      --retry-transient-after "$retry_after" \
      "${include_covered_args[@]}" \
      --limit "$CAMPAIGN_LIMIT" \
      --spacing-seconds "$CAMPAIGN_SPACING_SECONDS" \
      --lookback-days "$LOOKBACK_DAYS" \
      --max-articles "$MAX_ARTICLES"
  )
}

queue_remaining_no_feed_gaps() {
  (
    cd "$WORKDIR"
    docker compose exec -T server feed-admin news-import \
      --all \
      --limit "$CAMPAIGN_LIMIT" \
      --spacing-seconds "$CAMPAIGN_SPACING_SECONDS" \
      --lookback-days "$LOOKBACK_DAYS" \
      --max-articles "$MAX_ARTICLES"
  )
}

terminal_audit() {
  printf '%s supervisor_phase=terminal_audit\n' "$(timestamp)"
  (
    cd "$WORKDIR"
    docker compose exec -T server feed-admin news-ownership-audit \
      --apply \
      --fail-on-unscoped
  )
  psql "$DATABASE_URL" -X -P pager=off -c \
    "SELECT status,count(*)
     FROM jobs
     WHERE job_type='extract_company_news'
       AND created_at >= timestamptz '$CAMPAIGN_START'
     GROUP BY status
     ORDER BY status;"
  psql "$DATABASE_URL" -X -P pager=off -c \
    "SELECT status,count(*)
     FROM jobs
     WHERE job_type='crawl_source'
       AND created_at >= timestamptz '$CAMPAIGN_START'
       AND payload->>'trigger'='recipe_activation'
     GROUP BY status
     ORDER BY status;"
  psql "$DATABASE_URL" -X -P pager=off -c \
    "SELECT state.freshness_status,state.correctness_status,
            state.rebuild_required,count(*) AS recipes,
            count(DISTINCT recipe.company_id) AS companies
     FROM company_news_recipe_state AS state
     JOIN company_news_recipes AS recipe ON recipe.id=state.recipe_id
     WHERE recipe.status='active'
     GROUP BY state.freshness_status,state.correctness_status,
              state.rebuild_required
     ORDER BY 1,2,3;"
  psql "$DATABASE_URL" -X -P pager=off -c \
    "WITH healthy_feed AS (
       SELECT DISTINCT source.company_id
       FROM sources AS source
       JOIN source_state AS state ON state.source_id=source.id
       WHERE source.status='approved'
         AND source.kind IN ('rss','atom')
         AND state.last_success_at IS NOT NULL
         AND state.consecutive_failures=0
         AND state.consecutive_zero_runs < 3
     ),
     healthy_recipe AS (
       SELECT DISTINCT recipe.company_id
       FROM company_news_recipes AS recipe
       LEFT JOIN company_news_recipe_state AS state
         ON state.recipe_id=recipe.id
       WHERE recipe.status='active'
         AND COALESCE(state.rebuild_required,false)=false
         AND COALESCE(state.freshness_status,'unknown') <> 'content_stale'
     )
     SELECT
       count(*) AS eligible_companies,
       count(*) FILTER (WHERE feed.company_id IS NOT NULL)
         AS healthy_feed_companies,
       count(*) FILTER (WHERE recipe.company_id IS NOT NULL)
         AS healthy_recipe_companies,
       count(*) FILTER (
         WHERE feed.company_id IS NOT NULL OR recipe.company_id IS NOT NULL
       ) AS covered_companies,
       count(*) FILTER (
         WHERE feed.company_id IS NULL AND recipe.company_id IS NULL
       ) AS remaining_uncovered_companies
     FROM companies AS company
     LEFT JOIN healthy_feed AS feed ON feed.company_id=company.id
     LEFT JOIN healthy_recipe AS recipe ON recipe.company_id=company.id
     WHERE company.discovery_enabled
       AND company.lifecycle_status <> 'inactive';"
  psql "$DATABASE_URL" -X -P pager=off -c \
    "SELECT
       count(*) FILTER (WHERE NOT item.is_private) AS public_items,
       count(DISTINCT item.company_id) FILTER (WHERE NOT item.is_private)
         AS public_item_companies,
       count(*) FILTER (
         WHERE NOT item.is_private
           AND item.published_at >= now()-interval '31 days'
       ) AS recent_public_items,
       count(*) FILTER (
         WHERE item.content_processing #>> '{quality_quarantine,state}'
           = 'quarantined'
       ) AS quarantined_items
     FROM feed_items AS item;"
  curl --fail --silent --show-error --max-time 30 \
    http://127.0.0.1:18080/api/v1/company-news-recipe-coverage
  printf '\n'
  curl --fail --silent --show-error --max-time 30 \
    'http://127.0.0.1:18080/api/v1/news-items?limit=3'
  printf '\n'
  printf '%s recipe_campaign_terminal\n' "$(timestamp)"
}

printf '%s supervisor_started campaign_start=%s initial_retry_after=%s\n' \
  "$(timestamp)" "$CAMPAIGN_START" "$INITIAL_RETRY_AFTER"
wait_for_extraction_quiet "initial_gap_campaign"

printf '%s supervisor_phase=initial_transient_retry_queueing\n' "$(timestamp)"
queue_transient_retry "$INITIAL_RETRY_AFTER" true
wait_for_extraction_quiet "initial_transient_retry"

readonly FINAL_GAP_START="$(timestamp)"
printf '%s supervisor_phase=remaining_no_feed_gap_queueing\n' "$FINAL_GAP_START"
queue_remaining_no_feed_gaps
wait_for_extraction_quiet "remaining_no_feed_gap"

printf '%s supervisor_phase=final_transient_retry_queueing\n' "$(timestamp)"
queue_transient_retry "$FINAL_GAP_START" false
wait_for_extraction_quiet "final_transient_retry"

wait_for_activation_quiet
terminal_audit
