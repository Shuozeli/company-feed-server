# Changelog

All notable changes will be recorded here. The project follows
[Semantic Versioning](https://semver.org/).

## Unreleased

### Added

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
