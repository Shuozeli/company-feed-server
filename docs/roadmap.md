# Roadmap

Current status: the RSS/Atom product, broad company registry, separated
discovery/validation/crawl runtimes, deterministic validation signals,
recall-first provisional activation, audited review, coverage dashboard, Git
export, and a separate manual company-news import are complete.
Browser rendering remains an optional adapter expansion.

## Phase 0: Design Seed

- Create repository.
- Document architecture and boundaries.
- Define database model.
- Define component boundaries.
- Define Git exporter contract.

## Phase 1: Postgres Foundation

- [x] Add Rust workspace.
- [x] Add Docker Compose Postgres.
- [x] Add `sqlx` migrations.
- [x] Implement validated config loading and synchronization.
- [x] Implement the initial DB access layer.
- [x] Add lease-fenced durable jobs, retries, and stale-worker recovery.
- [x] Add replayable raw crawl item storage and export-run state.

## Phase 2: Discovery MVP

- [x] Load `companies.yaml`.
- [x] Discover RSS/Atom links.
- [x] Probe common feed paths.
- [x] Store source candidates.
- [x] Add `feed-admin` approval commands.
- [x] Add periodic `discover_company` jobs.

## Phase 3: Crawling MVP

- [x] RSS/Atom crawler.
- [x] Durable `feed-admin crawl` trigger.
- [x] Track source state and crawl runs.
- [x] Add periodic `crawl_source` jobs.

## Phase 4: Content Processing

- [x] Initial owned HTML sanitizer.
- [x] Initial deterministic HTML-to-Markdown converter.
- [x] Plain-text extraction and initial content-processing metrics.
- Golden tests with representative company newsroom HTML snippets.
- Article-body extraction, size guards, and Markdown edge-case hardening.

## Phase 5: Normalization

- [x] URL canonicalization.
- [x] Dedup keys and content hashing.
- [x] Normalized feed item upsert using `feed-content` output.

## Phase 6: API

- [x] Add `feed-server` startup, migrations, config synchronization, and graceful shutdown.
- [x] Expose health and Postgres readiness.
- [x] Expose companies, candidates, sources, items, health, and run history.
- [x] Add bounded pagination and filters.
- [x] Keep `feed-server` API-only and expose operator review separately from
  background job execution.

## Phase 7: Scheduler / Worker

- [x] Add handler-filtered durable job claiming.
- [x] Add renewable lease heartbeats and token fencing.
- [x] Add exponential retry and stale-final-attempt recovery.
- [x] Add optional `feed-worker` runtime with health/readiness.
- [x] Implement SLO selection.
- [x] Implement backoff and zero-run health.
- Add per-domain limits.
- Add configurable in-process crawl concurrency.

## Phase 8: HTML and Browser Fallback

- [x] Add a bounded generic crawler for suggested individual public article
  pages.
- [x] Add a URL-only private-adapter contract for operator-selected companies.
- [x] Add an independent sequential import worker, run audit, and API.
- [x] Require an explicit company name/key and remove every automatic import
  producer, due selector, and cadence setting.
- [x] Preserve per-origin source ownership, disabled-source state, and opt-in
  public export.
- General static HTML listing crawler.
- Public `pwright` browser adapter.
- Public recipe format.
- Browser-required source health.

## Phase 9: Git Export

- [x] Add export targets.
- [x] Materialize readable Markdown, canonical JSON, and sharded JSONL.
- [x] Commit locally.
- [x] Optional explicit push to GitHub.
- [x] Export adaptive hash-trie indexes with integrity manifests.
- [x] Publish OpenAPI 3.1 and JSON Schema 2020-12 contracts.
- [x] Add periodic `export_target` jobs.

## Phase 10: Company Source Expansion

- [x] Add a provider-neutral external web-discovery adapter contract and
  independently validate returned URL seeds.
- [x] Import the broad company universe through an auditable staging contract.
- [x] Add explicit market-cap-ordered activation waves and global discovery
  queue backpressure.
- [x] Replace ticker-first identity with canonical company names, aliases,
  source-scoped external IDs, and optional zero-to-many listings.
- [x] Support broad public/private company imports with zero required listings.
- [x] Add bounded, resumable activation and recovery cohorts.
- [x] Reconcile support-domain, CMS oEmbed, and comment-feed false positives
  without deleting audit evidence.
- [x] Add an aggregate company-profile API for operator exploration.
- [x] Add recall-first multi-property AI-adapter discovery while keeping
  provider code outside the open-source repository.
- Resolve aliases across sources into canonical issuers.
- [x] Add durable candidate validation and decision audit.
- [x] Add bounded company-coverage validation waves.
- [x] Validate an initial 500-candidate wave and crawl activated sources.
- [x] Add a durable review-priority and source-health dashboard.

## Phase 11: Validation and Source Governance

- [x] Split API, discovery, validation, and crawl/export binaries.
- [x] Add `validate_candidate`, candidate-linked jobs, validation runs, and
  automatic/operator decisions.
- [x] Add deterministic ownership, editorial, freshness, locale, and risky
  scope policy.
- [x] Add explicit strict and trusted-adapter activation modes with provisional
  provenance.
- [x] Default automatic activation to no public-export permission.
- [x] Add single and audited batch validate/activate/reject APIs.
- [x] Make rejection disable an accepted source and hide its content.
- [x] Prefer one candidate per uncovered company in bulk waves.
- [x] Allow bounded product/brand expansion for already-covered companies.
- [x] Add review, company-coverage, and joined source-health UI.
- Continue bounded waves across the remaining unvalidated companies.
- Add cross-source canonical-URL deduplication for archive presentation while
  retaining source-local provenance.

## Phase 12: Open-Source Launch

- [x] Add an MIT license and explicit code/content licensing boundary.
- [x] Remove private deployment handoffs and machine-specific configuration
  from the release tree.
- [x] Add contribution, conduct, security, and responsible-use policies.
- [x] Add PostgreSQL integration CI, container smoke tests, dependency updates,
  and scheduled RustSec auditing.
- [x] Serve the crawled-news dashboard directly from the API.
- Publish `v0.1.0` after a clean-checkout release audit.
- Add synthetic screenshots and a minimal public demo dataset.
