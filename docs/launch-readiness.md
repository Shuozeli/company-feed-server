# Launch Readiness

Snapshot: 2026-07-28

Candidate: `v0.1.0-rc.1`

## Ready

- The GitHub repository is public.
- MIT licensing and the code/content rights boundary are explicit.
- Contribution, conduct, security, and responsible-use policies are present.
- Private vulnerability reporting is enabled.
- GitHub secret scanning and push protection are enabled.
- Dependabot alerts, security updates, and scheduled dependency updates are
  enabled.
- Repository topics identify the Rust, PostgreSQL, crawler, RSS, Atom, and
  company-news scope.
- Private provider code, credentials, live data, crawled content, local
  configuration, and dated operational handoffs are excluded.
- Source builds and tagged releases use the same multi-binary container.

## Verification

| Gate | Result |
|---|---|
| Rust toolchain | 1.88.0 |
| Formatting | passed |
| Clippy, all targets/features, warnings denied | passed |
| Workspace tests | 271 passed |
| Declarative database schema | clean initialization, generated live diff, and zero-drift Compose apply passed |
| RustSec | passed with the unreachable SQLx/MySQL lockfile exception documented in `SECURITY.md` |
| Dependency maintenance | active HTML parser path has no denied unmaintained warning |
| Secret scan | Git history and assembled release tree passed |
| GitHub Actions syntax | passed `actionlint` |
| Compose validation | passed |
| Clean container bootstrap | `/ready`, `/news`, and coverage API passed |
| Live deployment | API plus discovery, validation, crawl/export, recipe-build, and content workers healthy |
| Browser validation | embedded and optional static news dashboards passed |

## Publication Sequence

The public repository currently contains only the original design seed. To
publish this candidate safely:

1. Review the complete release diff and archive.
2. Commit the implementation as the `v0.1.0` release candidate.
3. Push the candidate and let CI and the security audit complete.
4. Protect `main` using the real `quality` and `compose-smoke` check contexts.
5. Merge any release-candidate fixes.
6. Date the changelog and tag the reviewed commit as `v0.1.0`.
7. Verify the tag workflow publishes the GHCR image, provenance, SBOM, and
   GitHub release.

Do not attach the production database or crawled article corpus to the
software release.
