# Normalization

Normalization converts raw crawl output into a stable company-news item contract.

HTML sanitization and Markdown conversion happen in `feed-content` before or during normalization. The normalizer should not implement its own sanitizer; it should consume `ProcessedContent` and preserve processor metrics in metadata.

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
  "body_markdown": "optional markdown",
  "published_at": "2026-07-16T00:00:00Z",
  "raw": {},
  "content_processing": {}
}
```

Manually imported articles use the same output with `source_kind: "html"`.
Their title, dates, canonical URL, and body are derived from the independently
fetched public page, not accepted from the URL-suggestion adapter.

## Output

```json
{
  "source_id": "co-nvda-newsroom",
  "company_key": "nvidia",
  "company_name": "NVIDIA",
  "external_id": "article-123",
  "url": "https://nvidianews.nvidia.com/news/example",
  "canonical_url": "https://nvidianews.nvidia.com/news/example",
  "title": "NVIDIA announces example",
  "summary": "optional preview",
  "body_text": "optional text",
  "body_html": "optional html",
  "body_markdown": "optional markdown",
  "published_at": "2026-07-16T00:00:00Z",
  "fetched_at": "2026-07-16T00:05:00Z",
  "content_hash": "sha256:...",
  "source_kind": "rss"
}
```

## Responsibilities

- URL canonicalization that removes fragments, tracking, locale, filter,
  pagination, and arbitrary query fields while retaining at most four bounded
  resource identity pairs from a provider-neutral key vocabulary (for example
  `content_id`, `newsid`, `post_id`, or `p`)
- title whitespace cleanup
- content-aware rejection of unedited CMS starter posts; both the localized
  placeholder title and short template body must match, preserving substantive
  “Hello World” launch articles
- cross-mode rejection of conservative non-editorial utility signatures such
  as cookie/privacy policies, newsletter sign-up forms, subscription screens,
  and investor-alert forms
- rejection of market quote/profile utilities only when both a bounded
  quote/equity URL namespace and a stock/share price-or-quote title agree;
  substantive market reporting remains eligible
- generic framework and collection-title handling: labels such as
  `Release Details`, `Image link`, `Arrow icon`, and short `Contact <brand>`
  utility titles are never emitted as headlines; independently observed or
  structural article headlines may replace framework chrome
- stable external ID fallback
- date parsing and validation
- content hash
- company mapping
- HTML-to-text fallback when needed
- `feed-content` Markdown and sanitizer metrics attachment
- export metadata redaction

## Dedup Policy

Dedup should prefer source-local identity first:

1. `(source_id, external_id)`
2. `(source_id, canonical_url)`
3. `content_hash` as a change-detection signal

`content_hash` is indexed but deliberately not globally unique. Two approved
sources may publish identical text, and both provenance records must remain
queryable.

When a tightened canonicalization policy collapses several legacy external IDs
onto one source-local canonical URL, the existing canonical owner wins. The
new raw observations point to that row, and a losing quarantined external-ID
row is not rewritten into a uniqueness collision or released merely because
its query noise disappeared.

Public news queries apply a second, provenance-preserving presentation layer.
They first collapse matching canonical URLs, preferring RSS over Atom and HTML.
For items with an independently observed publication date, they also collapse
same-company mirrors whose normalized titles fall on the same calendar day.
The underlying source rows remain available for audit. Undated items are never
title-deduplicated because recurring dividends, buyback updates, and investor
events often reuse a legitimate headline.
