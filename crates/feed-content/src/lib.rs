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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessedContent {
    pub clean_html: String,
    pub markdown: String,
    pub text: String,
    pub metrics: ContentProcessMetrics,
}

pub fn process_html(raw_html: &str, options: &ContentProcessOptions) -> ProcessedContent {
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
                serialize_sanitized_node(document, child.id(), base_url, options, metrics, output);
            }
        }
        Node::Text(text) => {
            output.push_str(&html_escape::encode_text(text.text.as_ref()));
        }
        Node::Comment(_) => {}
        Node::Element(element) => {
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
                serialize_sanitized_node(document, child.id(), base_url, options, metrics, output);
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
    render_markdown_node(document, document.root_element().id(), options, &mut output);
    output
}

fn render_markdown_node(
    document: &Html,
    node_id: NodeId,
    options: &ContentProcessOptions,
    output: &mut String,
) {
    let Some(node_ref) = document.tree.get(node_id) else {
        return;
    };

    match node_ref.value() {
        Node::Document => {
            for child in node_ref.children() {
                render_markdown_node(document, child.id(), options, output);
            }
        }
        Node::Text(text) => push_markdown_text(output, text.text.as_ref()),
        Node::Comment(_) => {}
        Node::Element(element) => {
            let tag = element.name();
            if DROP_SUBTREE_TAGS.contains(&tag) {
                return;
            }

            match tag {
                "html" | "head" | "body" | "article" | "main" | "section" | "div" | "span" => {
                    for child in node_ref.children() {
                        render_markdown_node(document, child.id(), options, output);
                    }
                    push_block_break(output);
                }
                "p" => {
                    push_block_break(output);
                    for child in node_ref.children() {
                        render_markdown_node(document, child.id(), options, output);
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
                        render_markdown_node(document, child.id(), options, output);
                    }
                    push_block_break(output);
                }
                "strong" | "b" => render_wrapped(document, node_ref.id(), options, output, "**"),
                "em" | "i" => render_wrapped(document, node_ref.id(), options, output, "*"),
                "code" => {
                    output.push('`');
                    for child in node_ref.children() {
                        render_markdown_node(document, child.id(), options, output);
                    }
                    output.push('`');
                }
                "pre" => {
                    push_block_break(output);
                    output.push_str("```text\n");
                    output.push_str(&collect_text(document, node_ref.id()));
                    output.push_str("\n```");
                    push_block_break(output);
                }
                "blockquote" => {
                    push_block_break(output);
                    let mut inner = String::new();
                    for child in node_ref.children() {
                        render_markdown_node(document, child.id(), options, &mut inner);
                    }
                    for line in normalize_markdown(inner).lines() {
                        output.push_str("> ");
                        output.push_str(line);
                        output.push('\n');
                    }
                    push_block_break(output);
                }
                "ul" => render_list(document, node_ref.id(), options, output, false),
                "ol" => render_list(document, node_ref.id(), options, output, true),
                "li" => {
                    for child in node_ref.children() {
                        render_markdown_node(document, child.id(), options, output);
                    }
                }
                "a" => {
                    let text = normalize_text(collect_text(document, node_ref.id()));
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
                        render_markdown_node(document, child.id(), options, output);
                    }
                    if matches!(tag, "tr" | "table") {
                        push_block_break(output);
                    }
                }
                _ => {
                    for child in node_ref.children() {
                        render_markdown_node(document, child.id(), options, output);
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
            render_markdown_node(document, child.id(), options, &mut inner);
        }
        output.push_str(&normalize_text(inner));
    }
    output.push_str(wrapper);
}

fn render_list(
    document: &Html,
    node_id: NodeId,
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
        render_markdown_node(document, child.id(), options, output);
        output.push('\n');
    }
    push_block_break(output);
}

fn extract_text(document: &Html) -> String {
    collect_text(document, document.root_element().id())
}

fn collect_text(document: &Html, node_id: NodeId) -> String {
    let Some(node_ref) = document.tree.get(node_id) else {
        return String::new();
    };

    match node_ref.value() {
        Node::Text(text) => text.text.to_string(),
        Node::Element(element) if DROP_SUBTREE_TAGS.contains(&element.name()) => String::new(),
        Node::Comment(_) => String::new(),
        _ => {
            let mut output = String::new();
            for child in node_ref.children() {
                output.push_str(&collect_text(document, child.id()));
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
}
