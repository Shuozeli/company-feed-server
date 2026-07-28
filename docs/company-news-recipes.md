# Company News Crawl Recipes

Company news recipes are the durable fallback when RSS or Atom is unavailable
or when a large company has additional corporate, engineering, research,
product, or brand publications. A recipe is not a one-off article URL and is
not provider-generated executable code. It is a versioned, bounded public crawl
contract that the open-source crawler validates independently.

## Lifecycle

```text
explicit build job
  -> neutral adapter suggests public publication and evidence URLs
  -> public HTTP validation crawl
  -> correctness calibration
  -> active versioned recipe + approved HTML source
  -> scheduled crawl_source jobs
  -> passed / failed health checks
  -> stale after configured failure streak
  -> explicit resumable rebuild campaign
```

Only a nonempty validation crawl can activate a recipe. Changing a recipe
creates a new version and supersedes the prior active version. A stale recipe
is retained for audit but is no longer selected by the scheduler.

Validation compares accepted canonical article URLs with approved RSS and Atom
items for the same company. When at least half of the validation set is already
supplied by an approved feed, the HTML recipe is recorded as
`overlaps_approved_feed` and is not activated. Low-overlap corporate, product,
engineering, research, or brand publications remain eligible, preserving
large-company breadth without duplicating an existing feed.

The same comparison runs on every scheduled recipe crawl. If an approved feed
appears after recipe activation and covers at least half of the recipe sample,
the run publishes nothing and records `overlaps_approved_feed` as a correctness
failure. The now-redundant recipe is superseded immediately, its source loses
the active-recipe pointer, and the replacement evidence is retained in the
audit event instead of consuming three health-check intervals.

HTML recipes are also compared at the canonical-article level within the same
company. A candidate is suppressed when at least three articles and at least
80% of its accepted sample are already covered by an active recipe. Within one
build, even a one- or two-item candidate is suppressed when its entire sample
is already produced by a recipe selected earlier in that build. This uses exact
canonical article identity, so distinct corporate, product, Azure-style
engineering, research, and brand publications remain eligible as soon as they
expose any distinct article. If a later candidate strictly contains the entire
sample of an earlier selection, the broader recipe replaces that subset;
equivalent publication identities such as trailing-slash variants converge the
same way. Runtime drift uses the broader threshold against an older preferred
recipe, so the redundant current variant is superseded immediately without
retiring the preferred copy. When an
operator-directed rebuild proves that an existing active recipe is wholly
redundant with a selected recipe or approved feed, that old recipe is
superseded immediately, its source's active-recipe pointer is cleared, any
still-pending activation crawl is cancelled, and the reason is written to the
audit log.

An exact publication identity may be claimed by several imported security rows
only when their name-first issuer identity is the same, such as two Alphabet
share classes. A healthy claim from a different issuer suppresses activation
and becomes a correctness failure if an older recipe later drifts into that
collision. This prevents a subsidiary, managed fund, or unrelated company from
silently inheriting another company's newsroom while preserving genuine
multi-publication coverage on distinct URLs. Generic BlackRock closed-end-fund
and Gabelli manager press collections are treated as unscoped rather than as
the official publication of any one imported fund.

## Open Recipe Contract

`company-news-recipe.v1` contains:

- publication/listing URL;
- `http` or optional `browser` render mode;
- bounded CSS link selector;
- allowed hosts;
- included and excluded path prefixes;
- maximum links per run;
- freshness policy;
- correctness policy;
- item-scope policy (`company_identity` or `publication_boundary`);
- evidence article URLs used during bootstrap.

Allowed hosts are explicit after normalizing the ordinary `www.` alias. A
recipe may also follow a link to a parent host or a company-family child host,
but an implicit child is rejected when its leading label identifies a
non-production environment such as `preview`, `staging`, or `uat`. Implicit
documentation, developer, help, support, and tutorial hosts require a strong
editorial URL namespace such as `blog`, `changelog`, `news`, `press`,
`release`, `research`, or `updates`. An exact evidence-backed host remains
authoritative. This keeps corporate-to-investor and IBM-style parent-host
boundaries available without treating every manual linked from a blog as
news.

The current delivered executor supports `http`. `browser` is represented in the
portable contract but cannot activate without a configured browser adapter.

The recipe contains no private provider endpoint, prompt, credential, search
call, JavaScript snippet, cookie, login state, or site-specific parser.
For Adobe AEM SPA pages whose server HTML declares a same-origin page
`.model.json`, scheduled crawling may use the generic bounded AEM rich-text
fallback documented in [Crawling](crawling.md); the recipe itself still stores
no site-specific parser or executable extraction code. Standard Next.js
article shells may likewise use a bounded `script#__NEXT_DATA__` data fallback.
It is part of the shared crawler rather than the recipe and never executes the
script or provider-generated code. The fallback accepts only an embedded
object whose `slug`, `id`, `path`, or `url` exactly matches the fetched URL's
terminal identity and whose title and substantive rich-HTML body live in that
same object.

## Correctness Contract

Each run:

1. fetches the listing with redirect, DNS, SSRF, timeout, content-type, and byte
   limits;
2. rejects reserved example/test hosts before they can consume network or retry
   budget;
3. requires an editorial path or editorial publication subdomain; generic
   corporate, investor-relations, event, and unscoped press-wire landing pages
   cannot activate and fail the same gate during later runtime crawls; exact
   global listing roots on shared wire and market-news hosts are rejected while
   company-scoped listings and direct evidence articles remain eligible;
4. rejects author, category, collection, topic, tag, search, series, and other
   taxonomy sub-listings, plus subscription, unsubscribe, and email/news-alert
   utility pages; explicit `/category/` and `/cat/` paths are always taxonomy,
   while a non-terminal semantic path component such as
   `/products/search/<article>` remains eligible;
5. refuses to turn a positively identified direct article URL into a broad
   site recipe when its article-path crawl failed; an explicit editorial
   listing root is still eligible when an imperfect adapter also repeated that
   same URL as evidence;
6. applies the selector and host/path allow-list before fetching, then requires
   both the fetched final URL and the page canonical URL to remain inside the
   same effective boundary; a redirect or canonical rewrite outside the
   recipe scope records `article_outside_recipe_scope` and cannot publish;
7. normalizes terminal `default`/`index` documents to their containing listing
   directory; when that scope yields no correct sample, it tries a path prefix
   shared by at least two independently fetched evidence articles; when the
   inferred listing-directory scope discovers zero URLs, it also tries a broad
   host-bounded crawl and retains it only if the complete correctness gate
   passes. Validated out-of-directory article URLs are persisted as recipe
   evidence so runtime revalidation preserves that proven scope;
8. bounds and deduplicates candidate URLs; textless overlay-card anchors remain
   eligible, and a later anchor to the same URL can enrich the retained entry
   with stronger title, date, or companion-document evidence without changing
   listing order; absent that evidence, the independently fetched page must
   supply its own valid title; a textless overlay may borrow exactly one visible
   heading and date only from a nearest semantic list item, with hidden CMS
   condition variants excluded;
9. fetches each candidate as an independent public article page; a
   document-backed exception requires a distinct stable HTML detail identity
   and an exact same-card PDF, so a direct PDF cannot serve as its own fallback;
10. rejects taxonomy segments anywhere in the path, collection roots, and
   terminal `index`/`default` listing documents, including nested paths such as
   `newsroom/press-releases`; this same final-and-canonical URL gate applies to
   direct evidence even when its HTML emits misleading article metadata;
11. scans a bounded link pool of at most 50 times the requested article count
    and 2,000 links, then demotes weak collection links and embedded taxonomy
    markers before applying the per-run article limit. Exact `product`,
    `content-type`, and `audience` path segments are taxonomy markers as well;
    this lets current article cards outrank very large corporate navigation
    menus without site-specific selectors. Strong explicit detail paths may
    outrank navigation, while links at equal confidence retain listing DOM
    order as the recency signal; numeric segments and path depth are never
    treated as recency because old dated archives can otherwise displace
    current flat article URLs; when a card anchor wraps category, title, and
    summary text, its nested semantic heading is preserved as the listing title
    hint instead of concatenating the entire card; a nonempty generic action
    label such as `Read More` may use exactly one heading from its nearest
    bounded card scope or its immediately preceding sibling at that scope,
    while the generic label itself and an empty overlay anchor contribute no
    title hint; `Read more about <headline>` is unwrapped to `<headline>` as a
    reusable CTA convention; when a publication exposes
    generic year/month archive links instead of articles, the crawler expands
    at most the first two archive collections by the same selector, host/path
    rules, and total article cap, while preserving direct current articles
    ahead of archive contents;
12. rejects plural and prefixed taxonomy paths, terminal coverage/event/webinar
    and other explicit navigation hubs, weak path-plus-H1 pages whose short
    title describes a collection, and short weak-signal pages whose link
    density proves they are indexes rather than articles; year-like numeric
    paths are rejected only when the title also identifies a year archive;
    this includes explicit archive wording, `Press Releases in YYYY`, short
    branded newsroom headings, and `YYYY - <brand>` titles while preserving
    substantive numeric article IDs and year-in-review articles;
    locale publication roots require locale-query, collection-title, or
    link-density evidence, while breadcrumb-prefixed media-library titles and
    short high-link-count taxonomy titles ending in aggregate counts are
    rejected despite misleading Article metadata; an undated weak-signal page
    that needs the generic paragraph-cluster body fallback receives a bounded
    stricter density check only after 50 links;
13. refuses any candidate whose final or canonical URL resolves back to the
    publication listing itself; identity drops tracking, locale, filter,
    pagination, and arbitrary query fields but preserves a bounded
    provider-neutral set of resource-bearing keys such as `content_id`,
    `newsid`, `post_id`, and `p` for legacy query-addressed articles; values
    must be nonempty safe scalars and at most four distinct identity pairs are
    retained; a broken site-wide canonical pointing only to the same host's
    root is replaced by the final detail URL solely when an independently
    observed listing title exists, and the replaced value is retained in
    provenance; a declared canonical whose parsed host is literally `http` or
    `https` is structurally malformed and falls back to the independently
    fetched final URL, while an H1 explicitly marked as an archive, category,
    or taxonomy title remains a collection regardless of Article metadata;
14. requires article semantics, a bounded non-generic title, substantive
    sanitized body, and a non-future publication timestamp; it prefers the most
    substantive H1 inside the narrowest available `article`/`main`/document
    scope, rejects embedded SVG/CSS declarations as title text, can use the
    independently observed listing-link title when page metadata is generic,
    accepts exactly one usable semantic H2/H3 headline when investor-site
    framework chrome occupies the H1/title, and repairs repeated site-wide
    titles from listing-link evidence; generic category/framework labels such
    as `Developer Spotlight`, `Release Details`, `Image link`, `Arrow icon`,
    `Guides & Articles`, `General Information`, and
    `<brand> Blogs | <section>`, plus short `Contact <brand>` utilities, are
    never emitted as headlines; short category, utility, and branded-section labels such as
    `Archives`, `Calendar`, `Insights`, `Results`, `Subscribe`, and `Webinars`
    are likewise treated as page chrome so a real detail page can fall through
    to its independently declared headline; date-only headings and short
    site-name fragments ending in a bare separator are not headlines, and a
    heading whose DOM fragments every letter into a separate text node falls
    through to intact social/document metadata; soft-404 and `Coming Soon`
    headings are never publishable article titles;
    shallow category routes with a thin generic body surrounded by at least ten
    article cards, and thin `Featured Articles` bodies with previous/next pager
    chrome, remain collections even if listing evidence changes their title;
    ten-or-more-card pages with no H1-bearing article and no substantive
    primary article element are likewise rejected, with those structural
    metrics retained in extraction provenance;
    `why-invest` pages are investor
    navigation rather than articles; when strong
    individual-article metadata is present but
    the page also contains multiple related-card `<article>` elements, it
    prefers a substantive semantic rich-text body (including an
    `article#article-content` detail root and common
    CSS-module single-post, HubSpot post-body/blog-post-content, Elementor
    single-post, Divi, Joomla, Gatsby or component ArticlePage/RichText,
    single-blog WYSIWYG, named Framer content/body regions and complete
    Framer Content wrappers split across sibling rich-text nodes, Sitecore
    field-content and RTE-field wrappers, repeated bounded
    `richtext-editor-place` components, plus the largest substantive Framer
    rich-text container when no semantic name is emitted, Tailwind prose,
    Chakra containers, standard rich-text
    containers, custom-code modules, bounded content, and press-detail
    containers). A thin or empty earlier generic match cannot shadow a later
    substantive body container; selector precedence applies to the first match
    that satisfies the content floor, with the largest thin match retained only
    for failure diagnostics. If no known semantic wrapper qualifies, a bounded
    generic fallback scores low-link, paragraph-rich `div`/`section` clusters,
    excludes navigation/header/footer/aside/form subtrees and multi-card
    containers, and requires a higher content floor. The extractor then
    falls back to the most substantive `<article>` element; with strong
    page-level article metadata, the same comparison replaces one unrelated
    navigation-card `<article>` when the semantic body is at least twice as
    substantive; independently observed listing-title evidence plus a
    substantive semantic body can likewise disambiguate related
    `<article>` cards. An article-like detail path plus H1 may do the same when
    the semantic body is independently substantive and contains at most one
    `<article>` descendant. On an explicit article path, two or more
    non-chrome `richtext-editor-place` components are aggregated before
    sanitization; when that bounded body is more than twice as substantive as
    every related card, a large recommendation grid cannot hide the primary
    article. Taxonomy and known collection paths remain
    rejected before this fallback. An explicit article-like path can also use
    an `og:title` or `twitter:title` that agrees with the URL plus a substantive
    semantic body (including common `.wd_news_body` and bounded
    `#newsContent` newsroom markup) as a weak article signal. A root-level slug
    qualifies for that path rule only on a dedicated editorial subdomain,
    including a `media` publication host; it remains subject to the same
    collection, link-density, and title-safety gates;
    image-only listing links may supply that title only from an explicit
    anchor `title`/`aria-label` or one unique usable descendant image `alt`;
    short visual captions containing asset terms such as image, banner, logo,
    painting, illustration, photograph, or article-cover descriptions are
    decorative evidence rather than headlines, and an unlabeled empty overlay
    still supplies no title;
    terminal press/media/brand kits, photo/logo request forms, and short
    logo-use guidelines inside media-asset libraries are utility resources
    rather than articles, including URLs wrapped in terminal `default`
    documents; HTML RSS subscription directories are likewise utilities
    without affecting RSS/Atom source ingestion;
    CMS social-feed records and explicit job-listing paths are non-editorial
    unless an explicit blog/news root proves article scope;
    numbered test posts, please-ignore fixtures, CMS multi-asset/pagination
    samples, and self-titled collection roots are non-editorial;
    generic path recognition includes `blog-posts`, `posts`, and `changelog`
    detail roots, both `pressroom` and `press-room`, plus multi-token root slugs
    on dedicated `blog.`, `updates.`, and similar editorial subdomains, while
    leaving corporate-site root slugs ineligible; presentation-only or hidden
    `<article>` wrappers do not count as article elements;
    explicit publication metadata remains the strongest date signal, while an
    HTML `<time>` structurally associated with the page H1 outranks dates inside
    earlier related cards; when the article page has no date, one unambiguous
    date from the link's nearest listing card is retained as lower-priority
    publication evidence, while page metadata always wins; for an already
    article-like detail path, exactly one full date among the first eight text
    nodes of its qualified semantic body is retained as page-level fallback
    freshness evidence; a compact byline may place that full date in its final
    delimiter-bounded field, but neither visible form can create article
    semantics;
    article, listing-card, RSS, and Atom timestamps outside 1990 through two
    years after the current calendar year are discarded, preventing truncated
    two-digit years and epoch sentinels from corrupting freshness; dotted
    abbreviated English month dates such as `Dec. 3 2015` are normalized as
    dates and cannot replace a real headline;
    an OpenGraph-qualified article may also use a Yoast-style WebPage
    `datePublished`, while Tumblr-style `SocialMediaPosting` and
    `LiveBlogPosting` JSON-LD are article date evidence and an unqualified
    WebPage date remains ignored; CMS item pages that omit all article markup
    can pass only when an article-like URL, a usable independently observed
    listing-card title, and a substantive semantic body are all present; detail
    descendants under bounded official collection roots named `resources`,
    `perspectives`, `publications`, or `research-and-press` may use only this
    listing-proven form, while those roots and standalone pages remain
    collections; positive numeric IDs under `updates` may use the same
    listing-proven form, without making generic numeric blog paths article-like;
    generic `publish-date`, `published-date`, and `publication-date` meta
    fields are explicit publication evidence, including timezone-less CMS
    timestamps such as `16-Jun-2026 06:05:22`, which are interpreted as UTC;
15. rejects batches of three or more items when fewer than half their normalized
    titles are distinct after repair, or when fewer than half their extracted
    article bodies are distinct; the latter catches HTTP-200 soft-404 and SPA
    catch-all routes that serve one homepage body at many apparent article
    URLs;
16. compares discovered count, accepted count, and acceptance ratio with the
   recipe's calibrated floor;
17. collapses publication mirrors that differ only by scheme, conventional
    `www.` host alias, trailing slash, or leading locale segment, while keeping
    distinct editorial paths such as product and engineering blogs separate;
18. suppresses activation when at least half of the accepted sample overlaps an
   approved RSS or Atom source for the company, considering only public items;
19. suppresses near-duplicate HTML recipes when at least three articles and 80%
    of the accepted sample are already covered by another active recipe for the
    company; runtime comparison prefers broader public coverage, then the
    earlier verified recipe as a stable tie-breaker, and ignores quarantined
    items; within one build, explicit child publications are evaluated before
    their parent hub, and a lower-volume listing mirror is also suppressed when
    its entire exact sample is already covered; a later broader sample replaces
    earlier strict subsets, without merging a product or engineering property
    that yields distinct articles;
20. rejects a publication already claimed by a different name-first issuer,
    and also rejects a validated article sample when at least half of it is
    already owned by an approved RSS/Atom feed for a distinct issuer; security
    classes whose stripped issuer identity is equal remain allowed, while exact
    known unscoped fund-manager collections are rejected outright;
21. for inferred publications and known shared multi-company news hosts, keeps
    only articles whose title or canonical URL identifies the company by name,
    alias, distinctive name token, or name-derived acronym; fewer than half
    relevant articles fails the entire run. A dedicated publication returned
    directly by the research adapter persists `publication_boundary` scope
    only when its host matches the company identity or a known company entry
    point, so verified legal-name/public-brand differences and company renames
    do not require the legal entity name in every headline. A two-letter
    name-derived acronym is accepted in a title only when it is an exact token
    and the article or canonical host contains the same exact non-terminal DNS
    label; the top-level domain alone never corroborates it. Narrative alias
    annotations such as `formerly`, `in process of incorporating as`,
    and `due to trademark` are metadata, not identity tokens. Other
    non-distinctive short words are excluded while digit-bearing brands remain
    usable. Generic trailing
    transport descriptors such as `Railway` and `Railroad` are excluded from
    this bounded brand acronym, so a legal name such as `Canadian National
    Railway Company` can prove the `CN` brand without treating an unrelated
    `CN` headline as sufficient;
22. publishes nothing when the correctness gate fails.

Subscription-promo headings are page chrome, not article titles. The generic
extractor ignores them and continues to independently scoped social metadata,
document-title, semantic-heading, or listing evidence.

The activation crawl records baseline discovered/accepted counts. Runtime
floors default to 25% of that baseline, at least one item, and at least half the
observed acceptance ratio with a 10% absolute floor. These thresholds tolerate
ordinary editorial variation while detecting selector drift, listing redesign,
taxonomy pollution, and thin/non-article results.

The company-scope gate distinguishes a useful nonofficial company page from a
broad third-party news collection. The durable recipe records how the
publication boundary was established. A dedicated listing returned directly
by the research adapter uses `publication_boundary` only when its host is tied
to the company identity or a known company entry point; its host/path
allow-list is then the company scope. Unrelated adapter listings, inferred
listings, and known shared multi-company news hosts use `company_identity`,
remove off-company articles before persistence, and record
`article_not_company_scoped` diagnostics. A batch with less than 50%
relevant articles records `company_scope_relevance_below_minimum`, publishes
nothing, and supersedes that inferred recipe immediately. This preserves the AI
ownership decision needed for SLM/Sallie Mae,
Alphabet/Google, acquired products, and renamed companies without weakening
broad press-wire or market-news collections. Shared-host URL matching rejects
ambiguous identity tokens shorter than four characters while title matching
continues to support short company names and acronyms.

Recipes generated before `item_scope` was serialized retain their adapter run
provenance. At crawl time, that provenance restores `publication_boundary`
only for a dedicated publication host already tied to the company; unrelated
and known shared news hosts continue to require `company_identity`. This
compatibility rule supports verified renamed brands without turning an
unrelated dedicated blog into ownership evidence.

Evidence-derived path prefixes are calculated within one exact host. A shared
locale directory such as `/english/` on corporate, press, and investor
subdomains is not proof of one article namespace. A one-segment directory
prefix must itself identify an editorial root; legacy adapter-generated scopes
that fail this proof fall back to ordinary article-like-path validation at
runtime.

Company-owned and known product subdomains are also exempt so Microsoft-style
corporate, Azure, Windows, and engineering properties do not need to repeat the
parent company name in every headline. Identity filtering derives from company
names and aliases, not securities tickers. Host matching recognizes
name-derived legal-brand acronyms such as `AIG`, compact owned brands such as
`coremt`, and the narrow `Bancorp`-to-`bank` issuer/brand form. Short acronyms
must match a complete host label or a bounded brand form such as `joinroot`;
item-level prefix variants are limited to a three-character suffix difference,
so a compound brand such as `MarketWise` cannot claim generic `Market`
headlines or paths. Arbitrary prefixes and substrings remain rejected. For item-level filtering,
the special two-letter case additionally requires that exact acronym in both
the title and a non-terminal article/canonical host label.

Every run stores a listing-structure fingerprint. A changed fingerprint is
audited even when the independent article checks still pass. A structure
change is therefore observable but not automatically treated as wrong; a
redesign that continues yielding valid articles remains usable.

Correctness diversity is calculated from normalized titles and the exact
sanitized body text that public persistence stores. For batches of at least
three accepted pages, fewer than 50% distinct titles or bodies blocks
publication. Unique attributes or other stripped HTML template markup cannot
make repeated boilerplate pass this gate.

The executor additionally removes an individual cluster whenever one exact
sanitized body appears under multiple distinct titles. This preserves unique
articles from a partially healthy recipe instead of either publishing the
boilerplate cluster or discarding the entire batch. Historical clusters use a
reversible `recipe-content-diversity.v2` quarantine.

Shallow editorial routes also fail article validation when their normalized
body explicitly announces `SHOWING POSTS <scope>` and exposes at least ten
links. This catches category listings even when a CMS labels the page as an
article. A short shallow route backed only by a generic paragraph cluster with
at least seven `Read More` cards is treated the same way.
Terminal media-asset download routes and short
`<topic> news and updates | <site> Blog` topic headings are shared utility
identities. Historical rows use reversible listing-artifact quarantines, and
healthy active recipes are immediately revalidated against the same runtime
rule. Slug-matched taxonomy archives, terminal article indexes, and bodies that
explicitly expose filter or newsroom-navigation collections use the same
repair path.
Generic legal-notice pages and video-library hubs are utilities as well,
including terminal `/video` or `/videos` routes and branded
`Videos | News and media | …` headings.

A publication can also become incorrect without changing hosts or selectors.
The shared `publication-topic-compromise.v1` check rejects a five-item-or-larger
sample when at least 80% contains gambling SEO signals unrelated to the
company profile. It runs during feed validation, direct evidence import,
recipe construction, and recurring feed/recipe crawls. Expected casino,
gaming, sportsbook, lottery, hotel, resort, amusement, entertainment,
payments, and prediction-market publications are explicitly profile-exempt;
one incidental gambling reference never trips the batch gate. A majority of
headlines explicitly naming the requested company also prevents retirement,
covering incomplete upstream industry profiles without trusting a first-party
hostname by itself.
Detected source rows remain private and auditable, and an active recipe fails
correctness instead of publishing the residual sample.

Deterministic article-level drift is enforced on existing data as well as new
data. When a runtime crawl rejects a URL as an obvious listing path, generic
listing title, high-link-density collection, multi-article collection, or
year archive, any previously public item with that source-local URL identity
is moved into a reversible quarantine before the accepted batch is persisted.
Transient request failures and ambiguous extraction failures do not quarantine
old content. The run metadata records `quality_quarantined_item_count`, and a
future valid normalization of the same page can release the row.

## Freshness Contract

Freshness has two independent dimensions:

- crawl freshness: whether a correct run completed within
  `crawl_interval_seconds` (12 hours by default);
- content freshness: whether the newest dated article is newer than
  `content_stale_after_seconds` (180 days by default).

The scheduler marks an overdue recipe before queuing it. A correct but quiet
publication can be `content_stale` while remaining structurally correct. This
does not rewrite the recipe automatically. It is still reported as unhealthy
freshness rather than a successful/fresh source.

`fresh` requires complete publication-date coverage for the accepted result
set. A recipe that passes every structural and article check but has any
undated accepted item is `unknown`, while its independent correctness status
remains `passing`. An old maximum date proves `content_stale` only when every
accepted item is dated; otherwise an undated item may be newer. Dates may come
from the article page or, only when that page is undated, from one unambiguous
date inside the article link's nearest listing card. Previously observed
timestamps remain stored for audit, but a later incompletely dated crawl is
classified `unknown` rather than stale.

Incorrect or empty runs increment independent streaks. At three consecutive
failures or three consecutive empty runs by default, the active recipe becomes
`stale`, `rebuild_required=true`, and leaves due-source scheduling. The next
explicit campaign selects that company again. Separate scheduled jobs are
spaced by the source crawl interval even after a failed attempt, so structural
drift requires observations over time rather than three immediate reruns of the
same page state. Retry attempts belonging to one durable job retain their own
bounded backoff policy.

Retryable transport failures are availability evidence, not recipe-structure
evidence. Timeouts, HTTP 408/429 responses, and retryable HTTP 5xx responses
record a failed run, but preserve the prior freshness and correctness states
and do not advance failure, empty, or correctness streaks. When a partial crawl
misses only the accepted-item or acceptance-ratio gate, it is also transient if
the retryable article URLs could satisfy those gates; successfully fetched
pages are not misclassified as a broken selector. The durable crawl job may
therefore use its full retry policy without making a previously correct recipe
stale. A completed fetch that exposes genuinely incorrect or empty extraction
output still advances the appropriate structural
drift streak.

Article fetches are grouped by normalized host. The campaign profile permits
up to six requests per host and eighteen total requests inside the one active
company job. Results are restored to adapter candidate order before canonical
deduplication, so a faster lower-priority host alias cannot win by completing
first. When correctness validation tries alternate path scopes for the same
publication, successful publication and archive listing fetches are cached
only for that build attempt; the crawler still re-fetches them on the next
company or later freshness crawl.

Company profiles are also part of the correctness contract. A publication on a
known homepage, investor-relations, newsroom, or blog host is first-party and
its articles need not repeat the company name in every title. The same applies
to a dedicated publication that the adapter explicitly cited and the recipe
persisted with `publication_boundary` scope. Inferred publications and shared
multi-company hosts continue to require item-level company identity.
Name-to-host matching supports legal-name cleanup and bounded brand forms, but
does not accept an arbitrary substring as ownership evidence.

For a managed trust or fund on a known asset-manager domain, item identity is
composite: a manager name such as BlackRock, Blackstone, Gabelli, Invesco,
MFS, Royce, or Sprott cannot claim an article by itself. A bounded combination
of the vehicle name must appear in the title or canonical path. This does not
constrain the manager's own operating-company profile. The shared-manager
classification overrides legacy `publication_boundary` recipe state. Each successful
revalidation also applies the current composite rule to every public
historical item for that source; off-vehicle rows move to the non-replay-safe
`company-scope-relevance.v4` quarantine.

When adapter evidence identifies articles on another verified company-profile
host, the generated recipe carries both hosts in `allowed_hosts`. A scheduled
crawl may also cross an implicit parent/child company-host boundary under the
bounded host-label policy above. Sibling product, engineering, research, and
investor publications remain separate recipes unless explicitly evidenced;
this supports ordinary multi-publication companies without weakening
third-party collection filtering. Profile hosts are not added to a recipe that
starts on an unverified third-party publication.

## Building One Company

```bash
cargo run -p feed-admin -- news-import \
  --company "Stripe" \
  --include-covered \
  --lookback-days 31 \
  --max-articles 20
```

Names and aliases drive private web research. Tickers are not accepted.
Security descriptions such as `Class A Common Stock` and
`American Depositary Shares` are removed from the search name while the exact
registry name remains an alias. An alias that exactly matches another active
company's canonical name is omitted from the adapter request so ambiguous
historical names cannot redirect one company's research to another company.
When an observed publication is later proven to belong to a different
company, the association is retained as a disabled source and non-replayable
ownership quarantine. Incorrect historical aliases are removed before the next
company-name build. That terminal stale recipe is excluded from future
`--include-covered` fallback inputs, and `get_or_create` will not reactivate
the disabled URL even if a later adapter response repeats it.
The company profile can also persist a reviewed host decision under
`metadata.publication_host_policy`:

```json
{
  "verified_hosts": ["owned-brand.example"],
  "excluded_hosts": ["different-company.example"],
  "direct_evidence_excluded_hosts": ["ambiguous-same-name.example"]
}
```

Subdomains inherit the reviewed decision. An excluded publication host cannot
produce a recipe or feed source. Direct third-party evidence remains useful
when its title or URL identifies the requested company—for example, a customer
case study—but a direct-evidence exclusion blocks same-name collisions
outright. `excluded_hosts` takes precedence over both name matching and
`verified_hosts`. A branding suffix in a company name, such as `.com` or
`.ai`, is never itself host-ownership evidence; only the registrable and
subdomain labels participate in company-name matching.
Corporate affiliation alone is not a full-publication boundary. A parent,
subsidiary, product, or commercial-arm site may still carry a broad industry
digest. The reviewed DeepAware/Silicon Valley Robotics Center repair records
the affiliation but excludes the current digest, retains its observations in
the reversible `company-scope-relevance.v4` quarantine, and requires an
operator to remove that exclusion if the site's editorial scope later changes.
Approved feed items are independent ownership evidence: a candidate whose
validated sample overlaps a distinct issuer's RSS/Atom items by at least 50%
is rejected before activation. The same comparison runs during recipe crawls
so a feed discovered later can retire an older wrong-company recipe. This
comparison groups by company name, not ticker, and preserves legitimate
same-issuer share-class records.
Every public item also passes an exact canonical-URL ownership boundary across
RSS/Atom and active recipes. When the same article identity is associated with
different name-first issuers, each association must identify its company in
the article title/path or belong to that company's first-party host. Shared
news, wire, and manager hosts always require explicit company identity.
Same-issuer security classes and dual-listed legal forms remain eligible.
An older wrong association is retained privately under the reversible
`cross-company-item-scope.v1` quarantine, while the correctly scoped
association remains live.
Candidate validation also treats an exact approved RSS/Atom URL as an
issuer-level claim. A later distinct company cannot activate that same feed;
same-issuer security classes remain permitted. Global Simply Wall St news
feeds, shared market streams, and other unrelated feed hosts must independently
prove a company-relevant majority. A reviewed verified host is treated as a
company publication boundary.

The neutral `company-news-extraction.v2` response carries publication URLs and
optional recent evidence article URLs. If the adapter returns an article
detail as a publication, the builder first derives and validates its stable
editorial parent (for example, `/blog/`) and still rejects the detail itself as
a listing. A passing parent replaces an older active detail-page recipe only
when its validation sample fully covers every public item from that recipe.
Existing high-confidence editorial candidates are merged, locale
mirrors are collapsed, and taxonomy sub-listings such as tags and topics are
excluded. Documentation and help hosts remain eligible only at an explicit
editorial listing such as `/blog`, `/news`, or a named help-center release
section; API-reference trees and individual support articles cannot become
publications. Large companies may activate several independent recipes. Those
recipes remain separate because their publication URLs differ; the
cross-company ownership guard does not collapse corporate, product,
engineering, research, or brand properties into one URL.
At every scheduled crawl, a reviewed host exclusion overrides the immutable
recipe and blocks publication. Otherwise an older explicit adapter
`publication_boundary` remains intact: issuer brands, products, acronyms, and
renamed-company domains are not always recoverable from name heuristics.
Legacy recipes without an explicit scope recover a publication boundary only
from a current company entry point or reviewed verified host; unrelated and
shared hosts remain article-level `company_identity` sources.

Default gap builds directly revalidate existing stale, rebuild-required, and
`content_stale` publication URLs, so recovery does not depend on the adapter
rediscovering a URL already stored in the database. For an operator-directed
`--include-covered` run, healthy active publication URLs are also revalidated.
A passing rebuild may activate a new immutable recipe version—for example,
after a verified cross-domain profile host is added—while the preceding version
remains active until the replacement passes. A failed rebuild does not displace
working coverage.

Suggested evidence articles and publication validation are independent. When
all cited article fetches are transiently unavailable, the job still validates
adapter publications, deterministic HTML/browser discovery candidates, and
explicitly included existing recipes. It retries the whole company only when
none of those publication paths exists. The immutable run audit records whether
it continued through this fallback.

For a company with no approved feed, plausible adapter publication URLs are
handed to the separate discovery worker through a durable seeded-discovery job.
That worker makes no second adapter call: it probes public standard feed paths
and HTML alternate links and then uses the existing candidate-validation
pipeline. This allows a previously unknown WordPress or CMS feed to replace an
overlapping HTML fallback without coupling RSS/Atom discovery to recipe
construction. The default validator admits one such feed while coverage is
HTML/browser-only and stops admitting remaining variants once an RSS/Atom
source is approved.

## Resumable All-Company Campaign

There is no timer or automatic producer for recipe construction. An operator
starts a bounded campaign explicitly:

```bash
cargo run -p feed-admin -- news-import \
  --all \
  --include-covered \
  --limit 10000 \
  --spacing-seconds 1 \
  --lookback-days 31 \
  --max-articles 20
```

Selection includes active, non-inactive companies that lack a healthy active
recipe. It therefore skips fresh coverage and naturally selects
rebuild-required or content-stale recipes on a later explicit pass. Stable
per-company active job keys make repeated campaign commands idempotent while
work is pending or running. The dedicated worker defaults to one durable
company job and supports a bounded `NEWS_EXTRACTION_JOB_CONCURRENCY` pipeline.
Public-page validation can overlap across configured lanes, while the private
adapter owns provider-level serialization and attempt-start throttling.

A completed build run may legitimately activate zero recipes. That company
remains missing and is eligible on the next explicit pass; it is not placed in
an infinite automatic retry loop.

Once the broad campaign is terminal, an operator can materialize a bounded
retry wave from its own durable audit:

```bash
cargo run -p feed-admin -- news-import \
  --retry-transient-after 2026-07-24T12:41:32Z \
  --include-covered \
  --limit 10000 \
  --spacing-seconds 1
```

This mode uses only each company's latest extraction attempt since the supplied
RFC3339 boundary. It retries explicit transient adapter, evidence-page,
publication-page, and recipe-artifact fetch failures. Permanent failures,
superseded transient attempts, zero-result companies without a transient
failure, inactive companies, and companies with an already active build job
are not selected. The same database-level claim lock keeps the retry wave
within the configured job pipeline width.

For an end-to-end operator run, the delivered supervisor performs the bounded
phases in order: initial gap drain, transient retry, one final pass over
companies that still have neither a healthy feed nor a healthy recipe, a
transient retry scoped to that final pass, activation-crawl drain, and terminal
coverage/freshness/correctness/live-item audit:

```bash
CAMPAIGN_START=2026-07-24T12:41:32Z \
INITIAL_RETRY_AFTER=2026-07-26T21:30:33Z \
  scripts/run-company-news-campaign.sh
```

The final no-feed pass deliberately omits `--include-covered`. Companies with
a healthy RSS/Atom source remain covered; the extra provider budget is spent on
the actionable no-feed gap. Zero-result companies are retried once, not placed
in an endless loop.

The terminal phase also applies and verifies the reversible cross-company item
scope audit:

```bash
cargo run -p feed-admin -- news-ownership-audit \
  --apply --fail-on-unscoped
```

An operator can immediately verify one existing recipe through its source ID:

```bash
cargo run -p feed-admin -- crawl --source-id <HTML_SOURCE_UUID>
```

Explicit HTML/browser recrawls are accepted only when that source has a healthy
active recipe. They receive high priority and use the same durable crawl and
recipe-run audit path as scheduled work.

## Observability

```text
GET /api/v1/company-news-recipe-coverage
GET /api/v1/company-news-recipes?company_id=<UUID>&status=active
GET /api/v1/company-news-recipe-runs?recipe_id=<UUID>
GET /api/v1/company-news-extraction-runs?company_id=<UUID>
GET /api/v1/companies/<company_key>/profile
```

Coverage reports approved RSS/Atom inventory separately from healthy RSS/Atom
coverage, plus active recipe coverage, their intersection and operational
union, companies still missing either healthy form of coverage, fresh/correct
and content-stale recipe counts, stale/missing recipes, active recipe count, and
build queue/run totals. Feed health requires a successful runtime crawl, zero
current consecutive failures, and fewer than three consecutive empty crawls.
Active recipe coverage likewise excludes rebuild-required and
`content_stale` recipes, matching the default `--all` rebuild selector.
The operational gap is split between companies awaiting a completed build and
companies still uncovered after a completed build; the latter is the exact
default second-pass cohort.
`companies_missing_recipe` remains the recipe-only gap, while
`companies_missing_feed_or_recipe` is the total operational gap. Job counters
distinguish pending retry work from companies whose latest durable build job is
terminally failed; a newer pending, running, or completed retry resolves that
operational failure. Run counters retain every completed or failed attempt for
audit, so a failed run does not imply a failed job. Recipe runs
expose counts, ratios, fingerprints, reasons, errors, freshness/correctness
state, and whether any items were published.
The same response also reports public normalized recipe-item count and the
current count of reversible quality-quarantined recipe artifacts, which the
live dashboard renders beside campaign health.

Public serving is recipe-state-aware. RSS and Atom items remain eligible while
their source is approved. HTML and browser items are returned by the item APIs
and exporter only when their source has an active recipe with
`rebuild_required=false`. Staling, superseding, disabling, or invalidating a
recipe therefore removes its historical items from the live product without
destroying the retained crawl and item records.

Feed publication does not bypass the shared article contract. Deterministic
CMS fixtures, legal/subscription utilities, and entries that reuse one
sanitized body under different headlines are normalization failures for
RSS/Atom sources too. Existing rows found by the same rules are reversibly
quarantined, while a source confirmed to belong to a different company is
disabled and retained privately for audit.
Migration 0134 applies that contract to generic legal-notice/video hubs and
records the confirmed Infina WordPress casino-SEO incident under
`publication-topic-compromise.v1`. The two affected blog sources are disabled,
their HTML recipe is stale, and all observed rows remain private for a later
operator-confirmed recovery.
Migration 0135 extends the reviewed incident repair to the compromised CEL-SCI,
Cerus, Reitar, and Infina-root sources, and classifies Discourse `/discuss/`
publications as non-editorial forum scope. RADCOM's exact observed compromise
window is quarantined without disabling its recovered feed; its ten clean
company articles are retained and the source is immediately revalidated.
Migration 0136 quarantines the one residual Discourse thread formerly retained
under a stale blog source and records RADCOM as recovered only after that
dedicated revalidation crawl completes successfully.

An adapter citation is not sufficient by itself to grant
`publication_boundary` scope to an unrelated domain. A publication host must
match the company identity or one of its known web entry points; otherwise the
recipe remains `company_identity` scoped and its article sample must identify
the requested company. This protects against same-name companies, historical
brand names, and plausible but unrelated dedicated blogs.
When a historical alias is proven to identify a distinct current company, the
terminal repair also removes that ambiguous alias from later adapter requests
and rejects existing candidates on the conflicting domain.
An exact historical direct-evidence row on a shared host can be terminally
quarantined without disabling the whole shared origin; current imports already
apply the article-level company-identity gate before persistence.
A relevant customer case study can likewise remain as direct evidence while an
over-broad recipe for the publisher's full blog is retired. Direct evidence is
article-scoped and never proves ownership of its containing publication.
