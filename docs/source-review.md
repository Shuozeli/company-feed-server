# Source Review and Validation

Discovery and validation are separate components with separate job types,
workers, audit records, and failure domains.

Discovery answers “what public URL might be useful?” Validation answers “does
this URL currently provide usable RSS/Atom content, what trust signals apply,
and does the configured activation policy admit it?”

## Runtime Separation

`feed-discovery-worker` is the only runtime that claims `discover_company`.
It may use configured official URLs and, when enabled, the provider-neutral web
adapter.

`feed-validation-worker` is the only runtime that claims
`validate_candidate`. It has no search-provider or AI dependency. It consumes
persisted candidates, fetches public RSS/Atom URLs, computes deterministic
signals, applies an explicit activation policy, and records evidence.

`feed-worker` claims `crawl_source` and `export_target`. It never claims
discovery or validation jobs.

`feed-server` serves the API and review dashboard only.

## Durable State

Each attempt creates a `candidate_validation_runs` row containing:

- candidate and durable job IDs;
- start and finish timestamps;
- terminal status;
- detected RSS/Atom kind and final URL;
- HTTP status when available;
- total and titled item counts;
- latest item timestamp;
- machine-readable policy reasons;
- safe sample item metadata; and
- failure details.

Every automatic or human outcome creates a `candidate_decisions` row:

- `activated`, `rejected`, or `kept_for_review`;
- `automatic` or `operator`;
- actor and non-empty reason;
- associated source ID when applicable; and
- policy/run metadata.

Repeated validation attempts remain separate rows. The UI and dashboard use the
latest attempt while preserving all historical evidence.

## Validation Signals

Every attempt computes the same technical, ownership, editorial, freshness,
locale, and scope signals. Policy decides which signals are required for
activation; evidence collection does not change between policies.

### Technical validity

- RSS or Atom parsing succeeds.
- Fetch timeout, response bytes, redirects, and item count stay within the
  crawler limits.
- At least one item and one titled item are present.
- At least one titled item is not a short, content-proven CMS starter post such
  as the stock WordPress “Hello world!” / “Olá, mundo!” article.
- A sample of at least five items must contain at least two distinct normalized
  titles. This conservative diversity gate rejects syntactically valid feeds
  that expose one framework or test title for every URL.
- A strong source marker (`boardmessages`, `feed/topics`, or trust-alert feed)
  or an 80%-of-20 sample dominated by documentation, forum, or comment URLs
  rejects the feed as non-editorial. Press-release documents and legitimate
  community-hosted blogs are explicitly preserved.
- A feed on a known shared press-wire or market-news host, including global
  Simply Wall St news feeds, must identify the
  candidate company by name or alias in at least half of its sampled item
  titles or URLs. This rejects provider-wide feeds discovered from a
  company-specific page while preserving genuinely company-scoped third-party
  feeds; any minority noise is removed again at crawl time.
- On a non-shared host whose brand domain does not lexically match the legal
  issuer name, an exact distinctive company or alias phrase in the parsed feed
  title may corroborate dedicated publication scope. One generic word is not
  sufficient, and feed-title evidence never bypasses the sampled-item majority
  requirement on a known shared host.
- A candidate with at least three distinct item identities is redundant when
  every identity is already present in approved RSS/Atom sources for the same
  company. Validation rejects that alias or strict subset while preserving
  feeds that add even one distinct sampled item, including separate product,
  engineering, regional, and topical publications.
- An exact RSS/Atom URL already approved for a different name-first issuer
  cannot be activated again. Security classes whose stripped issuer identity
  is equal are allowed, so one issuer's Class A/Class C records can share the
  same official feed while unrelated companies and subsidiaries cannot
  silently inherit it.
- A candidate is also rejected when at least half of its canonical sample
  article identities are already claimed by an approved feed for a distinct
  name-first issuer. This catches an apparently official endpoint that has
  been repointed to an acquirer or another CMS tenant even when the old feed
  URL still uses the requested company's domain.
- When most entries from an otherwise related feed escape to article hosts
  unrelated to the requested company, the per-article company-scope majority
  becomes mandatory unless the parsed feed title independently identifies the
  company.

Historical HTML provenance is reversibly quarantined when it used generic
detail-page chrome such as `News Release Details`. An obsolete HTML source may
also be disabled when every public item repeats one sitewide headline, it has
no active recipe, and a healthy active recipe already supplies corrected
titles for at least 80% of the same canonical URLs. Investor-navigation items
titled `Why Invest` and headings contaminated by embedded SVG/CSS rules are
also reversibly quarantined; the generic crawler rejects both patterns on
future runs. The same reversible policy covers static navigation rows such as
governance and management pages, archives, media/contact resources, investor
documents, presentation indexes, and listing filter labels.

### Official ownership

At least one conservative proof is required:

- candidate and final hosts are related to a configured homepage, newsroom,
  blog, IR URL, or hint;
- an official configured page supplied the RSS/Atom alternate or explicitly
  labeled subscription link;
- a company-named public page supplied the alternate link; or
- the canonical company name or alias conservatively matches both the
  candidate and final host.

Legal and security-name suffixes such as “Inc.”, “Class A Common Stock”, and
“Ordinary Shares” are ignored for hostname matching. Generic redirects to an
unrelated host do not inherit a company-name match.

Cross-domain services such as Medium and FeedBurner remain reviewable unless an
official page supplied the feed link.

### Editorial scope

Evidence must indicate a company blog, engineering publication, newsroom,
press feed, research/security lab, or similar editorial channel. Community,
support, status, alerts, comments, careers, jobs, notifications, and generic
resource feeds are held for review even when the domain is official.
Strong item-level evidence that the feed is actually documentation, forum
traffic, comment replies, or operational alerts is invalid rather than merely
reviewable.

### Freshness and locale

A dated feed must have a latest item within
`VALIDATION_MAX_ITEM_AGE_DAYS` (default `730`). Undated feeds may pass when the
other signals are strong.

Default-locale and English paths may auto-activate. Recognized non-English
locale variants are held for review so one discovery wave does not activate
many translated copies of the same newsroom.

## Activation Policies

`VALIDATION_ACTIVATION_POLICY` accepts:

- `strict` — the default. Every technical, ownership, editorial, freshness,
  locale, and safe-scope requirement above must pass.
- `trusted_adapter` — an adapter-backed candidate may activate when RSS/Atom
  parsing succeeds, the feed has at least one item and one titled item, and any
  sample of five or more items passes the title-diversity gate. Strong
  non-editorial item-scope evidence still blocks activation.

The second mode is intentionally recall-first. Missing ownership/editorial
proof, weaker risky-scope markers, locale, and freshness failures are still
persisted as evidence, but do not block activation when neutral
`external_web_adapter` provenance is present. A source admitted this way is
marked `provisional=true` when it did not also pass strict policy. Sitemap and
strong item-level non-editorial evidence remain hard stops, as does a shared
multi-company feed whose sample is not majority scoped to the requested
company.

The external adapter still cannot write a source. It proposes a public URL;
the open-source discovery worker fetches and classifies it, and the independent
validation worker proves that it is a usable feed before activation.

Empty feeds, feeds without usable titled items, placeholder-only feeds,
degenerate repeated-title feeds, and unsupported payloads are automatically
rejected in both modes. Transient network and upstream failures remain
retryable and auditable.

## Bounded Company-Coverage Waves

Automatic queue refills and `feed-admin candidates validate --limit N` share
one transaction and advisory lock. The selector:

1. considers only new, unvalidated RSS/Atom candidates;
2. excludes companies that already have an approved source by default;
3. ranks candidates within each company by confidence and stable creation
   order;
4. selects at most one candidate per company; and
5. fills only the available bounded queue slots.

This makes `N` a company-coverage wave rather than a request that can be
consumed by dozens of variants from a single company.
`feed-admin candidates validate --include-covered` retains the one-per-company
bound while allowing already-covered companies to gain product, brand,
engineering, or research publications. Operators can also validate any
specific additional candidate directly.

Keep `VALIDATION_SCHEDULE_JOBS=false` for manually released waves. Set it to
`true` only when continuous bounded refill is intended.

## Activation and Removal Semantics

No human approval is required for a candidate that passes the configured
policy. It is activated with `public_export_allowed=false` by default and
receives an initial crawl job.

With `strict`, technically usable candidates that miss a required trust signal
remain `needs_review`. With `trusted_adapter`, adapter-backed usable feeds
activate provisionally instead; there is no ownership-review queue to clear by
hand. Non-adapter candidates continue to use strict behavior.

Public export is a separate approval. Set it per operator decision or explicitly
enable `VALIDATION_PUBLIC_EXPORT`; the safe default is `false`.

Rejecting a new candidate prevents activation. Rejecting an accepted candidate
also:

- disables its source;
- clears public-export permission;
- cancels pending crawl jobs;
- excludes its items from normal REST and export queries; and
- records an operator rejection linked to the disabled source.

Historical crawl, item, validation, and decision rows remain available for
audit.

The activated-source table shows each source basis as `verified`, `operator`,
or `AI-assisted / provisional`. Its `Wrong / disable` control invokes the same
audited rejection path, making recall-first mistakes cheap to reverse.

## Review Dashboard

`GET /review` provides:

- total company and feed-candidate coverage;
- companies with activated and healthy sources;
- validation pending/running/status counts;
- reviewable candidates with technical and policy evidence;
- batch validate, activate, and reject controls;
- actor, reason, and public-export inputs; and
- activated-source health, stored items, latest article, failures, and disable
  controls, including strict/operator/provisional activation basis.

The backing JSON routes are:

```text
GET  /api/v1/review/dashboard
GET  /api/v1/review/candidates
GET  /api/v1/review/sources
GET  /api/v1/candidate-validation-runs
GET  /api/v1/candidate-decisions
POST /api/v1/source-candidates/{candidate_id}/validate
POST /api/v1/source-candidates/{candidate_id}/activate
POST /api/v1/source-candidates/{candidate_id}/reject
POST /api/v1/source-candidates/batch
```

Batch requests are limited to 100 candidate IDs. Actor is always required;
activation and rejection also require a reason. The response reports success or
failure per candidate instead of hiding partial outcomes.

The application does not implement operator authentication. Deploy these write
routes only behind a trusted network or an authenticated reverse proxy/API
gateway.

## Operator Examples

Queue a bounded wave:

```bash
feed-admin candidates validate --limit 500

feed-admin candidates validate --limit 500 --include-covered
```

After a validation-policy upgrade, reconsider only candidates whose latest
rejection was automatic, whose latest usable adapter-backed feed failed the
company-scope gate, and whose feed title is available for the current policy:

```bash
feed-admin candidates reconsider-automatic-scope --limit 500
```

The command reopens at most one candidate per company, records a
`source_candidate.reopened_for_validation` event, and enqueues normal durable
validation. It never reopens a candidate with an operator rejection. The
current validator still rejects shared/global, non-editorial, redundant,
empty, or company-mismatched feeds.

Run a validation worker without automatic refill:

```bash
RUN_JOBS=true \
SCHEDULE_JOBS=false \
VALIDATION_ACTIVATION_POLICY=trusted_adapter \
cargo run -p feed-validation-worker
```

Review the queue and sources:

```bash
curl --fail http://localhost:8080/api/v1/review/dashboard
curl --fail 'http://localhost:8080/api/v1/review/candidates?status=new&limit=100'
curl --fail 'http://localhost:8080/api/v1/review/sources?limit=100'
```
