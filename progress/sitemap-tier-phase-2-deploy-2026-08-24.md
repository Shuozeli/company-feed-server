<!-- agent-updated: 2026-08-24T00:00:00Z -->

# Sitemap Discovery Tier - Phase 2 deploy + article hydration (full pipeline live)

## Status

GREEN. The full auto pipeline runs end to end on the deployed compose fleet and
produces real `source_kind='sitemap'` feed_items. Dark-launched on 4 tickers;
Apple went 0 -> 40 feed_items after the article-hydration fix.

## Goal recap

Deploy the Phase-2 code to the fleet and validate the whole chain on AAPL / TSLA /
AVGO / AMZN without touching the 584 backlog. The dark-launch surfaced that the
sitemap crawler emitted URL-only items that failed normalization ("no usable
title"); the operator directed: sitemap should DISCOVER article URLs, then reuse
the same website/article crawl to fetch each article's content (separate
discovery from crawling). This phase implements that and proves the result live.

## Work completed

- **Deployed the fleet** (`docker compose build` + `--force-recreate`) with
  `DISCOVERY_PROBE_SITEMAPS=true` (and `DISCOVERY_SCHEDULE_JOBS=false`, so only
  explicit `feed-admin discover` runs the probe - the 584 are untouched).
  - Pitfall: `docker compose up -d --build` rebuilt the image but did NOT recreate
    the worker containers (they showed 12-day uptime, empty new env). Had to
    `up -d --force-recreate` the workers explicitly. Lesson: verify each worker's
    uptime + a new env var after deploy; a shared-image tag change does not force
    recreation.

- **Two kind-gate fixes found via the live run** (`feed-db/validation.rs`):
  `enqueue_unvalidated_candidate_jobs` and `begin_candidate_validation_run` both
  filtered `candidate_kind IN ('rss','atom')`, so sitemap candidates were never
  enqueued for validation nor deemed validation-eligible. Added `'sitemap'`.
  - Lesson: a new source kind must be added at EVERY kind-filtered SQL across the
    lifecycle (discovery filter, validation producer, validation eligibility,
    scheduler, healthy-feed) - not just the dispatch match. The compiler can't
    catch SQL string filters; only a live run does.

- **Article hydration = the operator's directive** (`feed-crawler` `SitemapCrawler`).
  - What: `SitemapCrawler` now owns an `HtmlArticleCrawler`. `crawl()` does
    DISCOVERY (parse sitemap -> rank recent editorial URLs) then CRAWLING (fetch
    each URL's title + body via `crawl_recipe_links`, the SAME article crawl the
    HTML-recipe path uses), seeding the sitemap's date as a hint. Bounded to
    `max_articles` (40) fetched per crawl from a wider ranked pool.
  - Why: the normalizer requires a title; URL-only sitemap items failed with "feed
    entry has no usable title". Reusing the existing article crawler (rather than a
    new fetch path) means sitemap articles normalize exactly like recipe articles,
    and inherits the residential-browser option for WAF hosts for free.

## End-to-end smoke results (deployed fleet)

`feed-admin discover --company {apple,amazon...,broadcom...}` then the fleet ran
the full chain unattended:

    discovery  -> 8 sitemap candidates (apple/amazon/broadcom, news-scoped 0.85)
    validation -> 8 runs, 4 valid (detected_kind='sitemap')
    activation -> 4 approved sitemap sources
    crawl      -> apple 40 discovered / 40 fetched / 40 new
                  amazon 5 / 5 / 5
                  broadcom 0 new (articles already covered -> correct dedup)
    RESULT     -> 45 feed_items with source_kind='sitemap', real titles + dates
                  ("Apple kicks off Worldwide Developers Conference", ...)

`cargo check --workspace --tests`: green. Unit tests (core/crawler/discovery):
pass.

## Pitfalls encountered

- **Silent kind-filters in SQL** (above) - the two validation gates each cost a
  rebuild+redeploy cycle to find because they only manifest at runtime.
- **URL-only items fail normalization** - the "downstream content stage hydrates
  titles" assumption was wrong; normalization runs first and hard-requires a
  title. Fixed by hydrating during the crawl (recipe-parity).
- **Worker containers not recreated on image rebuild** (above).
- **Dedup hides success** - broadcom showed 0 new because its blog articles were
  already ingested via other sources; item_count/fetched prove the crawl worked
  even when new=0. Read fetched+discovered, not just new_item_count.

## Delayed / deferred

- **Heal the 584**: enable the probe for the stale cohort and re-run discovery.
  Still an operator step (bulk mutate); untouched here by design.
- **Commit**: code is uncommitted per the golden rule.
- **Scoping refinement** (prefer `/news/` paths; cap evergreen lastmod pages).
- The 4 test sitemap sources remain active and will re-crawl daily (now correctly
  producing feed_items); clean them up if the dark-launch cohort should not persist.

## Glossary

- **article hydration**: fetching an article page to obtain its title/body, vs.
  emitting a URL-only item. Sitemap now hydrates via `HtmlArticleCrawler`.
- **discovery vs crawling**: discovery = find the URLs to fetch (parse sitemap);
  crawling = fetch each URL's content. The operator asked to keep these separate
  and reuse the existing article crawl for the crawling half.
- **crawl_recipe_links**: the `HtmlArticleCrawler` method (shared with the HTML
  recipe path) that fetches a batch of article URLs into `RawCrawlItem`s with
  title/body.
- **kind-gate**: an SQL `kind IN ('rss','atom')` filter; each must include
  'sitemap' for sitemap sources to flow through that lifecycle stage.
- **new_item_count vs item_count**: item_count = URLs the crawl processed;
  new_item_count = net-new feed_items after dedup. 0-new with high item_count =
  articles already covered, not a failure.
