# Manual Company News Bootstrap

Companies without a usable RSS or Atom source can be bootstrapped through an
operator-triggered news and tech-blog bootstrap. A dedicated worker asks a
neutral adapter for public publication entry points and optional recent
evidence articles. The open-source worker fetches every page itself, activates
only independently validated versioned crawl recipes, and persists accepted
evidence through the normal raw-content and normalization contracts.

This path has no timer or automatic queue refill. Starting the worker does not
create work. Only an explicit `feed-admin news-import --company ...` or
`feed-admin news-import --all ...` command creates build jobs. Scheduled recipe
execution is a separate `feed-worker` responsibility after activation.

## Runtime Separation

| Path | Trigger | Input | Owner |
|---|---|---|---|
| RSS/Atom crawl | Source freshness scheduler | Approved feed URL | `feed-worker` |
| Publication-seeded feed discovery | Completed recipe build with no approved feed | Public adapter publication URLs | `feed-discovery-worker` |
| Company recipe build/bootstrap | Explicit operator command | Named company or bounded resumable campaign | `feed-news-extraction-worker` |
| Company recipe crawl | Recipe freshness scheduler | Active validated recipe | `feed-worker` |

`feed-worker` cannot claim `extract_company_news`. The dedicated import worker
claims only that job type. It defaults to one durable company job and supports
a bounded `NEWS_EXTRACTION_JOB_CONCURRENCY` pipeline. It contains no producer;
an all-company pass is an explicit, durable, resumable operator action rather
than a recurring sweep.

## Manual Trigger

Start the idle worker after configuring the neutral adapter:

```bash
NEWS_EXTRACTION_ENABLED=true \
NEWS_EXTRACTION_ADAPTER_URL=http://private-adapter:8090/ \
docker compose --profile news-extraction up --build -d news-extraction-worker
```

Import one company by exact name or company key:

```bash
cargo run -p feed-admin -- news-import \
  --company "Acme" \
  --lookback-days 31 \
  --max-articles 20
```

Tickers are not accepted. Repeating the command while that company already has
a pending or running import returns the same durable job instead of creating a
second active job. After the job reaches a terminal state, the operator can
explicitly trigger another import.

An explicit `--company` request is queued at operator priority, so a manual
build or canary runs after the currently leased company instead of waiting
behind the rest of an all-company campaign. If the company already has a
pending campaign job, that same job is promoted.

By default, a company with a healthy approved RSS/Atom source is rejected
before it is queued. Health requires at least one successful runtime crawl, no
current consecutive fetch failures, and fewer than three consecutive empty
crawls. An approved feed that has drifted or exhausted retries therefore enters
the fallback-recipe cohort. For large companies where an operator intentionally
wants additional product, engineering, research, or brand publications, use:

```bash
cargo run -p feed-admin -- news-import \
  --company "Microsoft Corporation" \
  --include-covered
```

The lookback window scopes this one import request; it is not a schedule.
Without an expansion override, known stale, rebuild-required, and
`content_stale` publication URLs are direct validation inputs rather than
depending on the adapter to rediscover them. `--include-covered` additionally
revalidates healthy active publication URLs. If company-profile ownership or
cross-domain hosting has changed, a correctness-passing rebuild creates a new
immutable recipe version; the prior active version is not displaced by a
failed rebuild.
Reviewed brand, rename, and conflict decisions live in
`company.metadata.publication_host_policy`. Verified hosts may provide a full
publication; excluded hosts cannot create a recipe or feed. An unrelated
direct article is retained only when its title or URL identifies the requested
company, so a third-party case study does not expand into that publisher's
entire blog.
An affiliated site is evaluated the same way. Affiliation can be retained as
profile evidence without treating every article on that site as company news.
The current DeepAware repair therefore keeps Silicon Valley Robotics Center's
official commercial-arm relationship, disables its broad robotics digest, and
stores the rejected rows in a reversible scope quarantine. A future editorial
change requires a fresh reviewed-host decision and recipe rebuild.

Build missing or stale recipes across the active company registry explicitly:

```bash
cargo run -p feed-admin -- news-import \
  --all \
  --include-covered \
  --limit 10000 \
  --spacing-seconds 1
```

The selector skips companies with either a healthy approved RSS/Atom feed or a
healthy active recipe. Re-running the same command safely resumes missing,
rebuild-required, content-stale, and unhealthy-feed coverage; zero-result
companies are not retried automatically.

After a broad wave reaches a terminal state, retry its transient gaps without
repeating permanent 404/403 failures or every legitimate zero-result company:

```bash
cargo run -p feed-admin -- news-import \
  --retry-transient-after 2026-07-24T12:41:32Z \
  --include-covered \
  --limit 10000 \
  --spacing-seconds 1
```

The RFC3339 boundary is inclusive. For each company, the selector examines only
the latest extraction attempt at or after that time. It selects retryable
adapter failures, direct-article failures, listing crawl failures, and
correctness failures with retryable fetch diagnostics. A later successful or
permanent attempt supersedes an older transient failure. Companies with a
pending or running build are excluded, and `--include-covered` is required to
retain transient expansion gaps for companies that already have an approved
RSS/Atom feed.

## Boundary Contract

The open-source request uses `company-news-extraction.v2` and contains:

- an extraction-attempt UUID, reused as the idempotency key for that attempt;
  durable job retries receive a new request UUID because their known public
  URLs may have changed since the preceding attempt;
- company ID, a name-first organization search name, canonical-name aliases,
  and known public URLs; listed-security prose such as share classes,
  depositary-share ratios, par-value notes, and trailing jurisdiction labels is
  removed from the search name without introducing a ticker dependency;
- a fixed UTC `window_start` and `window_end`;
- a bounded `max_articles`.

The adapter response contains only public publication/listing URLs, individual
evidence article URLs, and neutral rank scores. It cannot provide trusted
selectors, executable code, titles, dates, summaries, bodies, ownership
decisions, or persistence instructions. Provider identity, prompts,
credentials, raw responses, and proxy topology stay outside this repository.

Plausible publication URLs are also copied into a durable `discover_company`
job as untrusted seeds when the company lacks a healthy approved feed. An
explicit `--include-covered` build performs the same handoff for expansion
publications even when another feed is already active. Stable editorial roots
inferred from the adapter's evidence article paths are included alongside its
listing suggestions; this lets discovery probe the actual `/press/` or `/news/`
root when a suggested landing page is stale. That job is owned by the separate
discovery worker and bypasses a second AI request. It checks standard feed paths
and HTML alternate links, persists candidates, and lets the ordinary validation
worker decide whether any RSS/Atom result is usable. A later healthy approved
feed can supersede an overlapping HTML recipe through the normal runtime
overlap gate.
Expansion candidates for an already feed-covered company still require an
explicit validation wave with `--include-covered`; normal validation refills
remain bounded to uncovered companies.

## Building the Normalization Path

For every suggested URL, the worker:

1. rejects unsafe, root, and search-result URLs;
2. resolves DNS and rejects private, loopback, link-local, multicast, and
   reserved addresses before and after connecting;
3. follows at most five checked redirects, preserving HTTPS for an otherwise
   identical same-host downgrade target (a common slash-canonicalization
   misconfiguration);
4. enforces per-request timeout and response-byte limits;
5. permits up to the configured per-host concurrent article fetches (eight in
   the campaign profile) while preserving adapter candidate priority and the
   configured global concurrency limit (twenty-four in the campaign profile);
   alternate narrow, evidence-prefix, and broad checks reuse the same fetched
   publication and archive listing pages inside one company build;
6. requires an article signal such as `<article>`, a publication timestamp,
   `og:type=article`, article-like JSON-LD, or an article-like URL path paired
   with an `<h1>`;
7. extracts from `<article>`, `<main>`, or `role=main`, not a listing page;
8. derives title, publication date, canonical URL, summary, and body from the
   fetched document;
9. sanitizes the body through `feed-content` and requires at least 200 body
   characters by default;
10. groups accepted pages by public origin, creates or reuses an HTML source,
   stores replayable raw content, and writes normalized items transactionally.

On shared multi-company release hosts, a declared canonical URL is the fetched
page identity. An adapter-supplied request slug cannot establish company scope
when that canonical URL and the extracted headline identify another issuer.
This prevents numeric-ID release endpoints that ignore arbitrary trailing slugs
from admitting a fabricated company match.

Publication entry points additionally undergo a bounded listing crawl. Only a
nonempty run that yields substantive independently fetched articles activates
`company-news-recipe.v1`. The durable result is versioned recipes, their
freshness/correctness health, known publication origins, and normalized article
history. One company can legitimately have corporate, product, engineering,
research, and brand publications on different domains. No provider-generated
site-specific parser or executable code is committed. See
[Company news crawl recipes](company-news-recipes.md).

If articles live outside the listing's directory, the builder can derive a
restrictive path prefix only when at least two fetched evidence articles share
it on the same exact host. A shared language directory across corporate,
press, and investor subdomains is not treated as an article scope, and a
one-segment directory must itself be editorial. Terminal `default` and `index`
listing documents are normalized to their containing directory. Semantic
listing documents such as `press-releases.html` and
`feature-articles.html` map to the same-named detail directory instead of an
impossible `*.html/` prefix. A broad fallback is attempted only when fetched
adapter evidence lies outside that directory or when the inferred directory
discovers zero candidate URLs. The latter fallback still has to pass every
article, ownership, diversity, and acceptance gate; accepted out-of-directory
articles become durable evidence so runtime revalidation does not silently
restore the incorrect directory prefix. Older adapter recipes that omitted
their serialized path scope recover the same safe directory boundary at
runtime. This reusable policy does not introduce provider-generated parsing
code.

The generic listing crawler preserves DOM order among links with equal
confidence, rather than assuming numeric or deeper paths are newer. It demotes
collection and embedded taxonomy links, and it permits textless overlay-card
anchors to reach independent page validation. An empty listing hint is never
accepted as an article title: the fetched page must still provide valid article
semantics, title, and body; freshness remains `unknown` when neither the page nor
its listing card has a reliable date. When a card anchor wraps category, title,
and summary text, a nested semantic heading is used as the title hint instead
of the concatenated card text. A nonempty generic action label such as
`Read More` may use exactly one heading from its nearest bounded card scope;
`Read more about <headline>` is normalized to `<headline>`;
an image-only link may use an explicit anchor `title`/`aria-label` or exactly
one usable descendant image `alt`, while an unlabeled empty overlay still
contributes no title hint. If the publication instead exposes generic
year/month archive links, at most the first two are expanded one level with the
same selector, host/path rules, and total article cap. A suggested publication
that itself ends in a temporal archive such as `/2026`, `/2026.html`,
`/2026/07`, or `/2026/07.aspx` is normalized to its stable parent before recipe
validation. Query-addressed articles retain only bounded resource identity keys such as
`content_id`, `newsid`, `post_id`, and `p`; locale, category, pagination,
tracking, and arbitrary scanner fields are ignored for canonical identity.
Empty or unsafe resource values never make a listing URL look like a detail
page. Locale publication roots require independent collection evidence before
rejection, preserving genuine two-letter article slugs. Breadcrumb-prefixed
media-library labels and short high-link-count taxonomy titles with aggregate
counts are rejected even when their templates emit Article metadata. When an
individual
investor page uses framework H1/title text such as `News Details` or
`Press Release Details`, exactly one usable semantic H2/H3 headline can replace
that chrome and the replacement is recorded in item provenance. When individual
pages use
`<article>` for related-content cards, strong article metadata allows the
crawler to select the substantive semantic rich-text or CSS-module single-post
body instead; Elementor/Divi post content, Joomla article bodies, Gatsby
rich-text content, named Framer content/body regions, HubSpot post bodies,
Sitecore field-content/RTE-field wrappers, repeated bounded
`richtext-editor-place` components, Tailwind prose, Chakra containers,
conventional rich-text containers, DNN/EasyDNN `main_content` detail bodies,
and custom-code modules are supported by the same generic body contract. On an
explicit article path, two or more non-chrome rich-text components are
aggregated before sanitization. When that bounded body is more than twice as
substantive as every related card, a large recommendation grid does not hide
the primary article. Body candidates must clear the content floor using
sanitized text; raw CSS/script/form text embedded in a CMS field cannot shadow
real article prose. With strong page-level
article metadata, a semantic body at least twice as substantive replaces one
unrelated navigation-card `<article>`. Independently observed listing-title
evidence plus a substantive semantic body can also disambiguate related
`<article>` cards; pages without either proof remain rejected as collections.
For an explicit article-like path, matching `og:title`/`twitter:title` URL
evidence plus a substantive semantic body (including common `.wd_news_body` or
bounded `#newsContent` newsroom markup) can supply the same weak article proof.
A root-level detail slug qualifies only on a dedicated editorial subdomain,
including a `media` publication host. Collection, link-density, and
generic-title rejection still apply.
When a standard Next.js detail response contains only a server-side loading
skeleton, the shared crawler can recover a rich article from
`script#__NEXT_DATA__` without running JavaScript. The embedded object must
carry a bounded `slug`/`id`/`path`/`url` identity that exactly matches the
fetched URL's terminal segment, plus a usable title and substantive rich-HTML
body in that same object. Unmatched listing-array entries are ignored.
Traversal depth, visited nodes, response bytes, field names, title quality,
content floor, and publication-date plausibility are bounded; recovered HTML
still passes the ordinary sanitizer and article correctness gates. Successful
items record `framework_fallback=next-data-json.v1` and the matched identity,
title, body, and optional date fields so later drift is observable.
SvelteKit detail pages that expose only a loading shell may be recovered from
their same-origin `<article-path>/__data.json` route. The shared crawler
recognizes SvelteKit bootstrap markers, fetches the JSON under the normal
network and byte limits, and decodes its reference table without executing
JavaScript. It requires an exact same-origin path identity, usable title, and
substantive rich HTML reachable from that same bounded content object.
Successful items record `framework_fallback=sveltekit-data-json.v1`, the
resource URL, matched fields, and optional date provenance.
Explicit publication metadata is preferred for dates;
generic `publish-date`, `published-date`, and `publication-date` meta fields
are supported, including timezone-less CMS timestamps such as
`16-Jun-2026 06:05:22`, which are interpreted as UTC;
when only HTML time elements exist, the time structurally nearest the page H1
outranks dates embedded in earlier related-content cards. If the article page
uses a conventionally named `publish-date`, `published-date`, or `post-date`
element—or an H1-local element explicitly begins with `Published on`,
`Published`, `Posted on`, or `Posted`—exactly one parseable date is accepted
only when that visible element shares a small non-page-chrome wrapper with the
page H1; dates on related cards elsewhere in `main`/`body` cannot qualify. If
the article page is undated, exactly one parseable date from the link's nearest listing card is used
as lower-priority freshness evidence; it never overrides a page date. For an
already article-like path, exactly one full date among the first eight text
nodes of its qualified semantic body is accepted as a page fallback; the same
rule accepts a full date in the final delimiter-bounded field of a compact
byline such as `Company | May 7, 2025`. Neither form becomes independent
article evidence.
An OpenGraph-qualified article may use a Yoast-style WebPage
`datePublished`; an unqualified WebPage timestamp remains ignored.
All HTML, RSS, and Atom dates must fall between 1990 and two calendar years
after the current year. Truncated two-digit years and epoch-sentinel values are
ignored rather than used for freshness.

Company ownership and publication integrity are independent. A first-party
host is still rejected when a five-item-or-larger sample is at least 80%
casino/gambling SEO material and the company profile does not identify an
expected gaming, sportsbook, lottery, hotel, resort, or casino publication.
This `publication-topic-compromise.v1` gate runs before direct evidence can be
persisted and before an HTML recipe can pass correctness. Candidate validation
and later RSS/Atom or recipe crawls use the same signal contract.
Amusement, entertainment, payments, and prediction-market profiles are also
explicitly exempt so a legitimate adjacent business is not retired by the
batch-level gate. Discourse `/discuss/` scopes remain ineligible regardless of
an adapter's corporate-blog label because they resolve to forum categories or
user threads.

Some CMS item templates omit `<h1>`, `<article>`, dates, and article metadata
even though the fetched page contains a complete rich-text article. These pages
are accepted only when the listing crawl independently supplied a usable card
title, the detail URL is article-like, and the page has a substantive semantic
body. The same page without that listing evidence remains rejected.
Standard detail namespaces include both `pressroom` and the hyphenated
`press-room`; their collection roots still remain non-article pages.
The same listing-only proof may qualify a detail descendant of a bounded
official editorial collection named `resources`, `perspectives`, `publications`,
or `research-and-press`. Those roots remain collections, and an H1 or social
title without the independently observed listing title cannot promote a generic
page.
Numeric detail IDs are likewise accepted only in a listing-proven
`/updates/<positive-id>` namespace; generic numeric blog paths remain
ineligible as likely pagination.
When one card links the same article through an image overlay and a separate
titled content anchor, URL deduplication keeps the first position but merges the
stronger later title, date, or companion-document evidence.
A textless overlay may also borrow exactly one visible heading and date from
its nearest semantic `role=listitem`/Webflow dynamic item. Hidden CMS condition
variants are excluded; an unbounded empty overlay still contributes no title.

When multiple suggested listings point into the same article namespace, the
builder evaluates explicit child publications before their parent hub. A
lower-volume mirror is skipped when its complete canonical article sample is
already covered; a parent is skipped when the union of already selected child
samples fully covers it. Distinct corporate, product, engineering, and
research properties remain eligible when they expose any distinct article.
Among otherwise comparable publications, supported evidence count and the
adapter's explicit rank take precedence over arbitrary URL path length.
During an explicit rebuild, a later broader sample replaces earlier strict
subsets, and equivalent publication identities converge. An older active
recipe proven redundant with that selected sample or an approved feed is
superseded immediately and audited; its not-yet-started activation crawl is
cancelled.

Correctness and content freshness remain independent. A correct recipe is
reported as `fresh` only when every accepted item has a publication timestamp.
If any accepted item is undated, freshness is `unknown`; an old date on another
item cannot prove the undated article is stale.

Before the listing crawl, the open-source worker also requires an editorial
path or an editorial publication subdomain. It rejects generic homepages,
corporate profiles, investor-relations roots, event pages, and unscoped
press-wire landing pages even when the private research adapter suggested one;
this includes Access Newswire and GlobeNewswire's global pages plus exact
global listing roots on Business Wire, PR Newswire, Nasdaq market activity,
StockTitan, and Investing.com. Company-scoped wire listings and direct evidence
articles remain eligible and retain item-level company checks. Exact aliases
owned as canonical names by another active company are excluded from research
requests.
Taxonomy/category pages are excluded, and a failed article-path crawl cannot
fall back to treating that direct article as a broad site listing. The runtime
crawler repeats these checks so a previously activated recipe cannot continue
publishing navigation or category pages after the rule is tightened.
Subscription, unsubscribe, and email/news-alert utility paths are rejected at
both activation and runtime even when they contain links to real releases.
Static investor and newsroom destinations such as governance, management,
archives, media kits, presentation indexes, policy pages, and filter labels
receive the same treatment.

The worker also checks the normalized publication URL against healthy active
recipes across the registry. A different issuer cannot activate the same exact
publication; duplicate securities for the same stripped issuer name remain
allowed. This ownership check uses company names, never tickers, and does not
prevent one large company from activating multiple distinct corporate,
product, engineering, research, or brand URLs. Exact generic BlackRock
closed-end-fund and Gabelli manager press collections are rejected as unscoped
manager publications.

A dedicated publication returned directly by the research adapter persists
`publication_boundary` item scope. The verified host/path boundary can then
carry public-brand, acquired-product, and renamed-company articles even when
their headlines do not repeat the imported legal name. Inferred publications
and known shared multi-company news hosts retain `company_identity` scope and
must identify the company at article level. This keeps the AI ownership
decision in the durable recipe without weakening press-wire and market-news
collection filtering.

Known asset-manager domains are also shared when the requested company is a
managed trust or fund. Those items must match a composite vehicle identity:
the manager brand alone is insufficient, while a fund-specific name or path
remains eligible. The manager's operating company retains normal corporate,
engineering, and product-publication coverage. This rule overrides legacy
adapter boundary state and revalidates all currently public history for the
source; rejected off-vehicle rows are retained privately under
`company-scope-relevance.v4`.

Evidence articles whose canonical URLs already exist in an approved RSS or
Atom source are not re-imported through an HTML source. Recipe validation also
suppresses an HTML publication when at least half of its accepted sample
overlaps approved feed items. Distinct low-overlap publications remain
eligible, so this removes redundant recipes without collapsing legitimate
multi-property coverage. The same proof during a later scheduled crawl
immediately supersedes the redundant recipe and records the replacement
evidence rather than waiting for the structural-drift streak.

Direct evidence returned on a known shared press-wire or market-news host is
also filtered article by article before normalization. Each retained article
must identify the requested company in its title or canonical URL; unrelated
articles are recorded as `article_not_company_scoped` and never reach a
company source. URL evidence requires a nontrivial identity token, so short
aggregator paths such as Quiver Quantitative's `/news/Art` cannot masquerade as
an Art's-Way match. Narrative text embedded in an imported alias—such as
`formerly`, `in process of incorporating as`, or `due to trademark`—is not
identity evidence. A two-letter acronym in a title is accepted only as an
exact token when the article or canonical host also contains the same exact
non-terminal DNS label; the top-level domain alone is never corroboration.
Generic trailing `Railway`/`Railroad` descriptors are omitted when deriving
this bounded public-brand acronym. The shared-host registry covers press wires,
financial-news aggregators, and publisher mirrors observed across multiple
companies; this is an item-scope policy, not a site-specific parser.
Historical direct evidence from these hosts is held in the replay-safe
`shared-direct-scope.v2` quarantine
until a later manual import observes the same canonical item through this gate.

Near-duplicate HTML publications use a stricter gate requiring at least three
overlapping articles and 80% canonical overlap. This collapses URL variants and
nested listing pages while retaining separate large-company product,
engineering, research, and corporate publications; runtime overlap supersedes
only the copy that loses the preferred-recipe ordering.

## Failure and Review Semantics

URL rejection is recorded in the import run and does not fail an otherwise
useful company result. If no cited article page succeeds, a transient failure
causes bounded retry only when no independent publication path is available.
An editorial publication returned by the adapter, a deterministic discovery
candidate, or an active/stale recipe explicitly included for rebuild is still
validated independently; one inaccessible press-wire citation cannot block an
accessible official listing. Such runs record
`continued_after_transient_evidence_failure=true`. Provider failures are
sanitized at the adapter boundary. A failed company does not automatically
enqueue itself or any other company.

Retryable neutral-adapter failures also impose a lane-level cooldown before
that runner claims another company: at least 30 seconds, or a longer
adapter-supplied retry interval. This is separate from the failed company's
durable retry delay. It prevents one short shared-adapter outage from consuming
the attempt budget of many companies in rapid succession. Postgres serializes
company-news claims with a transactional advisory lock and admits at most
`NEWS_EXTRACTION_JOB_CONCURRENCY` running rows across worker instances. The
deployment can pipeline public-page validation, while the private adapter owns
its provider-specific concurrency and rate limits.
Public listing validation may follow a normalized exact host, a bounded parent
host, or a safe company-family child host. Implicit preview/staging/test
environments are excluded. Documentation, developer, support, help, and
tutorial child hosts require an editorial path namespace unless the adapter
explicitly evidenced that exact host.

Recipe-build audit metadata records bounded article-fetch diagnostics for each
correctness failure: counts by stable reason, retryable counts, and up to three
public URL samples. This makes generic crawler gaps measurable across the
campaign without storing provider prompts or generating site-specific code.
Stable reasons include `generic_listing_title` and
`publication_page_returned_as_article`; neither is retried as a network error.
`publication_claimed_by_distinct_company` and
`unscoped_manager_publication` identify ownership failures without requiring a
manual review queue.

A previously disabled origin remains disabled and is skipped; the import never
silently re-enables it. Created sources default to
`public_export_allowed=false`. Set `NEWS_EXTRACTION_PUBLIC_EXPORT=true` only
when deployment policy deliberately selects these sources for a Git archive.
An export target configured with
`metadata.publication_scope=approved_public` selects otherwise eligible
approved sources regardless of this source flag. Selection does not establish
redistribution rights.

Inspect the immutable run audit through:

```text
GET /api/v1/company-news-extraction-runs?company_id=<UUID>&limit=50
GET /api/v1/company-news-recipes?company_id=<UUID>&limit=50
GET /api/v1/company-news-recipe-runs?recipe_id=<UUID>&limit=50
GET /api/v1/company-news-recipe-coverage
GET /api/v1/companies/<company_key>/profile
```

The coverage response separates recipe-only gaps from companies missing both
an approved RSS/Atom feed and an active recipe. The latter is the actionable
gap for the default `--all` campaign without `--include-covered`.

The delivered `scripts/run-company-news-campaign.sh` supervisor drains the
initial wave, queues its transient retry, makes one final `--all` pass over
that actionable no-feed gap, retries transient failures from the final pass,
waits for recipe-activation crawls, and emits the terminal coverage,
freshness/correctness, cross-company item ownership, quarantine, and live-news
audit. It requires six
consecutive quiet checks between phases so delayed durable jobs cannot be
mistaken for completion.

## Private Adapter Ownership

The private adapter owns provider selection, credentials, prompts, raw
responses, throttling, and retry policy. None of those implementation details
or operational logs cross the neutral HTTP boundary into this repository.

The open-source worker remains provider-neutral and independently verifies every
public URL before normalization. `NEWS_EXTRACTION_ADAPTER_TIMEOUT_SECONDS` is
the end-to-end budget for one neutral adapter call and must exceed the
adapter's complete provider-attempt and retry-backoff budget. The default is
750 seconds so an adapter with bounded long-running attempts and backoff can
finish its own retry policy instead of being disconnected prematurely.
