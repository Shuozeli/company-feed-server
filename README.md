# Company Feed Server

Open-source company news feed discovery, crawling, normalization, and export server.

The project focuses on public company-related news sources:

- official newsroom pages
- investor relations news pages
- engineering and product blogs
- RSS and Atom feeds
- public HTML pages that can be fetched without login or paywall bypass

It intentionally excludes private crawling tactics, paid-source recipes, logged-in browser profiles, and proprietary production source strategies.

## Goals

- Discover whether company news/blog/IR pages expose RSS or Atom.
- Crawl approved public company sources on a freshness schedule.
- Fall back from RSS/Atom to static HTML, then to browser crawling with `pwright` when needed.
- Normalize crawled output into a stable company-news item contract.
- Store everything in Postgres.
- Export public company news/blog archives into GitHub repositories.

## Non-Goals

- No SQLite runtime mode.
- No Prisma.
- No private feed data.
- No paywall bypass.
- No logged-in browser crawling.
- No dependency on DragB production services.

## Stack

- Rust
- Postgres through Docker Compose
- `sqlx` for database access and migrations
- `axum` for HTTP APIs
- `tokio` for scheduler and workers
- `reqwest` for HTTP fetching
- RSS/Atom parser crate, exact crate TBD
- HTML parsing/readability extraction, exact crate TBD
- optional `pwright` browser crawling adapter

## Planned Binaries

- `feed-server`: REST API.
- `feed-discover`: company source discovery.
- `feed-crawl`: one-shot crawling.
- `feed-scheduler`: long-running SLO scheduler.
- `feed-export`: Git/GitHub exporter.
- `feed-admin`: operator CLI for approvals and maintenance.

## Documentation

- [Architecture](docs/architecture.md)
- [Components](docs/components.md)
- [Database](docs/database.md)
- [Discovery](docs/discovery.md)
- [Crawling](docs/crawling.md)
- [Normalization](docs/normalization.md)
- [Exporters](docs/exporters.md)
- [Roadmap](docs/roadmap.md)

