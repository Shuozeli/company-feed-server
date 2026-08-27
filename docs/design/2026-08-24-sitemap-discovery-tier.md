<!-- agent-updated: 2026-08-24T00:00:00Z -->

# Sitemap Discovery Tier (RSS/Atom -> Sitemap -> HTML fallback chain)

## Status

Completed + deployed (2026-08-24). Phase 1 (crawl tier) + Phase 2 (discovery
auto-probe + validation + activation + scheduler + `DISCOVERY_PROBE_SITEMAPS`
flag + article hydration) implemented, tested, deployed to the compose fleet, and
verified end to end: Apple/Amazon sitemaps produced 45 `source_kind='sitemap'`
feed_items with real titles+dates. The sitemap crawl DISCOVERS article URLs then
reuses the shared `HtmlArticleCrawler` to fetch each article's content.
Remaining (operator step): enable the flag for the ~1,100 no-active-recipe
companies (incl. the 584 stale) and re-run discovery to heal them off the HTML
tier. Code uncommitted per the golden rule.

## Problem

Company-news discovery today is a two-tier chain:

1. **RSS/Atom feed** if the company has a healthy approved feed
   (`CompanyNewsExtractionHandler` skips recipe build when
   `company_has_healthy_approved_feed()` is true, `feed-jobs/src/lib.rs:2506`).
2. **HTML recipe** otherwise: fetch a listing page and extract article links via a
   CSS `article_link_selector` (`HtmlRecipeCrawler`).

The HTML tier is the fragile one: listing-page selectors drift, IR-CMS hosts are
WAF/bot-blocked (403/429), and shared sources like `stocktitan.net` rate-limit.
This is the root of the recipe-health backlog (2026-08-24: ~584 rebuild
candidates, ~70% went stale in a single 2026-07-21..24 event; plus daily 429
throttle failures dominated by one shared host).

Sitemaps are currently **rejected on purpose** at every layer
(`is_sitemap_url()` in `feed-core/src/url_policy.rs:33-107`, filtered in
`feed-discovery/src/lib.rs:755`, rejected in `feed-jobs/src/lib.rs:1264` with
`sitemap_is_not_editorial_feed`). The rejection exists to avoid ingesting a
site's non-editorial URL inventory (shop/retail/category pages) as if it were
news.

But most newsrooms publish a **news-scoped sitemap** that is a near-perfect,
rate-limit-friendly, structurally-stable article-URL inventory. Verified live
2026-08-24 on Apple:

- `https://www.apple.com/robots.txt` lists 5 sitemaps; one is
  `https://www.apple.com/newsroom/sitemap.xml` (HTTP 200).
- That sitemap is a flat `<urlset>` with **2,536 dated article URLs**
  (`/newsroom/2026/08/apple-reports-third-quarter-results/`).
- It is a **plain** sitemap: no `<lastmod>` and no `<news:publication_date>`.
  The **publish date is embedded in the URL path** (`/YYYY/MM/`).

This last point kills the naive "accept only Google-News sitemaps" rule: it would
reject Apple's clearly-editorial sitemap. The acceptance rule must derive
freshness from `<lastmod>` OR `<news:publication_date>` OR a date in the URL path.

## Decision (with the user, 2026-08-24)

Insert a **sitemap tier** between the feed tier and the HTML tier:

    RSS/Atom (preferred)  ->  news-scoped sitemap  ->  HTML listing-page scrape (last resort)

### Sitemap acceptance rule (the "refined rule" the user approved)

A sitemap is accepted as a company-news source only when ALL hold:

1. **Source of the URL**: found via the site's `robots.txt` `Sitemap:` directive,
   OR via a common-path probe (`/sitemap.xml`, `/news-sitemap.xml`,
   `/sitemap_index.xml`, `/newsroom/sitemap.xml`, ...).
2. **News-scoped selection**: when a site lists several sitemaps, pick only the
   news-relevant one -- its URL or the URLs it contains match editorial tokens
   (`news`, `newsroom`, `press`, `press-release`, `media`, `blog`), and/or its
   host+path prefix matches the company's known publication URL. Shop / retail /
   product / store / today / support sitemaps are excluded.
3. **Freshness derivable**: at least a plurality of entries expose a date via
   `<lastmod>`, `<news:publication_date>`, or a `/YYYY/MM/` (or `/YYYY-MM-DD`)
   segment in the URL path. Entries with no derivable date fall back to fetching
   the article page for its date (same as the HTML path does today).

This keeps the anti-"non-editorial URL inventory" guard the original rejection
was protecting, while unlocking the common case (Apple-style newsroom sitemaps).

## Architecture

Mirror the existing `RssAtomCrawler` vs `HtmlRecipeCrawler` split with a third,
parallel crawler.

### 1. `SourceKind::Sitemap` (new enum variant)

`feed-core/src/domain.rs`: add `Sitemap => "sitemap"`. The `string_enum!` macro
gives `as_str`/`Display`/`FromStr` for free. Every exhaustive `match source.kind`
(53 sites / 15 files) is updated compiler-driven; most treat Sitemap like a
non-HTML feed source (it does NOT use a `CompanyNewsRecipeSpec`).

### 2. Sitemap parser module (`feed-crawler/src/sitemap.rs`)

Pure, unit-tested. Input: XML bytes + the sitemap URL. Output:
`SitemapParse { kind: UrlSet | Index, entries: Vec<SitemapEntry> }` where
`SitemapEntry { loc: Url, lastmod: Option<DateTime>, news_date: Option<DateTime> }`.

- Parse with `quick-xml` (new workspace dep; no generic XML parser exists today).
- Handle `<sitemapindex>` (list of child sitemaps) vs `<urlset>` (list of pages).
- Extract `<loc>`, `<lastmod>`, and `news:publication_date` when present.
- `date_from_url_path()` helper: pull `/YYYY/MM(/DD)?/` or `/YYYY-MM-DD/` from the
  path when no explicit date tag exists.
- `is_news_scoped()` / `news_token_score()`: editorial-token classifier used both
  for selecting among multiple sitemaps and for the acceptance gate.

### 3. `SitemapCrawler` (`feed-crawler/src/lib.rs`)

Parallel to `RssAtomCrawler`. `crawl(source) -> CrawlBatch`:

- Fetch the sitemap URL (plain HTTP; sitemaps are not WAF-gated in practice).
- If it is an index, fetch child sitemaps, preferring news-scoped children, up to
  a bounded fan-out.
- Turn each entry into a `RawCrawlItem` (loc as url, derived date as
  `published_at`). Bounded by `max_links`.
- Emits the SAME `CrawlBatch`/`RawCrawlItem` shape the RSS path emits, so the
  downstream normalize/persist pipeline is unchanged.

### 4. Dispatch (`feed-jobs/src/lib.rs:6735`)

    match source.kind {
        Html | Browser => recipe crawl (HtmlRecipeCrawler),
        Sitemap        => sitemap crawl (SitemapCrawler),   // NEW
        Rss | Atom     => feed crawl (RssAtomCrawler),
    }

### 5. Discovery probe (`feed-discovery/src/lib.rs`)

When RSS/Atom discovery yields no feed AND `DISCOVERY_PROBE_SITEMAPS=true`:

- Fetch `robots.txt`, parse `Sitemap:` directives; also probe common sitemap
  paths.
- Fetch candidate sitemaps (bounded), select the news-scoped one per the rule,
  verify freshness-derivable, and emit a `SourceKind::Sitemap` candidate with
  confidence BETWEEN feed (0.96-0.99) and HTML (0.65-0.75) -- proposed **0.85**.
- HTML candidates still emitted, at lower confidence, so HTML remains the
  last-resort tier.

### 6. Config flag

`DISCOVERY_PROBE_SITEMAPS` (bool) in `feed-core/src/config.rs`
`AppSettings::from_env()`, threaded to `DiscoveryConfig.probe_sitemaps`. Default
`false`; enabled first only for the dark-launch cohort, then globally.

## Rollout (dark launch, projectrulev3 #1)

1. Land the code behind `DISCOVERY_PROBE_SITEMAPS=false` (no behavior change).
2. Unit-test the parser against the real Apple fixture
   (`crates/feed-crawler/tests/fixtures/apple_newsroom_sitemap.xml`) + synthetic
   Google-News + sitemapindex + non-news (shop) fixtures.
3. Dark-launch on AAPL / TSLA / AVGO / AMZN: run discovery with the flag on for
   just those tickers, confirm a news-scoped sitemap source is selected, crawl it,
   and verify the emitted items match the newsroom (dates + editorial URLs).
4. Widen the flag globally; then re-run discovery on the ~584 stale rebuild
   candidates so sitemap-capable companies self-heal off the fragile HTML tier.

## Content-quality guardrails (pipeline-quality.md)

- Content contract for sitemap source: an accepted sitemap must yield >= 1 entry
  with a derivable date and an editorial-scoped host/path; else fail loud (do not
  activate a Sitemap source that is actually a product inventory).
- Emit per-run metrics: entries seen, entries dated, entries date-from-path,
  entries dropped non-editorial. A sitemap run that accepts 0 dated editorial
  entries logs WARN, mirroring the scraper-empty guard.

## Related

- `docs/design/2026-08-18-recipe-rebuild-pipeline.md` -- the recipe-health backlog
  this tier is meant to relieve.
- `docs/discovery.md`, `docs/company-news-recipes.md`, `docs/crawling.md` --
  updated alongside implementation.
