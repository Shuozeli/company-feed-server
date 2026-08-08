use std::collections::HashSet;

use ego_tree::NodeId;
use scraper::{Html, Node};
use url::Url;

const DROP_SUBTREE_TAGS: &[&str] = &[
    "script", "style", "noscript", "iframe", "svg", "canvas", "object", "embed", "form", "input",
    "button", "select", "textarea", "nav", "header", "footer", "aside",
];

const UNWRAP_TAGS: &[&str] = &["html", "head", "body"];

const ALLOWED_TAGS: &[&str] = &[
    "article",
    "main",
    "section",
    "div",
    "p",
    "br",
    "hr",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "pre",
    "code",
    "ul",
    "ol",
    "li",
    "strong",
    "b",
    "em",
    "i",
    "a",
    "img",
    "table",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
    "time",
    "span",
];

const VOID_TAGS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

const MAX_ELEMENT_DEPTH: usize = 512;

const MAX_PREPARSE_DEPTH: usize = 2048;

const AUTO_CLOSING_SAME_TAG: &[&str] = &[
    "p", "li", "dt", "dd", "tr", "td", "th", "option", "optgroup", "caption", "colgroup", "thead",
    "tbody", "tfoot", "rt", "rp",
];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContentProcessOptions {
    pub base_url: Option<String>,
    pub keep_images: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContentProcessMetrics {
    pub stripped_elements: usize,
    pub stripped_attributes: usize,
    pub link_count: usize,
    pub image_count: usize,
    pub depth_rejected: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessedContent {
    pub clean_html: String,
    pub markdown: String,
    pub text: String,
    pub metrics: ContentProcessMetrics,
}

pub fn process_html(raw_html: &str, options: &ContentProcessOptions) -> ProcessedContent {
    if nesting_depth_exceeds_limit(raw_html) {
        return ProcessedContent {
            clean_html: String::new(),
            markdown: String::new(),
            text: String::new(),
            metrics: ContentProcessMetrics {
                depth_rejected: true,
                ..ContentProcessMetrics::default()
            },
        };
    }
    let (clean_html, metrics) = sanitize_html(raw_html, options);
    let clean_document = Html::parse_document(&clean_html);
    let markdown = normalize_markdown(render_markdown(&clean_document, options));
    let text = normalize_text(extract_text(&clean_document));

    ProcessedContent {
        clean_html,
        markdown,
        text,
        metrics,
    }
}

fn nesting_depth_exceeds_limit(raw_html: &str) -> bool {
    let bytes = raw_html.as_bytes();
    let len = bytes.len();
    let mut index = 0;
    let mut depth = 0usize;
    let mut max_depth = 0usize;
    let mut last_open: Option<&[u8]> = None;
    while index < len {
        if bytes[index] != b'<' {
            index += 1;
            continue;
        }
        match bytes.get(index + 1).copied() {
            None => break,
            Some(b'!') => {
                if bytes.get(index + 2) == Some(&b'-') && bytes.get(index + 3) == Some(&b'-') {
                    let Some(end) = find_bytes(bytes, index + 4, b"-->") else {
                        break;
                    };
                    index = end + 3;
                } else {
                    let Some(end) = find_byte(bytes, index + 2, b'>') else {
                        break;
                    };
                    index = end + 1;
                }
            }
            Some(b'/') => {
                let (end, found) = skip_to_tag_end(bytes, index + 2);
                if found {
                    depth = depth.saturating_sub(1);
                    last_open = None;
                }
                index = end;
            }
            Some(b'?') => {
                let Some(end) = find_byte(bytes, index + 2, b'>') else {
                    break;
                };
                index = end + 1;
            }
            Some(byte) if byte.is_ascii_alphabetic() => {
                let mut name_end = index + 1;
                while name_end < len
                    && (bytes[name_end].is_ascii_alphanumeric()
                        || matches!(bytes[name_end], b'-' | b':'))
                {
                    name_end += 1;
                }
                let name = &bytes[index + 1..name_end];
                let (tag_end, found) = skip_to_tag_end(bytes, name_end);
                if name.eq_ignore_ascii_case(b"script") || name.eq_ignore_ascii_case(b"style") {
                    let closer: &[u8] = if name.eq_ignore_ascii_case(b"script") {
                        b"</script"
                    } else {
                        b"</style"
                    };
                    let Some(position) = find_ci(bytes, tag_end, closer) else {
                        break;
                    };
                    index = position;
                    continue;
                }
                let self_closing = found && tag_end >= 2 && bytes[tag_end - 2] == b'/';
                let skipped_tag = !found || self_closing || is_void_tag(name);
                let counts_toward_depth =
                    !(skipped_tag || (is_auto_closing_tag(name) && Some(name) == last_open));
                if counts_toward_depth {
                    depth += 1;
                    last_open = Some(name);
                    if depth > max_depth {
                        max_depth = depth;
                    }
                    if max_depth > MAX_PREPARSE_DEPTH {
                        return true;
                    }
                }
                index = tag_end;
            }
            Some(_) => index += 1,
        }
    }
    max_depth > MAX_PREPARSE_DEPTH
}

fn is_void_tag(name: &[u8]) -> bool {
    VOID_TAGS
        .iter()
        .any(|tag| tag.as_bytes().eq_ignore_ascii_case(name))
}

fn is_auto_closing_tag(name: &[u8]) -> bool {
    AUTO_CLOSING_SAME_TAG
        .iter()
        .any(|tag| tag.as_bytes().eq_ignore_ascii_case(name))
}

fn skip_to_tag_end(bytes: &[u8], from: usize) -> (usize, bool) {
    let len = bytes.len();
    let mut index = from;
    while index < len {
        match bytes[index] {
            b'>' => return (index + 1, true),
            b'"' | b'\'' => {
                let quote = bytes[index];
                index += 1;
                while index < len && bytes[index] != quote {
                    index += 1;
                }
                index += usize::from(index < len);
            }
            b'/' if bytes.get(index + 1) == Some(&b'>') => return (index + 2, true),
            _ => index += 1,
        }
    }
    (len, false)
}

fn find_byte(bytes: &[u8], from: usize, needle: u8) -> Option<usize> {
    bytes
        .get(from..)?
        .iter()
        .position(|&byte| byte == needle)
        .map(|position| position + from)
}

fn find_bytes(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let window = bytes.get(from..)?;
    if window.len() < needle.len() {
        return None;
    }
    (0..=window.len() - needle.len())
        .find(|&position| &window[position..position + needle.len()] == needle)
        .map(|position| position + from)
}

fn find_ci(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let window = bytes.get(from..)?;
    if window.len() < needle.len() {
        return None;
    }
    (0..=window.len() - needle.len())
        .find(|&position| window[position..position + needle.len()].eq_ignore_ascii_case(needle))
        .map(|position| position + from)
}

pub fn sanitize_html(
    raw_html: &str,
    options: &ContentProcessOptions,
) -> (String, ContentProcessMetrics) {
    let document = Html::parse_document(raw_html);
    let mut output = String::new();
    let mut metrics = ContentProcessMetrics::default();
    let base_url = options
        .base_url
        .as_deref()
        .and_then(|value| Url::parse(value).ok());

    serialize_sanitized_node(
        &document,
        document.root_element().id(),
        0,
        &base_url,
        options,
        &mut metrics,
        &mut output,
    );

    (output, metrics)
}

fn serialize_sanitized_node(
    document: &Html,
    node_id: NodeId,
    depth: usize,
    base_url: &Option<Url>,
    options: &ContentProcessOptions,
    metrics: &mut ContentProcessMetrics,
    output: &mut String,
) {
    let Some(node_ref) = document.tree.get(node_id) else {
        return;
    };

    match node_ref.value() {
        Node::Document => {
            for child in node_ref.children() {
                let child_depth = depth + usize::from(matches!(child.value(), Node::Element(_)));
                serialize_sanitized_node(
                    document,
                    child.id(),
                    child_depth,
                    base_url,
                    options,
                    metrics,
                    output,
                );
            }
        }
        Node::Text(text) => {
            output.push_str(&html_escape::encode_text(text.text.as_ref()));
        }
        Node::Comment(_) => {}
        Node::Element(element) => {
            if depth > MAX_ELEMENT_DEPTH {
                metrics.stripped_elements += 1;
                return;
            }

            let tag = element.name();
            if DROP_SUBTREE_TAGS.contains(&tag) {
                metrics.stripped_elements += 1;
                return;
            }

            if UNWRAP_TAGS.contains(&tag) || !ALLOWED_TAGS.contains(&tag) {
                if !UNWRAP_TAGS.contains(&tag) && !ALLOWED_TAGS.contains(&tag) {
                    metrics.stripped_elements += 1;
                }
                for child in node_ref.children() {
                    serialize_sanitized_node(
                        document,
                        child.id(),
                        depth + 1,
                        base_url,
                        options,
                        metrics,
                        output,
                    );
                }
                return;
            }

            if tag == "img" && !options.keep_images {
                metrics.stripped_elements += 1;
                return;
            }

            output.push('<');
            output.push_str(tag);

            for (name, value) in element.attrs() {
                match sanitized_attr(tag, name, value, base_url) {
                    Some((safe_name, safe_value)) => {
                        if tag == "a" && safe_name == "href" {
                            metrics.link_count += 1;
                        }
                        if tag == "img" && safe_name == "src" {
                            metrics.image_count += 1;
                        }
                        output.push(' ');
                        output.push_str(safe_name);
                        output.push_str("=\"");
                        output.push_str(&html_escape::encode_double_quoted_attribute(&safe_value));
                        output.push('"');
                    }
                    None => metrics.stripped_attributes += 1,
                }
            }

            output.push('>');
            for child in node_ref.children() {
                serialize_sanitized_node(
                    document,
                    child.id(),
                    depth + 1,
                    base_url,
                    options,
                    metrics,
                    output,
                );
            }
            if !VOID_TAGS.contains(&tag) {
                output.push_str("</");
                output.push_str(tag);
                output.push('>');
            }
        }
        _ => {}
    }
}

fn sanitized_attr(
    tag: &str,
    name: &str,
    value: &str,
    base_url: &Option<Url>,
) -> Option<(&'static str, String)> {
    match (tag, name) {
        ("a", "href") => sanitize_url(value, base_url).map(|value| ("href", value)),
        ("img", "src") => sanitize_url(value, base_url).map(|value| ("src", value)),
        ("img", "alt") => Some(("alt", value.to_string())),
        ("img", "title") => Some(("title", value.to_string())),
        ("time", "datetime") => Some(("datetime", value.to_string())),
        _ => None,
    }
}

fn sanitize_url(value: &str, base_url: &Option<Url>) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let parsed = match Url::parse(trimmed) {
        Ok(url) => Some(url),
        Err(_) => base_url.as_ref().and_then(|base| base.join(trimmed).ok()),
    }?;

    match parsed.scheme() {
        "http" | "https" => Some(parsed.to_string()),
        _ => None,
    }
}

fn render_markdown(document: &Html, options: &ContentProcessOptions) -> String {
    let mut output = String::new();
    render_markdown_node(
        document,
        document.root_element().id(),
        0,
        options,
        &mut output,
    );
    output
}

fn render_markdown_node(
    document: &Html,
    node_id: NodeId,
    depth: usize,
    options: &ContentProcessOptions,
    output: &mut String,
) {
    let Some(node_ref) = document.tree.get(node_id) else {
        return;
    };

    match node_ref.value() {
        Node::Document => {
            for child in node_ref.children() {
                let child_depth = depth + usize::from(matches!(child.value(), Node::Element(_)));
                render_markdown_node(document, child.id(), child_depth, options, output);
            }
        }
        Node::Text(text) => push_markdown_text(output, text.text.as_ref()),
        Node::Comment(_) => {}
        Node::Element(element) => {
            if depth > MAX_ELEMENT_DEPTH {
                return;
            }

            let tag = element.name();
            if DROP_SUBTREE_TAGS.contains(&tag) {
                return;
            }

            match tag {
                "html" | "head" | "body" | "article" | "main" | "section" | "div" | "span" => {
                    for child in node_ref.children() {
                        render_markdown_node(document, child.id(), depth + 1, options, output);
                    }
                    push_block_break(output);
                }
                "p" => {
                    push_block_break(output);
                    for child in node_ref.children() {
                        render_markdown_node(document, child.id(), depth + 1, options, output);
                    }
                    push_block_break(output);
                }
                "br" => output.push_str("  \n"),
                "hr" => {
                    push_block_break(output);
                    output.push_str("---");
                    push_block_break(output);
                }
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    let level = tag.trim_start_matches('h').parse::<usize>().unwrap_or(2);
                    push_block_break(output);
                    output.push_str(&"#".repeat(level));
                    output.push(' ');
                    for child in node_ref.children() {
                        render_markdown_node(document, child.id(), depth + 1, options, output);
                    }
                    push_block_break(output);
                }
                "strong" | "b" => {
                    render_wrapped(document, node_ref.id(), depth, options, output, "**")
                }
                "em" | "i" => render_wrapped(document, node_ref.id(), depth, options, output, "*"),
                "code" => {
                    output.push('`');
                    for child in node_ref.children() {
                        render_markdown_node(document, child.id(), depth + 1, options, output);
                    }
                    output.push('`');
                }
                "pre" => {
                    push_block_break(output);
                    output.push_str("```text\n");
                    output.push_str(&collect_text(document, node_ref.id(), depth));
                    output.push_str("\n```");
                    push_block_break(output);
                }
                "blockquote" => {
                    push_block_break(output);
                    let mut inner = String::new();
                    for child in node_ref.children() {
                        render_markdown_node(document, child.id(), depth + 1, options, &mut inner);
                    }
                    for line in normalize_markdown(inner).lines() {
                        output.push_str("> ");
                        output.push_str(line);
                        output.push('\n');
                    }
                    push_block_break(output);
                }
                "ul" => render_list(document, node_ref.id(), depth, options, output, false),
                "ol" => render_list(document, node_ref.id(), depth, options, output, true),
                "li" => {
                    for child in node_ref.children() {
                        render_markdown_node(document, child.id(), depth + 1, options, output);
                    }
                }
                "a" => {
                    let text = normalize_text(collect_text(document, node_ref.id(), depth));
                    let href = element.attr("href").unwrap_or_default().trim();
                    if text.is_empty() {
                        return;
                    }
                    if href.is_empty() {
                        output.push_str(&escape_markdown_text(&text));
                    } else {
                        output.push('[');
                        output.push_str(&escape_markdown_text(&text));
                        output.push_str("](");
                        output.push_str(href);
                        output.push(')');
                    }
                }
                "img" => {
                    if !options.keep_images {
                        return;
                    }
                    let alt = element.attr("alt").unwrap_or_default().trim();
                    let src = element.attr("src").unwrap_or_default().trim();
                    if !src.is_empty() {
                        output.push_str("![");
                        output.push_str(&escape_markdown_text(alt));
                        output.push_str("](");
                        output.push_str(src);
                        output.push(')');
                    }
                }
                "table" | "thead" | "tbody" | "tr" | "th" | "td" | "time" => {
                    for child in node_ref.children() {
                        render_markdown_node(document, child.id(), depth + 1, options, output);
                    }
                    if matches!(tag, "tr" | "table") {
                        push_block_break(output);
                    }
                }
                _ => {
                    for child in node_ref.children() {
                        render_markdown_node(document, child.id(), depth + 1, options, output);
                    }
                }
            }
        }
        _ => {}
    }
}

fn render_wrapped(
    document: &Html,
    node_id: NodeId,
    depth: usize,
    options: &ContentProcessOptions,
    output: &mut String,
    wrapper: &str,
) {
    if !output.is_empty() && !output.ends_with(char::is_whitespace) {
        output.push(' ');
    }
    output.push_str(wrapper);
    if let Some(node_ref) = document.tree.get(node_id) {
        let mut inner = String::new();
        for child in node_ref.children() {
            render_markdown_node(document, child.id(), depth + 1, options, &mut inner);
        }
        output.push_str(&normalize_text(inner));
    }
    output.push_str(wrapper);
}

fn render_list(
    document: &Html,
    node_id: NodeId,
    depth: usize,
    options: &ContentProcessOptions,
    output: &mut String,
    ordered: bool,
) {
    push_block_break(output);
    let Some(node_ref) = document.tree.get(node_id) else {
        return;
    };

    let mut index = 1;
    for child in node_ref.children() {
        let Node::Element(element) = child.value() else {
            continue;
        };
        if element.name() != "li" {
            continue;
        }
        if ordered {
            output.push_str(&format!("{index}. "));
            index += 1;
        } else {
            output.push_str("- ");
        }
        render_markdown_node(document, child.id(), depth + 1, options, output);
        output.push('\n');
    }
    push_block_break(output);
}

fn extract_text(document: &Html) -> String {
    collect_text(document, document.root_element().id(), 0)
}

fn collect_text(document: &Html, node_id: NodeId, depth: usize) -> String {
    let Some(node_ref) = document.tree.get(node_id) else {
        return String::new();
    };

    match node_ref.value() {
        Node::Text(text) => text.text.to_string(),
        Node::Element(element) if DROP_SUBTREE_TAGS.contains(&element.name()) => String::new(),
        Node::Element(_) if depth > MAX_ELEMENT_DEPTH => String::new(),
        Node::Comment(_) => String::new(),
        _ => {
            let mut output = String::new();
            for child in node_ref.children() {
                let child_depth = depth + usize::from(matches!(child.value(), Node::Element(_)));
                output.push_str(&collect_text(document, child.id(), child_depth));
                output.push(' ');
            }
            output
        }
    }
}

fn push_block_break(output: &mut String) {
    let trimmed = output.trim_end();
    if trimmed.is_empty() {
        output.clear();
        return;
    }
    let trailing_newlines = output
        .chars()
        .rev()
        .take_while(|value| *value == '\n')
        .count();
    if trailing_newlines < 2 {
        output.push_str(&"\n".repeat(2 - trailing_newlines));
    }
}

fn push_markdown_text(output: &mut String, text: &str) {
    let normalized = normalize_text(text);
    if normalized.is_empty() {
        return;
    }

    if !output.is_empty()
        && !output.ends_with(char::is_whitespace)
        && !normalized.starts_with(char::is_whitespace)
    {
        output.push(' ');
    }
    output.push_str(&escape_markdown_text(&normalized));
}

fn normalize_text(value: impl AsRef<str>) -> String {
    value
        .as_ref()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_markdown(value: String) -> String {
    let mut output = String::new();
    let mut blank_seen = false;
    let mut seen_lines = HashSet::new();

    for line in value.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !blank_seen && !output.trim().is_empty() {
                output.push('\n');
                output.push('\n');
                blank_seen = true;
            }
            continue;
        }

        if !seen_lines.insert(trimmed.to_string()) && trimmed.len() > 80 {
            continue;
        }

        output.push_str(trimmed);
        output.push('\n');
        blank_seen = false;
    }

    output.trim().to_string()
}

fn escape_markdown_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '[' | ']' | '`') {
            output.push('\\');
        }
        output.push(ch);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizer_strips_unsafe_elements_and_attributes() {
        let input = r#"
            <article class="promo">
              <nav>menu</nav>
              <h1 onclick="x()">Launch</h1>
              <p>Alpha <a href="/news/launch" style="x">story</a></p>
              <a href="javascript:alert(1)">bad</a>
              <script>alert(1)</script>
            </article>
        "#;

        let options = ContentProcessOptions {
            base_url: Some("https://example.com/root/".to_string()),
            keep_images: false,
        };
        let processed = process_html(input, &options);

        assert!(processed.clean_html.contains("<article>"));
        assert!(processed.clean_html.contains("<h1>Launch</h1>"));
        assert!(
            processed
                .clean_html
                .contains("<a href=\"https://example.com/news/launch\">story</a>")
        );
        assert!(!processed.clean_html.contains("onclick"));
        assert!(!processed.clean_html.contains("javascript"));
        assert!(!processed.clean_html.contains("menu"));
        assert!(!processed.clean_html.contains("script"));
        assert_eq!(processed.metrics.link_count, 1);
    }

    #[test]
    fn markdown_preserves_basic_article_structure() {
        let input = r#"
            <main>
              <h1>NVIDIA announces a thing</h1>
              <p>First <strong>important</strong> paragraph.</p>
              <ul><li>One</li><li>Two</li></ul>
            </main>
        "#;

        let processed = process_html(input, &ContentProcessOptions::default());

        assert!(processed.markdown.contains("# NVIDIA announces a thing"));
        assert!(
            processed
                .markdown
                .contains("First **important** paragraph.")
        );
        assert!(processed.markdown.contains("- One"));
        assert!(processed.markdown.contains("- Two"));
        assert_eq!(
            processed.text,
            "NVIDIA announces a thing First important paragraph. One Two"
        );
    }

    #[test]
    fn images_are_opt_in() {
        let input = r#"<p>Intro</p><img src="/hero.png" alt="Hero">"#;
        let without_images = process_html(
            input,
            &ContentProcessOptions {
                base_url: Some("https://example.com/news/item".to_string()),
                keep_images: false,
            },
        );
        let with_images = process_html(
            input,
            &ContentProcessOptions {
                base_url: Some("https://example.com/news/item".to_string()),
                keep_images: true,
            },
        );

        assert!(!without_images.clean_html.contains("<img"));
        assert!(with_images.clean_html.contains("<img"));
        assert!(
            with_images
                .clean_html
                .contains("src=\"https://example.com/hero.png\"")
        );
        assert!(with_images.clean_html.contains("alt=\"Hero\""));
        assert!(
            with_images
                .markdown
                .contains("![Hero](https://example.com/hero.png)")
        );
    }

    #[test]
    fn pre_parse_guard_rejects_pathological_nesting_before_parsing() {
        const DEPTH: usize = 5_000;
        let mut html = String::with_capacity(DEPTH * 5 + DEPTH * 6 + 16);
        for _ in 0..DEPTH {
            html.push_str("<div>");
        }
        html.push_str("deep payload");
        for _ in 0..DEPTH {
            html.push_str("</div>");
        }

        let started = std::time::Instant::now();
        let processed = process_html(&html, &ContentProcessOptions::default());
        let elapsed = started.elapsed();

        assert!(nesting_depth_exceeds_limit(&html));
        assert!(processed.metrics.depth_rejected);
        assert!(processed.clean_html.is_empty());
        assert!(processed.markdown.is_empty());
        assert!(processed.text.is_empty());
        assert!(
            elapsed.as_secs() < 10,
            "pre-parse guard took too long: {elapsed:?}"
        );
    }

    #[test]
    fn pre_parse_guard_ignores_messy_but_shallow_html() {
        let html = concat!(
            "<!DOCTYPE html><!-- a comment <div> -->",
            "<html><head><style>.x::before{content:\"</div>\"}</style></head>",
            "<body><script>const s = \"<div><div>\"; const t = 1 < 2;</script>",
            "<div title=\"a > b and </div>\">",
            "<p>5 < 3 is false</p><br><img src=\"x.png\"><div/>",
            "<span>payload</span></div></body></html>"
        );
        assert!(!nesting_depth_exceeds_limit(html));

        let processed = process_html(html, &ContentProcessOptions::default());

        assert!(!processed.metrics.depth_rejected);
        assert!(processed.text.contains("payload"));
    }

    #[test]
    fn deep_element_nesting_is_bounded_and_does_not_panic() {
        const DEPTH: usize = 1024;
        let mut html = String::with_capacity(DEPTH * 6 + DEPTH * 7 + 16);
        for _ in 0..DEPTH {
            html.push_str("<div>");
        }
        html.push_str("deep payload");
        for _ in 0..DEPTH {
            html.push_str("</div>");
        }

        let processed = process_html(&html, &ContentProcessOptions::default());

        assert!(!processed.metrics.depth_rejected);
        assert!(processed.clean_html.len() < 10_000);
        assert!(processed.markdown.len() < 10_000);
        assert!(!processed.text.contains("deep payload"));
        assert!(processed.metrics.stripped_elements > 0);
    }

    #[test]
    fn moderate_nesting_is_fully_processed() {
        let mut html = String::new();
        for _ in 0..100 {
            html.push_str("<div>");
        }
        html.push_str("deep payload");
        for _ in 0..100 {
            html.push_str("</div>");
        }

        let processed = process_html(&html, &ContentProcessOptions::default());

        assert!(processed.text.contains("deep payload"));
        assert_eq!(processed.metrics.stripped_elements, 0);
    }
}
