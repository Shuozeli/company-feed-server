# Exporters

The exporter publishes normalized public company news into Git repositories.

Exporting is a first-class component, not an afterthought. A public archive repo is useful for downstream users, model experiments, and reproducible processing.

Exports normally run as periodic jobs. Manual export commands exist for debug, backfill, and one-off publishing.

## Export Target

```yaml
targets:
  - target_id: company-news-data
    repo_url: git@github.com:org/company-news-data.git
    local_path: ./exports/company-news-data
    branch: main
    format: markdown_json
    layout: by_company_date
    cadence_seconds: 3600
    push_enabled: false
    metadata:
      publication_scope: approved_public
```

Without `metadata.publication_scope`, the default selection additionally
requires `source.public_export_allowed=true`. The checked-in sample explicitly
uses `approved_public`, which selects non-private records from all approved,
currently valid sources. `push_enabled=false` is independent: it prevents a Git
push, not local materialization.

## Archive Layout

```text
company-news-data/
  .github/
    ISSUE_TEMPLATE/
    workflows/validate.yml
  index.json
  HEAD.json
  README.md
  ARCHITECTURE.md
  CONTRIBUTING.md
  CONTENT_RIGHTS.md
  SECURITY.md
  articles/v1/
    <company-hash>/
      <company-key>/
        company.json
        index/pages/<page>.json
        <year>/<month>/<document-hash>/<document-id>/
          article.md
          record.json
  index/v1/current/
    manifest.json
    recent/
      manifest.json
      pages/<page>.json
    companies/
      manifest.json
      buckets/<letter>.json
    categories/
      manifest.json
      <category-key>/pages/<page>.json
    partitions/<year>/<month>/
      manifest.json
      shards/
        root.jsonl
        <hash-prefix>.jsonl
  schemas/v1/
  openapi/openapi.json
  scripts/validate_archive.py
```

The two-character company and document hash buckets keep Git trees narrow while
company key and date components remain readable. Paths never depend on a stock
ticker, so private companies use the same layout.

## Identity

`document_id` is a SHA-256 digest over the v1 identity namespace, stable
company key, and normalized canonical URL. Source-local UUIDs and external IDs
remain provenance fields but do not control public identity.

The first v1 path is retained through `exported_items`, so a corrected
publication date does not repeatedly rename an article. A canonical URL
identity correction intentionally produces a new public identity.

## Article Files

`article.md` is the human-readable normalized article. Its frontmatter contains
the schema version, document/company/source identity, canonical URL, timestamps,
and normalized content hash.

```markdown
---
schema_version: "1.0.0"
document_id: "7c..."
company_key: nvidia
company: NVIDIA
source_id: co-nvda-newsroom
canonical_url: https://example.com/article
published_at: 2026-07-16T00:00:00Z
first_seen_at: 2026-07-16T00:04:00Z
fetched_at: 2026-07-16T00:05:00Z
content_hash: sha256:...
---

# Article title

Article body text.
```

`record.json` is canonical metadata and points to `article.md`; it does not
duplicate the body. Raw HTML is deliberately omitted. Index JSONL records carry
normalized plaintext for direct indexing.

## Shards and Manifests

The index first partitions records by archival month. Each partition starts as
`root.jsonl` and recursively splits on successive hexadecimal characters of
`document_id` when it exceeds either 5,000 records or 1 MiB. Every JSONL leaf is
UTF-8, sorted by document ID, compact, and newline terminated.

`index.json` is the small browser bootstrap. It points to bounded newest-first
summary pages, an alphabetical company directory, and a category directory.
Each category exposes lazily loaded pages capped at 100 of the same compact
company-directory rows. Category names derive from
`companies.metadata.universe.sector`, are NFC-normalized, and collapse
whitespace. An absent sector falls back to `Uncategorized`
(`uncategorized`). Other category keys combine a bounded readable ASCII slug
with a stable SHA-256 suffix, preserving arbitrary Unicode display names while
preventing slug collisions. Article bodies are loaded only after selection,
while bulk consumers continue to use full-text JSONL shards.

`index.json`, category manifests, and category pages carry the article dataset
`generation` plus a separate deterministic `taxonomy_generation`. Moving a
company between categories therefore invalidates taxonomy caches without
changing article document IDs, paths, summaries, or the dataset generation.
`HEAD.json` remains the schema-`1.0.0` bulk article checkpoint; taxonomy-aware
clients bootstrap through `index.json`.

`HEAD.json` points to the current root manifest. The root references monthly
partition manifests, which reference leaf shards with:

- SHA-256 prefix;
- relative path;
- record and byte counts;
- SHA-256 content digest; and
- minimum and maximum document IDs.

The generation ID is a deterministic digest of the schema version and all
index documents. An unchanged export therefore produces no file or Git commit
change.

## Schema Contract

Canonical file contracts use JSON Schema Draft 2020-12 under `schemas/v1/`.
The OpenAPI 3.1.2 document under `openapi/` references those schemas rather than
maintaining a second model. It describes both the static Git paths and
compatible read-only HTTP adapters.

The bootstrap `contract_version` and OpenAPI contract are `1.1.0` because the
required taxonomy checkpoint and category navigation are additive bootstrap
capabilities. Article and summary documents retain `schema_version: 1.0.0`;
the change does not rewrite their identity or dataset generation.

Run `python3 scripts/validate_archive.py` inside a generated repository to
verify schema/OpenAPI JSON parsing, manifest and content hashes, counts, shard
ordering, unique document IDs, and all record/article references.

## Export Selection and Publication Responsibility

The exporter only considers non-private, non-future items from approved,
currently valid public source kinds. HTML/browser items must also retain a
valid active recipe.

The target then uses one of two source-selection scopes:

- default: require `source.public_export_allowed=true`;
- explicit broader scope:
  `metadata.publication_scope=approved_public` includes every otherwise
  eligible approved source.

The repository sample uses the broader scope and disables Git push. Neither
scope determines ownership, permission, licensing, fair use, or another legal
basis for redistribution. `push_enabled` controls transport only. The operator
is responsible for reviewing applicable publisher terms and rights before
making a generated archive public.

## Idempotency

`exported_items` tracks:

- target
- feed item
- exported path
- commit
- exported timestamp
- exported content hash

Re-running an export should be safe. Existing files should only change when the normalized item changes.

Materializing files and creating a local commit are separate from pushing. A periodic or manual run may only push when the target has `push_enabled: true`; the default is local-only. `export_runs` records the commit SHA and whether it was pushed.

## Commands

```bash
feed-admin export --target company-news-data
feed-admin export
```

These commands enqueue the same periodic `export_target` job contract. Pushing
is controlled only by the reviewed target configuration; the command cannot
silently override a safe `push_enabled: false` setting.

## Periodic Export

Export targets define cadence:

```yaml
targets:
  - target_id: company-news-data
    repo_url: git@github.com:org/company-news-data.git
    local_path: ./exports/company-news-data
    branch: main
    format: markdown_json
    layout: by_company_date
    cadence_seconds: 3600
    push_enabled: false
    metadata:
      publication_scope: approved_public
```

The scheduler creates `export_target` jobs for due targets. A worker materializes new exportable items, commits them locally, and optionally pushes when configured.
