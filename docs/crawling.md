# Crawling

Crawling runs only on approved public sources and is owned by `feed-worker`.
Discovery and validation workers cannot claim crawl jobs.

Crawling normally runs as periodic durable jobs. `feed-admin crawl` queues the
same job type for immediate operator-triggered work. A specific HTML/browser
source may be recrawled with `--source-id` only when it has a healthy active
recipe; omitting `--source-id` retains the bounded RSS/Atom bulk behavior.

Public fetchers use one shared, identifiable user agent by default. Operators
should set `PUBLIC_FETCH_USER_AGENT` to include a monitored deployment contact.
The same identity is used consistently by feed, discovery, listing, and article
fetches.

## Delivered RSS/Atom Adapter

Use this whenever possible. It is cheap, stable, and transparent.

Responsibilities:

- fetch feed URL
- parse RSS or Atom
- reject sitemap resources even when a CMS serializes its URL inventory with
  RSS syntax; a sitemap is not an editorial feed
- refuse to publish a batch of at least five items when every normalized title
  is identical; the crawl fails with backoff so a feed that drifts into
  framework/test content cannot contaminate the public stream and can recover
  on a later healthy crawl
- reject short CMS starter posts only when both their localized placeholder
  title and “first post” template body match; substantive launch articles named
  “Hello World” remain valid
- refuse feeds with explicit reply/topic/operational endpoints or a bounded
  latest-item sample dominated by documentation, forum, or comment URLs;
  Discourse `/discuss/` feeds and `/discuss/t/` entries are always
  non-editorial, while press-release PDFs and independently editorial
  community blogs remain eligible
- require a company-relevant majority on known shared press-wire and
  market-news hosts, then publish only matching items; a provider-wide global
  feed therefore cannot be copied into every company that linked to it
- treat the parser-detected payload type as authoritative when a declared MIME
  type is wrong, updating the source and item provenance in the crawl transaction
- derive external ID from GUID, URL, or title/date hash
- preserve feed metadata in `raw`
- emit `RawCrawlItem`

## Delivered HTML Recipe Adapter

Validated `company-news-recipe.v1` sources are scheduled through the same
`crawl_source` job type as feeds. The generic HTTP executor fetches the listing,
applies bounded host/path/selector rules, fetches each resulting article page,
and publishes only when the complete correctness contract passes.

Recipe correctness includes minimum discovered and accepted counts, acceptance
ratio, title bounds, future-date rejection, substantive sanitized bodies,
cross-item title/content diversity, and structure-fingerprint audit. A live
HTTP 200 listing with zero or wrong links is therefore a failed correctness
run, not a successful empty crawl. Three
consecutive failures or empty runs stale the recipe by default and remove it
from scheduling until an explicit rebuild. The public API and exporter also
require every HTML/browser item's source to retain an active,
non-rebuild-required recipe, so older rows from a superseded or drifted source
stop being live immediately while remaining available for audit. A runtime
crawl that proves at
least 50% overlap with an approved feed, at least three items and 80% overlap
with a preferred active recipe, or a later duplicate publication identity is
superseded immediately because a healthy replacement already exists.
A recipe sample with at least 50% overlap against an approved RSS/Atom feed
owned by a distinct issuer is also rejected immediately; stripped name-first
issuer identity keeps multiple security classes from becoming false
conflicts.
An official corporate affiliation is not enough to make a broad industry
digest a company publication. Reviewed host policy may preserve the
relationship while excluding the digest until its editorial scope is reviewed
again.
Feed candidates apply the same issuer-level canonical-article claim before
activation. A related-domain feed whose entries mostly escape to unrelated
article hosts must also pass the per-article company-scope majority unless its
feed title identifies the requested company; a stale corporate endpoint
cannot borrow ownership from its old hostname after being repointed.

The crawler retains a bounded one-day future-skew allowance so timezone or
date-only metadata does not make an otherwise correct recipe fail. Public
listing APIs and exports nevertheless withhold a future-timestamped item until
its timestamp arrives. `GET /api/v1/news-items?include_future=true` is the
explicit operator preview for scheduled or suspicious future rows.
Approved feeds repeat the canonical-article claim on every crawl, so a later
CMS repoint or tenant mix-up fails before wrong-company rows are normalized.
An inferred or shared-host recipe whose article sample is less than 50%
company-relevant is likewise superseded immediately. Wrong-company content
therefore leaves the public API after one conclusive crawl instead of waiting
through the ordinary layout-drift failure streak.

Generic article fetching uses bounded concurrency across and within normalized
hosts. The campaign profile permits at most eight requests to one host and
twenty-four requests total within each active company lane. Fetch completion order never
changes adapter candidate priority when canonical duplicates are collapsed.
This shortens multi-article verification without introducing site-specific
parsers or host allow-lists. Alternate path-scope checks within one manual
company build reuse successful publication and archive listing fetches, while
later jobs and scheduled freshness crawls always start with an empty cache.
Short media/navigation labels such as `Photos & Videos`, a terminal
`Timeline` hub, and `View more from …` collection headings are rejected by
the shared item policy before normalization. A shallow editorial child page
whose independently observed listing label is `All <page title>`, whose title
matches its terminal URL segment, and whose body contains multiple article
elements is likewise a collection rather than an article.
The same policy rejects exact static investor labels such as
`Financial Highlights`, `Leadership Team`, `Shareholder Remuneration`, and
`Events & Financial Calendar`. On conventional `ir.`, `ri.`, `investor.`, or
`investors.` hosts, bounded governance, overview, shareholder, stock, and
financial-information namespaces are utility pages unless the URL also has an
explicit news, press, blog, research, or update segment.
Market quote/profile pages are also non-editorial when a bounded
`equities`, `quote`, `market-activity/stocks`, or equivalent namespace agrees
with a stock/share price-or-quote page title. The combined path-and-title rule
preserves equity research, press releases about share prices, and substantive
articles nested below a market site's news route.
Legacy CMS detail URLs may retain a bounded resource identifier such as
`item`, `content_id`, `newsid`, or `release_id` on a listing-shaped path. A
safe nonempty resource query is treated as the article identity; empty,
unsafe, filter-only, locale, analytics, and pagination queries do not promote
a collection into an article.

The shared item policy also rejects conservative navigation and placeholder
identities such as `No title`, `Test`, `Homepage`, `Previous`, `Next Page`,
operational exchange pages such as `Corporate Actions` and `Market Notices`,
static utility pages such as comment policies, corporate fact sheets, press
offices, status hubs, `Who We Are`, and contributor solicitations. An undated
topic page whose short title matches its terminal slug is also rejected when
it is thin or navigation-link dense; a dated article or a substantive
low-link undated post is preserved. A short undated title supplied only by its
listing anchor is likewise rejected at collection-like link density; page-owned
headings and publication-date evidence do not use that fallback.
Common collection labels such as `All Articles`, `Clinical Case Studies`,
`Fixed Income`, `Insights Library`, `New Product Announcements`,
`Other Archives`, and `People and Culture`, plus media-request, sign-up, and
gallery utilities, including submit-media-request variants, cannot become HTML
recipe items.
For ambiguous corporate publication roots such as `/engineering`, `/innovation`,
or `/technology`, a validation sample is narrowed to an explicit child
namespace such as `/articles/` only when at least five items and a strict
majority of the accepted sample share that child. The same filter runs during
scheduled crawls, so mixed service/product links are quarantined while
publications that place posts directly below the root remain unchanged.
`Blog Post`, `Guides & Articles`, `General Information`, branded
`<brand> Blogs | <section>` category hubs, and short `About <brand>` pages.
Terminal media-download collections such as `press-assets`, `brand-assets`,
`materials-for-media`, and photo or b-roll libraries are utilities as well.
Broad resource and library hubs can mix editorial material with event,
webinar, training, video, demo-series, product-tour, and testimonial pages.
Those non-editorial subsections are rejected unless the URL carries a more
specific editorial scope after the resource/library boundary, such as `blog`,
`news`, `press`, or `research`. A broad parent segment such as `/insights/`
does not convert a product utility below `/resources/` into an article.
Short `<topic> news and updates | <site> Blog` headings identify topic indexes,
not individual posts. Slug-matched `<topic> Archives - <site>` headings,
terminal section routes such as `latest-articles` and `research-library`, and
pages whose body is an explicit filter or newsroom-navigation index are
collections under the same shared policy.
Accessibility suffixes such as `opens in new window` are removed before this
generic-title check, so utility links cannot evade the rule through template
wording changes.
Because normalization applies the same rule to RSS/Atom and HTML, a feed cannot
reintroduce an artifact removed from a recipe crawl. The normalizer also strips
an unambiguous trailing React `self.__wrap_n!=...` payload when framework data
leaks into an otherwise valid article title; ordinary editorial references to
`self.__wrap_` remain intact.

The effective host/path scope is enforced twice: once when a listing link is
selected and again after the article fetch. Both the final response URL and the
page canonical URL must remain inside the recipe boundary. This prevents a
formerly editorial route that now redirects into a support/documentation tree
from silently publishing unrelated pages.

Listing DOM extraction runs outside the asynchronous request executor so a
large official publication cannot starve health responses or durable-job lease
heartbeats. Title, date, and release-document fallback evidence is inspected
only inside a bounded card-sized ancestor; page-sized navigation or collection
containers are never rescanned for every link and cannot donate a global date
to an otherwise undated article.

Recipe freshness is distinct from correctness. The scheduler marks a recipe
overdue using its own crawl interval. A correct crawl is recorded as
`content_stale` only when every accepted item has publication-date evidence and
the newest date is older than the content-stale threshold. If any accepted item
is undated, freshness is `unknown`: an older dated item cannot prove that the
undated content is also old. Previously observed dates remain stored for audit,
but incomplete date coverage does not turn them into staleness proof. See
[Company news crawl recipes](company-news-recipes.md).

Retryable transport failures such as timeouts, HTTP 408/429, and retryable HTTP
5xx responses remain visible as failed crawl runs, but preserve prior freshness
and correctness and do not advance structural failure, empty, or correctness
streaks. A partial run that could clear its accepted-item and ratio gates by
retrying transport failures is handled the same way. Those counters advance
only after the crawler received enough response evidence to judge the recipe
itself. This keeps durable job retries from incorrectly staling an otherwise
correct recipe during temporary upstream throttling or outages.

After any completed source attempt, successful or failed, the producer waits
for that source's crawl interval before creating a new scheduled job. Retries
inside the same durable job still use bounded retry backoff. Consequently,
multi-run correctness streaks represent observations across crawl intervals,
not a tight scheduler loop over one unchanged upstream response.

## Delivered Manual Public-Article Bootstrap

For an operator-selected company, the separate import worker uses:

```text
RSS/Atom preferred -> URL-only private suggestion -> public article-page fetch
```

The article-page crawler is delivered. Its responsibilities are:

- fetch a suggested individual article page over HTTP with redirect, SSRF,
  timeout, and byte limits
- fetch up to the configured per-host article-page limit (six in the campaign
  profile) while preserving adapter candidate priority and the configured
  global request-concurrency ceiling (twenty-four in the campaign profile)
- require generic article semantics and extract title, date, canonical URL,
  preview, and article body
- pass article-body HTML to `feed-content` for sanitization, Markdown
  conversion, and text extraction
- reject listing-like or thin pages below the configured content threshold

Redirect handling repairs one narrow class of broken publisher configuration:
when an HTTPS page redirects to the same host over HTTP, the crawler preserves
the target path/query but keeps HTTPS. Cross-host redirects and all other
scheme changes remain unchanged and must pass the ordinary redirect and SSRF
checks.

One bounded exception covers official document-backed releases. If an
independently fetched publication card contains both an HTML detail link and a
same-title PDF link on an allowed organizational host, a thin or empty detail
shell may fall back to that PDF. The normalized item keeps the stable detail
URL as its external identity, links to the actual document, and records
`official-listing-document.v1` provenance. Unrelated documents elsewhere on
the listing and document links without an exact same-card title match do not
qualify. A direct PDF cannot act as both the stable article identity and its
own fallback document.

Accepted evidence pages are grouped into sources by their actual public origin.
Publication URLs are separately validated into bounded generic recipes; the
system does not contain one handwritten parser per company. See
[Manual company news bootstrap](manual-company-news-import.md).

Generic semantics include explicit article markup/metadata and an
article-like URL path plus `<h1>`. The latter supports sites that emit only a
`<main>` container while excluding category, tag, author, archive, search, and
pagination paths. The generic path vocabulary covers common newsroom variants
such as `news-releases`, `press-releases`, `investor-news`, `media-center`,
`announcements`, `what-s-new`, `blog-posts`, `posts`, and `changelog`; this is
shared policy rather than a company-specific parser. Article headlines must
also be more specific than a section or utility label such as
`Archives`, `Calendar`, `Insights`, `Results`, `Subscribe`, or `Webinars`; a
real detail page can instead fall through to its independently declared social
or document title. Date-only headings and short site-name fragments ending in
a bare separator are likewise rejected. A heading whose DOM separates every
letter into its own text node falls through to intact social/document metadata.
Soft-404 and `Coming Soon` headings are not article titles.
Dedicated editorial
subdomains such as `blog.`, `updates.`, and `engineering.` may also expose
multi-token article slugs directly at their root. Main corporate-site root
slugs do not receive that exception. Explicit CMS paths such as `/category/`
and `/cat/` remain taxonomy, but a semantic word such as `search` is rejected
as a utility only when terminal; it may appear inside a genuine hierarchy such
as `/products/search/<article>`. Nested collection roots and terminal `index` or
`default` documents are not articles. The obvious-listing-path gate evaluates
both final and canonical URLs for recipe links and direct evidence URLs, so
misleading `Article` markup on an archive or publication root cannot bypass it.
A normalized exact recipe host always remains eligible. Implicit company-family
host expansion admits parent hosts and ordinary child hosts, but blocks
non-production labels (`preview`, `staging`, `uat`, and similar labels).
Implicit documentation, developer, help, support, and tutorial hosts must carry
an editorial path token such as `changelog`, `news`, `press`, `release`,
`research`, or `updates`. An exact evidence-backed host is not demoted by this
generic boundary. Sibling publications should be represented explicitly as
separate recipes.
A four-digit terminal path is
also rejected when its title identifies a year archive, including explicit
archive wording, `Press Releases in YYYY`, short branded newsroom labels, and
`YYYY - <brand>` archive headings. Numeric article IDs and substantive
year-in-review articles remain valid. A candidate that redirects or
canonicalizes to the publication listing is rejected explicitly. Tracking,
locale, taxonomy, pagination, and arbitrary scanner parameters do not create a
new resource. Canonical identity retains only a bounded, provider-neutral
vocabulary of resource keys such as `content_id`, `newsid`, `post_id`, and `p`,
with nonempty safe scalar values, so legacy `index.php?content_id=...` articles
remain crawlable without treating filter pages as articles. Generic
CMS templates that incorrectly emit the same site-root canonical on every
article are repaired only when the final URL is an article-detail path on that
host and the listing independently supplied a usable title; the replaced
canonical remains in item provenance. Non-root listing canonicals and direct
pages without that independent evidence remain rejected. A malformed declared
canonical whose parsed host is literally `http` or `https` is replaced by the
independently fetched final URL and retained in provenance; this cannot bypass
the article checks. H1 elements explicitly marked as archive, category, or
taxonomy titles remain collections even when their template emits OpenGraph
Article metadata. Generic
listing titles such as `Press Releases`, category labels such as
`Developer Spotlight` or `Solution Briefs`, framework chrome such as
`Release Details`, `Image link`, or `Arrow icon`, collection labels such as
`Guides & Articles`, `General Information`, or
`<brand> Blogs | <section>`, short `Contact <brand>` utility labels, and the
exact CMS placeholder `Headline` cannot become items.
When a real detail page carries that chrome, independently observed listing
evidence or a usable structural headline can replace it. The extractor
prefers the most substantive H1 within the narrowest available structural
scope (`article`, then `main`, then the document), so a longer site-header logo
cannot displace an article headline. It rejects embedded SVG/CSS rules as title
text, uses a substantive listing-link title when page metadata is generic, and,
when investor-site framework chrome such as `News Details` or
`News Release Details` occupies the H1/title, accepts exactly one usable
semantic headline from `itemprop=headline`, a titled H2/H3, or an article
header. When one narrow H1 is an unrelated recommendation card, two agreeing
metadata titles may replace it only if their distinctive terms also match the
article URL path. It also ignores H1 candidates inside hidden subscription modals and
accessibility-only chrome, and repairs repeated site-wide titles from
independently observed link titles. For
generic CTA links such as `Read More`,
one usable headline may be recovered from the bounded card or its immediately
preceding sibling; the CTA itself is never title evidence. A descriptive CTA
of the form `Read more about <headline>` is normalized to the bounded
`<headline>` rather than exposed as public title text. A three-or-more-item
result with less than 50% distinct normalized titles or less than 50% distinct
sanitized body text fails correctness rather than publishing. The
body-diversity gate uses the same content processor as public normalization, so
unique HTML attributes and other stripped template markup cannot make
identical boilerplate appear distinct. It catches soft-404 and SPA catch-all
routes that serve the same homepage for many apparent article URLs. A shallow
editorial route whose normalized body explicitly begins with
`SHOWING POSTS <scope>` and contains at least ten links is likewise a
deterministic category collection, independent of its page metadata.
When one sanitized body is reused by multiple URLs with different titles, the
executor rejects every member of that ambiguous cluster before applying batch
correctness. Unique articles from the same recipe remain eligible. A shallow,
short-title editorial route with no article element and at least seven
`Read More` cards is also rejected as a collection.
For a recipe inferred outside the company's known or name-matched domains, or
one hosted by a known shared multi-company news service, the executor filters
articles whose title and canonical URL do not identify the company by its name,
aliases, distinctive name terms, or name-derived acronym. If fewer than half
of the otherwise accepted batch is company-scoped, the run publishes nothing.
Narrative alias annotations do not contribute identity words: transition text
such as `formerly`, `in process of incorporating as`, `due to trademark`, and
other non-distinctive short connectors is excluded before title matching.
Digit-bearing brands remain eligible, while a generic two-letter word cannot
claim an unrelated shared publication.
Dedicated publication listings explicitly returned by the research adapter
persist `publication_boundary` scope only when their host is tied to the
company identity or one of its known web entry points. Unrelated dedicated
domains remain `company_identity` scoped, just like known shared
multi-company hosts. This allows verified public-brand properties without
letting a plausible same-name or historical-brand match assign another
company's blog wholesale. Adapter-generated recipes created before this field
was persisted recover a boundary at runtime only under the same host rule.
If a historical alias is proven to identify a distinct current company, its
terminal ownership repair rejects existing candidates on that domain and
removes the ambiguous alias from later adapter requests.
Legacy adapter recipes that omitted a path scope also recover the normalized
listing directory at runtime unless their stored article evidence proves that
the publication intentionally links outside it.

The manual recipe builder applies the same identity vocabulary to each direct
article suggested on a known shared news host before it creates or updates an
HTML evidence source. Unlike recipe activation, direct evidence does not need
a majority batch because one correctly scoped article is still useful; every
off-company article is independently rejected. Short URL-only tokens are not
identity proof on aggregators (for example Quiver Quantitative `/news/Art`);
short exact company names and acronyms still match when they occur in titles.
When the fetched page declares a canonical URL, that canonical URL replaces the
adapter-supplied request path as URL identity evidence. Shared release
platforms often route only by numeric ID and ignore an arbitrary trailing slug,
so a fabricated requested slug cannot override an unrelated canonical
headline.
Rows imported before this gate remain private until a later scoped replay
proves and releases them.
Terminal `default` and `index` documents resolve to their containing directory.
Owned-host recognition includes complete-label legal-brand acronyms and
compact name-derived brands, plus bounded short-brand forms such as
`joinroot.com`, while remaining independent of listing tickers.
Link collection scans a bounded candidate pool and ranks strong detail paths
ahead of shallow navigation links before applying the recipe limit. This keeps
sector menus on organization-scoped press-wire pages from consuming the entire
article budget. Plural or prefixed taxonomy paths, terminal media-coverage
pages, explicit navigation hubs such as event, webinar, social-media,
`why-invest`, pillar, and tagged indexes, author/user profiles, month archives,
and generic investor-relations destinations such as subscription and email-alert pages,
calendars, governance, filings, media resources, and contact pages are rejected
as navigation rather than treated as articles. Media-asset utilities such as
terminal press/media/brand kits, photo/logo request forms, and short logo-use
guidelines under media libraries are rejected even when a newsroom listing
links to them through a terminal `default` document. HTML RSS subscription
directories are also utilities; this does not affect RSS/Atom feed ingestion.
CMS social-feed records, explicit job directories, and
legacy `career_...` job slugs are likewise non-editorial unless an explicit
blog/news root proves article scope. Numbered test posts, explicit
please-ignore fixtures, CMS multi-asset/pagination samples, and self-titled
collection roots are quarantined without blocking substantive articles whose
subject legitimately begins with “Test.” Exact `/list/` category paths plus governance-document
and white-paper collection roots receive the same treatment. Localized category
routes, contributor profiles, CMS `content-type`, `type`, `label-name`, and
`production-platform` taxonomies, `/blog/hub/` collections, terminal `P10` or
`page-10` pagination, and scoped news-wire `latest-news/*-list` category pages
are also excluded. A generic `hub` segment alone is not sufficient: an
individual `/hub/blog/<article>` URL remains eligible. The same gate recognizes
short branded section titles such as `Acme Research` or `Acme in the News`,
including templates that
incorrectly emit OpenGraph `Article` metadata. Branded collection labels such
as `Acme Blogs`, `Acme Stories - Newsroom`, and
`Acme Glossary`, plus short navigation labels such as `View our Webcasts` and
localized `Voir tout`, are handled by the same collection guard. A weak-signal
page with a short title, at least 20 links, and at least 15 links per 1,000 content
characters is also rejected as a high-link-density collection.
When an undated page has only weak path evidence and requires the generic
paragraph-cluster body fallback, a bounded lower density threshold applies
only after at least 50 links. This catches large category bodies without
penalizing dated or semantically marked articles.
Locale publication roots such as `/newsroom/fr/` require page-level collection
evidence before rejection, so a genuine short article slug such as `/blog/ga`
remains valid. Breadcrumb-derived media-library titles such as
`Newsroom Media Manufacturing` and short taxonomy titles ending in a bounded
aggregate count are rejected even when a collection template emits Article
metadata.
Shallow editorial paths are also rejected when a short listing label resolves
to a thin generic paragraph body surrounded by at least ten article cards, or
when the selected body contains the common `Featured Articles` plus
previous/next pager chrome. This remains structural: a substantive article
body with related cards is preserved.
The same collection gate catches a taxonomy page whose own generic heading
matches its terminal URL slug but whose first card headline is supplied as a
listing-title repair. A genuine detail URL with generic site chrome does not
match the repaired heading to its own slug and remains eligible.
Independently of path depth, a page with at least ten `<article>` elements is a
card grid when none contains an H1 and even the largest card remains below 1,000
characters. Accepted items retain the H1 count and largest-card size in raw
extraction provenance so this decision remains auditable.
A thin selected body is also a card grid when at least four of ten or more
separate `<article>` elements contain their own H1. This catches collection
templates that misuse article metadata on every card without penalizing a
substantive primary article surrounded by related-card headings.
Terminal `YYYY/MM` paths are always monthly archives, even when their page title
is malformed or names a different date. Year-named collection slugs such as
`2024-news-archive`, `press-releases-2020`, and `2020-press-releases` are
archives as well. A publication listing whose first
links are generic year/month archives is unwrapped one level for at most the
first two archives, using the same selector, allow-list, and total article cap;
direct current articles remain first. Media and investor labels such as
`Presentations`, `Webcasts`, `Media Gallery`, `Success Stories`, `Publications`,
`Webcasts & Presentations`, `Annual Reports & Proxies`, `Contact Info`, and
`Shareholder Services` are navigation, not articles. A page containing
multiple `<article>` cards must also provide item-level published-time,
OpenGraph Article, Article JSON-LD metadata, or an independently observed
listing title paired with a substantive semantic body. An article-like detail
path plus H1 may also disambiguate cards when the semantic body is substantive
and contains at most one `<article>` descendant. Otherwise the card grid is
rejected before its first card can be mistaken for the requested page. A short
non-numeric title that matches the terminal URL slug is still treated as a
taxonomy/card-grid page when the selected body is only a thin card or when at
least ten cards remain at collection-like link density. This catches CMS
category pages that stamp page-level Article metadata and a card date onto
every taxonomy route while preserving a substantive individual article with
related cards. Explicit `filter-blog-*` and `category.*` routes are taxonomy
paths regardless of misleading metadata. After those independent article
signals pass, body selection also recognizes standard
CMS containers such as `articleBody`, `article#article-content`,
`article-content`, `entry-content`,
`post-content`, HubSpot-style `blog-post-content`, and Webflow rich-text blocks.
Framework-neutral fallbacks cover Elementor and Divi post content, Joomla
article bodies, Gatsby and component `ArticlePage`/`RichText` content,
single-blog WYSIWYG bodies, Framer's named and nested Content/Blog/Body Content rich-text
regions, including a complete Framer Content wrapper when prose is split
across sibling rich-text nodes, or the largest substantive Framer rich-text
container when the page omits semantic names, HubSpot post-body roots, Sitecore
`field-content`, Drupal body fields,
Tailwind prose, Chakra containers, conventional rich-text containers, AEM
body-copy/article roots, React `Blogpost_body` component roots, BEM-style
press-release/rich-text bodies, DNN/EasyDNN `.main_content` detail bodies,
custom-code modules, and a unique `#content` root. Thin or empty matches from
an earlier generic selector cannot shadow a later substantive body container;
selector precedence selects the first match whose sanitized text clears the
content floor. Raw CSS, scripts, forms, and other sanitizer-dropped markup do
not count toward body qualification, so CMS configuration nodes cannot shadow
real prose. When no known wrapper qualifies, a bounded paragraph-cluster
fallback can select a low-link, paragraph-rich `div` or `section`; it excludes
page chrome and multi-card containers and requires more content than an
explicit semantic body. With strong page-level article metadata, a semantic body at least twice
as substantive replaces one unrelated navigation-card `<article>`. Editorial
`/journal/<article>` paths are recognized alongside
blog, newsroom, and press-release paths. These fallbacks run only after
independent article semantics pass; taxonomy/listing pages cannot qualify from
a body class alone. Framework media wrappers marked
`<article role="presentation">` or `aria-hidden="true"` are not counted as
article semantics or selected as article bodies.

Adobe AEM SPA article shells may declare a same-origin page
`*.model.json` resource with `rel=preload` and `as=fetch`. If the public HTML
passes metadata/path checks but has no usable article body, the crawler may
fetch that declared resource under the same DNS, redirect, size, timeout, and
private-network policy. It follows AEM `:itemsOrder`, keeps rich-text
components in an editorial/main-content context when available, injects only
that bounded HTML into the normal sanitizer, and records
`framework_fallback=aem-model-json.v1`. Cross-origin model URLs, undeclared
resources, scripts, selectors, and executable component data are never used.

Standard Next.js detail shells may expose article data in
`script#__NEXT_DATA__[type=application/json]`. After ordinary HTML extraction
fails for an article-signal/body/content reason, the crawler parses but never
executes that JSON. It walks at most 64 levels and 50,000 values, accepts only
an object whose bounded `slug`, `id`, `path`, or `url` field exactly matches
the fetched URL's terminal identity, and requires a usable title plus
substantive rich HTML in the same object. A bounded plausible date from that
object may supply freshness. The injected article remains under the response
byte ceiling, passes the normal sanitizer and correctness checks, and records
`framework_fallback=next-data-json.v1` with its matched field provenance.
Unmatched listing-array objects and arbitrary embedded application state are
ignored.

SvelteKit detail pages that server-render only a loading shell may expose the
same public route data at `<article-path>/__data.json`. The crawler recognizes
the SvelteKit bootstrap markers before making that same-origin request, applies
the ordinary DNS, redirect, timeout, and response-byte policy, and decodes the
bounded reference-table JSON without executing JavaScript. A candidate content
object must carry an exact same-origin `url`/`path` identity (or exact terminal
`slug`/`id`), a usable title, and substantive rich HTML reachable through
content-like fields. Traversal is capped at 64 levels and 50,000 references;
unmatched route records are ignored. Successful recovery passes the normal
sanitizer and records `framework_fallback=sveltekit-data-json.v1`, the resource
URL, matched fields, and optional publication-date provenance.

Image-only listing links can contribute a headline from an explicit anchor
`title`/`aria-label` or exactly one usable descendant image `alt`; an
unlabeled empty overlay remains titleless and must pass page-level validation.
Publication dates are read from standard OpenGraph/meta fields, `<time
datetime>`, and Article/NewsArticle/BlogPosting/LiveBlogPosting/
SocialMediaPosting JSON-LD. Timezone-less ISO and SQL-style CMS timestamps are
interpreted as UTC. Generic `publish-date`, `published-date`,
`publication-date`, and underscore variants are accepted as explicit meta
fields. Full English month dates, dotted abbreviated forms such as
`Dec. 3 2015`, and CMS timestamps such as `16-Jun-2026 06:05:22` are normalized
consistently; the same date-only forms are rejected when they appear as a
supposed article title. A visible element whose
class/id conventionally denotes `publish-date`, `published-date`, or
`post-date`, or whose visible text explicitly begins with `Published on`,
`Published`, `Posted on`, or `Posted`, can also contribute exactly one date
only when it shares a small non-page-chrome wrapper with the page H1;
related-card dates do not qualify.
Parsed dates must fall between 1990 and two calendar years after the crawler's
current year; truncated two-digit years and epoch-sentinel metadata are
discarded rather than distorting freshness. RSS and Atom entry dates use the
same plausibility window. If the article page is undated, the crawler can use
exactly one parseable date found in the link's nearest listing card; this
lower-priority evidence never overrides an article page date and still
participates in the content-freshness contract. On an already article-like
detail path, exactly one full date among the first eight text nodes of its
qualified semantic body is also accepted as a page fallback; it is never
counted as an independent article signal.
An OpenGraph-qualified article may use a Yoast-style WebPage
`datePublished`; an unqualified WebPage timestamp remains ignored.

## Optional Browser Adapter Contract

Use only when public pages require JavaScript rendering.

Constraints:

- no logged-in profiles
- no paywall bypass
- no private browser state
- browser endpoint must be explicitly configured
- recipes must live under public recipe directories only

The portable recipe schema represents `browser` render mode, but the delivered
executor currently activates `http` recipes only. A configured browser adapter
must return the same `RawCrawlItem` and correctness evidence as every other
adapter.

Rendered article HTML must still pass through `feed-content`. Browser crawling is only a rendering fallback; it is not a separate content-cleaning policy.

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

## Periodic Scheduling

Each approved source has a freshness SLO. RSS/Atom sources and HTML/browser
sources with a healthy active recipe are eligible. The scheduler periodically
creates or claims `crawl_source` jobs when:

```text
now >= last_success_at + freshness_slo
and now >= backoff_until, if backoff exists
```

Sources with no successful crawl are due immediately after approval.

Deterministic validation activation also queues an initial high-priority crawl,
so a newly approved source can become visible without waiting for the recurring
scan.

RSS/Atom batches use the same content-diversity invariant as HTML recipes.
When one sanitized body is reused by differently titled entries, those entries
are retained in the raw crawl audit with
`quality quarantine: repeated_sanitized_content` and are not normalized into
public items. Candidate validation rejects a feed when at least half of a
five-item-or-larger sample has this failure shape. This blocks catch-all,
template, and unrelated example feeds while preserving a feed with an
occasional malformed entry.

First-party ownership is not sufficient when a publication itself is
compromised. Candidate validation, recurring RSS/Atom crawls, manual evidence
imports, recipe construction, and recurring recipe crawls share a bounded
topic-compromise check. For a company profile that does not identify a
gambling, gaming, resort, entertainment, amusement, payments, prediction
market, or related publication, a sample of at least five items is rejected
when at least 80% combine casino/gambling terms with wager, promotion,
casino-game, or known casino-SEO campaign signals. A detected active source is
failed unless at least half of the sampled headlines explicitly identify the
company; this preserves a legitimate gaming-adjacent issuer whose imported
industry label is incomplete. Its currently public rows are retained in the reversible
`publication-topic-compromise.v1` quarantine. Confirmed incident sources can
then be disabled without treating their valid hostname as company-ownership
evidence for the compromised content.

Disabling an accepted source removes it from due-source selection and cancels
its pending crawl jobs. A crawl already holding a valid lease may finish, but
normal item and export queries exclude content from disabled sources.

The job runner must be idempotent:

- re-running the same source should not duplicate feed items
- shutdown, lease loss, and handler errors close in-flight crawl and recipe
  audit rows as `cancelled`
- a reclaimed job cancels any abandoned attempt before opening its replacement
- a stale attempt cannot complete after its crawl-run row has been closed
- source cursors are committed only after item upserts complete

Each adapter first persists its output in `raw_crawl_items`, keyed within the crawl run. Normalization status and errors live on that row. This makes a fetched batch replayable after a crash and allows `normalize_backfill` jobs to retry without hitting the public source again.

## Zero-Run Health

A successful crawl that returns zero items is not a transport failure, but repeated zero runs may mean the source layout changed.

Track:

- consecutive zero runs
- average items per run
- last nonzero run

The `/review` dashboard joins this state with company and source records. It
shows healthy, failing, empty, and awaiting-first-crawl sources, stored item
counts, latest article dates, and an audited disable action.
