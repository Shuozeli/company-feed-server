# Company Feed Server

[![CI](https://github.com/Shuozeli/company-feed-server/actions/workflows/ci.yml/badge.svg)](https://github.com/Shuozeli/company-feed-server/actions/workflows/ci.yml)
[![Security audit](https://github.com/Shuozeli/company-feed-server/actions/workflows/security-audit.yml/badge.svg)](https://github.com/Shuozeli/company-feed-server/actions/workflows/security-audit.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Company Feed Server is an open-source, company-first news aggregation stack. It
finds and validates company blogs, newsrooms, press feeds, engineering
publications, and product publications; crawls RSS, Atom, and public HTML;
hydrates article content; records review evidence; and publishes deterministic
static indexes.

**Explore the public reader:** [Company News](https://shuozeli.github.io/company-news-ui/)

[![Company News reader showing the category, company, and article tree](https://raw.githubusercontent.com/Shuozeli/company-news-ui/main/public/og-card.png)](https://shuozeli.github.io/company-news-ui/)

The project is split into three independently usable repositories:

| Repository | Role |
|---|---|
| [`company-feed-server`](https://github.com/Shuozeli/company-feed-server) | Rust/Postgres discovery, validation, crawling, review, and export engine |
| [`company-news-data`](https://github.com/Shuozeli/company-news-data) | Generated static archive, lazy indexes, schemas, and provenance |
| [`company-news-ui`](https://github.com/Shuozeli/company-news-ui) | Vite/React static reader that loads the archive through `index.json` |

The system is name-first. Public and private companies use the same company
record; stock listings are optional metadata and are never used as company
identity or web-search input.

> Status: pre-release software on `main`. Build locally from source; no tagged
> container release is available yet. The deployment is self-hosted and
> intended for trusted operators, not as a hosted multi-tenant service. See
> [Launch readiness](docs/launch-readiness.md) for the current preflight.

Only public web content is in scope. The server does not use logged-in browser
profiles, bypass paywalls, or require private provider code.

## What Is Delivered

- an auditable `company-universe.v2` importer with staged activation waves
- deterministic public-web discovery from known company URLs
- an optional provider-neutral HTTP boundary for external URL suggestions
- durable, evidence-backed RSS, Atom, and HTML candidates
- a separate RSS/Atom validation worker with strict and trusted-adapter policies
- audited automatic and operator candidate decisions
- a responsive review, coverage, and source-health dashboard
- a self-contained live crawled-news dashboard served by the API
- provisional-source provenance and one-click wrong-source disable
- bounded batch validation, activation, rejection, and accepted-source disable
- scheduled RSS/Atom and validated HTML-recipe crawling, raw replay records,
  and normalized articles
- a separate, self-throttled article-content crawler with durable attempts,
  retries, freshness refresh, and coverage reporting
- explicit named-company and resumable all-company recipe construction through
  a URL-only private-adapter boundary
- versioned recipes with crawl/content freshness, correctness gates, structure
  drift audit, automatic stale retirement, and coverage metrics
- generic public listing/article fetching, quality gates, audit, and normalization
- paginated REST APIs for companies, profiles, candidates, sources, items,
  health, decisions, and execution history
- deterministic Markdown, JSON, and JSONL Git archives
- independently scalable API, discovery, validation, and crawl/export runtimes
- a single declarative PostgreSQL schema reconciled on demand with generated
  schema diffs
- an independently scalable manual import runtime that claims no feed jobs
- lease-fenced Postgres jobs with retry, heartbeat, recovery, and queue bounds

RSS/Atom remains the preferred recurring ingestion path. An operator can
explicitly bootstrap one company or materialize a resumable all-company recipe
campaign. Generic HTTP listing execution is delivered; browser rendering
remains an optional adapter contract.

## Pipeline and Runtime Ownership

```text
company registry
      |
      v
feed-discovery-worker  ---- optional neutral web adapter
      |
      v
untrusted source_candidates
      |
      v
feed-validation-worker
      |                       \
      | usable feed + policy   \ empty / invalid
      v                         v
approved source             automatic rejection
  | strict or provisional
      |
      v
feed-worker (crawl + normalize + export)
      |
      +--> discovered article URLs / feed payloads
      |
      v
feed-content-worker (independent article-page fetch + hydration)
      |
      +--> REST API
      +--> Git archive

companies without healthy approved RSS/Atom
      |
      v
feed-news-extraction-worker ---- neutral URL-only adapter
      |
      +--> independently fetch public article pages
      +--> generic article quality gate
      +--> raw replay records + normalized items
```

Each runtime claims only its own durable job types:

| Binary | Responsibility | Default port |
|---|---|---:|
| `feed-server` | REST API and review dashboard only | 8080 |
| `feed-discovery-worker` | `discover_company` only | 8082 |
| `feed-validation-worker` | `validate_candidate` only | 8083 |
| `feed-worker` | `crawl_source` and `export_target` only | 8081 |
| `feed-news-extraction-worker` | `extract_company_news` only | 8084 |
| `feed-content-worker` | `crawl_content` only | 8085 |
| `feed-admin` | bounded operator commands that use the same DB contracts | n/a |

Discovery is therefore not embedded in the API server and cannot consume API
capacity. Set `SCHEDULE_JOBS=false` on a worker to run only explicitly queued,
bounded waves.

## Quick Start

Docker and Docker Compose are sufficient to start the API. Export the sample
environment into the current shell as well so later `cargo run` operator
commands use the same database; those host-side commands additionally require
the Rust toolchain.

```bash
cp .env.example .env
set -a
source .env
set +a
docker compose up --build -d postgres server
curl --fail http://localhost:8080/ready
```

The sample database credentials are for loopback-only local development.
Replace them and add authenticated network controls before any non-local
deployment.

Start validation plus crawl/export processing:

```bash
docker compose --profile validation --profile workers up --build -d
```

Start independent article-page hydration and inspect its durable coverage:

```bash
docker compose --profile content-crawl up --build -d content-worker
cargo run -p feed-admin -- content-crawl status
```

Discovery/source crawling and content crawling are deliberately separate.
Source crawling finds article identities and keeps feeds current; the content
worker independently fetches every eligible public article page regardless of
source type or pre-existing body, replaces source observations with sanitized
HTML/Markdown/text on success, records failures, and refreshes successful
content after the configured interval.

Start discovery only when its inputs and optional adapter are configured:

```bash
docker compose --profile discovery up --build -d discovery-worker
```

Start the idle news-import worker and trigger exactly one company after
configuring its neutral adapter:

```bash
docker compose --profile news-extraction up --build -d news-extraction-worker
cargo run -p feed-admin -- news-import --company "Acme"
```

If port `8080` is occupied, set `SERVER_PORT=18080` and use port `18080` in
requests. Runtime settings are documented in [`.env.example`](.env.example).

The two built-in dashboards are available at:

```text
http://localhost:8080/review
http://localhost:8080/news
```

Operator write routes are intentionally not an authentication system. Put the
review surface behind your normal private network, reverse-proxy authentication,
or API gateway before exposing it outside a trusted operator environment.

Public fetching uses an identifiable default user agent. Set
`PUBLIC_FETCH_USER_AGENT` to an identity containing a monitored contact URL or
email for your deployment. See [Responsible use](docs/responsible-use.md).

## Company Identity and Broad Import

The canonical fields are `name`, `aliases`, and a stable `company_key`.
Discovery requests use names and aliases. `company_key` is an operational slug
for URLs and archives.

Public companies may have zero or more `company_listings`; private companies
normally have none. No workflow requires a ticker.

Validate a neutral universe offline:

```bash
cargo run -p feed-admin -- companies import \
  --file ./company-universe.json --validate-only
```

Import it atomically with new companies staged:

```bash
export DATABASE_URL=postgresql://company_feed:company_feed@localhost:55432/company_feed
cargo run -p feed-admin -- companies import \
  --file ./company-universe.json
```

Release an explicit discovery wave:

```bash
cargo run -p feed-admin -- companies activate \
  --import-run-id <IMPORT_RUN_UUID> \
  --limit 500 \
  --spacing-seconds 30
```

See [Company universe import](docs/company-universe-import.md) for the versioned
contract, merge rules, and audit model.

## Web Discovery Adapter

The public repository contains a provider-neutral
`company-web-discovery.v2` request/response contract and an HTTP client. It does
not contain provider-specific search implementations, prompts, provider
credentials, raw provider responses, or provider-specific retry logic.

Adapter suggestions are public URL seeds. The discovery worker still performs
URL safety checks, fetches the page itself, classifies it, and writes evidence.
The adapter cannot write an approved source. An operator may, however,
explicitly configure validation to trust adapter provenance after the
open-source worker proves that a feed parses and contains usable titled items.

See [Web discovery adapter](docs/web-discovery-adapter.md).

## Company News Recipe Build

The `company-news-extraction.v2` adapter contract returns public publication
entry points and optional evidence article URLs only. The open-source worker
independently fetches every URL, derives a bounded `company-news-recipe.v1`, and
activates it only after a nonempty correctness-passing crawl.

The dedicated builder worker has no automatic producer. It defaults to one
company job, while `NEWS_EXTRACTION_JOB_CONCURRENCY` can pipeline a small
number of jobs so one company's public-page validation overlaps the next
company's provider lookup. Postgres serializes claims and enforces that global
job limit; a private adapter may apply stricter provider throttling. An
operator may select one company or explicitly queue a
bounded resumable `--all` campaign. Companies with a healthy approved feed
require `--include-covered` for intentional product or
engineering-publication expansion. A feed whose runtime crawl is failing or
repeatedly empty does not suppress fallback recipe
construction. The default gap build directly revalidates known stale,
rebuild-required, and content-stale publication URLs. `--include-covered`
additionally revalidates healthy active publications for intentional expansion,
allowing a passing profile-aware rebuild to replace an older immutable recipe
version safely. See
[Company news crawl recipes](docs/company-news-recipes.md) and
[Manual company news bootstrap](docs/manual-company-news-import.md).

```bash
cargo run -p feed-admin -- news-import --company "Stripe" --include-covered

cargo run -p feed-admin -- news-import \
  --all --include-covered --limit 10000 --spacing-seconds 1

# After that wave is terminal, retry only companies whose latest attempt since
# the campaign start was blocked by transient HTTP, DNS, or rate-limit errors.
cargo run -p feed-admin -- news-import \
  --retry-transient-after 2026-07-24T12:41:32Z \
  --include-covered --limit 10000 --spacing-seconds 1

# Immediately verify one already-active HTML recipe through its source.
cargo run -p feed-admin -- crawl --source-id <HTML_SOURCE_UUID>

# Reconcile historical exact-URL associations and fail if any distinct-issuer
# item remains unscoped.
cargo run -p feed-admin -- news-ownership-audit \
  --apply --fail-on-unscoped
```

## Validation and Approval

Queue one candidate or a bounded wave:

```bash
cargo run -p feed-admin -- candidates validate \
  --candidate-id <CANDIDATE_UUID>

cargo run -p feed-admin -- candidates validate --limit 500

cargo run -p feed-admin -- candidates validate \
  --limit 500 \
  --include-covered
```

The default bulk selector chooses at most one highest-confidence unvalidated
feed per uncovered company. `--include-covered` keeps the one-per-company wave
bound but includes companies that already have a source, enabling Microsoft-
style product, engineering, research, and brand publication expansion.

`VALIDATION_ACTIVATION_POLICY` has two explicit modes:

- `strict` is the open-source default. Automatic activation requires official
  ownership, editorial scope, freshness, preferred locale, safe scope, and
  usable feed-content evidence.
- `trusted_adapter` is recall-first. A candidate carrying neutral web-adapter
  provenance activates when it parses and has at least one titled item. Failed
  strict signals remain recorded and the source is marked provisional.

Empty feeds and non-feed responses are automatically rejected in both modes.
Transient network failures remain retryable and auditable. Automatic activation
sets the source-level `public_export_allowed` flag only when
`VALIDATION_PUBLIC_EXPORT=true`, which defaults to `false`. A target configured
with the broader `approved_public` scope selects approved sources regardless of
that source flag.

In `trusted_adapter` mode, ambiguous ownership, cross-domain hosting, staleness,
locale, and scope do not create a manual-review obligation. The dashboard labels
sources that did not also pass strict policy as `AI-assisted / provisional`.
Operators remove a bad association with `Wrong / disable`. Public Git export
remains a separate operator-controlled publication setting. That setting
selects records; it does not determine or grant rights in publisher material.

Rejecting an already accepted candidate disables its source, cancels pending
crawls, removes it from public item/API results, and records the source-bearing
operator decision.

See [Source review and validation](docs/source-review.md).

## REST API

All collection endpoints accept bounded `limit` and `offset` values. Applicable
endpoints also accept entity and status filters.

```text
GET  /health
GET  /ready
GET  /review
GET  /api/v1/health
GET  /api/v1/companies
GET  /api/v1/companies/{company_key}
GET  /api/v1/companies/{company_key}/profile
GET  /api/v1/source-candidates
GET  /api/v1/review/dashboard
GET  /api/v1/review/candidates
GET  /api/v1/review/sources
GET  /api/v1/candidate-validation-runs
GET  /api/v1/candidate-decisions
POST /api/v1/source-candidates/{candidate_id}/validate
POST /api/v1/source-candidates/{candidate_id}/activate
POST /api/v1/source-candidates/{candidate_id}/reject
POST /api/v1/source-candidates/batch
GET  /api/v1/sources
GET  /api/v1/feed-items
GET  /api/v1/feed-items/{item_id}
GET  /api/v1/news-items
GET  /api/v1/source-health
GET  /api/v1/discovery-runs
GET  /api/v1/company-news-extraction-runs
GET  /api/v1/company-news-recipes
GET  /api/v1/company-news-recipe-runs
GET  /api/v1/company-news-recipe-coverage
GET  /api/v1/crawl-runs
GET  /api/v1/export-targets
GET  /api/v1/export-runs
```

The company-profile endpoint combines the name-first company record, latest
discovery run, current candidates, and approved sources. The review dashboard
adds company coverage, validation queues, review load, activated-source health,
stored-item counts, latest publication dates, and audited batch controls.
`/api/v1/news-items` adds company/source labels, text and source-kind filters,
and canonical-URL deduplication for the live news dashboard. When the same
company article arrived through multiple approved sources, the projection
prefers RSS, then Atom, then HTML/browser while retaining every raw row for
provenance. Published timestamps determine recency; an undated item falls back
to its stable first-seen timestamp so a routine recrawl cannot resurface old
evergreen content as new.

## Git Archive

The default exporter writes an owned archive tree:

```text
index.json
HEAD.json
README.md
articles/v1/<company-hash>/<company-key>/company.json
articles/v1/<company-hash>/<company-key>/index/pages/<page>.json
articles/v1/<company-hash>/<company-key>/<YYYY>/<MM>/<document-hash>/<document-id>/article.md
articles/v1/<company-hash>/<company-key>/<YYYY>/<MM>/<document-hash>/<document-id>/record.json
index/v1/current/manifest.json
index/v1/current/recent/manifest.json
index/v1/current/recent/pages/<page>.json
index/v1/current/companies/manifest.json
index/v1/current/companies/buckets/<letter>.json
index/v1/current/categories/manifest.json
index/v1/current/categories/<category-key>/pages/<page>.json
index/v1/current/partitions/<YYYY>/<MM>/manifest.json
index/v1/current/partitions/<YYYY>/<MM>/shards/<hash-prefix>.jsonl
schemas/v1/
openapi/openapi.json
```

The maintained output is published in
[company-news-data](https://github.com/Shuozeli/company-news-data).

The JSONL index is partitioned by archival month and adaptively split as a
SHA-256 prefix trie. Manifests publish record/byte counts and content hashes;
`HEAD.json` provides a deterministic generation checkpoint. A small
`index.json` points browsers to bounded recent, company-summary, and
category-directory pages, so interactive clients never load full-text shards.
Categories derive from the imported universe sector and publish a separate
taxonomy generation in `index.json` and bounded category pages for cache
invalidation. The bootstrap contract is `1.1.0`; `HEAD.json`, article schemas,
and article identity remain `1.0.0`. Article identity uses company name-first
keys and canonical URLs, never a stock ticker.

`push_enabled` is `false` in the sample config. Under the default source-flag
selection scope, a source must also have `public_export_allowed=true`;
validation leaves that flag `false` unless an operator changes it. An export
target may instead explicitly set
`metadata.publication_scope=approved_public`, which selects non-private items
from all approved, currently valid sources. The checked-in
[`configs/export_targets.yaml`](configs/export_targets.yaml) sample uses that
broader selection scope while keeping Git push disabled.

Selection for export is an operator publication decision, not a statement that
the project or operator owns, licenses, or has permission to redistribute the
selected publisher material. Operators must review applicable terms and rights
before publishing an archive. See [Data and content
policy](docs/data-and-content-policy.md).

## Native Development and Verification

```bash
docker compose up -d postgres
cp .env.example .env
set -a
source .env
set +a

cargo run -p feed-server
```

Run the full checks:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
# TEST_DATABASE_URL must point to a disposable database.
TEST_DATABASE_URL="$DATABASE_URL" \
  cargo test --workspace --all-features --all-targets -- --test-threads=1
```

## Optional Tailscale Dashboard Serving

The API already serves the news dashboard at `/news`; no additional server is
required. Maintainers who use `serve-lib` can additionally expose the static
dashboard through a private Tailscale address:

```bash
tailscale ip -4
```

Put the reported address in `.env`:

```dotenv
COMPOSE_FILE=docker-compose.yml:docker-compose.tailscale.yml
TAILSCALE_BIND_IP=<tailscale-ipv4>
```

Then recreate the API listener and register the dashboard:

```bash
TAILSCALE_IP="$(tailscale ip -4)"
docker compose up -d --force-recreate server
serve-lib daemon status
serve-lib register ./docs \
  --route /company-feed-docs \
  --port 18190 \
  --bind "$TAILSCALE_IP" \
  --name company-feed-docs
```

The base Compose file publishes Postgres and every service health port on
loopback only. The Tailscale override adds a second API listener on the
specified private Tailscale address; it does not expose worker or database
ports. The `serve-lib` registration deliberately omits `--timeout`, so it
remains active until it is explicitly deregistered or the daemon stops. The
dashboard reads live data from the API and does not copy database results into
the served directory.
Confirm both surfaces after registration:

```bash
curl --fail "http://${TAILSCALE_IP}:18190/company-feed-docs/news-viewer.html"
curl --fail "http://${TAILSCALE_IP}:18080/api/v1/news-items?limit=1"
```

## Documentation

- [Architecture](docs/architecture.md)
- [Components](docs/components.md)
- [Database](docs/database.md)
- [Discovery](docs/discovery.md)
- [Source review and validation](docs/source-review.md)
- [Company universe import](docs/company-universe-import.md)
- [Web discovery adapter](docs/web-discovery-adapter.md)
- [Company news crawl recipes](docs/company-news-recipes.md)
- [Manual company news bootstrap](docs/manual-company-news-import.md)
- [Crawled news dashboard](docs/news-viewer.html)
- [Crawling](docs/crawling.md)
- [Content processing](docs/content-processing.md)
- [Normalization](docs/normalization.md)
- [Exporters](docs/exporters.md)
- [Responsible use](docs/responsible-use.md)
- [Data and content policy](docs/data-and-content-policy.md)
- [Launch readiness](docs/launch-readiness.md)
- [Show HN preflight worksheet](docs/show-hn-preflight.md)
- [Roadmap](docs/roadmap.md)
- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)
- [Release guide](RELEASE.md)

## License

The source code and project-authored documentation are available under the
[MIT License](LICENSE). Third-party content and trademarks are not licensed by
this repository; see [Data and content policy](docs/data-and-content-policy.md).
