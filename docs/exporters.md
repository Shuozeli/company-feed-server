# Exporters

The exporter publishes normalized public company news into Git repositories.

Exporting is a first-class component, not an afterthought. A public archive repo is useful for downstream users, model experiments, and reproducible processing.

Exports normally run as periodic jobs. Manual export commands exist for debug, backfill, and one-off publishing.

## Export Target

```yaml
targets:
  - target_id: company-news-archive
    repo_url: git@github.com:org/company-news-archive.git
    local_path: ./exports/company-news-archive
    branch: main
    format: markdown_json
    layout: by_company_date
    cadence_seconds: 3600
    push_enabled: false
```

## Archive Layout

```text
company-news-archive/
  companies/
    nvidia/
      index.json
      2026/
        07/
          2026-07-16-nvidia-announces-example.md
          2026-07-16-nvidia-announces-example.json
    amd/
      index.json
  feeds/
    latest.jsonl
  indexes/
    by_company.json
    by_date/
      2026-07-16.json
```

## Markdown Format

```markdown
---
company_key: nvidia
company: NVIDIA
source_id: co-nvda-newsroom
url: https://example.com/article
canonical_url: https://example.com/article
published_at: 2026-07-16T00:00:00Z
fetched_at: 2026-07-16T00:05:00Z
content_hash: sha256:...
---

# Article title

Article body text.
```

Archive paths and frontmatter use `company_key`, so private and public companies
share one stable layout. Listing symbols are not used in filenames or identity.

## Export Safety Rules

Exporter must only export items when:

- source is approved
- source has `public_export_allowed = true`
- source kind is public
- title and URL are present
- item is not marked private
- metadata passes redaction

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
feed-admin export --target company-news-archive
feed-admin export
```

These commands enqueue the same periodic `export_target` job contract. Pushing
is controlled only by the reviewed target configuration; the command cannot
silently override a safe `push_enabled: false` setting.

## Periodic Export

Export targets define cadence:

```yaml
targets:
  - target_id: company-news-archive
    repo_url: git@github.com:org/company-news-archive.git
    local_path: ./exports/company-news-archive
    branch: main
    format: markdown_json
    layout: by_company_date
    cadence_seconds: 3600
    push_enabled: false
```

The scheduler creates `export_target` jobs for due targets. A worker materializes new exportable items, commits them locally, and optionally pushes when configured.
