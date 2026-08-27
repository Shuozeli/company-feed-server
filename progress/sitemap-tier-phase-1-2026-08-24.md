<!-- agent-updated: 2026-08-24T00:00:00Z -->

# Sitemap Discovery Tier - Phase 1 (crawl tier + dark launch)

## Status

GREEN. The sitemap crawl tier is implemented, compiles workspace-wide (incl.
tests), unit + fixture tested, and dark-launched live on 3 real companies
(Apple / Amazon / Broadcom). Discovery auto-probe + rollout flag + healing the
584 stale recipes are Phase 2 (deferred, by design).

## Goal recap

Insert a sitemap tier into company-news discovery so the chain becomes
RSS/Atom (preferred) -> news-scoped sitemap -> HTML listing-page scrape (last
resort). The HTML tier is the fragile one (selector drift, WAF 403, 429 throttle
- the root of the recipe-health backlog). Newsroom sitemaps are a
rate-limit-friendly, structurally stable article-URL inventory that most sites
already publish (found via robots.txt), so preferring them over HTML scraping
should relieve a large slice of the fragility. This phase builds and proves the
crawl half of that chain on real data before wiring auto-discovery.

## Work completed

- **`SourceKind::Sitemap` enum variant** (`feed-core/src/domain.rs`).
  - What: added a 5th source kind + its `string_enum!` mapping + round-trip test.
  - Why: sitemap is a distinct crawl kind (not a feed, not an HTML recipe). Using
    the type system means every exhaustive `match source.kind` is compiler-forced
    to handle it - the "if it compiles, it works" guard. Blast radius was only 1
    real exhaustive match (feed-admin); everything else already used `matches!`
    or wildcard arms.

- **Sitemap parser module** (`feed-crawler/src/sitemap.rs`, `quick-xml` dep).
  - What: `parse_sitemap()` -> `SitemapDoc::{UrlSet, Index}`; `SitemapEntry` with
    `published_at()` = news_date -> lastmod -> date-from-URL-path; `news_score()`
    editorial classifier; `news_acceptance()` gate; `url_is_sitemap()`.
  - Why: the date-from-path derivation is the crux. The naive "accept only Google
    News sitemaps" rule was disproven live - Apple's newsroom sitemap carries
    NEITHER `<lastmod>` NOR `<news:publication_date>`; its only date signal is the
    `/YYYY/MM/` URL path. Rejecting it would have missed the cleanest case. The
    parser is pure + unit-tested so this logic is provable without network.

- **`SitemapCrawler`** (`feed-crawler/src/lib.rs`), parallel to `RssAtomCrawler`.
  - What: fetch -> parse -> (recurse news-scoped children one level) -> rank
    recent-first -> emit `RawCrawlItem`s (URL + derived date). Shares the SSRF /
    redirect / size-cap fetch loop, which was extracted into
    `fetch_public_document()` so both crawlers use identical, compiler-guaranteed
    fetch semantics (composition over copy-paste).
  - Why: emitting the same `CrawlBatch`/`RawCrawlItem` shape as the RSS path means
    the entire downstream normalize/persist pipeline is unchanged - the only new
    surface is fetch+parse.

- **Dispatch branch** (`feed-jobs/src/lib.rs` `CrawlJobHandler`).
  - What: `match source.kind { Sitemap => sitemap_crawler, _ => feed_crawler }`;
    the feed title-diversity gate is skipped for Sitemap kind.
  - Why: sitemap items legitimately have no title at crawl time (titles/bodies are
    hydrated downstream), so the "no usable titles" feed guard would wrongly fail
    every sitemap run. Skipping it for this kind is correct, not a workaround.

- **Schema** (`schema/postgres.sql` + applied to the compose DB).
  - What: added `'sitemap'` to the CHECK constraints on `sources.kind`,
    `feed_items.source_kind`, `source_candidates.candidate_kind`.
  - Why: without it, inserting or activating a sitemap source (or persisting a
    feed_item from one) fails the CHECK. `candidate_validation_runs.detected_kind`
    was left rss/atom-only - sitemap is not validated through the feed-validation
    path.

## End-to-end smoke results

`cargo run -p feed-crawler --example sitemap_probe -- <urls>` (real network, the
actual `SitemapCrawler`):

    ==== apple.com/newsroom/sitemap.xml ====
    entries_total 2565, dated_editorial 2504, emitted 200, dated 200/200 (url_path)
      2026-08 | url_path | .../newsroom/2026/08/apple-reports-third-quarter-results/

    ==== aboutamazon.com/sitemap.xml ====
    entries_total 4610, dated_editorial 4463, emitted 200, dated 200/200 (lastmod)

    ==== broadcom.com/sitemap.xml ====   (after nested-sitemap fix)
    child_sitemaps_fetched 1, emitted 10, dated 10/10 (lastmod)
      -> recursed into the news-scoped /blog child sitemap

Tesla: robots.txt exposes no sitemap on either www or ir - the expected case
that falls through to the HTML tier.

`cargo test -p feed-crawler sitemap`: 8 unit + 1 fixture (real Apple sitemap)
pass. `cargo check --workspace --tests`: green.

## Pitfalls encountered

- **Google-News-only acceptance was wrong.** Apple's sitemap has no news/lastmod
  tags; date lives in the URL path. Lesson: validate acceptance rules against a
  real target before coding them - the "strict" option I first proposed would
  have silently excluded the flagship case.
- **`<urlset>` of child sitemaps (Broadcom).** Broadcom serves nested sitemaps
  inside a `<urlset>` (non-standard; standard is `<sitemapindex>`), so the first
  cut emitted `.xml` child URLs as if they were articles. Fixed by splitting
  urlset entries on `url_is_sitemap()` and recursing news-scoped children one
  level. Lesson: real sitemaps abuse the schema; detect child sitemaps by shape
  (`.xml`), not just by the root element.
- **lastmod-ranking surfaces evergreen pages.** Amazon's top-by-lastmod includes
  re-touched `/impact/...` and `/` pages, not only new articles. Not fatal
  (still editorial-scoped + dated) but the `news_score >= 0` filter is permissive;
  tightening to prefer `/news/`-path entries is a Phase 2 refinement.
- **Backgrounded `docker exec` gets SIGKILLed** (carried lesson): long compiles
  were run foreground-blocking and polled by sentinel, never relaunched.

## Delayed / deferred (Phase 2)

- **Discovery auto-probe**: fetch robots.txt, parse `Sitemap:` directives, probe
  common paths, select the news-scoped sitemap, and emit a `SourceKind::Sitemap`
  candidate at confidence ~0.85 (between feed and HTML). Requires touching
  discovery + candidate validation + activation. (`feed-discovery`.)
- **`DISCOVERY_PROBE_SITEMAPS` flag**: not added yet, to avoid dead config - it is
  only consumed by the Phase 2 discovery probe.
- **Heal the 584**: once auto-probe lands, re-run discovery on the stale rebuild
  candidates so sitemap-capable companies move off the fragile HTML tier.
- **Scoping refinement**: prefer `/news/`-path entries; cap evergreen pages.

## Next-phase entry checklist

- `SourceKind::Sitemap` exists end to end (enum, string map, DB constraints).
- `SitemapCrawler` is constructed in the crawl registry and dispatched by kind;
  a manually-created approved Sitemap source will crawl through the full pipeline.
- Parser + crawler proven on real Apple/Amazon/Broadcom data.
- Phase 2 only needs to CREATE sitemap sources (discovery/activation); the crawl
  + persist path already accepts them.

## Glossary

- **Sitemap**: an XML file (`sitemap.xml`) a site publishes listing its page
  URLs, often linked from `robots.txt`. `<urlset>` lists pages; `<sitemapindex>`
  lists child sitemaps. Google-News sitemaps add `<news:publication_date>`.
- **lastmod**: the `<lastmod>` element - when the URL was last modified.
- **date-from-URL-path**: deriving a publish date from a `/YYYY/MM/` (or
  `/YYYY-MM-DD/`) segment in the article URL when no date tag exists.
- **news-scoped / news_score**: an editorial classifier - net count of
  news/press/blog tokens minus shop/retail/product tokens over host+path.
- **RawCrawlItem / CrawlBatch**: the crawler's output types (feed-core) consumed
  by the normalizer; sitemap items reuse them unchanged.
- **fetch_public_document**: the shared fetch loop (SSRF guard + redirect cap +
  byte cap) used by both the feed and sitemap crawlers.
- **SSRF guard**: refusing to fetch URLs that resolve to private/non-public IPs.
- **dark launch**: exercising new code on a few real inputs (AAPL/TSLA/AVGO/AMZN)
  before enabling it fleet-wide (projectrulev3 #1).
- **the 584**: the recipe-rebuild backlog (mostly HTML recipes that went stale in
  a 2026-07-21..24 event) this tier is meant to relieve.
