# Architecture

Company Feed Server is a multi-binary Rust workspace backed by one Postgres database.

The system has a strict public-data boundary: it collects and publishes company-related data from public sources only. Discovery can propose sources, but only approved sources are crawled and exported.

## Data Flow

```text
companies.yaml
  |
  v
feed-discover
  |
  v
source candidates
  |
  v
feed-admin approval
  |
  v
approved sources
  |
  +--> feed-crawl      one-shot crawl
  |
  +--> feed-scheduler  repeated SLO crawl
          |
          v
      raw crawl batch
          |
          v
      normalizer
          |
          v
      feed_items
          |
          +--> feed-server REST API
          |
          +--> feed-export GitHub archive repos
```

## Workspace Shape

```text
company-feed-server/
  Cargo.toml
  docker-compose.yml
  configs/
    companies.yaml
    sources.yaml
    export_targets.yaml
  migrations/
  crates/
    core/
    db/
    discovery/
    crawler/
    normalizer/
    scheduler/
    exporter/
    api/
  bins/
    feed-server
    feed-discover
    feed-crawl
    feed-scheduler
    feed-export
    feed-admin
```

## Runtime Model

Postgres is the shared state store. All binaries use the same database and coordinate through durable tables:

- `companies`
- `sources`
- `source_candidates`
- `feed_items`
- `source_state`
- `crawl_runs`
- `discovery_runs`
- `export_targets`
- `export_runs`
- `exported_items`
- `event_log`

The API is intentionally not the control plane for every operation. Batch and operator workflows should be available as CLI binaries so local and CI runs are simple.

## Public Boundary

Allowed:

- RSS and Atom feeds
- public HTML pages
- public company newsroom, blog, and IR pages
- browser crawling for public pages when static HTML is insufficient
- normalized article metadata and text
- GitHub export of public company-news archives

Disallowed:

- logged-in source crawling
- paywall bypass
- private source lists
- private production database snapshots
- internal message buses
- private LLM/enrichment services
- private browser profiles

