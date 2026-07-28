# Components

## `feed-core`

Shared domain types and validated configuration:

- canonical companies, optional listings, candidates, sources, and items;
- discovery, validation, crawl, export, and health records;
- company-news build runs, versioned recipe specs, recipe health, and recipe runs;
- audited candidate decisions;
- durable `Job`, `ClaimedJob`, and `JobSpec` contracts;
- environment-backed runtime settings.

`feed-core` contains no network, database, provider, or business-process
implementation.

## `feed-universe`

Provider-neutral `company-universe.v2` document validation:

- schema and bound checks;
- canonical name, aliases, company key, classification, and optional listings;
- public URL validation;
- source-scoped external IDs;
- exact input SHA-256 for idempotent import audit.

It is independent of search providers and Postgres.

## `feed-db`

The `sqlx` Postgres access layer:

- migrations and seed synchronization;
- atomic company-universe import and activation;
- durable job enqueue, claim, heartbeat, retry, failure, and recovery;
- discovery candidate persistence;
- validation-run and decision audit;
- atomic activation, rejection, and accepted-source disable;
- raw crawl and normalized item transactions;
- source health and export state;
- per-origin company-news source creation, recipe version/state/run persistence,
  coverage summaries, and manual build audit;
- paginated API repositories and dashboard aggregates.

Operational SQL remains explicit because queue selection, advisory locks,
lease fencing, audit transactions, and JSONB evidence are central invariants.

## `feed-discovery`

Public source discovery:

Input:

- canonical company name and aliases;
- configured homepage, blog, newsroom, IR URL, and hints;
- optional public URL seeds returned by the neutral adapter.

Output:

- RSS, Atom, or HTML candidates;
- confidence;
- public fetch/classification evidence.

The component enforces public-network policy, response limits, feed parsing,
HTML alternate-link parsing, same-site editorial link rules, URL
canonicalization, and false-candidate filters.

It does not approve or crawl a production source.

## `feed-web-adapter`

Versioned provider-neutral HTTP types and client:

- `company-web-discovery.v2`;
- `company-news-extraction.v2` publication/evidence URL-only request/response types;
- names, aliases, broad classification, known public URLs, and requested roles;
- timeout, response-size, candidate-count, URL, schema, and role validation;
- safe bearer authentication and retry classification.

It deliberately excludes ticker-based search, provider names, prompts, raw
provider responses, SDKs, and approval logic.

## `feed-crawler`

Bounded RSS/Atom, public article-page, and HTML recipe fetching:

- public URL request;
- timeout and byte limits;
- parser-detected RSS/Atom kind;
- bounded raw items;
- final feed URL and feed metadata.
- checked redirects plus DNS and connected-address SSRF protection;
- generic article signals, title/date/canonical derivation, and substantive
  sanitized-body quality gates.
- bounded listing selectors, host/path policy, correctness yield/ratio gates,
  content freshness, and structure fingerprints.

The same crawler is used by candidate validation and approved-source crawling,
so technical validation matches the production ingestion path.

The recipe path fetches generic public listing pages but contains no
company-specific parser. It publishes nothing unless the resulting article
pages pass the recipe contract. Browser render mode remains an optional adapter
contract rather than a delivered executor.

## `feed-content`

Owned HTML sanitization and Markdown/text conversion:

- remove active content, unsafe schemes, handlers, forms, page chrome, styles,
  and unknown attributes;
- resolve safe relative URLs;
- emit deterministic HTML, Markdown, and plain text;
- report extraction-quality metadata.

## `feed-normalizer`

Raw-to-stable article conversion:

- canonical URL;
- stable external ID;
- title and date normalization;
- sanitized content outputs;
- versioned content hash;
- private-metadata removal.

## `feed-scheduler`

Lease-fenced job execution:

- handler-filtered claiming;
- renewable heartbeat and lease token;
- exponential retry;
- expired-job recovery;
- cancellation reconciliation for in-flight discovery, validation, and
  company-news import runs.

The runner handles one job at a time per process. Deploy additional processes
for concurrency; Postgres claims prevent duplicate ownership.

Recurring producers are component-specific:

- `DiscoveryJobProducer`;
- `ValidationJobProducer`;
- `CrawlExportJobProducer`.

Each producer can be disabled independently with `SCHEDULE_JOBS=false`.
There is deliberately no automatic company-news recipe-build producer.

## `feed-jobs`

Application job handlers and producer assembly:

- `DiscoveryJobHandler`;
- `CandidateValidationJobHandler`;
- `CrawlJobHandler`;
- `ExportJobHandler`;
- `CompanyNewsExtractionJobHandler`;
- separate registry builders for discovery, validation, manual news import,
  and crawl/export.

Candidate validation owns deterministic technical, ownership, editorial,
freshness, locale, and risky-scope signals plus the explicit `strict` and
`trusted_adapter` activation policies. Passing candidates activate with an
audited automatic decision and an initial crawl job. Provisional basis,
rejections, failures, and remaining review outcomes retain their evidence.

Company-news extraction additionally owns explicit one/all-company build
jobs, publication candidate merging and locale collapse, independent recipe
validation/calibration, activation, and initial crawl enqueue. `CrawlJobHandler`
executes active recipes, records freshness/correctness, and retires drifted
versions after their configured streak.

## `feed-api`

Axum read and operator API:

- company registry and aggregate company profiles;
- candidates and review joins;
- validation runs and decisions;
- sources, source health, and coverage aggregates;
- normalized items and execution histories;
- recipe coverage, versions, health, and run audit;
- single and batch validate/activate/reject actions;
- embedded `/review` dashboard.

The review UI includes candidate evidence, batch decision inputs, coverage
metrics, source health, strict/operator/provisional activation basis, and
wrong-source disable controls. Batch size is limited to 100 and
activation/rejection require actor plus reason.

Authentication is deployment-owned; these operator routes should not be
exposed directly to an untrusted network.

## Runtime Binaries

### `feed-server`

- applies migrations;
- synchronizes seed config;
- serves REST, readiness, and `/review`;
- claims no background job type.

### `feed-discovery-worker`

- registers only discovery handlers;
- optionally runs bounded discovery refill;
- exposes health/readiness on its own port.

### `feed-validation-worker`

- registers only candidate validation;
- optionally runs bounded validation refill;
- has no web-search or AI dependency;
- exposes health/readiness on its own port.

### `feed-worker`

- registers crawl and export handlers only;
- schedules approved-source and enabled-target work when configured;
- exposes health/readiness on its own port.

### `feed-news-extraction-worker`

- registers only `extract_company_news`;
- processes only explicitly queued companies, one job at a time;
- obtains publication/evidence URL-only suggestions from the neutral adapter;
- independently fetches and validates public article pages;
- activates only validated versioned recipes, groups evidence pages by actual
  origin, and preserves disabled-source state;
- exposes health/readiness on its own port.

### `feed-admin`

- universe import and company activation;
- bounded candidate validation, including already-covered company expansion;
- candidate list, accept, reject, and accepted-source disable;
- immediate crawl and export enqueue;
- named-company or explicit resumable all-company recipe build campaign.

Commands use durable DB contracts rather than alternate inline
implementations.

## `feed-exporter`

Deterministic Git archive materialization:

- readable Markdown plus canonical JSON article records;
- monthly adaptive hash-trie JSONL index shards;
- OpenAPI 3.1 and JSON Schema 2020-12 contracts;
- generation, partition, shard, count, and SHA-256 manifests;
- local commits;
- optional explicit push;
- per-target item hash/path state;
- exporter-owned staging boundaries.

An item is exportable only when its source is approved and
`public_export_allowed=true`.
