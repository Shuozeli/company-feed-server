# Components

## `core`

Shared domain types, config loading, errors, URL helpers, and time utilities.

Primary types:

- `Company`
- `Source`
- `SourceCandidate`
- `RawCrawlItem`
- `NormalizedFeedItem`
- `CrawlBatch`
- `DiscoveryResult`
- `ExportTarget`

## `db`

Postgres access layer using `sqlx`.

Responsibilities:

- run migrations
- upsert companies and sources
- persist source candidates
- persist crawl runs and source state
- upsert normalized feed items
- track export state
- append structured event log entries

`sqlx` is preferred over a heavier ORM because this service needs operational queries, batch upserts, JSONB payloads, and async-first runtime behavior.

## `discovery`

Finds candidate company sources.

Input:

- company homepage
- optional newsroom URL
- optional investor relations URL
- optional blog URL
- optional hints

Output:

- candidate RSS/Atom sources
- candidate static HTML source pages
- browser-required source candidates
- confidence score and evidence

## `crawler`

Adapter-based crawling.

Adapters:

- `rss_atom`: parse RSS and Atom feeds
- `static_html`: fetch public HTML and extract article links/content
- `browser_pwright`: use `pwright` for public pages that require JavaScript rendering

Adapter order:

```text
RSS/Atom -> static HTML -> pwright browser
```

## `normalizer`

Converts raw crawl output into stable company news items.

Responsibilities:

- canonical URL
- stable external ID
- title cleanup
- date parsing
- body extraction normalization
- content hash
- company mapping
- metadata redaction for export

## `scheduler`

Long-running SLO scheduler.

Responsibilities:

- select due approved sources
- apply backoff
- enforce concurrency and per-domain limits
- run crawl jobs
- update source health

## `api`

Axum REST API.

Read APIs:

- companies
- sources
- source health
- feed items
- crawl runs
- export runs

Write APIs are limited to operator-safe actions and can be gated later.

## `exporter`

Exports normalized public company-news data into Git repositories.

Responsibilities:

- materialize Markdown and JSON files
- maintain indexes
- create commits
- optionally push to GitHub
- track exported item state for idempotency

