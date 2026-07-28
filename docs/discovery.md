# Discovery

Discovery answers: “which public company web properties and feeds might be
useful?” It does not approve sources and it does not crawl production content.

Discovery runs only in `feed-discovery-worker` through durable
`discover_company` jobs. API, validation, and crawl workers do not register
that job type.

## Inputs

The canonical company identity is name-first:

```yaml
companies:
  - company_key: gusto
    name: Gusto
    aliases:
      - ZenPayroll
    ownership_status: private
    lifecycle_status: active
    listings: []
    homepage_url: https://gusto.com/
    blog_url: https://engineering.gusto.com/
    hints: []
```

Public companies use the same contract. Optional listings are stored as
metadata but never enter discovery requests:

```yaml
companies:
  - company_key: nvidia
    name: NVIDIA
    aliases:
      - NVIDIA Corporation
    ownership_status: public
    listings:
      - ticker: NVDA
        exchange: NASDAQ
        is_primary: true
    homepage_url: https://www.nvidia.com/
    investor_relations_url: https://investor.nvidia.com/
    hints:
      - https://nvidianews.nvidia.com/
```

Configured entry points are homepage, newsroom, blog, IR URL, and hints.
Name-only records may additionally use the optional neutral web adapter.

## Deterministic Public Discovery

For each seed, the worker:

1. enforces the public-network URL policy;
2. fetches within timeout, redirect, and byte limits;
3. parses RSS/Atom payloads;
4. parses real RSS/Atom `<link rel="alternate">` declarations;
5. probes bounded common feed paths;
6. extracts same-site newsroom, press, media, blog, engineering, and IR links;
7. classifies results as `rss`, `atom`, or `html`;
8. canonicalizes locale and transient URL variants;
9. filters sitemaps, oEmbed, comment feeds, search-support citations, and article-level
   false positives; and
10. upserts confidence plus evidence into `source_candidates`.

Evidence records where the URL was found, method, content type, HTTP result,
feed parse result, sample count, adapter provenance, and editorial roles when
available.

A seed page may be unavailable while a bounded same-origin feed probe is
healthy. A successfully parsed RSS or Atom probe is sufficient to keep the
discovery report and candidate even when every configured entry-page fetch
fails. The failed entry attempts remain recorded as evidence; discovery fails
only when neither an entry point nor a valid feed probe succeeds.

## Optional Web Adapter

Modes:

- `disabled`: configured-entry discovery only; default;
- `fallback`: use the adapter only when no configured entry point exists;
- `augment`: combine adapter suggestions with configured entry points.

The adapter can return several public properties for one company, including
corporate, engineering, research, product, and brand publications. This is
deliberate: company coverage and publication coverage are different goals.

The adapter returns public URL suggestions, not source records. The open-source
discovery worker re-fetches each suggestion and applies all normal safety,
size, parsing, classification, and evidence rules before persistence. It also
retains neutral adapter provenance. The default validation policy treats that
as informational; the opt-in `trusted_adapter` policy may use it to activate a
technically usable feed provisionally.

Provider prompts, provider-specific search behavior, credentials, throttling,
and raw responses remain outside this repository. The boundary is documented
in [Web discovery adapter](web-discovery-adapter.md).

## Candidate Boundary

Discovery writes:

```text
source_candidates.status = new
```

It never writes an approved source. RSS/Atom candidates proceed to the
independent validation worker. HTML candidates remain company-profile evidence
until a static HTML crawler exists.

Candidate lifecycle is governed downstream:

```text
new -> accepted -> approved source (strict, operator, or provisional)
new -> rejected
accepted -> rejected + disabled source
```

See [Source review and validation](source-review.md).

## Company Profile

`GET /api/v1/companies/{company_key}/profile` aggregates:

- canonical company identity and optional listings;
- configured public entry points;
- latest discovery run;
- current candidates and their totals; and
- approved sources and their totals.

This is the exploration surface for understanding a company's discovered blog,
newsroom, engineering, press, and feed footprint. The global operator workflow
uses `/review`.

## Operator Commands

```bash
feed-admin discover --company "Gusto"
feed-admin discover --company nvidia
feed-admin discover
feed-admin candidates list --company "Gusto" --status new
```

`--company` accepts a company key or exact canonical name. It does not accept a
ticker.

Commands enqueue the same durable job type claimed by
`feed-discovery-worker`.

Discovery uses the same identifiable public-fetch user agent as feed and
article crawling. Set `PUBLIC_FETCH_USER_AGENT` to include a monitored
deployment contact. Candidate evidence and validation remain the trust
boundary.

## Periodic Scheduling

Companies must satisfy both gates:

- `discovery_enabled=true`;
- `discovery_not_before <= now`.

Broad imports stage new companies. `feed-admin companies activate` releases a
bounded wave and can space its timestamps. The discovery producer then:

- evaluates per-company cadence and latest run;
- counts global pending/running discovery work;
- serializes refill with an advisory lock; and
- fills only free slots below `DISCOVERY_QUEUE_TARGET`.

Set `SCHEDULE_JOBS=false` to process explicitly queued discovery without
recurring refill.

If shutdown or lease loss interrupts discovery, its run is closed as
`cancelled` and the durable job is safely retried. A later attempt creates a new
run; company profiles do not retain permanently running rows.
