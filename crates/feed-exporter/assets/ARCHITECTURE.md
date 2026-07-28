# Archive architecture

This repository is a generated, version-controlled publication artifact. The
source service remains the system of record.

## Layout

```text
HEAD.json
articles/v1/<company-bucket>/<company-key>/<year>/<month>/<document-bucket>/<document-id>/
  article.md
  record.json
index/v1/current/
  manifest.json
  partitions/<year>/<month>/
    manifest.json
    shards/<hash-prefix>.jsonl
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

## Checkpoints

`HEAD.json` points to the current root manifest. Its `generation` is a
deterministic digest of the schema version and exported document/content
identities. Re-running an unchanged export produces no file or Git change.

## Compatibility

Paths under `v1` follow semantic compatibility:

- optional fields may be added;
- existing field meaning does not change;
- required-field removal, type changes, and identity changes require `v2`.

Consumers should start at `HEAD.json`, verify manifest and shard hashes, and
ignore unknown fields.

## Repository scale boundary

Text stays in ordinary Git rather than Git LFS so shallow clones, raw-file
consumers, and content diffs continue to work. The exporter targets 1 MiB JSONL
leaves and narrow directory trees. If compressed Git history approaches 1 GiB,
the logical archive should add an epoch catalog and place closed publication
years in separate repositories instead of allowing one clone to grow without
bound.
