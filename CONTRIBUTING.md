# Contributing

Thanks for helping improve Company Feed Server. Contributions to the generic
public-web discovery, validation, crawling, normalization, API, and export
engine are welcome.

## Project Boundary

Contributions must use public inputs and provider-neutral contracts. Do not
submit credentials, private provider code, prompts, raw private-provider
responses, paywall bypasses, logged-in browser state, or copied publisher
content.

Read [Responsible use](docs/responsible-use.md) and
[Data and content policy](docs/data-and-content-policy.md) before contributing
to crawling or export behavior.

## Development Setup

Requirements:

- Rust 1.88.0, installed automatically by `rust-toolchain.toml`
- Docker with Docker Compose
- PostgreSQL 16, normally through Docker Compose

Start a local database:

```bash
docker compose up -d postgres
cp .env.example .env
set -a
source .env
set +a
```

Run the API:

```bash
cargo run -p feed-server
```

## Required Checks

Use a disposable test database. The integration suite changes database state;
never point `TEST_DATABASE_URL` at a production or valuable development
database.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
TEST_DATABASE_URL=postgresql://company_feed:company_feed@localhost:55432/company_feed \
  cargo test --workspace --all-features --all-targets -- --test-threads=1
docker compose -f docker-compose.yml config --quiet
docker build .
```

## Changes

- Add tests for behavior changes.
- Edit the single `schema/postgres.sql` target for structural database changes,
  inspect `scripts/schema-plan.sh`, and apply only the generated diff.
- Update the relevant documentation and `docs/MANIFEST.md`.
- Keep source discovery separate from API request handling.
- Preserve URL safety, bounded concurrency, replay evidence, and audit history.
- Keep public export disabled unless an operator explicitly enables it.

Open a pull request with the problem, approach, verification, and any schema
diff or operational impact. Report security issues through [the private security
channel](SECURITY.md), not a public issue.
