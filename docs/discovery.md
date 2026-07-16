# Discovery

Discovery answers one question: for a company, where should we get public company news?

It does not crawl approved production feeds. It proposes candidate sources for review.

## Inputs

```yaml
companies:
  - ticker: NVDA
    name: NVIDIA
    homepage_url: https://www.nvidia.com/
    investor_relations_url: https://investor.nvidia.com/
    hints:
      - https://nvidianews.nvidia.com/
```

## Discovery Steps

1. Fetch the homepage and hints with HTTP.
2. Parse `<link rel="alternate">` RSS/Atom declarations.
3. Probe common feed paths:
   - `/feed`
   - `/rss`
   - `/rss.xml`
   - `/atom.xml`
   - `/news/rss`
   - `/newsroom/rss`
   - `/blog/feed`
   - `/press-releases/rss`
4. Extract links whose text or URL suggests:
   - newsroom
   - news
   - press
   - media
   - blog
   - engineering
   - investor relations
5. Fetch likely pages and repeat RSS/Atom discovery.
6. Classify candidates as `rss`, `atom`, `html`, or `browser`.
7. Store candidates with confidence and evidence.

## Candidate Status

```text
new -> accepted -> source row
new -> rejected
```

Discovery must not auto-approve sources by default. Auto-approval can be added later for high-confidence RSS/Atom candidates.

## Evidence

Candidate evidence should include:

- where the URL was found
- link text
- HTTP status
- content type
- feed validation result
- sample item count
- whether JavaScript rendering appears required

## Commands

```bash
feed-discover --company NVDA
feed-discover --all
feed-discover --company NVDA --write-candidates
```

