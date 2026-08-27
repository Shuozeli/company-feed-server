<!-- agent-updated: 2026-08-27T00:44:00Z -->

# Time-Partitioned, Append-Only Data Archive (v2) with a Versioned, Portable Shard Selector

## Status

In Progress (2026-08-26). Writer + reader implemented and validated on the live
141k-article dataset (dual-write under `v2/`, v1 still authoritative for the UI).

## Validation on real data (2026-08-26)

Measured on the production export (141,216 articles, 6,587 companies, ~190,832
v2 files) by running two back-to-back exports and diffing the `v2/` tree:

- **Steady-state diff: 23 files** (vs v1's ~74,828 files / ~1.76M line churn for
  the same re-export). Breakdown: `meta/timestamps.jsonl` (1 file, the volatile
  sidecar), 9 `data/` files (genuinely new/content-changed articles from the live
  crawler), 3 of 16 article index buckets touched (**no reshard** — the other 13
  are byte-identical), recent-day + company-month indexes, and 3 header files
  (`config.json`/`index.json`/`HEAD.json`, whose generation fingerprint changes
  every run).
- Old day partitions are never rewritten; the diff is proportional to genuinely
  new content, not archive size.

Two diff-minimality bugs were found and fixed during this validation:

1. **v2 tree never committed (owned-pathspec gap).** The exporter stages only the
   paths it owns via `git add --all -- <owned pathspecs>`; `v2/` was outside that
   set, so the whole tree stayed `?? v2/` and no v2 change ever committed. Fix:
   `archive_v2::OWNED_PATHS = ["v2"]`, chained into `existing_owned_pathspecs`.
   Regression guard: `materializes_and_commits_an_idempotent_archive` now asserts
   `v2/` is tracked and clean after export.
2. **`first_seen_at` flapped, churning ~50 data files/export.** One `doc_id` can
   have several `feed_item` rows (same URL, repeated crawls) with differing
   `created_at`; a stable `sort_by(doc_id)` kept whichever row the DB returned
   first, so `first_seen_at` (and the day partition, when `published_at` was null)
   drifted between runs. Fix: group by `doc_id`, set `first_seen_at =
   MIN(created_at)` and pick the content representative by a total order
   `(created_at, id)`. Regression guard:
   `duplicate_rows_pick_stable_first_seen_regardless_of_order`. This dropped the
   real-data data-file churn from 52 to 9 (the residual 9 are true new content).

## Problem

The current `company-news-data` export (archive_v1) rewrites a large fraction of
the ~154k-file / ~1GB tree on EVERY export, even when almost nothing changed:

- Volatile per-crawl timestamps (`fetched_at`, `last_updated_at`) are embedded in
  every article record, every browse-page summary, and every JSONL shard, so a
  no-content re-crawl dirties hundreds–thousands of files.
- `recent` and browse pages sort by `fetched_at`, so re-crawling one article
  reorders whole pages.
- `generated_at = max(updated_at)` changes every run.
- Partition shards re-split (re-shard) as data grows, cascading rewrites.

This huge, high-frequency churn (plus using raw.githubusercontent as a CDN) is
what got the `datayuacx26` account suspended (see memory
`datayuacx26-github-suspended-incident`). We need an archive whose incremental
update touches the MINIMUM number of files and NEVER re-shards existing data.

## Goals

1. **Immutable past.** Once a day's data is written, its files never change again.
   A re-export that adds nothing produces zero diff.
2. **Append-only incremental.** New articles only CREATE files (in their own day)
   and APPEND small entries to a fixed, bounded set of index shards. Old
   partitions are never rewritten.
3. **No re-sharding of existing data.** Shard membership is a pure function of the
   article, independent of dataset size, so adding article N+1 never moves
   article N.
4. **Volatile data isolated to one place.** Per-crawl timestamps live in a single
   sidecar, so re-crawl churn is one file, not thousands.
5. **Versioned, portable selector.** The article→shard function is versioned (so we
   can re-shard in the future by publishing a new scheme) and computes IDENTICALLY
   in Rust (exporter) and JavaScript (UI), so the frontend can locate any article
   client-side with no server round-trip.

## Layout (archive v2)

```
v2/
  config.json                      # scheme versions + bucket counts (see below)
  data/
    <YYYY>/<MM>/<DD>/<doc_id>.json  # ONE article = ONE file. Content only, immutable.
  index/
    articles/<scheme>/<bucket>.jsonl   # id -> {date, company_key} lookup, hash-sharded
    recent/<YYYY>/<MM>/<DD>.json        # per-day {articles:[summary rows]} (newest days = the feed)
    companies/<scheme>/<bucket>.jsonl   # company_key -> {article count, day list, ...}
    companies/<company_key>/<YYYY-MM>.json  # per-company, per-month {articles:[summary rows]} (immutable months)
  meta/
    timestamps.jsonl                 # THE only volatile file: doc_id -> {fetched_at, last_updated_at}
  taxonomy/
    directory.json                   # all categories + aggregate counts (+ taxonomy_generation)
    categories/<category_key>.json   # that category's companies {company_key, name, record_count, last_published_at}
  HEAD.json  index.json              # top-level pointers + generation (small)
```

### `data/<YYYY>/<MM>/<DD>/<doc_id>.json` — the article (immutable)

Partitioned by the article's **publish date** (`published_at`; fall back to
`first_seen_at` when null). Holds ONLY stable content:

```json
{
  "schema": "article/v2",
  "doc_id": "<sha256 hex>",
  "company_key": "apple",
  "company_name": "Apple",
  "source_kind": "sitemap",
  "url": "...", "canonical_url": "...",
  "title": "...", "summary": "...", "body_markdown": "...",
  "published_at": "2026-08-25T00:00:00Z",
  "first_seen_at": "2026-08-25T13:10:00Z",   // created_at: set once, stable
  "content_hash": "<sha256>"
}
```

No `fetched_at` / `last_updated_at` here — those are the only per-crawl-volatile
fields and they live in `meta/timestamps.jsonl`. Once this file is written for a
past day it is never rewritten unless the article's CONTENT actually changes
(rare) — a normal re-crawl leaves it byte-identical.

### `meta/timestamps.jsonl` — the single volatile file

One line per article: `{"doc_id":"...","fetched_at":"...","last_updated_at":"..."}`.
This is the ONE file that reflects re-crawl bookkeeping. A re-crawl of any article
(old or new) diffs only this file. (If it ever grows too large to rewrite cheaply,
it may itself be sharded by the same selector — see "Future re-shard"; for v2 it
starts as one file per the "diff is one file" requirement.)

### `index/articles/<scheme>/<bucket>.jsonl` — permalink lookup (hash-sharded)

The frontend, given a `doc_id` (permalink), must find which day the article lives
in WITHOUT scanning. It computes `bucket = selector(doc_id)` (below) and reads
`index/articles/v1/<bucket>.jsonl`, whose lines are
`{"doc_id":"...","date":"2026/08/25","company_key":"apple"}`. New articles APPEND
their line to exactly one of the N bucket files → only that bucket diffs.

### `index/recent/<YYYY>/<MM>/<DD>.json` — the recency feed

`{date, articles:[summary row]}` for that day. A **summary row** is
`{doc_id, company_key, company_name, source_kind, title, summary, published_at,
first_seen_at}` — enough to render a list card WITHOUT fetching the article, but
deliberately excluding `body_markdown` (heavy) and `fetched_at` (volatile → the
sidecar). The UI's "latest" feed = read the most recent day files, newest first.
Past days are immutable; only the current day's file grows, and a normal re-crawl
(only `fetched_at` moves) leaves every day file byte-identical.

### `index/companies/...` — browse-by-company

`index/companies/<company_key>/<YYYY-MM>.json` holds `{company_key, month,
articles:[summary row]}` for that month (immutable once the month passes) — the
same summary rows as the recency feed, so the company page renders from one fetch
per month with no per-article hop. A small hash-sharded
`companies/<scheme>/<bucket>.jsonl` maps `company_key -> {months present, total}`
so the UI can find a company's month files by computing `selector(company_key)`.

## The versioned, portable shard selector

The selector MUST produce the same bucket in Rust and JavaScript. Definition:

```
selector(key, scheme):
    h    = SHA256( scheme.salt + "\0" + key )     # bytes
    top8 = first 4 bytes of h as a big-endian u32
    return top8 % scheme.bucket_count
```

- `key` is the `doc_id` (for the article index) or `company_key` (for the company
  index) — a stable ASCII string.
- SHA256 is available in Rust (`sha2`) and in browsers (`crypto.subtle.digest`),
  so both sides agree bit-for-bit.
- `scheme` is read from `v2/config.json`:

```json
{
  "archive_version": "v2",
  "generation": "<content hash>",
  "schemes": {
    "articles": { "current": "v1", "v1": { "bucket_count": 16, "salt": "cnd/articles/v1" } },
    "companies": { "current": "v1", "v1": { "bucket_count": 16, "salt": "cnd/companies/v1" } }
  }
}
```

Reference implementations (kept in lock-step; a shared test vector file pins
agreement):

Rust (feed-exporter):
```rust
pub fn selector(key: &str, salt: &str, buckets: u32) -> u32 {
    let mut h = Sha256::new();
    h.update(salt.as_bytes()); h.update([0u8]); h.update(key.as_bytes());
    let d = h.finalize();
    let top = u32::from_be_bytes([d[0], d[1], d[2], d[3]]);
    top % buckets
}
```

JavaScript (company-news-ui):
```ts
export async function selector(key: string, salt: string, buckets: number) {
  const enc = new TextEncoder();
  const bytes = new Uint8Array([...enc.encode(salt), 0, ...enc.encode(key)]);
  const d = new Uint8Array(await crypto.subtle.digest('SHA-256', bytes));
  const top = ((d[0]<<24)>>>0) + (d[1]<<16) + (d[2]<<8) + d[3];
  return (top >>> 0) % buckets;
}
```

A committed `v2/selector-testvectors.json` (key → expected bucket for each scheme)
is asserted by BOTH a Rust test and a UI test so the two never drift.

## Incremental update — what diffs

On an export where K new articles arrived and M old articles were re-crawled with
no content change:

- **Created**: K new `data/<today>/<id>.json` files (+ their lines appended to K's
  buckets and today's recent/company files). Small, additive.
- **Diffed**: `meta/timestamps.jsonl` (the M re-crawls + K new), the ≤N article
  index buckets that got new ids, today's `recent` file, the touched company-month
  files, and `index.json`/`HEAD.json`/`config.json` (tiny).
- **Untouched**: every past day's `data/...`, every past `recent`/company-month
  file, every article file whose content didn't change. NO re-shard.

So a "nothing new, everything re-crawled" export diffs exactly ONE file
(`meta/timestamps.jsonl`), and a "20 new articles" export creates ~20 files +
appends to a bounded handful of indexes.

## Future re-shard (why the scheme is versioned)

If N buckets become too coarse, publish a new scheme (e.g. `articles.v2` with
`bucket_count: 256`) alongside the old one, backfill
`index/articles/v2/<bucket>.jsonl` once, then flip `schemes.articles.current` to
`v2`. Old `v1` bucket files can be deleted after the UI cutover. The UI reads
`current` from `config.json`, so it always uses the right selector; during
migration both can coexist. Crucially this re-shards only the INDEX (small), never
the immutable `data/` partitions.

## UI read contract

- **Latest feed**: list `index/recent/<Y>/<M>/<D>.json` for the newest days, pull
  their doc_ids, fetch the `data/.../<id>.json` files. (Newest day file is the only
  one that changes; browser can cache older days forever.)
- **Permalink (`/article/<doc_id>`)**: `b = selector(doc_id, articles.salt,
  articles.bucket_count)` → fetch `index/articles/<scheme>/<b>.jsonl` → find the
  line → fetch `data/<date>/<doc_id>.json`. No global scan.
- **Company page**: `b = selector(company_key, ...)` → company bucket → month list →
  fetch `index/companies/<company_key>/<Y-M>.json` → article ids → data files.
- **Timestamps** (optional display): fetch `meta/timestamps.jsonl` (or a future
  sharded form) only if the UI wants to show "fetched at"; otherwise ignore it. The
  `published_at ?? first_seen_at` fallback needs no volatile data.
- **Consistency**: `config.json`/`index.json` carry the content `generation`; the
  UI can still verify a load is from one snapshot, but per-day immutability means
  cross-file drift is far less likely than in v1.

## Migration

1. Land the v2 exporter path, writing `v2/` alongside the existing archive
   (dual-write), so nothing breaks. **DONE** — `archive_v2` + `OWNED_PATHS` wiring;
   validated on the live 141k dataset (see "Validation on real data").
2. Add the JS selector + v2 read path to `company-news-ui`; verify against the
   shared test vectors. **DONE** — `data/selector.ts`, `data/client_v2.ts`,
   `data/adapt_v2.ts`, `hooks/useV2.ts`; test vectors + client/adapter tests pass.
3. Wire the reader pages to the v2 client. **DONE — all reader pages (Home /
   Article permalink / Company / Explore / Category / Latest) read `v2/`** from the
   same base URL (v2 is a subdir of the same repo, so no base-URL cutover is needed
   to read it). The exporter emits `taxonomy/directory.json` +
   `taxonomy/categories/<key>.json` (13 GICS categories, per-company `record_count`
   + `last_published_at`), so `Explore`/`Category`/the home category grid moved to
   v2 (a category is one file, no pagination). **Decision (list-view strategy): the
   recent-day and company-month indexes now carry inline summary rows**
   (`{doc_id, company_key, company_name, source_kind, title, summary, published_at,
   first_seen_at}` — NO `body_markdown`, NO `fetched_at`), so `Latest` and `Company`
   render from one index fetch per day/month instead of N per-article data fetches,
   and the `CompanyPage` no longer resolves each article's day via the permalink
   bucket. Validated on real data: recent/company files contain `articles[]` summary
   rows and nothing leaks the body or volatile timestamps. Past day/month files stay
   immutable because none of the summary fields move on a normal re-crawl (the
   `deterministic_and_recrawl_diffs_only_timestamps` invariant still holds).
4. Backfill + host + incremental publish. **DONE.** The v2 tree is published to a
   dedicated, clean, **v2-only** repo (`yuacxshard1/company-news-data`, public) with
   its OWN small history — NOT the exporter's working repo (75G `.git`, ~5G frozen v1
   tree). Wiring:
   - The exporter's **`v2_mirror`** metadata block (in `configs/export_targets.yaml`)
     redirects the v2 write into the clean repo (`repo_path`), then commits and
     **incrementally** pushes to `remote_url` (branch `main`). The push PAT is
     injected from env `DATA_HOST_GITHUB_PAT` at push time (never persisted in a
     committed config); a push failure is logged and swallowed (local commit kept,
     next export fast-forwards it) so a host outage never fails the export.
   - Because we push to an explicit authed URL, the local `origin/<branch>` tracking
     ref is re-synced via `update-ref` after a successful push.
   - Baseline: one clean commit (192k files, 2G, `.git` 1.2G) pushed once. Measured
     steady state: an incremental export pushed **18 files**, and `raw.githubusercontent`
     served the new `generation`/`record_count` within seconds (anonymous 200).
   - The UI `DEFAULT_DATA_BASE_URL` (+ footer link, `.env.example`) points at
     `raw.githubusercontent.com/yuacxshard1/company-news-data/main/`.
5. Retire the v1 dual-write. **DONE (flag-gated, reversible).** The exporter writes
   v1 only when `export_target.metadata.dual_write_v1 == true` (absent → defaults to
   true, preserving old behavior for any un-migrated target); v2 is always written
   and is the source of the returned records. `company-news-data` sets the flag to
   `false` in `configs/export_targets.yaml` (+ `.local.yaml`), applied to the DB by
   `config_sync` at worker startup. The already-committed v1 tree simply **freezes**
   (not deleted — avoids a giant delete diff). Measured on real data: a v1-off export
   touched **0** v1-tree files, 66 v2 files, and ran in **224s vs 577s** with v1 on
   (~2.6x faster). `archive_v1` stays compiled + reachable via the flag, so nothing
   is dead code and dual-write can be resumed by flipping the seed config.

### Known follow-ups

- Deploy the UI: the base-URL change is committed to code but the live reader updates
  only on push to `main` (Cloudflare Pages auto-build). Not yet pushed.
- Delete the frozen v1 tree + `archive_v1` module entirely once nothing external reads
  v1 (a large one-time delete, deferred on purpose).
- The `cnd-v2-clean` mirror repo lives in the `export-data` volume; if that volume is
  recreated the exporter re-inits it and the next export re-pushes (fast-forward). A
  full re-clone/re-baseline is only needed if the remote and local diverge.

### Gotcha: `config_sync` owns `export_targets` — do NOT hand-edit the DB row

`export_target.metadata` is reconciled from `configs/export_targets.yaml` by
`config_sync` on every worker startup (`upsert_export_target`, `metadata =
EXCLUDED.metadata`). A direct `UPDATE export_targets SET metadata=...` is silently
overwritten on the next deploy/restart. Any per-target export setting (like
`dual_write_v1` or `v2_mirror`) MUST live in the seed YAML, not just the DB.

## Related

- `docs/publication-model.md` — incremental-push-only rule (this design makes the
  push tiny by construction).
- memory `datayuacx26-github-suspended-incident`, rule `no-shuozeli-data-repo`.
- `crates/feed-exporter/src/archive_v1.rs` — the current design being replaced.
