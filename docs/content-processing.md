# Content Processing

The `feed-content` component converts public company-news HTML into a stable content representation for normalization and export.

This is intentionally separate from crawling. Crawlers fetch bytes and discover article URLs; `feed-content` decides what HTML is safe to keep and how that HTML becomes Markdown.

## Scope

Inputs:

- raw HTML from RSS/Atom content fields
- raw HTML from static article pages
- rendered HTML from the public `pwright` adapter

Outputs:

- `clean_html`: sanitized HTML with unsafe tags, event handlers, styles, scripts, navigation, and private browser state removed
- `markdown`: deterministic Markdown for GitHub archive export and downstream reading
- `text`: plain text for hashing, search, and lightweight previews
- `metrics`: stripped element count, stripped attribute count, link count, and image count

## Contract

```rust
pub struct ContentProcessOptions {
    pub base_url: Option<String>,
    pub keep_images: bool,
}

pub struct ProcessedContent {
    pub clean_html: String,
    pub markdown: String,
    pub text: String,
    pub metrics: ContentProcessMetrics,
}
```

The processor must be deterministic. The same HTML and options must produce the same `clean_html`, `markdown`, and `text`, so content hashes and Git exports remain stable.

## Sanitizer Policy

Drop complete subtrees for:

- active content: `script`, `style`, `noscript`, `iframe`, `svg`, `canvas`, `object`, `embed`
- form and interaction chrome: `form`, `input`, `button`, `select`, `textarea`
- page chrome: `nav`, `header`, `footer`, `aside`

Allowed attributes are intentionally narrow:

- `a[href]`, only `http` and `https`
- `img[src|alt|title]`, only when image retention is enabled
- `time[datetime]`

Relative URLs are resolved with `base_url`. Unsafe schemes such as `javascript:`, `data:`, and `file:` are dropped.

## Markdown Policy

The Markdown converter is owned by this project rather than delegated to a generic HTML-to-Markdown crate. Company newsroom pages need predictable archive output, and generic converters tend to preserve too much page chrome.

Supported first:

- headings
- paragraphs and line breaks
- links
- emphasis and strong text
- unordered and ordered lists
- blockquotes
- code blocks
- optional images
- basic table text fallback

Unsupported or unknown tags are unwrapped when safe, or dropped when they are active/chrome content.

## Pipeline Placement

```text
crawler adapter
  -> raw crawl item
  -> feed-content process_html
  -> normalizer
  -> feed_items
  -> exporter
```

RSS/Atom adapters should process embedded item HTML when present. Static HTML and browser adapters should process the article page HTML after article-body extraction, not the full listing page whenever a more specific article node can be found.
When a CMS uses a chrome-like `header` or `aside` element as that independently
validated semantic article-body node, the extractor passes its children to the
sanitizer. This preserves the article prose while ordinary page headers and
sidebars remain removable chrome. Common vendor newsroom detail roots such as
`.wd_news_body`, DNN/EasyDNN `.main_content`, and bounded `#newsContent` detail
regions participate in the same generic semantic-body selection. These are
reusable markup contracts rather than company-specific parsers.
Semantic-body candidates are measured after this sanitizer, not from raw DOM
text. This prevents CSS stored in a CMS body field, scripts, or form labels from
appearing substantive during selection and then collapsing to an empty stored
article.
Explicit article pages that split prose across repeated
`.richtext-editor-place` components aggregate those non-chrome components
before sanitization. Related `<article>` cards are excluded, and the aggregate
must materially outweigh every card before it can disambiguate a card-heavy
page.
For a Next.js detail page whose initial DOM is only a loading skeleton, the
crawler may supply rich HTML from the page's non-executed `__NEXT_DATA__`
JSON. The containing object must exactly match the fetched URL's terminal
identity and contain its own usable title and substantive article body; nearby
listing objects cannot contribute fields. The recovered body enters this same
`process_html` contract, so active content and page chrome receive no special
framework exemption.
SvelteKit loading shells receive the same treatment through their same-origin
route `__data.json` resource. The crawler decodes SvelteKit's bounded
reference-table JSON without evaluating the page bootstrap script, requires an
exact path plus title in one content object, and selects substantive rich HTML
only from content-like fields reachable from that object. The result still
enters `process_html`; framework data receives no sanitizer exemption.

## Quality Signals

Store processor metrics in raw or normalized metadata so bad extractors are visible:

- high stripped element count may indicate page chrome was passed instead of article body
- zero Markdown with non-empty HTML is a parser or selector bug
- repeated Markdown across different URLs may indicate cookie/banner extraction
- image count changes can indicate newsroom template changes
