# Changelog

All notable changes will be recorded here. The project follows
[Semantic Versioning](https://semver.org/).

## Unreleased

### Added

- Sitemap crawl tier (`SourceKind::Sitemap` + `SitemapCrawler`) between the
  RSS/Atom and HTML-recipe tiers: parses `<urlset>`/`<sitemapindex>` (incl.
  `<urlset>`-of-child-sitemaps), derives publish dates from
  `news:publication_date`/`lastmod`/URL-path, keeps news-scoped editorial
  entries, and emits `RawCrawlItem`s through the unchanged normalize pipeline.
  Dark-launched on Apple/Amazon/Broadcom.
- Sitemap auto-discovery (`DISCOVERY_PROBE_SITEMAPS`, default off): discovery
  probes robots.txt + common sitemap paths and emits ranked news-scoped
  `SourceKind::Sitemap` candidates; the validation worker crawls + accepts them
  (>= 3 dated editorial articles) and activates a sitemap source; the scheduler
  crawls sitemap sources and they count as a healthy feed (suppressing HTML-recipe
  building). The sitemap crawl DISCOVERS article URLs then reuses the shared
  `HtmlArticleCrawler` to fetch each article's title + body (so items normalize).
  Verified end to end on the compose fleet: Apple/Amazon sitemaps produced 45
  `source_kind='sitemap'` feed_items.
- Incremental sitemap crawls: a per-source date watermark in `source_state.cursor`
  means steady-state crawls fetch only new articles plus lastmod-bumped (re-edited)
  ones; re-fetched changed articles update the existing feed_item (re-snapshot) via
  the existing conflict-update path. Lastmod-less sitemaps fetch new-only.
- Name-first company registry for public and private companies.
- Separated API, discovery, validation, crawl/export, and manual recipe-build
  runtimes.
- RSS, Atom, and generic HTML recipe discovery and crawling.
- Provider-neutral discovery and company-news adapter contracts.
- Versioned recipes with correctness, freshness, drift, and ownership gates.
- PostgreSQL-backed durable jobs, audit history, normalized items, and Git
  export.
- Review, coverage, source-health, and crawled-news dashboards.
- Docker Compose deployment, release documentation, and CI.
- Tagged GHCR image publishing with provenance and SBOM attestations.

### Security

- Private-network URL blocking is enabled by default.
- Public archive export and adapter-backed news extraction are disabled by
  default.
- Compose host bindings are loopback-only unless an operator explicitly adds a
  private-network override.
- Public fetches use an identifiable, operator-configurable user agent.
- The HTML parser stack was updated to remove an unmaintained hashing
  dependency.
- Operator write routes (candidate validate/activate/reject, batch decisions)
  now require a Bearer token (`OPERATOR_API_TOKEN`), validated in constant time.
- Crawlers no longer follow HTTP redirects and refuse proxies; private-network
  and DNS failures are retryable rather than silently treated as dead sources.
- The HTML content parser enforces element-depth limits to prevent stack
  exhaustion on pathological markup.
- Running-job cancellation is fenced by lease token so an expired worker can no
  longer cancel a job another worker has since claimed.
