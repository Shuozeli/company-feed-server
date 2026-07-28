# Release Guide

This repository is preparing its first `v0.1.0` release.

## Maintainer Settings

Before publishing the implementation:

- enable private vulnerability reporting;
- enable secret scanning and push protection;
- enable Dependabot alerts and security updates;
- protect `main` and require the `CI / quality` and `CI / compose-smoke` jobs;
- require pull requests for future changes;
- add a concise repository description and the topics `rust`, `rss`, `atom`,
  `crawler`, `postgresql`, and `company-news`; and
- confirm that GitHub Actions may write release contents and packages for the
  tag-only release workflow.

## Release Candidate

From a clean checkout:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
TEST_DATABASE_URL=<disposable-postgres-url> \
  cargo test --workspace --all-features --all-targets -- --test-threads=1
docker compose -f docker-compose.yml config --quiet
docker compose -f docker-compose.yml up --build -d postgres server
curl --fail http://localhost:8080/ready
curl --fail http://localhost:8080/news
docker compose -f docker-compose.yml down --volumes
```

Then:

1. Run a secret scan over the complete Git history and release tree.
2. Confirm `docs/handoff-*.md`, `.env`, exports, artifacts, database volumes,
   crawled content, and private adapter code are absent.
3. Run `cargo audit --deny warnings --ignore RUSTSEC-2023-0071` and review the
   narrowly scoped SQLx exception documented in `SECURITY.md`.
4. Replace the `Unreleased` heading in `CHANGELOG.md` with
   `0.1.0 - YYYY-MM-DD`.
5. Merge the release candidate to `main`.
6. Tag the exact reviewed commit as `v0.1.0`.
7. Verify the release workflow publishes the GitHub release and
   provenance/SBOM attestations for the GHCR image.
8. Verify the source-build and tagged-image quick starts from a fresh clone.

## After Launch

- publish a minimal architecture diagram and synthetic dashboard screenshot;
- triage issues against the documented public/private boundary;
- keep `schema/postgres.sql` authoritative and review generated schema plans;
- publish security fixes for the latest release line; and
- do not publish live crawled content without a separate rights review.
