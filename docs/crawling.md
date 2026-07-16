# Crawling

Crawling runs only on approved public sources.

## Adapter Order

```text
RSS/Atom -> static HTML -> pwright browser
```

## RSS/Atom Adapter

Use this whenever possible. It is cheap, stable, and transparent.

Responsibilities:

- fetch feed URL
- parse RSS or Atom
- derive external ID from GUID, URL, or title/date hash
- preserve feed metadata in `raw`
- emit `RawCrawlItem`

## Static HTML Adapter

Use for public newsroom/blog pages without feed support.

Responsibilities:

- fetch page over HTTP
- extract likely article links
- optionally fetch article pages
- extract title, date, preview, and body with readability-style heuristics

## `pwright` Browser Adapter

Use only when public pages require JavaScript rendering.

Constraints:

- no logged-in profiles
- no paywall bypass
- no private browser state
- browser endpoint must be explicitly configured
- recipes must live under public recipe directories only

The browser adapter should return the same `RawCrawlItem` contract as every other adapter.

## Source State

Each crawl updates `source_state`:

- last attempt
- last success
- last error
- consecutive failures
- backoff
- cursor

Backoff sequence:

```text
2m, 4m, 8m, 16m, 32m, 60m max
```

## Zero-Run Health

A successful crawl that returns zero items is not a transport failure, but repeated zero runs may mean the source layout changed.

Track:

- consecutive zero runs
- average items per run
- last nonzero run

