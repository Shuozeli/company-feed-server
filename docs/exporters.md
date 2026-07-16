# Exporters

The exporter publishes normalized public company news into Git repositories.

Exporting is a first-class component, not an afterthought. A public archive repo is useful for downstream users, model experiments, and reproducible processing.

## Export Target

```yaml
targets:
  - target_id: company-news-archive
    repo_url: git@github.com:org/company-news-archive.git
    local_path: ./exports/company-news-archive
    branch: main
    format: markdown_json
    layout: by_company_date
```

## Archive Layout

```text
company-news-archive/
  companies/
    NVDA/
      index.json
      2026/
        07/
          2026-07-16-nvidia-announces-example.md
          2026-07-16-nvidia-announces-example.json
    AMD/
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
ticker: NVDA
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

Re-running an export should be safe. Existing files should only change when the normalized item changes.

## Commands

```bash
feed-export --target company-news-archive
feed-export --company NVDA --since 2026-01-01
feed-export --target company-news-archive --push
```

