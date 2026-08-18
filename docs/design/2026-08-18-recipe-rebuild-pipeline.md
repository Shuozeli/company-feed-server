<!-- agent-updated: 2026-08-18T00:40:00Z -->

# Recipe Auto-Rebuild Pipeline + Earlier Stale Detection

## Status

Pillar A implemented + verified (2026-08-18): `feed-admin recipe-rebuild`
(list/--approve) built; dark-launched --approve --limit 2 enqueued 2
ExtractCompanyNews rebuilds (BlackRock, Cummins). Pillars B/C designed here,
deferred. Follow-up: scheduled detector/alert; rebuild the service image so the
new subcommand ships to the `docker compose exec` path.

## Problem

The news-crawl recipe lifecycle detects staleness but does not close the loop, and
detects it late:

1. **No auto-rebuild (dead end).** When a recipe crosses the failure thresholds it
   is flagged `rebuild_required=true` and moved to `status='stale'`, which removes
   it from the scheduler (`crates/feed-db/src/recipes.rs:1252-1260`, `:1404-1420`;
   scheduler excludes rebuild_required at `:306`). Nothing rebuilds it — an
   operator must manually run `feed-admin news-import --all`. Broken recipes sit
   idle until someone notices.
2. **Detection is reactive + late.** A recipe only flags after **3 consecutive**
   hard failures/empties (`recipes.rs:1252-1260`, default 3). The page-structure
   fingerprint is already computed each crawl (`last_structure_fingerprint`,
   `structure_changed` in run metadata, `recipes.rs:1381,1297`) **but is not used
   to trigger anything** — a recipe whose page structure drifted (selector about to
   break, or already extracting wrong items) is not caught until it fully fails 3×.
3. **No alerting.** Health is query-only (`get_company_news_recipe_coverage`,
   `recipes.rs:266-417`; GetDashboardStats). Nobody is notified when recipes go
   stale (violates project rule #29 data-freshness monitoring).

Current health (2026-08-18): 4,128 active recipes over 3,506 companies; **3,481
companies have a working recipe (99.3%)**, only **24 companies fully broken** and
**29 active recipes failing**. The ~2,622 other "failing" recipes are `stale`
/`superseded` retired versions (not in use) — not a problem.

## Design decisions (with the user, 2026-08-18)

- **Auto-rebuild = auto-detect + enqueue candidates, but human approval before
  execution.** No unattended rebuild storm / LLM-cost blowout.
- **Earlier detection = on structure drift, lower the failure threshold + alert
  early** (don't wait for 3 hard failures; don't blindly auto-rebuild on drift).
- **Deliver Pillar A first** (the approval-gated rebuild loop).

## Pillar A — Approval-gated auto-rebuild loop (implementing now)

Reuse the existing execution machinery — `list_company_ids_needing_news_recipes`
(candidate selection: companies lacking a healthy active recipe, i.e. missing OR
`rebuild_required`) and `queue_news_extraction_campaign_jobs` (spaced enqueue of
`ExtractCompanyNews` rebuild jobs). Add a review + approval verb around them:

- `feed-admin recipe-rebuild list [--limit N]` — **review** the rebuild candidate
  queue: company, reason (`no_active_recipe` vs `rebuild_required`),
  consecutive_failures, stale age, last_attempt. Read-only. New DB query
  `list_recipe_rebuild_candidates`.
- `feed-admin recipe-rebuild approve [--limit N] [--spacing-seconds S]` — the
  human **approval → execution**: enqueues rebuild jobs for the top-N candidates
  via the existing campaign path. Resumable + spaced; respects the same dedup
  (`jobs_one_active_key_idx`) so it never double-queues a company.

Loop: detector surfaces candidates → operator `list` (review) → `approve --limit`
(bounded, deliberate) → workers rebuild. The approval gate is the `approve` verb;
the operator chooses the batch size.

Follow-up within Pillar A (deferred): a scheduled detector in `feed-scheduler`
that periodically counts candidates and emits a `recipe_rebuild_pending` gauge +
WARN event so the operator knows to review (rather than polling by hand).

## Pillar B — Earlier stale detection via structure drift (designed, deferred)

The fingerprint signal already exists but is inert. Change
`complete_company_news_recipe_run` / `next_recipe_health_counts`
(`recipes.rs:1252-1297`) so that when `structure_changed=true` (fingerprint
differs from the recipe's baseline):
- lower the effective failure/empty threshold for that recipe from 3 to 1-2
  (a drifted page that then under-delivers is flagged after 1 weak run, not 3), and
- emit a `company_news.recipe_drift_suspected` event immediately (early warning),
  even if the current run still technically passed.

This catches "structure changed but still scraping something" before it becomes 3
hard failures. It does **not** auto-rebuild on drift alone (per the decision) —
drift lowers the bar + alerts; the approval-gated loop still executes.

## Pillar C — Alerting / freshness metrics (designed, deferred)

Per rule #29: expose `recipe_rebuild_pending`, `recipe_stale_total`,
`recipe_rebuild_success_ratio`, and `recipe_drift_suspected_total` gauges; add a
Prometheus alert when the rebuild backlog grows or the stale rate spikes; surface
in GetDashboardStats.

## Fetch-layer for WAF-blocked / JS sources (ResidentialBrowser) — status 2026-08-18

The 104 rebuild failures are two systemic fetch-layer classes, not per-company
bugs: WAF/bot-blocked IR-CMS hosts (Q4/West `ir.*`/`investor.*` `.aspx` that
403/429 datacenter IPs) and JS-rendered feeds. A plain HTTP GET can't read them;
a real Chrome over CDP (verified: dragbv2-browser `FetchPage` via the alienware
residential CDP + wait for the article-link selector returns Inovio's 17 links
where curl gets 403).

Implemented + shipped (CI green):
- `RecipeFetchProfile { Default, ResidentialBrowser }` on the recipe spec.
- 208 IR-CMS stale recipes bulk-marked `residential_browser`.
- **HtmlRecipeCrawler** fetches listing + article bodies via dragbv2-browser when
  `fetch_profile == ResidentialBrowser` (first gRPC client in the repo; env
  `RESIDENTIAL_BROWSER_ENDPOINT` + `RESIDENTIAL_BROWSER_CDP_URL`, wired on
  worker + news-extraction-worker).
- Builder sets `fetch_profile = ResidentialBrowser` for `ir.*`/`investor.*`/`.aspx`
  publication URLs.

KNOWN GAP (next session): end-to-end rebuild of a marked recipe still yields
accepted=0. The rebuild's article discovery/validation runs through a SECOND
article-crawl path — the standalone `HtmlArticleCrawler` built in
`crates/feed-jobs/src/lib.rs` (the news-extraction handler, `browser: None`) —
NOT the browser-enabled `HtmlRecipeCrawler` path. So candidate article pages on
the WAF host still 403 during validation ("all suggested article pages were
transiently unavailable"). To close it: give the news-extraction article crawler
the browser config and thread `use_browser` from the candidate recipe's
`fetch_profile` through `CompanyNewsExtractionJobHandler`'s validation crawl.
Also: classify by the ARTICLE host, not only the publication host (Inovio's
listing is `inovio.com` but its articles are on the blocked `ir.inovio.com`).

## Testing / rollout

- Pillar A: unit-test candidate selection + the approve enqueue (Fake DB /
  postgres-tests). Dark-launch on the 24 broken companies first (rule
  projectrulev3 #1), review the `list`, `approve --limit 5`, verify rebuilds run
  and correctness flips to passing, then widen.
- Pillar B: guard behind a config flag; validate on a few known-drifted pages.
