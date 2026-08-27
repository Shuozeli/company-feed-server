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

/// Editorial path/host tokens that mark a sitemap (or the URLs inside it) as
/// news-scoped. Used to pick the news sitemap among several and to gate
/// sitemap-source acceptance.
const NEWS_TOKENS: &[&str] = &[
    "news",
    "newsroom",
    "press",
    "pressroom",
    "media",
    "blog",
    "article",
    "articles",
    "story",
    "stories",
    "announcement",
    "announcements",
    "insights",
];

/// Non-editorial tokens that mark a sitemap as NOT a news source. These are the
/// URL inventories the sitemap-rejection guard was protecting against.
const NON_NEWS_TOKENS: &[&str] = &[
    "shop",
    "store",
    "retail",
    "product",
    "products",
    "catalog",
    "today",
    "support",
    "legal",
    "career",
    "careers",
    "job",
    "jobs",
    "category",
    "categories",
    "tag",
    "tags",
    "author",
    "image",
    "images",
    "video",
    "videos",
];

/// Net editorial score of a URL: (+1 per news token) - (1 per non-news token),
/// counted over host + path. Positive => news-scoped.
pub fn news_score(url: &Url) -> i32 {
    let haystack = format!("{}{}", url.host_str().unwrap_or_default(), url.path())
        .to_ascii_lowercase();
    let tokens: Vec<&str> = haystack
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    let positive = tokens
        .iter()
        .filter(|token| NEWS_TOKENS.contains(token))
        .count() as i32;
    let negative = tokens
        .iter()
        .filter(|token| NON_NEWS_TOKENS.contains(token))
        .count() as i32;
    positive - negative
}

/// A sitemap URL is news-scoped if it (or its typical entries) lean editorial.
pub fn is_news_scoped_url(url: &Url) -> bool {
    news_score(url) > 0
}

/// True when a URL points at another sitemap document (path ends in `.xml`).
/// Some sites (verified: Broadcom) serve a `<urlset>` whose entries are child
/// sitemaps rather than the standard `<sitemapindex>`; those must be recursed
/// into, not emitted as articles.
pub fn url_is_sitemap(url: &Url) -> bool {
    url.path().to_ascii_lowercase().ends_with(".xml")
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
