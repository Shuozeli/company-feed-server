# Normalization

Normalization converts raw crawl output into a stable company-news item contract.

## Input

```json
{
  "source_id": "co-nvda-newsroom",
  "external_id": "article-123",
  "url": "https://nvidianews.nvidia.com/news/example",
  "title": " NVIDIA announces example ",
  "summary": "optional preview",
  "body_text": "optional text",
  "body_html": "optional html",
  "published_at": "2026-07-16T00:00:00Z",
  "raw": {}
}
```

## Output

```json
{
  "source_id": "co-nvda-newsroom",
  "company_ticker": "NVDA",
  "company_name": "NVIDIA",
  "external_id": "article-123",
  "url": "https://nvidianews.nvidia.com/news/example",
  "canonical_url": "https://nvidianews.nvidia.com/news/example",
  "title": "NVIDIA announces example",
  "summary": "optional preview",
  "body_text": "optional text",
  "body_html": "optional html",
  "published_at": "2026-07-16T00:00:00Z",
  "fetched_at": "2026-07-16T00:05:00Z",
  "content_hash": "sha256:...",
  "source_kind": "rss"
}
```

## Responsibilities

- URL canonicalization
- title whitespace cleanup
- stable external ID fallback
- date parsing and validation
- content hash
- company mapping
- HTML-to-text fallback when needed
- export metadata redaction

## Dedup Policy

Dedup should prefer source-local identity first:

1. `(source_id, external_id)`
2. `(source_id, canonical_url)`
3. `content_hash`

The global `content_hash` constraint catches cross-source duplicates, but source-local keys preserve provenance.

