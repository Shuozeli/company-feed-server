# Database Design

Postgres is the only runtime database. [`schema/postgres.sql`](../schema/postgres.sql)
is the single structural source of truth. A blank database may initialize
directly from that file; existing databases are reconciled only through the
generated `pg-schema-diff` plan exposed by `scripts/schema-plan.sh` and
`scripts/schema-apply.sh`. Runtime startup verifies the required schema and
does not execute a sequential migration history.

## Table Catalog

### Company registry and import

- `companies`: name-first canonical registry, aliases, ownership/lifecycle,
  optional public entry points, discovery activation, and cadence;
- `company_listings`: optional zero-to-many public-market listings;
- `company_external_ids`: source-scoped identifiers;
- `company_import_runs`: immutable universe input hash and summary;
- `company_import_rows`: complete per-row import audit.

`companies` has no ticker identity. Names and aliases drive discovery.
Evidence-backed corporate renames update the canonical name, retain the former
name as an alias, and preserve former/current listings as historical metadata
without changing the stable `company_key`.

Universe imports use one transaction and an advisory lock. Exact bytes are
hashed and `(source_name, input_sha256)` is unique. Existing curated URLs are
not overwritten by empty import fields. New broad-import companies are staged
until an explicit activation wave assigns their discovery release time.

### Candidate governance

- `source_candidates`: untrusted discovery URL, kind, confidence, evidence,
  state, and optional accepted-source link;
- `candidate_validation_runs`: every technical/policy validation attempt;
- `candidate_decisions`: append-only automatic/operator decision audit;
- `sources`: approved or disabled production sources only;
- `source_state`: crawl freshness, success, failure, backoff, and zero-run
  health.

### Company news recipes

- `company_news_recipes`: immutable versioned recipe specs and active, stale,
  superseded, or disabled lifecycle;
- `company_news_recipe_state`: last correct/nonempty run, crawl/content
  freshness, correctness streaks, structure fingerprint, and rebuild flag;
- `company_news_recipe_runs`: per-execution counts, acceptance ratio,
  normalized/new counts, fingerprint, reasons, error, and publish audit.

At most one recipe version is active per source. Activation supersedes the
prior version, updates the source cadence from the recipe, and records the
independent validation crawl. Incorrect scheduled runs publish no items. A
threshold breach makes the recipe stale while retaining every version and run.

Candidate lifecycle:

```text
new --validation pass--> accepted + approved source
new --ambiguous-------> new + kept_for_review decision
new --operator--------> rejected
accepted --operator---> rejected + disabled source
```

Activation creates the source, source state, candidate link, event, and
decision in one transaction. Rejecting an accepted candidate disables its
source, revokes public export, cancels pending crawl work, clears the active
candidate link, and stores a source-bearing rejection decision.

Historical validation, crawl, item, and decision rows are retained. Public
item and export queries require an approved source. RSS/Atom sources are
eligible directly; HTML/browser sources additionally require an active recipe
whose state does not require a rebuild. This read-time gate immediately hides
items from stale, superseded, disabled, or drifted recipes without deleting
their audit history.

### Durable jobs and execution history

- `jobs`: logical work, scheduling, attempts, and fenced lease state;
- `discovery_runs`: discovery attempts;
- `candidate_validation_runs`: validation attempts;
- `crawl_runs`: source crawl attempts;
- `content_crawl_state`: current per-item attempt, retry, freshness, extraction
  version, and content-size state;
- `content_crawl_attempts`: append-only article-page fetch outcomes and
  diagnostics;
- `company_news_extraction_runs`: operator-selected import windows, outcome counts,
  sanitized failure, and adapter/content audit metadata;
- `export_runs`: materialization, commit, and push attempts;
- `event_log`: structured operational events.

Supported job types:

```text
discover_company
validate_candidate
crawl_source
crawl_content
extract_company_news
export_target
normalize_backfill
```

Run rows reference jobs with `ON DELETE SET NULL`, so execution history remains
useful if old job rows are pruned.

### Content and export

- `raw_crawl_items`: replayable crawler output and processing state;
- `feed_items`: stable normalized public content;
- `content_crawl_state`: the current hydration/freshness projection;
- `content_crawl_attempts`: immutable content-fetch audit attempts;
- `export_targets`: repository, layout, cadence, enablement, and push policy;
- `exported_items`: per-target content hash, path, commit, and timestamp.

Raw output is persisted before normalization. A crash or normalization repair
does not require refetching the public source.

## Durable Job Contract

Lifecycle:

```text
pending -> running -> completed
                   -> pending      retryable failure
                   -> failed       permanent/exhausted failure
pending            -> cancelled    operator/source-disable action
```

Each caller supplies `job_key`. A partial unique index on
`(job_type, job_key)` permits only one active pending/running occurrence while
retaining completed and failed history.

Claiming uses `FOR UPDATE SKIP LOCKED` and:

- filters to job types supported by the worker;
- increments `attempt_count`;
- sets worker, lease token, heartbeat, and expiry.

Heartbeat, completion, and retry must match the active lease token and arrive
before expiry. This fences stale workers. Expired jobs below `max_attempts` can
be reclaimed; expired final attempts become failed rather than remaining
stuck. For `crawl_source`, shutdown, lease loss, or a handler error also closes
the associated `crawl_runs` and `company_news_recipe_runs` rows as cancelled.
The next attempt defensively cancels any abandoned rows for the same job before
creating new ones, preserving one immutable audit record per attempt.

## Queue Invariants

Discovery and validation refill operations each use a transaction-scoped
Postgres advisory lock and count global pending/running work before filling
their configured target.

Validation eligibility is additionally coverage-oriented:

- new RSS/Atom candidate;
- no prior validation for that candidate;
- no active validation job;
- company has no approved source;
- candidate ranks first for its company by confidence and stable order.

At most one candidate per uncovered company enters a bulk refill. Direct
single-candidate validation remains available for deliberate additional
sources.

Company-news recipe construction has no automatic refill query. The operator
may enqueue one named company or explicitly materialize a bounded
`news-import --all` campaign. The campaign selector chooses active companies
without either a healthy approved RSS/Atom feed or a healthy active recipe; a
later invocation therefore resumes missing, stale, or failing-feed coverage.
Stable per-company active job keys make repeated triggers idempotent while work
is pending or running. Companies with healthy approved RSS/Atom require an
explicit `--include-covered` override. Approval remains an audited inventory
state; operational health additionally requires a successful runtime crawl, no
current consecutive failures, and fewer than three consecutive empty crawls.

`news-import --retry-transient-after <RFC3339>` is the bounded second-stage
selector. It takes the latest extraction run per company at or after the
boundary and selects only explicitly retryable adapter, article, publication,
or recipe-artifact failures. A newer permanent or successful attempt wins over
an older transient one. Pending/running company builds are excluded, preventing
an early retry invocation from mutating the active sequential campaign.

The recipe-coverage projection reports approved-feed inventory, healthy-feed
coverage, and active-recipe counts separately. Its company-level operational
union uses healthy feeds rather than approval alone and excludes
rebuild-required or `content_stale` recipes. This keeps the recipe-only
`companies_missing_recipe` metric from being mistaken for the actionable
`companies_missing_feed_or_recipe` coverage gap, prevents a terminally failing
feed from hiding a company, and matches the `news-import --all` selector. That
gap is further split between companies awaiting a completed build and companies
still uncovered after a completed build, so queue progress is not conflated
with retry work.

Canonical item lookups separately measure overlap with approved RSS/Atom
sources and with active HTML recipes. Runtime HTML-to-HTML checks compare only
against an older preferred recipe, preventing symmetric retirement when two
listing variants converge on the same article set.

Exactly one `company_news_extraction_runs` row may be running for a job. The
window is fixed before enqueue; suggested, accepted, rejected, source,
normalized, and new-item counts are nonnegative and constrained for internal
consistency. Terminal runs always have `finished_at`.

Recipe-run counts additionally require:

```text
accepted + rejected <= discovered
normalized <= accepted
new <= normalized
acceptance_ratio_bps in 0..=10000
```

## Validation Audit Invariants

`candidate_validation_runs.status` is one of:

```text
running
valid
needs_review
invalid
failed
cancelled
```

Exactly one run may be `running` for a candidate. A running row has no
`finished_at`; every terminal row does. Item counts are nonnegative and titled
items cannot exceed total items.

`candidate_decisions` requires:

- `activated`, `rejected`, or `kept_for_review`;
- `automatic` or `operator`;
- nonblank actor and reason;
- optional source ID;
- structured metadata.

## Feed Item Identity

Normalization deduplicates within source by:

1. `(source_id, external_id)`;
2. `(source_id, canonical_url)`;
3. indexed versioned content hash for change detection.

Cross-source duplicates retain independent provenance. Bulk validation limits
automatic selection to one uncovered-company source, reducing accidental
format/locale duplication. `feed_items_company_canonical_idx` and the immutable
`public_url_identity_key` projection support the company/URL identity used by
the live news view. That view collapses cross-source duplicates across HTTP(S),
conventional `www.` aliases, trailing slashes, and fragments while preserving
path case and query parameters. It prefers RSS, then Atom, then HTML and browser
items; raw rows remain available for audit.
Global partial identity indexes on public `canonical_url`, `url`, and
`external_id` values additionally make cross-company source-ownership checks
bounded URL lookups instead of full feed-item scans during recipe construction
and runtime drift validation.

Public item reads require `NOT is_private` and `source.status='approved'`.
The same publication gate is used for a reversible quality quarantine: a
proven recipe listing artifact or normalized low-content-diversity batch is
retained with its raw evidence, marked private, annotated under
`content_processing.quality_quarantine`, and recorded as
`feed_item.quality_quarantined` in `event_log`. A corrected detail-page item is
therefore never deleted merely to clean the live news view. If a later
correctness-passing crawl normalizes that same canonical item, persistence
releases only a versioned `recipe-listing-artifact.vN` or
`recipe-content-diversity.v1` quarantine and records
`feed_item.quality_released`; unrelated private items remain private.
Version 2 of the content-diversity quarantine is item-scoped: it holds every
member of a source-local sanitized-body cluster that carries multiple distinct
titles. The recipe is queued for quality revalidation while unique rows from
that same source remain public.
The versioned cleanup covers publication self-pages, generic listing titles,
taxonomy/collection URLs, and repeated site-wide-title clusters that fail the
same runtime policy. Version 25 extends that replay-safe class to conservative
navigation/placeholder titles and short slug-matched multi-article card grids;
a corrected replay of the exact canonical detail page can still release it.
Version 26 covers residual CMS collection headings and framework-script title
suffixes. Collection pages remain private on replay, while a valid article
whose title is cleaned by the normalizer is released through the same
replay-safe mechanism.
Version 27 covers collection hubs whose CMS changes the exposed title during
replay. The runtime now proves those pages from shallow path, card-grid, generic
body, and pager-chrome evidence, so another title repair cannot make them
public.
Version 28 quarantines the historical high-confidence subset of thin
multi-card pages. Shared multi-company news hosts are excluded from that
historical classifier; the live crawler instead measures whether the page has
a primary H1-bearing or substantive article element.
Future drift no longer requires another historical cleanup wave. Every recipe
crawl maps its deterministic listing failures back to an already-public item
from the same source using public URL identity. Matching rows enter the
reversible `recipe-runtime-artifact.v1` quarantine before accepted items are
persisted. Only explicit listing-path, generic-title, high-link-density,
multi-article, and year-archive failures qualify; request, rendering,
insufficient-content, and other ambiguous extraction failures cannot hide a
previously valid item. A later correctness-passing normalization of that same
canonical page can release the runtime quarantine.
Version 29 is the bounded backfill for runtime gates added after earlier
collection waves: terminal glossary resources, thin grids containing many
independent H1-bearing cards, generic company/portfolio indexes, and short
branded `... Stories` collection labels. It also covers a collection URL no
longer present on the current publication page, which runtime replay cannot
revisit by itself.
Version 30 covers year-named archive slugs, short branded archive headings,
and employee-story indexes. These forms are deterministic collection
identities even when a template assigns the first card's publication date to
the whole page.
Later listing-artifact versions also cover terminal media-download collections
and short `<topic> news and updates | <site> Blog` topic indexes. The backfill
includes rows retained under superseded recipe versions so an obsolete source
cannot continue exposing a known utility through the public item API.
Residual versions cover slug-matched taxonomy archives, terminal article
indexes, generic section headings, and explicit filter/navigation bodies.
`feed-content-diversity.v1` extends the repeated-sanitized-body invariant to
RSS/Atom ingestion and backfills legacy rows. `recipe-listing-artifact.v46`
and `cms-placeholder.v3` cover legal/subscription utilities and deterministic
short CMS fixtures across both feed and recipe sources. These content repairs
are reversible; the separate cross-company source retirement remains
non-replay-safe.
`company-scope-relevance.v3` queues every healthy `company_identity` recipe
through the current ownership gate; a conclusive below-majority result
supersedes the recipe after that run and records item-scope counts in recipe-run
metadata.
The durable repair audit applies the runtime `company-scope-relevance.v4`
vocabulary to the historical minimum-threshold cohort whose narrative alias
annotations supplied non-company connector words. It stales the inferred
recipes, disables the provisional feeds, and reversibly quarantines only rows
from those sources; independently scoped direct evidence remains
unquarantined but still obeys the normal active-recipe API gate. The next
explicit all-company campaign can therefore rediscover the company without
replaying the false association. The same audit records two reviewed
distinctions.
`manh.com` and `ir.manh.com` are verified first-party Manhattan Associates
hosts, so the imported security name is replaced by the company name and its
scope-blocked recipes become explicit rebuild inputs. DeepAware's official
site identifies Silicon Valley Robotics Center as an affiliated commercial
arm, but the current `roboticscenter.ai` digest is not a DeepAware company
publication. Those sources are disabled, their four observed rows are retained
privately, and the host is excluded until a later operator review proves that
its publication scope changed.
The reviewed utility repair reversibly quarantines demo-series and product-tour
utilities found below broad resource/library routes. It immediately recrawls
affected active recipes with the matching runtime policy, allowing blog,
research, and news children from the same listing to remain active.
The separate reversible `company-scope-relevance.v1` quarantine covers
articles proven to come from an unscoped third-party multi-company collection.
Its runtime counterpart applies to recipes with `company_identity` item scope,
filters off-company articles before persistence, and immediately supersedes a
recipe whose fetched sample is less than 50% relevant. A directly adapter-cited dedicated
publication persists `publication_boundary` scope instead. Unlike a listing
artifact quarantine, a later crawl does not automatically release an
off-company item merely because its page remains structurally valid.
Version 2 also covers historical rows left by a shared-host recipe after the
current runtime has already proven that recipe unscoped and marked it stale.
The `shared-direct-scope.v1` and `shared-direct-scope.v2` quarantines are
narrower: they temporarily hold historical direct evidence from known shared
news hosts. Version 2 also requires a nontrivial URL identity token. Both are
replay-safe because persistence can release an item only after the manual
builder's article-level company-scope gate accepts that same canonical item.
`company-ownership.v1` is intentionally not replay-safe. It records a
publication proven to belong to a different company, disables the source,
stales its recipe, and retains the associated rows privately for audit. A
future build must discover a new source under the corrected company identity
instead of reviving the rejected association.
`cross-domain-company-ownership.v1` applies the same terminal treatment to a
dedicated domain whose current owner is a different company. Future
adapter-generated recipes on an unrelated host remain company-identity scoped
unless the host is tied to a known company entry point.
Version 2 extends that treatment across feed, direct-evidence, recipe, and
candidate sources for the conflicting domain, and suppresses a proven
ambiguous historical alias from future adapter requests.
Version 3 also quarantines exact legacy direct-evidence rows on a shared host
when the primary article identity belongs to the different company, without
disabling that shared origin for future correctly scoped evidence.
Version 4 distinguishes one relevant third-party case study from ownership of
that third party's complete publication, and retires same-name company feed
collisions across both HTML recipes and RSS candidates.
The durable `company-publication-host-policy.v1` profile records reviewed
`verified_hosts`, `excluded_hosts`, and `direct_evidence_excluded_hosts`.
Excluded hosts override name matching and adapter recommendations. Unrelated
feeds and direct articles must prove article-level company identity, while a
verified brand or rename can retain publication-boundary scope. Adapter
recipes re-evaluate reviewed exclusions on every crawl, so an immutable
historical scope cannot bypass a later ownership correction. An explicit
historical adapter boundary otherwise remains intact because issuer brands and
acronyms are not always derivable from the registry name. Historical boundary
audits restored valid brand publications where blanket narrowing proved too
conservative. The same reviewed profile contract applies to official feeds
exposed by an all-company campaign, preserves stable company keys through
legal renames, and keeps shared fund-manager hosts company-identity scoped.
`company-host-identity.v2` excludes the terminal DNS suffix from name-to-host
matching, so a name such as `Example.com` cannot claim every unrelated `.com`
publication. Historical corrections retired confirmed over-expansion and
queued affected domain-branded company recipes through the corrected rule.
Recrawling a privately retained item preserves any non-replay-safe
`quality_quarantine` metadata while refreshing the remaining normalized
content, so its audit reason is not erased by a structurally successful fetch.
Legacy private rows can recover the same structured reason from the latest
matching quarantine event if older metadata was overwritten.
`cross-company-feed-ownership.v1` applies the same terminal treatment when
canonical article identities prove that a recipe sample belongs to an approved
RSS/Atom feed for a different name-first issuer. Same-issuer security classes
are excluded from this conflict check.
`publication-topic-compromise.v1` is a separate content-integrity boundary:
valid first-party ownership cannot publish a feed or recipe sample dominated
by unrelated casino/gambling SEO material. The quarantine is reversible but
is not replay-released automatically; an operator must first confirm that the
publication and source are trustworthy again. Corrective audits disable
affected sources, stale their active recipes, retain observed rows privately,
and move generic legal-notice/video-library utilities into replay-safe
`recipe-listing-artifact.v52`. The same reversible incident record applies to
reviewed compromised sources. It separately records
Discourse `/discuss/` endpoints under `non-editorial-feed-scope.v2`. RADCOM's
reviewed attack-window rows are private while its recovered source and ten
clean rows remain active. The terminal recovery operation moves the recovery
marker from pending to recovered only when the dedicated crawl job is complete,
and applies the shared discussion-item quarantine to the final legacy thread.
The runtime `cross-company-item-scope.v1` policy handles partial overlaps that
do not justify retiring an entire source. Exact public article identities
claimed by distinct name-first issuers must explicitly scope each company or
reside on its first-party host. Wrong associations are made private, their raw
rows are marked skipped, and a reversible structured event is retained.
`feed-admin news-ownership-audit --apply --fail-on-unscoped` reconciles
historical rows and is part of the terminal campaign audit. Security classes
and dual-listed legal forms normalized to the same issuer are excluded.
`shared-feed-scope.v2` disables legacy global Simply Wall St feeds, and
`cross-company-source-ownership.v1` retires exact feed or publication sources
whose URL/content belongs to another imported issuer. Both retain the original
rows and structured repair events for audit. The repair catalog likewise
retains and reversibly quarantines a placeholder-only CMS feed and a
manager-wide fund feed that was incorrectly assigned to one trust; dedicated
replacements remain discoverable. It applies the same reversible repair to
exact global wire/market-news landing pages and manager-wide BlackRock,
Gabelli, and Angel Oak publications assigned to a different vehicle. It
supersedes affected recipes, rejects accepted candidates, and installs
reviewed manager-host exclusions without changing the manager operating
company's own profile.

Two host-level ownership collisions proven by current content and another
imported company's source inventory are retired: Public Storage sources
assigned to National Storage Affiliates, and Lattice sources assigned to
Maven. The sources, recipes, candidates, raw rows, and normalized items are
retained under non-replay-safe `cross-company-source-ownership.v3`, while the
wrong hosts are excluded from those two company profiles. The exact Blackstone
corporate-news and Sprott corporate-IR publications that composite
revalidation proved were not scoped to Blackstone Mortgage Trust and Sprott
Focus Trust are disabled. Neither manager host is excluded: fund-specific
paths, including Sprott Focus Trust's dedicated press page, remain eligible.

The repair catalog reversibly quarantines residual HTML navigation hubs titled
`Photos & Videos`, a terminal `Timeline`, or `View more from …`, plus shallow
collections whose listing supplied `All <page title>`, whose title matched the
terminal URL segment, and whose body exposed at least four article elements.
The same provider-neutral runtime policy prevents those classes from returning
after later template refreshes.

The durable job model uses an indexed, transactional advisory-lock claim
boundary. This preserves database-serialized claims while allowing the
configured bounded job pipeline width.

Exact investor utility labels and static governance, overview, shareholder,
stock, and financial-information pages on conventional investor-relations
subdomains are reversibly quarantined. Explicit editorial path segments remain
eligible. HTML/browser rows that entered through the former implicit
child-subdomain expansion are also quarantined when they resolve to preview,
staging, test, or UAT environments, or to documentation/help/tutorial hosts
without an editorial URL namespace. Exact evidence-backed hosts and
documentation-host changelog/release/news paths remain eligible. The runtime
uses the same host-boundary policy and permits a proven editorial subdomain to
link back to its parent company host.

Market quote/profile utilities are quarantined only when a bounded
market-profile URL namespace and a stock/share price-or-quote title both
match. The shared normalization policy rejects the same shape on future RSS,
Atom, HTML, and browser observations while preserving substantive market
articles.
Runtime `company-scope-relevance.v4` handles mixed asset-manager collections:
when a managed-vehicle recipe is revalidated, every public historical item for
that source is checked with the same composite vehicle identity as the live
sample. Off-vehicle rows are retained privately and are not automatically
released by a later structural crawl.
Export applies the same active-recipe gate as the API and withholds
future-timestamped rows until their publication time arrives. The default
scope additionally requires `source.public_export_allowed`. A target may
explicitly select `metadata.publication_scope=approved_public` to export every
non-private item from an approved, currently valid source. An item hidden for
recipe drift therefore cannot remain in a later Git materialization.

## Export Safety

Git materialization and push are separate decisions:

- `push_enabled` is target-level and defaults false;
- `public_export_allowed` is the default source-level gate and validation
  defaults it false;
- broader approved-source publication is an explicit target-level policy;
- `export_runs` records commit and push results;
- `exported_items` makes reruns idempotent.

## Local Validation

Inspect the generated plan before reconciling an existing database. The apply
script prompts by default and requires explicit hazard allowances.

```bash
docker compose up -d postgres
export DATABASE_URL=postgresql://company_feed:company_feed@localhost:55432/company_feed

scripts/schema-plan.sh
SCHEMA_AUTO_APPROVE=true scripts/schema-apply.sh

TEST_DATABASE_URL="$DATABASE_URL" \
  cargo test --workspace --all-features --all-targets -- --test-threads=1
```

The Postgres suite covers imports, queue bounds, lease fencing, retry/recovery,
discovery, validation auto-activation, operator disable, crawling,
normalization, API visibility, and Git export.
