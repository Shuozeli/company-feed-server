use url::Url;

const RESOURCE_QUERY_KEYS: &[&str] = &[
    "article",
    "article_id",
    "articleid",
    "cid",
    "content",
    "content_id",
    "contentid",
    "id",
    "item",
    "item_id",
    "itemid",
    "news_id",
    "newsid",
    "nid",
    "p",
    "page_id",
    "pageid",
    "post",
    "post_id",
    "postid",
    "record_id",
    "recordid",
    "release_id",
    "releaseid",
    "story",
    "story_id",
    "storyid",
];

/// Returns true for sitemap resources, which enumerate site URLs but are not
/// editorial RSS/Atom publications.
pub fn is_sitemap_url(url: &Url) -> bool {
    url.path_segments()
        .into_iter()
        .flatten()
        .any(contains_sitemap_token)
        || url
            .query_pairs()
            .any(|(key, value)| contains_sitemap_token(&key) || contains_sitemap_token(&value))
}

/// Returns the bounded query fields that identify an editorial resource.
///
/// Locale, filter, pagination, analytics, and arbitrary scanner parameters do
/// not create new article identities. Legacy query-addressed CMS articles are
/// retained through a small provider-neutral vocabulary of resource keys.
pub fn resource_query_pairs(url: &Url) -> Vec<(String, String)> {
    let mut candidates = url
        .query_pairs()
        .filter_map(|(key, value)| {
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            (RESOURCE_QUERY_KEYS.contains(&key.as_str()) && safe_resource_query_value(value))
                .then(|| (key, value.to_owned()))
        })
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    if candidates.len() <= 4 {
        candidates
    } else {
        Vec::new()
    }
}

/// Returns true when a URL attempts to use an editorial resource key with an
/// empty, unsafe, or excessively ambiguous value set.
pub fn has_invalid_resource_query(url: &Url) -> bool {
    let mut recognized = url
        .query_pairs()
        .filter_map(|(key, value)| {
            let key = key.trim().to_ascii_lowercase();
            RESOURCE_QUERY_KEYS
                .contains(&key.as_str())
                .then(|| (key, value.trim().to_owned()))
        })
        .collect::<Vec<_>>();
    if recognized.is_empty() {
        return false;
    }
    if recognized
        .iter()
        .any(|(_, value)| !safe_resource_query_value(value))
    {
        return true;
    }
    recognized.sort();
    recognized.dedup();
    recognized.len() > 4
}

fn safe_resource_query_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

fn contains_sitemap_token(value: &str) -> bool {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|token| matches!(token.to_ascii_lowercase().as_str(), "sitemap" | "sitemaps"))
}

#[cfg(test)]
mod tests {
    use super::{has_invalid_resource_query, is_sitemap_url, resource_query_pairs};
    use url::Url;

    #[test]
    fn distinguishes_sitemaps_from_editorial_feeds() {
        for value in [
            "https://example.com/sitemap.rss",
            "https://example.com/sitemap_index.xml",
            "https://example.com/post-sitemap.xml",
            "https://example.com/sitemaps/news.xml",
            "https://example.com/feed.xml?sitemap=posts",
        ] {
            assert!(
                is_sitemap_url(&Url::parse(value).expect("sitemap URL")),
                "{value}"
            );
        }
        for value in [
            "https://sitemap.example.com/news/feed.xml",
            "https://example.com/news/rss",
            "https://example.com/blog/feed.xml",
        ] {
            assert!(
                !is_sitemap_url(&Url::parse(value).expect("editorial feed URL")),
                "{value}"
            );
        }
    }

    #[test]
    fn keeps_resource_queries_but_ignores_filters_and_scanner_noise() {
        assert_eq!(
            resource_query_pairs(
                &Url::parse("https://example.com/news/index.php?content_id=682&utm_source=test")
                    .expect("URL")
            ),
            vec![("content_id".to_owned(), "682".to_owned())]
        );
        assert!(
            resource_query_pairs(
                &Url::parse("https://example.com/news?category=7&lang=en").expect("URL")
            )
            .is_empty()
        );
        assert!(
            resource_query_pairs(
                &Url::parse("https://example.com/news?id=%27payload&host=X&value=test")
                    .expect("URL")
            )
            .is_empty()
        );
        for value in [
            "https://example.com/news?id=",
            "https://example.com/news?id=%27payload",
            "https://example.com/news?id=1&post_id=2&newsid=3&content_id=4&story_id=5",
        ] {
            assert!(
                has_invalid_resource_query(&Url::parse(value).expect("invalid resource URL")),
                "{value}"
            );
        }
        assert!(!has_invalid_resource_query(
            &Url::parse("https://example.com/news?category=7&lang=en").expect("filter URL")
        ));
        assert!(!has_invalid_resource_query(
            &Url::parse("https://example.com/news?id=123&utm_source=test").expect("resource URL")
        ));
    }
}
