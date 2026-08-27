<!-- agent-updated: 2026-08-24T00:00:00Z -->

# Sitemap Discovery Tier - Phase 2 (auto-discovery + validation + activation)

## Status

GREEN (code). The full auto-discovery loop is implemented, compiles workspace-wide
with tests, unit-tested, and the discovery half is dark-launched live on 4 real
companies. Remaining is operational: deploy the new binaries to the fleet, enable
`DISCOVERY_PROBE_SITEMAPS`, and re-run discovery on the stale companies to heal
them (needs a fleet deploy, surfaced to the operator, not auto-executed).

## Goal recap

Phase 1 built the sitemap crawl tier but a sitemap source had to be created by
hand. Phase 2 closes the loop: discovery auto-finds a company's news sitemap,
validates it, and activates it as a `SourceKind::Sitemap` source that the
scheduler crawls - so the RSS/Atom -> sitemap -> HTML chain runs end to end with
no manual step, and sitemap-capable companies stop depending on the fragile HTML
recipe tier.

## Work completed

- **URL classifiers moved to feed-core** (`url_policy.rs`: `news_score`,
  `is_news_scoped_url`, `url_is_sitemap`).
  - Why: both the crawler (feed-crawler) and discovery (feed-discovery) need the
    editorial classifier. feed-core is the shared base both already depend on, so
    moving them there is the correct layering (vs. feed-discovery taking a new
    dependency on feed-crawler). feed-crawler's sitemap module now re-exports them.

- **`DISCOVERY_PROBE_SITEMAPS` flag** (`config.rs` -> `DiscoveryConfig.probe_sitemaps`,
  default false).
  - Why: gate the new probe so landing the code changes nothing until the operator
    opts in per rollout. Not dead config - it is consumed by the probe below.

- **Discovery sitemap probe** (`feed-discovery`: `augment_with_sitemaps` +
  `parse_robots_sitemaps` + `body_looks_like_sitemap`).
  - What: after the RSS/Atom pass, fetch each origin's robots.txt, parse `Sitemap:`
    directives, also probe common sitemap paths, cheaply sniff that the target is
    XML sitemap-shaped, and emit `SourceKind::Sitemap` candidates - news-scoped at
    0.85, generic at 0.70 (between feed 0.96-0.99 and HTML 0.65-0.75). The
    accumulator's sitemap filter is bypassed only for intentional Sitemap-kind
    candidates.
  - Why deliberately cheap (no XML parse in discovery): discovery proposes, the
    validator disposes. A generic `/sitemap.xml` (Amazon/Broadcom - `news_score`
    0) is still emitted as a 0.70 candidate so the validator's content check can
    accept it; relying on URL tokens alone would have missed those.

- **Validation sitemap branch** (`CandidateValidationJobHandler.validate_sitemap_candidate`).
  - What: an early branch - if the candidate is Sitemap, crawl it with the
    `SitemapCrawler`, accept only when it yields >= 3 dated editorial articles,
    then activate (`accept_source_candidate_with_decision`, kind-agnostic) +
    enqueue a crawl, else reject; a fetch error records a failed run and stays
    retryable.
  - Why a separate branch: the RSS/Atom policy engine is built to REJECT sitemaps
    (`sitemap_is_not_editorial_feed`) and assumes feed titles. Routing sitemaps
    through it would fail them. The branch reuses the same validation-run +
    decision DB machinery, just with a sitemap-appropriate gate.

- **Scheduler + feed-presence queries** (`feed-db`).
  - What: `enqueue_due_crawl_jobs` now enqueues `kind IN ('rss','atom','sitemap')`
    (sitemap crawls without needing a recipe); `company_has_approved_feed` and
    `company_has_healthy_approved_feed` count sitemap sources.
  - Why: without the scheduler change an activated sitemap source would never be
    crawled. Counting sitemap as a "healthy feed" makes it suppress HTML-recipe
    building - i.e. sitemap is genuinely preferred over the HTML tier.

- **Schema**: `candidate_validation_runs.detected_kind` CHECK now allows
  `'sitemap'` (postgres.sql + applied to the compose DB), so a sitemap validation
  run records its kind.

## End-to-end smoke results

Discovery dark-launch, `cargo run -p feed-discovery --example discover_sitemaps`
(real network, `probe_sitemaps=true`):

    Apple    -> newsroom/sitemap.xml conf 0.85 (+ 2 generic 0.70)
    Amazon   -> news-sitemap.xml     conf 0.85 (+ generic 0.70)
    Broadcom -> blog/sitemap.xml     conf 0.85 (+ generic 0.70)
    Tesla    -> entry points WAF-blocked -> no candidate (HTML fallback, expected)

The news-scoped sitemap is ranked above the generic one in every case, so the
validator tries the best one first.

`cargo test -p feed-core -p feed-crawler -p feed-discovery`: 17 + 127 + 13 pass
(incl. new `parses_robots_sitemap_directives`, `sniffs_sitemap_bodies`).
`cargo check --workspace --tests`: green.

## Pitfalls encountered

- **URL `news_score` alone can't select generic sitemaps.** Amazon/Broadcom serve
  the news content under a plain `/sitemap.xml` (score 0). Emitting only
  news-token URLs would miss them. Lesson: discovery emits both news-scoped (0.85)
  and generic (0.70) candidates and lets the validator's content check decide.
- **The RSS policy engine actively rejects sitemaps.** Reusing `handle()`'s policy
  path would have auto-rejected every sitemap. A dedicated early branch was
  required, not a tweak. Lesson: when an existing gate encodes "X is invalid",
  a new kind that redefines X needs its own path.
- **Scheduler kind filter is a silent gate.** An activated sitemap source compiled
  and validated fine but would never crawl, because `enqueue_due_crawl_jobs`
  whitelists kinds. Lesson: trace the whole lifecycle (activate -> schedule ->
  crawl); a new source kind must be added at every kind-filtered SQL, not just the
  dispatch match.

## Delayed / deferred

- **Fleet deploy + enable + heal the 584**: rebuild the service image, set
  `DISCOVERY_PROBE_SITEMAPS=true`, and re-run discovery for the ~1,100
  no-active-recipe companies (incl. the 584 stale). This mutates the running
  fleet, so it is surfaced to the operator rather than auto-run.
- **Scoping refinement**: prefer `/news/`-path entries and cap evergreen pages
  (Amazon lastmod-ranking nuance from Phase 1).
- **Per-company probe control**: the flag is global; a per-cohort allow-list would
  let the heal roll out gradually instead of fleet-wide at once.

## Next-phase entry checklist

- Discovery emits ranked Sitemap candidates when `probe_sitemaps` is on (proven).
- Validation accepts/rejects+activates sitemap candidates; scheduler crawls them.
- To heal a company: run a discovery job for it with the flag on; the loop does the
  rest. No further code needed.

## Glossary

- **augment_with_sitemaps**: the discovery method that probes robots.txt + common
  sitemap paths and emits Sitemap candidates.
- **candidate -> validation -> activation**: the discovery pipeline. Discovery
  writes a `source_candidates` row; the validation worker crawls it and, if it
  passes, calls `accept_source_candidate_with_decision` to create an approved
  `sources` row and enqueue a crawl.
- **news_score**: net editorial token score over a URL's host+path; > 0 =
  news-scoped (ranked at confidence 0.85 vs 0.70 generic).
- **healthy approved feed**: a company source (now incl. sitemap) that has
  succeeded recently; its presence suppresses HTML-recipe building, making sitemap
  preferred over HTML.
- **enqueue_due_crawl_jobs**: the scheduler query that picks approved sources due
  for a crawl; kind-filtered, so sitemap had to be added.
- **the 584**: the recipe-rebuild backlog (mostly stale HTML recipes) this tier
  heals by moving companies onto their sitemap.
- **dark launch**: exercising new code on a few real inputs before fleet-wide
  enable (projectrulev3 #1).
