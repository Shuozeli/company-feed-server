<!-- agent-updated: 2026-08-18T00:55:00Z -->

# Runbook: Rebuilding stale news-crawl recipes

Operational guide for the approval-gated recipe rebuild loop. Design rationale:
`docs/design/2026-08-18-recipe-rebuild-pipeline.md`.

## Background: why recipes go stale

Each company HTML/blog source is scraped by a **recipe** (XPath/CSS extraction
rules built by `news-import`). After each crawl, `company_news_recipe_state` is
updated (`crates/feed-db/src/recipes.rs`). A recipe is marked
`rebuild_required=true` and its status flipped to `stale` (removed from the
scheduler) after **3 consecutive** failures / empty runs / correctness failures
(default). A `stale` recipe stays broken until it is rebuilt — this runbook.

Note: `status='superseded'` recipes are OLD versions replaced by a newer active
one — they do NOT need rebuild. Only `stale` / `rebuild_required` recipes with no
healthy active replacement are rebuild candidates.

## The loop: detect -> review -> approve -> rebuild

Auto-detection surfaces candidates; a human reviews and approves; workers rebuild.
There is no unattended auto-rebuild (deliberate, to bound LLM/crawl cost).

### 1. Review the rebuild candidate queue (read-only)

```bash
# host binary:
DATABASE_URL=postgres://company_feed:company_feed@127.0.0.1:55432/company_feed \
  ./target/debug/feed-admin recipe-rebuild --limit 50
# or via the container once the image ships the subcommand:
docker compose exec server feed-admin recipe-rebuild --limit 50
```

Prints one row per company needing rebuild (companies with a stale /
rebuild_required recipe and NO healthy active recipe), worst-first, with
`reason`, `consecutive_failures`, `stale_at`, `last_attempt_at`. Read-only.

### 2. Approve + enqueue rebuilds

```bash
feed-admin recipe-rebuild --approve --limit 50 --spacing-seconds 3
```

Enqueues bounded, spaced `ExtractCompanyNews` rebuild jobs for the top-N
candidates. `--spacing-seconds` staggers `run_after` so workers don't burst the
private extraction adapter. Start small (e.g. `--limit 20`), verify, then widen.
Dedup is enforced by `jobs_one_active_key_idx` (never double-queues a company).

### 3. What happens next

`ExtractCompanyNews` jobs run on **feed-news-extraction-worker**: fetch the
company page -> LLM recipe generation -> validation crawl against evidence ->
activate the new recipe (`status='active'`, `correctness_status='passing'`) and
supersede the stale one. Each rebuild is LLM + browser heavy (~1-3 min).

## Monitor a rebuild campaign

```bash
# jobs still in flight
psql ... -c "SELECT status,count(*) FROM jobs WHERE job_type='extract_company_news' GROUP BY 1;"
# how many candidates recovered to a healthy active recipe
feed-admin recipe-rebuild --limit 10000   # candidate_count should shrink
```

The campaign is done when `extract_company_news` pending/running reaches ~0 and
`recipe-rebuild --limit 10000` `candidate_count` has dropped. Some companies will
stay stale (page truly removed / bot-blocked / no article structure) — those are
genuine misses, not a pipeline failure.

## Publishing rebuilt data — MANUAL

Rebuilt recipes produce new `feed_items`, which change the exported archive.
**Publishing the archive to the public GitHub data repo
(`datayuacx26/company-news-data`) is a MANUAL step owned by the operator** — the
rebuild loop does NOT auto-publish. To publish after a rebuild campaign settles:
trigger an export (`feed-admin export --target company-news-data`), then run the
orphan+batched push to datayuacx26 (see the memory / prior publish recipe).

## Troubleshooting

- **Candidate keeps reappearing after rebuild**: the page structure genuinely
  broke or is bot-blocked; the rebuild can't find article structure. Inspect the
  source URL manually.
- **Jobs starved**: `extract_company_news` runs on its own worker lane, separate
  from `crawl_source`; if starved, check feed-news-extraction-worker health and
  concurrency (`docker compose logs news-extraction-worker`).
- **Too aggressive**: lower `--limit` / raise `--spacing-seconds`.
