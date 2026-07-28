# Company Universe Import

Company Feed Server accepts broad organization lists through the
provider-neutral `company-universe.v2` JSON contract. The contract is
name-first and works for public, private, state-owned, and cooperative
companies.

```text
source-specific collection
  -> neutral JSON validation
  -> atomic staged import
  -> explicit discovery waves
```

The source-specific collector may remain private. This repository owns only the
portable document, validation, persistence, audit, and rollout behavior.

## Identity Model

Each row has three distinct identifiers:

- `source_company_id`: the source system's stable ID, scoped by source name
- `company_key`: a stable lowercase operational slug used by APIs and exports
- `name`: the canonical human name used for web discovery

`aliases` are also sent to discovery. A ticker is not company identity and is
never required. Public-market symbols, when known, belong in the optional
`listings` array. A private company normally has `listings: []`.

## Contract

A minimal private-company document is:

```json
{
  "schema_version": "company-universe.v2",
  "source": {
    "name": "example-directory",
    "revision": "2026-07-19",
    "generated_at": "2026-07-19T00:00:00Z",
    "metadata": {
      "selection_policy": "active operating companies"
    }
  },
  "companies": [
    {
      "source_company_id": "company-123",
      "company_key": "example-acme",
      "name": "Acme",
      "aliases": ["Acme Corporation"],
      "ownership_status": "private",
      "lifecycle_status": "active",
      "listings": [],
      "market_cap": null,
      "country": "United States",
      "ipo_year": null,
      "sector": "Technology",
      "industry": "Software",
      "identifiers": {},
      "homepage_url": "https://example.com/",
      "investor_relations_url": null,
      "newsroom_url": null,
      "blog_url": null,
      "hints": [],
      "metadata": {}
    }
  ]
}
```

If an organization is publicly listed, one optional listing looks like:

```json
{
  "ticker": "ACME",
  "exchange": "NASDAQ",
  "is_primary": true,
  "metadata": {}
}
```

The validator rejects unknown fields, duplicate source IDs or company keys,
malformed keys, duplicate listings, invalid HTTP(S) URLs, credentials embedded
in URLs, invalid metadata shapes, implausible IPO years, negative market caps,
more than 100,000 companies, and inputs larger than 64 MiB.

Source metadata should record enough information to reproduce the population:
input revision, row counts, selection rules, exclusions, and issuer-resolution
policy. Provider credentials, search prompts, raw responses, and private
implementation details do not belong in the document.

## Validate and Import

Validation is offline and does not require Postgres:

```bash
cargo run -p feed-admin -- \
  companies import --file ./company-universe.json --validate-only
```

Import requires `DATABASE_URL` and applies the complete document in one
transaction:

```bash
export DATABASE_URL=postgresql://company_feed:company_feed@localhost:55432/company_feed
cargo run -p feed-admin -- \
  companies import --file ./company-universe.json
```

The command returns the import-run UUID, exact input SHA-256, row counts, action
counts, cadence, and replay status.

New companies are staged by default:

- `discovery_enabled = false`
- no discovery job is created
- imported metadata and the complete source record remain auditable

Existing curated names, public URLs, and hints take precedence over incomplete
import data. A later universe revision refreshes universe-owned names and
metadata without erasing operator- or seed-managed values.

An exact replay for the same source name and input bytes returns the original
run summary and performs no writes. `--activate-new` exists for small trusted
inputs, but broad imports should use the staged default.

## Release Discovery Waves

Activate a bounded wave from one import:

```bash
cargo run -p feed-admin -- companies activate \
  --import-run-id <IMPORT_RUN_UUID> \
  --limit 4 \
  --spacing-seconds 300
```

`--start-at <RFC3339>` can delay the first company. The activation limit is
between 1 and 1,000. Rows are ordered by imported market cap when present, then
company key. This remains deterministic for private-company directories where
market cap is absent.

The recurring producer also enforces the global `DISCOVERY_QUEUE_TARGET`
(default `100`). It serializes queue refills in Postgres and inserts only enough
jobs to reach that target, preventing a synchronized multi-thousand-company
burst.

A conservative rollout is:

1. activate 4 companies and inspect adapter behavior, candidate provenance,
   fetch validation, and false positives;
2. activate 100 and measure official-site/feed coverage;
3. activate 500 and check queue latency, retries, and review load;
4. continue in bounded waves only after the prior wave meets its criteria.

Keep the universe staged when the discovery adapter is disabled and companies
have no known public URLs. Importing data and spending external search capacity
are separate decisions.

## Audit Tables

`company_import_runs` stores one summary per source name and exact input hash.
`company_import_rows` stores every source ID, company key, company name, source
record, action, and resulting company ID. `company_external_ids` preserves the
stable mapping from `(source_name, source_company_id)` to the canonical company.

`companies.metadata.universe` stores neutral source revision, classification,
market cap, geography, industry, source identifiers, and source metadata.
Optional exchange listings are normalized separately in `company_listings`.
