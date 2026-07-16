# Roadmap

## Phase 0: Design Seed

- Create repository.
- Document architecture and boundaries.
- Define database model.
- Define component boundaries.
- Define Git exporter contract.

## Phase 1: Postgres Foundation

- Add Rust workspace.
- Add Docker Compose Postgres.
- Add `sqlx` migrations.
- Implement config loading.
- Implement DB access layer.

## Phase 2: Discovery MVP

- Load `companies.yaml`.
- Discover RSS/Atom links.
- Probe common feed paths.
- Store source candidates.
- Add `feed-admin` approval commands.

## Phase 3: Crawling MVP

- RSS/Atom crawler.
- One-shot `feed-crawl`.
- Normalize and upsert feed items.
- Track source state and crawl runs.

## Phase 4: API

- Add `feed-server`.
- Expose companies, sources, items, health, and stats.
- Add pagination and filters.

## Phase 5: Scheduler

- Add `feed-scheduler`.
- Implement SLO selection.
- Implement backoff and zero-run health.
- Add per-domain limits.

## Phase 6: HTML and Browser Fallback

- Static HTML crawler.
- Public `pwright` browser adapter.
- Public recipe format.
- Browser-required source health.

## Phase 7: Git Export

- Add export targets.
- Materialize Markdown and JSON.
- Commit locally.
- Optional push to GitHub.
- Export indexes.

## Phase 8: Company Source Expansion

- Seed high-quality public company sources.
- Add semiconductors and big tech first.
- Add SaaS, cybersecurity, cloud, and infrastructure.

