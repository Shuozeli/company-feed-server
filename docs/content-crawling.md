# Article Content Crawling

Company Feed Server separates article discovery from article-content
hydration.

`crawl_source` discovers stable article identities from RSS, Atom, or a
validated HTML recipe and stores the source observation. That observation may
contain only a title, link, or excerpt; recipe validation may already have
encountered a full page. Neither case substitutes for hydration.
`crawl_content` fetches every eligible public article URL independently and,
on success, replaces the item body with sanitized HTML, Markdown, and plain
text.

RSS, Atom, HTML, and browser-backed items all pass through the same durable
content state. Existing substantive text is never used as a reason to skip the
independent visit. If that visit fails, the earlier source observation remains
available while the crawl state records the retry or permanent failure.

## Runtime

`feed-content-worker` is the only runtime that claims `crawl_content`. Its
producer keeps at most `CONTENT_CRAWL_JOB_CONCURRENCY` logical batch jobs
active, while each batch applies bounded concurrency:

- `CONTENT_CRAWL_JOB_CONCURRENCY` bounds independent durable slots;
- `CONTENT_CRAWL_BATCH_SIZE` bounds durable selections;
- `CONTENT_CRAWL_MAX_CONCURRENCY` bounds global requests;
- `CONTENT_CRAWL_MAX_PER_HOST_CONCURRENCY` self-throttles each origin;
- request time, response bytes, and minimum extracted characters are bounded;
- private/reserved network targets and unsafe redirects are rejected.

Exact duplicate requested URLs in one batch are fetched once and fan out to
their feed-item records. Cross-company repeated content is not rejected at
this stage because legitimate syndicated releases may share a body.
Article connections are deliberately not retained in an idle pool: a
large-company campaign touches thousands of unrelated origins, so keeping one
idle socket per host would defeat bounded request concurrency. The Compose
service also raises its file-descriptor ceiling as operational headroom.

## Durable State

`content_crawl_attempts` records each requested URL, job, start/finish time,
outcome, retry classification, extraction metadata, content size, and hash.
`content_crawl_state` records the current status, attempt/failure counts,
freshness deadline, last error, and extraction version.

Success schedules a refresh after `CONTENT_CRAWL_REFRESH_SECONDS`. Retryable
failures use exponential backoff capped by the shared job retry maximum.
Non-retryable or exhausted failures become `permanent_failure`; the original
discovered item remains available instead of being deleted.

## Operations

Start the worker:

```bash
docker compose --profile content-crawl up --build -d content-worker
```

Inspect or manually seed the next due batch:

```bash
cargo run -p feed-admin -- content-crawl status
cargo run -p feed-admin -- content-crawl enqueue
```

Coverage is also available from
`GET /api/v1/content-crawl-coverage?min_content_chars=200`. Important counts
are eligible, never attempted, running, succeeded, failed, permanent failure,
any-body, and substantive-body items.

The worker is safe to stop. Lease loss or shutdown cancels running attempt
records and returns their items to retryable state.
