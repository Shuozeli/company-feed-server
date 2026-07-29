# Launch Readiness

Snapshot: 2026-07-29

Status: **pre-release; coordinated public launch is blocked**

The implementation is public and usable from `main`, but there is no tagged
release yet. This document records the remaining launch decisions and the
evidence that must be collected from the exact commit being released. It does
not treat an earlier test count, a maintainer workstation, or a running private
deployment as release evidence.

## Blocking Decisions

### Publication model and third-party rights

The exporter has two source-selection scopes:

- the default requires `source.public_export_allowed=true`;
- a target can explicitly set
  `metadata.publication_scope=approved_public` to include all otherwise
  eligible approved sources.

The checked-in `configs/export_targets.yaml` sample uses the broader
`approved_public` scope and keeps `push_enabled=false`. These are technical
selection and transport controls. Neither scope determines ownership,
permission, licensing, fair use, or another legal basis for redistributing
publisher material.

The generated archive can contain article bodies as well as metadata. Before a
public launch, the maintainers must:

1. choose and document the intended publication model;
2. review the applicable rights and publisher terms for the material that model
   selects;
3. make every software, data, and UI statement consistent with that model;
4. provide a monitored provenance, correction, and rights-removal route; and
5. regenerate or remove previously published material if the chosen model
   requires it.

This blocker is resolved only when the review and any resulting corpus changes
are complete. An exporter flag or disclaimer alone is not sufficient.

### Git archive scale

The data architecture says to split the logical archive before compressed Git
history approaches 1 GiB. A maintainer measurement on this snapshot found the
published data repository's Git pack already exceeds 2 GiB, before normal clone
overhead.

Before launch, select and test a bounded distribution model, such as:

- a metadata/link-only Git repository with bodies in object storage;
- immutable year or epoch repositories behind a small catalog; or
- versioned compressed snapshots with a small manifest repository.

The chosen path must preserve deterministic identities, manifests, schema
validation, and lazy UI loading. This blocker is resolved only after a fresh
consumer can obtain and validate the documented distribution without cloning an
unbounded monolithic history.

### Canonical company identity

The current imported universe still contains security-level display names such
as “Common Stock” and “Ordinary Shares.” Multiple share classes can also appear
as separate company nodes even when they resolve to the same corporate entity
and article set. That contradicts the product's name-first identity model and
makes category browsing look ticker-derived even though the runtime does not
use tickers as identity.

Before launch:

1. retain raw universe and listing labels as aliases or provenance;
2. add a canonical corporate display name and stable entity key;
3. map multiple listings and share classes to one company entity;
4. publish redirects or an explicit compatibility map for superseded keys; and
5. regenerate and verify company/category indexes without duplicate
   share-class article branches.

This blocker is resolved when the public reader and archive expose one
unambiguous company node per corporate entity while retaining the original
listing evidence.

## Existing Foundations

- The server, generated data archive, static UI, and public reader are separate
  repositories with explicit roles.
- The software repository has an MIT source license plus contribution,
  conduct, security, responsible-use, and content-boundary documentation.
- CI, Compose smoke testing, dependency auditing, secret scanning, and push
  protection are configured.
- API, discovery, validation, crawl/export, recipe construction, and article
  hydration have separate runtime ownership.
- The archive has deterministic identities, bounded lazy indexes, JSON Schema,
  OpenAPI, content hashes, and a generated validator.

These are foundations, not a declaration that the current release candidate or
corpus has passed launch review.

## Exact-Commit Preflight

Record links or artifacts for every gate against the proposed release commit:

- clean-checkout formatting, lint, workspace tests, and Compose smoke;
- current dependency and security audit;
- declarative-schema initialization and zero-drift check;
- generated archive validation from the chosen distribution model;
- fresh local quick start and public reader smoke test;
- secret scan of reachable Git history and the assembled release artifact;
- review that `.env`, local adapter configuration, operational handoffs,
  database dumps, and credentials are absent;
- documentation review for version, image architecture, authentication
  boundary, publication scope, and known limitations;
- correction, security, and contribution routes tested from a logged-out
  browser; and
- final rights, archive-scale, and company-identity blockers explicitly signed
  off by the maintainer responsible for publication.

Do not record a fixed passing-test count here. Link to the immutable CI run for
the release commit instead.

## Publication Sequence

1. Resolve all blocking decisions above.
2. Run the exact-commit preflight from a clean clone.
3. Review `git archive` output rather than copying a maintainer worktree.
4. Date the changelog and tag the reviewed commit.
5. Verify the tag workflow, published artifacts, provenance, and SBOM.
6. Smoke-test the public UI and documented consumer path while logged out.
7. Follow the human-authored launch worksheet in
   [`show-hn-preflight.md`](show-hn-preflight.md).

Do not attach the production database, local configuration, private provider
material, or an unreviewed crawled corpus to the software release.
