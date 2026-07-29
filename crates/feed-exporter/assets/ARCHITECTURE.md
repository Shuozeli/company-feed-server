# Data architecture

This repository is a generated, version-controlled publication artifact. The
source service remains the system of record.

## Layout

```text
index.json
HEAD.json
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
    shards/<hash-prefix>.jsonl
articles/v1/<company-bucket>/<company-key>/
  company.json
  index/pages/<page>.json
articles/v1/<company-bucket>/<company-key>/<year>/<month>/<document-bucket>/<document-id>/
  article.md
  record.json
schemas/v1/
openapi/openapi.json
```

`company-bucket` and `document-bucket` are the first two hexadecimal characters
of SHA-256 identifiers. They keep Git trees narrow without making human
browsing depend on a stock ticker.

## Identity

`document_id` is SHA-256 over the schema namespace, company key, and normalized
canonical URL. It is stable across database rebuilds and source-local IDs. The
original source observation remains in `record.json` for provenance.

## Index shards

The index is first partitioned by archival month. Each month is then split by
successive hexadecimal characters of `document_id` until every leaf is below
both configured limits, unless one document is itself larger than the byte
target. Small partitions remain a single `root.jsonl` shard. Partition
manifests contain the prefix, byte count, record count, and SHA-256 digest of
every leaf.

JSONL is UTF-8, one compact JSON object per line, sorted by `document_id`, with a
final newline. Shards are snapshots; Git commits provide the change log.

## Lazy browser indexes

`index.json` is the stable, lightweight browser bootstrap. It points to:

- newest-first article-summary pages;
- an alphabetical company directory split into 37 bounded buckets;
- a category manifest with bounded 100-company pages per universe sector;
- the canonical full-text archive manifest.

Article-summary pages contain metadata and paths only. A browser fetches an
individual `record.json` and `article.md` after the reader selects an article.
Full `body_text` remains in the JSONL shards for downstream indexing, but is
never duplicated into browser navigation pages.

Category names come from `companies.metadata.universe.sector`. Display names
are NFC-normalized with surrounding and repeated whitespace removed. Missing
sector values are published under `Uncategorized` with the reserved key
`uncategorized`. Other keys use a bounded readable ASCII slug plus a stable
SHA-256 suffix, so Unicode and punctuation-heavy labels remain path-safe and
slug collisions do not merge categories.

`index.json`, category manifests, and category pages carry both the dataset
`generation` and a deterministic `taxonomy_generation`. The latter changes
when a company moves between categories even when every article remains
unchanged. `HEAD.json` remains the schema-`1.0.0` article archive checkpoint;
taxonomy-aware browsers use the versioned `index.json` bootstrap.

## Checkpoints

`index.json` and `HEAD.json` identify the same generation. `HEAD.json` points
to the current root manifest. Its `generation` is a
deterministic digest of the schema version and exported document/content
identities. Re-running an unchanged export produces no file or Git change.

## Compatibility

Paths under `v1` follow semantic compatibility:

- optional fields may be added;
- existing field meaning does not change;
- required-field removal, type changes, and identity changes require `v2`.

Interactive consumers should start at `index.json`; bulk consumers may start
at `HEAD.json`. Both should verify referenced hashes and ignore unknown fields.
The browser bootstrap contract is version `1.1.0` because category navigation
and the required taxonomy checkpoint were added. Per-document
`schema_version` remains `1.0.0`, preserving article identity, paths, summary
formats, and the article dataset generation.

## Repository scale boundary

Text stays in ordinary Git rather than Git LFS so shallow clones, raw-file
consumers, and content diffs continue to work. The exporter targets 1 MiB JSONL
leaves and narrow directory trees. If compressed Git history approaches 1 GiB,
the logical archive should add an epoch catalog and place closed publication
years in separate repositories instead of allowing one clone to grow without
bound.
