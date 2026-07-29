# Documentation Manifest

| File | Covers | Update When |
|------|--------|-------------|
| README.md | Product overview, quick start, operator flow, company profiles, API, and validation | Architecture or scope changes |
| docs/architecture.md | Separated runtimes, durable data flow, scheduling, and public/private boundary | Component/data-flow changes |
| docs/components.md | Crate, binary, API, and worker responsibilities | Component ownership changes |
| docs/database.md | Postgres schema, jobs table, and dedup model | Schema or data-model changes |
| docs/declarative-schema.md | Single-schema diff, review, apply, and drift workflow | Database deployment changes |
| docs/discovery.md | Source discovery workflow and periodic discovery jobs | Discovery behavior changes |
| docs/source-review.md | Strict/provisional activation policies, bounded waves, removal semantics, audit, APIs, and dashboard | Validation or source-governance changes |
| docs/company-universe-import.md | Name-first neutral bulk-import contract, optional listings, staging, audit, and activation waves | Universe contract or rollout behavior changes |
| docs/web-discovery-adapter.md | Optional neutral HTTP adapter, multi-property discovery, provisional trust, and private-provider boundary | Adapter contract or configuration changes |
| docs/manual-company-news-import.md | Operator-triggered one-company import, URL-only boundary, article contract, and sequential worker | Manual import contract or operations change |
| docs/company-news-recipes.md | Versioned recipe contract, build campaign, freshness/correctness gates, drift retirement, and coverage APIs | Recipe schema, builder, executor, or health policy changes |
| docs/news-viewer.html | API-embedded and optionally static-served crawled-news dashboard with search, source filters, and pagination | News-item API or dashboard behavior changes |
| docs/crawling.md | Crawl adapters, periodic crawl jobs, source state, backoff | Crawling or scheduler changes |
| docs/content-crawling.md | Separate article-page hydration worker, retry/freshness state, throttling, and coverage | Content-crawl behavior or operations change |
| docs/content-processing.md | HTML sanitizer and Markdown conversion contract | Content extraction or output-format changes |
| docs/normalization.md | Raw-to-normalized item contract | Output contract changes |
| docs/exporters.md | Git/GitHub export design and periodic export jobs | Export format or safety changes |
| docs/responsible-use.md | Public-fetch, access-control, identity, concurrency, and operator expectations | Crawl policy or safety defaults change |
| docs/data-and-content-policy.md | Repository and third-party data/content licensing boundary | Bundled data, fixtures, or export policy changes |
| docs/launch-readiness.md | Current launch blockers, exact-commit evidence gates, and publication sequence | Launch status, publication model, archive distribution, or release gates change |
| docs/show-hn-preflight.md | Human-authored Show HN worksheet, launch checks, and response conduct | HN guidance or launch process changes |
| docs/roadmap.md | Implementation phases | Milestone changes |
