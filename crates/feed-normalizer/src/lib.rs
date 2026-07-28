use feed_content::{ContentProcessOptions, process_html};
use feed_core::{
    NormalizedFeedItem, RawCrawlItem, Source, is_cms_placeholder_article,
    is_non_editorial_utility_article, resource_query_pairs,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use url::Url;

pub fn normalize_item(
    source: &Source,
    raw: &RawCrawlItem,
    fetched_at: chrono::DateTime<chrono::Utc>,
) -> Result<NormalizedFeedItem, NormalizeError> {
    let title = raw
        .title
        .as_deref()
        .map(normalize_title)
        .filter(|title| !title.is_empty())
        .ok_or(NormalizeError::MissingTitle)?;
    let canonical_url = canonicalize_url(raw.canonical_url.as_ref().unwrap_or(&raw.url));
    if is_non_editorial_utility_article(&title, &canonical_url) {
        return Err(NormalizeError::NonEditorialUtilityArticle);
    }
    let external_id = raw
        .external_id
        .as_deref()
        .map(str::trim)
        .filter(|external_id| !external_id.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| canonical_url.as_str().to_owned());

    let body_input = raw
        .body_html
        .as_deref()
        .or(raw.summary_html.as_deref())
        .unwrap_or_default();
    let processed = process_html(
        body_input,
        &ContentProcessOptions {
            base_url: Some(raw.url.as_str().to_owned()),
            keep_images: false,
        },
    );
    let summary = raw
        .summary_html
        .as_deref()
        .map(|summary| {
            process_html(
                summary,
                &ContentProcessOptions {
                    base_url: Some(raw.url.as_str().to_owned()),
                    keep_images: false,
                },
            )
            .text
        })
        .unwrap_or_default();
    if is_cms_placeholder_article(&title, &processed.text) {
        return Err(NormalizeError::CmsPlaceholderArticle);
    }
    let hash_input = format!(
        "feed-item-v1\0{}\0{}\0{}\0{}",
        canonical_url,
        title,
        raw.published_at
            .map(|timestamp| timestamp.to_rfc3339())
            .unwrap_or_default(),
        processed.text
    );
    let content_hash = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(hash_input.as_bytes()))
    );
    let metrics = &processed.metrics;

    Ok(NormalizedFeedItem {
        external_id,
        url: raw.url.clone(),
        canonical_url,
        title,
        summary,
        body_text: processed.text,
        body_html: processed.clean_html,
        body_markdown: processed.markdown,
        published_at: raw.published_at,
        fetched_at,
        content_hash,
        source_kind: source.kind,
        raw: raw.payload.clone(),
        normalized: json!({
            "hash_contract": "feed-item-v1",
            "source_item_key": raw.source_item_key,
        }),
        content_processing: json!({
            "stripped_elements": metrics.stripped_elements,
            "stripped_attributes": metrics.stripped_attributes,
            "link_count": metrics.link_count,
            "image_count": metrics.image_count,
            "input_html_bytes": body_input.len(),
        }),
    })
}

pub fn canonicalize_url(url: &Url) -> Url {
    let mut canonical = url.clone();
    canonical.set_fragment(None);
    let query = resource_query_pairs(&canonical);
    if query.is_empty() {
        canonical.set_query(None);
    } else {
        canonical.query_pairs_mut().clear().extend_pairs(&query);
    }
    canonical
}

fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_title(value: &str) -> String {
    let normalized = normalize_whitespace(value);
    // Some React Server Component payloads can leak an inline wrapper call
    // into otherwise valid document titles. It is framework data, not
    // editorial text, and always starts at this unambiguous token boundary.
    normalized
        .find(" self.__wrap_n!=")
        .map_or(normalized.as_str(), |index| &normalized[..index])
        .trim()
        .to_owned()
}

#[derive(Debug, thiserror::Error)]
pub enum NormalizeError {
    #[error("feed entry has no usable title")]
    MissingTitle,
    #[error("feed entry is an unedited CMS placeholder article")]
    CmsPlaceholderArticle,
    #[error("feed entry is a non-editorial utility page")]
    NonEditorialUtilityArticle,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use feed_core::{RawCrawlItem, SourceKind, SourceStatus};
    use serde_json::Value;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn canonicalization_preserves_only_bounded_resource_queries() {
        let url = Url::parse(
            "https://example.com/news?id=2&utm_source=email&a=1&fbclid=tracking#section",
        )
        .expect("valid URL");
        assert_eq!(
            canonicalize_url(&url).as_str(),
            "https://example.com/news?id=2"
        );
        let filters = Url::parse("https://example.com/news?category=7&lang=en").expect("valid URL");
        assert_eq!(
            canonicalize_url(&filters).as_str(),
            "https://example.com/news"
        );
    }

    #[test]
    fn normalizes_and_sanitizes_raw_feed_content() {
        let now = Utc::now();
        let source = feed_core::Source {
            id: Uuid::new_v4(),
            source_id: "acme-rss".to_owned(),
            company_id: Uuid::new_v4(),
            kind: SourceKind::Rss,
            url: Url::parse("https://example.com/feed").expect("valid URL"),
            status: SourceStatus::Approved,
            freshness_slo_seconds: 3600,
            browser_required: false,
            public_export_allowed: true,
            discovery_confidence: Some(1.0),
            metadata: Value::Object(Default::default()),
            created_at: now,
            updated_at: now,
        };
        let raw = RawCrawlItem {
            source_item_key: "key".to_owned(),
            external_id: Some("launch".to_owned()),
            url: Url::parse("https://example.com/launch?utm_source=rss").expect("valid URL"),
            canonical_url: None,
            title: Some(" Acme   Launch ".to_owned()),
            summary_html: Some("<p>Summary</p>".to_owned()),
            body_html: Some("<article><h1>Launch</h1><script>x()</script></article>".to_owned()),
            published_at: Some(now),
            payload: Value::Object(Default::default()),
        };

        let normalized = normalize_item(&source, &raw, now).expect("normalize item");
        assert_eq!(normalized.title, "Acme Launch");
        assert_eq!(
            normalized.canonical_url.as_str(),
            "https://example.com/launch"
        );
        assert!(!normalized.body_html.contains("script"));
        assert!(normalized.body_markdown.contains("# Launch"));
        assert!(normalized.content_hash.starts_with("sha256:"));
    }

    #[test]
    fn strips_react_wrapper_data_leaked_into_article_titles() {
        assert_eq!(
            normalize_title(
                "Aside for Developers self.__wrap_n!=1&&self.__wrap_b(\"_R_fixture_\",1)"
            ),
            "Aside for Developers"
        );
        assert_eq!(
            normalize_title("Why self.__wrap_ is useful in application code"),
            "Why self.__wrap_ is useful in application code"
        );
    }

    #[test]
    fn rejects_cms_placeholder_posts_but_preserves_real_launch_posts() {
        let now = Utc::now();
        let source = feed_core::Source {
            id: Uuid::new_v4(),
            source_id: "acme-rss".to_owned(),
            company_id: Uuid::new_v4(),
            kind: SourceKind::Rss,
            url: Url::parse("https://example.com/feed").expect("valid URL"),
            status: SourceStatus::Approved,
            freshness_slo_seconds: 3_600,
            browser_required: false,
            public_export_allowed: true,
            discovery_confidence: Some(1.0),
            metadata: Value::Object(Default::default()),
            created_at: now,
            updated_at: now,
        };
        let mut raw = RawCrawlItem {
            source_item_key: "placeholder".to_owned(),
            external_id: None,
            url: Url::parse("https://example.com/hello-world").expect("valid URL"),
            canonical_url: None,
            title: Some("Hello world!".to_owned()),
            summary_html: None,
            body_html: Some(
                "<p>Welcome to WordPress. This is your first post. Edit or delete it.</p>"
                    .to_owned(),
            ),
            published_at: Some(now),
            payload: Value::Object(Default::default()),
        };

        assert!(matches!(
            normalize_item(&source, &raw, now),
            Err(NormalizeError::CmsPlaceholderArticle)
        ));

        raw.body_html = Some(
            "<p>We are launching a governed analytics platform for every team.</p>".to_owned(),
        );
        normalize_item(&source, &raw, now).expect("substantive launch post remains valid");
    }

    #[test]
    fn rejects_non_editorial_utility_pages() {
        let now = Utc::now();
        let source = feed_core::Source {
            id: Uuid::new_v4(),
            source_id: "acme-rss".to_owned(),
            company_id: Uuid::new_v4(),
            kind: SourceKind::Rss,
            url: Url::parse("https://example.com/feed").expect("valid URL"),
            status: SourceStatus::Approved,
            freshness_slo_seconds: 3_600,
            browser_required: false,
            public_export_allowed: true,
            discovery_confidence: Some(1.0),
            metadata: Value::Object(Default::default()),
            created_at: now,
            updated_at: now,
        };
        let raw = RawCrawlItem {
            source_item_key: "newsletter".to_owned(),
            external_id: None,
            url: Url::parse("https://example.com/news/newsletter-sign-up").expect("valid URL"),
            canonical_url: None,
            title: Some("Newsletter Sign-up".to_owned()),
            summary_html: Some("<p>Enter your email address.</p>".to_owned()),
            body_html: None,
            published_at: Some(now),
            payload: Value::Object(Default::default()),
        };

        assert!(matches!(
            normalize_item(&source, &raw, now),
            Err(NormalizeError::NonEditorialUtilityArticle)
        ));
    }
}
