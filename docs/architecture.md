# Architecture

Company Feed Server is a multi-binary Rust workspace coordinated through one
Postgres database. API, discovery, candidate validation, and crawl/export work
have independent runtimes and durable job ownership.

The production boundary is public data only. Discovery may propose URLs;
validation may activate a technically and editorially safe feed; only approved
sources may be crawled; and only explicitly export-enabled sources may enter a
Git archive.

## Data Flow

```text
companies.yaml / company-universe.v2
  |
  +--> staged import and explicit activation waves
  |
  v
feed-discovery-worker
  |
  +--> configured public entry points
  +--> optional neutral web-adapter suggestions
  |
  v
public URL fetch, safety checks, classification
  |
  v
source_candidates (untrusted)
  |
  v
feed-validation-worker
  |
  +--> candidate_validation_runs
  +--> candidate_decisions
  |
  +--> strict policy pass ----------> approved source + initial crawl job
  |
  +--> trusted adapter + usable feed
  |                                  -> provisional source + initial crawl job
  |
  +--> empty / invalid -------------> automatic rejection
  |
  +--> remaining ambiguity ---------> operator review
                                         |
                                         +--> activate / reject / disable
  |
  v
feed-worker
  |
  +--> RSS/Atom fetch
  +--> active HTML recipe listing + independent article fetch
  +--> raw_crawl_items
  +--> feed-content + normalizer
  +--> feed_items + source_state
  +--> article identities and initial normalized feed payloads
  |
  v
feed-content-worker
  |
  +--> independent public article-page fetch
  +--> sanitized HTML, Markdown, and plain text
  +--> durable retry, extraction-version, and freshness state
  +--> optional Git archive
  |
  v
feed-server REST API and /review dashboard

explicit named/all-company build campaign
  |
  v
feed-news-extraction-worker
  |
  +--> neutral adapter: public publication + evidence URLs only
  +--> independent URL safety + public article fetch
  +--> durable publication-seed handoff to feed-discovery-worker
  +--> generic listing/article correctness and freshness gate
  +--> versioned active recipe + initial scheduled crawl
  +--> optional per-origin evidence crawl/raw/item transactions
  |
  v
feed-server REST API and extraction-run audit
```

## Workspace Shape

```text
company-feed-server/
  Cargo.toml
  Dockerfile
  docker-compose.yml
  configs/
  schema/
  crates/
    feed-core/
    feed-universe/
    feed-db/
    feed-api/
    feed-scheduler/
    feed-content/
    feed-discovery/
    feed-web-adapter/
    feed-crawler/
    feed-normalizer/
    feed-exporter/
    feed-jobs/
  bins/
    feed-server/
    feed-discovery-worker/
    feed-validation-worker/
    feed-news-extraction-worker/
    feed-content-worker/
    feed-worker/
    feed-admin/
```

## Runtime Model

Each long-running binary advertises and claims only its supported job types:

| Runtime | Job ownership | HTTP surface |
|---|---|---|
| `feed-server` | none | public REST API and `/review` |
| `feed-discovery-worker` | `discover_company` | health/readiness |
| `feed-validation-worker` | `validate_candidate` | health/readiness |
| `feed-worker` | `crawl_source`, `export_target` | health/readiness |
| `feed-news-extraction-worker` | `extract_company_news` | health/readiness |
| `feed-content-worker` | `crawl_content` | health/readiness |

`feed-admin` inserts the same durable jobs and invokes the same transactional
decision contracts. It has no private discovery, crawler, or exporter
implementation.

`RUN_JOBS` controls whether a worker claims jobs. For discovery, validation,
and crawl/export workers, `SCHEDULE_JOBS` independently controls recurring or
refill work. The company-news build worker has no automatic producer and only
processes explicit operator jobs, including a materialized resumable `--all`
campaign.

Discovery does not run inside the API process. A slow provider or public web
request therefore cannot consume API worker capacity or expand the API
container's permissions.

## Durable Coordination

Postgres stores canonical state and execution history:

- companies, optional listings, external IDs, import runs, and import rows;
- source candidates, candidate validation runs, and candidate decisions;
- approved or disabled sources and source health;
- durable jobs plus discovery, company-news build, recipe crawl, and export runs;
- versioned company-news recipes and freshness/correctness state;
- raw crawl items and normalized feed items;
- export targets, exported-item state, and operational events.

Workers claim jobs with `FOR UPDATE SKIP LOCKED`. Claims have renewable,
token-fenced leases. Heartbeat, retry, completion, and failure updates must
match the active token, which prevents an expired worker from committing after
a replacement has reclaimed its job. Crawl attempts are fenced at the run row
as well: interruption closes both crawl and recipe-run audit records, and a
replacement claim cancels any abandoned attempt before starting a new one.

Logical work is deduplicated by a partial unique index on active
`(job_type, job_key)`. Completed and failed history can coexist with a later
attempt.

## Bounded Scheduling

Broad-imported companies are staged until an explicit activation assigns a
release timestamp. Discovery queue refills are serialized by a Postgres
advisory lock and capped by `DISCOVERY_QUEUE_TARGET`.

Validation queue refills use a separate advisory lock and
`VALIDATION_QUEUE_TARGET`. Bulk selection chooses at most one unvalidated
candidate per uncovered company by default. `--include-covered` retains the
one-per-company bound while expanding product and brand publications for
companies that already have a source. This avoids a single company's locale or
format variants consuming a wave.
One narrow replacement exception applies to RSS/Atom candidates produced by a
recipe-seeded discovery run: they remain eligible while the company has only
approved HTML/browser coverage, then stop being selected as soon as one
healthy approved RSS/Atom source exists. An approved feed whose runtime crawl
is failing or repeatedly empty therefore cannot suppress a recipe-seeded
replacement candidate. This preserves feed preference without turning the
normal validation refill into an all-source sweep.

Crawl scheduling is driven by approved RSS/Atom sources or approved HTML/
browser sources with a healthy active recipe, source freshness SLO, and backoff
state. Disabling a source or staling its recipe removes it from scheduling,
normal API item queries, and export selection.

Company-news recipe construction is never produced by a timer.
`feed-admin news-import` accepts one company name/key or an explicit bounded
`--all` campaign. Its dedicated worker defaults to one company job; a
transactional advisory claim lock enforces the configured
`NEWS_EXTRACTION_JOB_CONCURRENCY` limit across all worker instances. A
bounded campaign can overlap public-page validation, while the private adapter
owns provider concurrency and throttling. The
all-company selector is resumable and skips companies
with a healthy approved feed or healthy active recipe; a zero-result build
completes without creating an automatic retry loop.

When the adapter supplies a plausible publication, the recipe worker can emit a
separate, durable `discover_company` job containing only those public seed
URLs. This happens for companies without a healthy approved feed and for
explicit `--include-covered` expansion builds. Stable editorial roots inferred
from evidence articles join the adapter's listing suggestions, so a stale
landing page does not hide a current publication-specific feed. The discovery worker
consumes those seeds without another adapter request, probes standard RSS/Atom
paths (including Q4 `/rss/pressrelease.aspx`, common investor-relations
`/rss/news-releases.xml` and `/news-events/press-releases/rss`, and
conventional WordPress/news paths such as `/feed.xml`, `/news/feed/`, and
`/blog/rss.xml`), HTML alternate links, and anchors whose text or path contains
an exact `RSS` or `Atom` token. This covers extensionless and `.aspx` subscription
endpoints without confusing words such as `press` with `rss`. Every result
remains in the normal candidate-validation path. Recipe construction does not
parse or approve the feed, and discovery does not construct the recipe. Covered-company expansion
candidates enter validation only through an explicit `--include-covered`
validation wave.

Retryable failures from the shared neutral web adapter pause the affected job
lane for at least 30 seconds, or for a longer adapter-provided retry interval,
before that lane can claim another company. Per-job retry scheduling remains
independent. The worker cooldown bounds cross-company attempt consumption
during an adapter outage without adding an automatic producer.

## Company Identity

Company identity is organization-first:

- canonical `name`;
- optional `aliases`;
- stable operational `company_key`;
- optional zero-to-many `company_listings`.

Names and aliases drive discovery and conservative hostname validation.
Listings never enter the web-adapter request and private companies require no
listing.

## Open-Source / Private Boundary

The open-source repository owns:

- `company-universe.v2`;
- `company-web-discovery.v2`;
- `company-news-extraction.v2`;
- `company-news-recipe.v1`;
- public URL safety and fetch logic;
- source discovery and evidence persistence;
- deterministic validation signals and configurable activation policy;
- crawling, normalization, API, health, and Git export.

An external private adapter may own provider credentials, Google Search or AI
Mode integration, prompts, provider throttling, and private raw logs. Only the
neutral request fields and public URL suggestions cross the HTTP boundary.
For company-news bootstrap, suggestions are public publication/listing and
evidence article URLs. All selectors, link filtering, titles, dates, canonical
URLs, bodies, correctness decisions, recipe versions, and persistence come
from the open-source worker's public fetch.

The open-source discovery worker always re-fetches and classifies provider
suggestions. Under the default `strict` policy, adapter provenance is
informational and the validation worker requires independent ownership,
editorial, freshness, locale, scope, and content evidence. Under the opt-in
`trusted_adapter` policy, neutral adapter provenance plus independently verified
usable feed content produces a provisional source. Both paths are audited and
remain reversible.

## Security Boundary

Allowed:

- public RSS and Atom;
- public HTML pages used for discovery;
- public company homepage, newsroom, blog, engineering, press, and IR pages;
- normalized public article metadata and text;
- explicitly approved public Git exports.

Disallowed:

- logged-in crawling or private browser state;
- paywall bypass;
- private source lists or database snapshots;
- provider prompts, queries, SDKs, credentials, and raw responses in this
  repository;
- automatic public export merely because a feed validates.

The operator write API is not an authentication layer. Deploy `/review` and
candidate mutation routes behind a trusted network or authenticated gateway.
