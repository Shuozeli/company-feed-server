<!-- agent-updated: 2026-08-12T20:40:00Z -->

# Offload raw_crawl_items.payload to MinIO/S3

## Status

In Progress (2026-08-12)

## Problem

`raw_crawl_items.payload` (jsonb, the raw crawled content) had grown to **~84 GB**
(15.26M normalized rows = 66 GB of payload + toast/index overhead), dominating a
92 GB Postgres volume and, on 2026-08-12, filling the disk and crashing Postgres
(`No space left on device` in pg_wal), which silently stopped the whole crawl
pipeline for ~2 hours.

## Evidence (why this is safe)

Two independent Explore passes over `/home/cyuan/projects/company-feed-server`:

- **payload is write-only.** It is INSERTed by `Database::complete_crawl_run`
  (`crates/feed-db/src/crawling.rs:531-561`) and **never SELECTed back**.
  Normalization is single-pass in-memory at crawl time; the row is written to
  `raw_crawl_items` AND to `feed_items` (which keeps its own `raw`/`normalized`
  jsonb + `title`/`body_html`/`body_text`/`body_markdown`/`summary` columns).
  No reprocessing/backfill/audit path re-reads `raw_crawl_items.payload`.
- So once a row is normalized, its payload is pure archive. Moving it out of
  Postgres has no functional readers to break.
- No S3 client existed in this repo; the monorepo's `image-host`/`s3_proxy`
  use `aws-sdk-s3` with `force_path_style(true)` (required for MinIO). Reused.

## Design

**Background offloader, not a hot-path change.** `complete_crawl_run` runs inside
a DB transaction; doing an external S3 upload there would couple crawl latency and
success to object-store availability. Instead a separate process sweeps rows and
moves payloads out. The crawler is untouched.

- New bin `bins/feed-payload-offloader` (self-contained: reads its own env,
  embeds a minimal `aws-sdk-s3` client, runs direct sqlx).
- Loop: `SELECT id, fetched_at, payload::text WHERE payload_s3_key IS NULL AND
  payload <> '{}' ORDER BY id LIMIT batch` → upload each to MinIO →
  `UPDATE payload='{}', payload_s3_key=$key WHERE id=$1 AND payload_s3_key IS NULL`.
  Uploads run `buffer_unordered(concurrency)`.
- **One tool, two jobs:** run to-dry for the one-time backfill of the 15.26M
  existing rows; run `OFFLOAD_CONTINUOUS=true` for steady-state offload of newly
  crawled rows. Both use the same idempotent, resumable query.
- **Best-effort:** if MinIO is down, rows keep their payload and are retried next
  pass; a fully-failed batch backs off. Crawl success never depends on MinIO.

### Object layout
- Bucket `company-feed-crawl-raw` (private, Tailnet-only — this is internal data,
  never served publicly). Key `raw/{YYYY}/{MM}/{DD}/{id}.json` from `fetched_at`.
- `payload_s3_key` (new nullable text column) records the key and doubles as the
  "migrated" marker. Partial index `raw_crawl_items_unmigrated_idx (id) WHERE
  payload_s3_key IS NULL` keeps the sweep query cheap as the backlog drains.

### Config (env, fail-fast)
`DATABASE_URL`, `RUSTFS_ENDPOINT`, `RUSTFS_ACCESS_KEY`, `RUSTFS_SECRET_KEY`,
`RUSTFS_REGION` (default us-east-1), `RAW_CRAWL_BUCKET` (default
company-feed-crawl-raw), `OFFLOAD_BATCH_SIZE` (500), `OFFLOAD_CONCURRENCY` (32),
`OFFLOAD_CONTINUOUS` (false), `OFFLOAD_IDLE_SLEEP_SECS` (30).

## Phases
- **P0 (done):** create bucket; add `payload_s3_key` column via pg-schema-diff;
  add partial index; build the offloader.
- **P1/P2 (unified):** run the offloader to-dry to backfill the 15.26M rows.
- **P3 (destructive, gated):** after verifying object count ≈ migrated row count,
  `VACUUM FULL raw_crawl_items` to return the freed space to the OS. Because
  payload is blanked first, the live data is tiny (~few GB), so the rewrite and
  its ACCESS EXCLUSIVE lock window are short.

## Notes
- Toolchain: the aws-sdk-s3 dep tree needs rustc ≥ 1.94. The workspace toolchain
  was bumped 1.88 → **1.95.0** (rust-toolchain.toml, Cargo.toml rust-version,
  Dockerfile `rust:1.95-bookworm`, ci.yml) so the offloader builds under the
  normal toolchain and can be deployed as a steady-state service. No
  `cargo +1.95.0` override is needed any more.
- The public `company-news-data` archive is intentionally NOT moved to MinIO:
  public serving must stay on GitHub (no self-hosted infra exposed). This design
  covers only the private raw crawl payloads.
