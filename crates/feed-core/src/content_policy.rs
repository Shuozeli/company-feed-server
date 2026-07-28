use url::Url;

pub fn is_cms_placeholder_article(title: &str, content: &str) -> bool {
    let original_title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized_title = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(['!', '！', '.', '。'])
        .to_lowercase();
    let numbered_fixture = ["test post ", "test article ", "test in the news "]
        .iter()
        .any(|prefix| {
            normalized_title.strip_prefix(prefix).is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            })
        });
    let numbered_blog_fixture =
        normalized_title
            .strip_prefix("blog title ")
            .is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            });
    let short_story_fixture = ["story-", "storie-"].iter().any(|prefix| {
        normalized_title.strip_prefix(prefix).is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
    }) && content.chars().count() <= 200;
    let generated_group_fixture = normalized_title
        .strip_prefix("group_")
        .and_then(|suffix| suffix.split_whitespace().next())
        .is_some_and(|token| {
            token.len() >= 8 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        && content.chars().count() <= 200;
    let static_fixture = numbered_fixture
        || numbered_blog_fixture
        || short_story_fixture
        || generated_group_fixture
        || (normalized_title == "coming soon" && content.chars().count() <= 200)
        || normalized_title == "test post created"
        || (normalized_title.starts_with("test post")
            && normalized_title.contains("please ignore"))
        || matches!(
            normalized_title.as_str(),
            "another item to trigger pagination"
                | "carousel display of multiple assets"
                | "gallery display of multiple assets"
                | "inline display of multiple assets"
        )
        || (original_title.starts_with("test ") && content.chars().count() <= 200);
    if static_fixture {
        return true;
    }

    let placeholder_title = matches!(
        normalized_title.as_str(),
        "hello world"
            | "olá, mundo"
            | "olá mundo"
            | "ola, mundo"
            | "ola mundo"
            | "¡hola mundo"
            | "hola mundo"
            | "bonjour tout le monde"
            | "hallo welt"
            | "ciao mondo"
            | "witaj, świecie"
            | "hallo wereld"
            | "merhaba dünya"
            | "привет, мир"
            | "こんにちは世界"
            | "你好，世界"
    );
    if !placeholder_title || content.chars().count() > 1_000 {
        return false;
    }

    let normalized_content = content.to_lowercase();
    [
        "first post",
        "primeiro post",
        "primer post",
        "premier article",
        "erster beitrag",
        "primo articolo",
        "pierwszy wpis",
        "первый пост",
        "eerste bericht",
        "ilk yazı",
    ]
    .iter()
    .any(|marker| normalized_content.contains(marker))
}

pub fn is_non_editorial_utility_article(title: &str, url: &Url) -> bool {
    let normalized_title = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_ascii_lowercase();
    if matches!(
        normalized_title.as_str(),
        "blog post"
            | "categories"
            | "category"
            | "chain reaction newsletter archive"
            | "comment policy"
            | "corporate backgrounder"
            | "corporate & financial"
            | "corporate and financial"
            | "corporate"
            | "corporate fact sheet"
            | "cookies policy (opens in new window)"
            | "engineering spotlight"
            | "events & financial calendar"
            | "events and financial calendar"
            | "explore topics"
            | "financial highlights"
            | "general information"
            | "guides & articles"
            | "history and profile"
            | "homepage"
            | "insights blog"
            | "investor communications sign up"
            | "key ratio"
            | "leadership team"
            | "learn more : gts north america"
            | "legal notice"
            | "legal notes"
            | "linkedin"
            | "listings"
            | "newsletter sign-up"
            | "next page"
            | "no title"
            | "other group companies"
            | "ownership breakdown"
            | "people & culture"
            | "photos & videos"
            | "previous"
            | "press office"
            | "property portfolio"
            | "product tour hub"
            | "qwe"
            | "read more data and research articles"
            | "resources"
            | "role spotlights"
            | "rss feeds"
            | "sec test"
            | "share monitor tool"
            | "shareholder remuneration"
            | "sign up for our investors news alert"
            | "subscribe to visualize"
            | "subscribe using the form below"
            | "team asana"
            | "test"
            | "the latest product offerings"
            | "view our latest articles"
            | "view more insights"
            | "videos"
            | "who we are"
            | "write for us"
    ) {
        return true;
    }
    let normalized_title_without_terminal_punctuation =
        normalized_title.trim_end_matches(['.', '!', '?']).trim();
    let normalized_title_word_count = normalized_title_without_terminal_punctuation
        .split_whitespace()
        .count();
    if (normalized_title_word_count <= 4
        && normalized_title_without_terminal_punctuation.starts_with("all ")
        && normalized_title_without_terminal_punctuation.ends_with(" articles")
        && !normalized_title_without_terminal_punctuation
            .chars()
            .any(|character| character.is_ascii_digit()))
        || (normalized_title_word_count <= 4
            && normalized_title_without_terminal_punctuation.ends_with(" media coverage")
            && !normalized_title_without_terminal_punctuation
                .chars()
                .any(|character| character.is_ascii_digit()))
    {
        return true;
    }
    if normalized_title_word_count <= 8
        && normalized_title_without_terminal_punctuation.starts_with("view more from ")
        && !normalized_title_without_terminal_punctuation
            .chars()
            .any(|character| character.is_ascii_digit())
    {
        return true;
    }
    if normalized_title
        .split_once(" | ")
        .is_some_and(|(section, category)| {
            section.split_whitespace().count() <= 3
                && section.ends_with(" blogs")
                && category.split_whitespace().count() <= 4
        })
    {
        return true;
    }
    if normalized_title
        .split_once(" | ")
        .is_some_and(|(section, site)| {
            let section_word_count = section.split_whitespace().count();
            let site_word_count = site.split_whitespace().count();
            section_word_count <= 8
                && section.ends_with(" news and updates")
                && !section.chars().any(|character| character.is_ascii_digit())
                && site_word_count <= 4
                && (site == "blog" || site.ends_with(" blog"))
        })
    {
        return true;
    }
    if normalized_title.split_whitespace().count() <= 4
        && normalized_title.starts_with("about ")
        && !normalized_title
            .chars()
            .any(|character| character.is_ascii_digit())
    {
        return true;
    }

    let path_segments = url
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let host_label = url
        .host_str()
        .map(|host| host.trim_start_matches("www.").to_ascii_lowercase())
        .and_then(|host| host.split('.').next().map(str::to_owned))
        .unwrap_or_default();
    let is_investor_host = matches!(host_label.as_str(), "ir" | "investor" | "investors" | "ri");
    let has_investor_utility_scope = is_investor_host
        && path_segments.iter().any(|segment| {
            matches!(
                segment.as_str(),
                "corporate-governance"
                    | "financial-information"
                    | "governance"
                    | "informacoes-aos-acionistas"
                    | "investor-information"
                    | "overview"
                    | "shareholder-information"
                    | "shareholders"
                    | "stock-information"
            )
        });
    let has_utility_scope = matches!(
        host_label.as_str(),
        "docs" | "help" | "helpcenter" | "knowledgebase" | "support"
    ) || has_investor_utility_scope
        || path_segments.iter().any(|segment| {
            matches!(
                segment.as_str(),
                "docs"
                    | "documentation"
                    | "discuss"
                    | "faq"
                    | "faqs"
                    | "forum"
                    | "forums"
                    | "hc"
                    | "help"
                    | "instagram-feed"
                    | "job"
                    | "job-openings"
                    | "jobs"
                    | "knowledge-base"
                    | "knowledgebase"
                    | "legal"
                    | "open-roles"
                    | "social-feed"
                    | "support"
                    | "vacancies"
            ) || segment.starts_with("career_")
        });
    let is_explicit_editorial_segment = |segment: &str| {
        matches!(
            segment,
            "blog"
                | "blogs"
                | "changelog"
                | "changelogs"
                | "company-news"
                | "engineering"
                | "insights"
                | "news"
                | "newsroom"
                | "press"
                | "press-release"
                | "press-releases"
                | "pressrelease"
                | "pressreleases"
                | "product-updates"
                | "release-notes"
                | "research"
                | "stories"
                | "updates"
                | "what-s-new"
                | "whats-new"
        ) || segment.contains("release-notes")
            || segment.contains("product-updates")
            || (segment.contains("releases-for-")
                && segment
                    .split(|character: char| !character.is_ascii_digit())
                    .any(|token| token.len() == 4))
    };
    let has_explicit_editorial_scope = path_segments
        .iter()
        .any(|segment| is_explicit_editorial_segment(segment));
    let has_market_profile_scope = path_segments
        .iter()
        .any(|segment| matches!(segment.as_str(), "equities" | "quote" | "quotes"))
        || path_segments.windows(2).any(|segments| {
            matches!(
                (segments[0].as_str(), segments[1].as_str()),
                ("investing", "stock") | ("market-activity", "stocks") | ("markets", "stocks")
            )
        });
    let padded_title = format!(" {normalized_title} ");
    let has_market_quote_title = [
        " share quote ",
        " share quote:",
        " stock quote ",
        " stock quote:",
    ]
    .iter()
    .any(|marker| padded_title.contains(marker));
    let has_market_price_title = [
        " share price ",
        " share price,",
        " share price:",
        " stock price ",
        " stock price,",
        " stock price:",
    ]
    .iter()
    .any(|marker| padded_title.contains(marker));
    let has_market_profile_marker = [
        " chart ",
        " finance ",
        " history ",
        " live ",
        " market cap ",
        " quote ",
        " real time ",
        " real-time ",
        " today ",
        " lse:",
        " nasdaq:",
        " nyse:",
    ]
    .iter()
    .any(|marker| padded_title.contains(marker));
    if has_market_profile_scope
        && (has_market_quote_title || (has_market_price_title && has_market_profile_marker))
    {
        return true;
    }
    if has_utility_scope && !has_explicit_editorial_scope {
        return true;
    }
    if is_investor_host
        && !has_explicit_editorial_scope
        && normalized_title.starts_with("why invest in ")
    {
        return true;
    }
    let broad_content_hub_index = path_segments.iter().position(|segment| {
        matches!(
            segment.as_str(),
            "library" | "resource" | "resource-center" | "resource-centre" | "resources"
        )
    });
    let has_non_editorial_hub_subsection = broad_content_hub_index.is_some_and(|hub_index| {
        path_segments.iter().skip(hub_index + 1).any(|segment| {
            matches!(
                segment.as_str(),
                "demo"
                    | "demo-series"
                    | "demos"
                    | "event"
                    | "events"
                    | "product-tour"
                    | "product-tour-hub"
                    | "product-tours"
                    | "testimonial"
                    | "testimonials"
                    | "training"
                    | "trainings"
                    | "video"
                    | "videos"
                    | "webinar"
                    | "webinars"
            )
        })
    });
    let has_explicit_editorial_scope_after_hub = broad_content_hub_index.is_some_and(|hub_index| {
        path_segments
            .iter()
            .skip(hub_index + 1)
            .any(|segment| is_explicit_editorial_segment(segment))
    });
    if has_non_editorial_hub_subsection && !has_explicit_editorial_scope_after_hub {
        return true;
    }

    let semantic_terminal = path_segments
        .iter()
        .rev()
        .map(|segment| strip_page_extension(segment))
        .find(|segment| !matches!(*segment, "default" | "index"));
    let has_media_asset_scope = path_segments.iter().any(|segment| {
        matches!(
            strip_page_extension(segment),
            "brand-resources"
                | "image-library"
                | "media-assets"
                | "media-library"
                | "media-resources"
        )
    });
    let title_word_count = normalized_title.split_whitespace().count();
    let terminal_segment = path_segments
        .last()
        .map(|segment| strip_page_extension(segment));
    let slugify_title_section = |value: &str| {
        value
            .split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    };
    let is_slug_matched_archive = [" | ", " - ", " – ", " — "]
        .iter()
        .find_map(|separator| normalized_title.split_once(separator))
        .and_then(|(section, _)| section.strip_suffix(" archives"))
        .is_some_and(|section| {
            terminal_segment.is_some_and(|terminal| slugify_title_section(section) == terminal)
        });
    if is_slug_matched_archive {
        return true;
    }
    if normalized_title.starts_with("subscribe")
        && normalized_title
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|word| word == "rss")
    {
        return true;
    }
    if semantic_terminal.is_some_and(|segment| {
        matches!(
            segment,
            "brand-kit" | "brandkit" | "media-kit" | "mediakit" | "press-kit" | "presskit"
        ) || matches!(segment, "photo-logo-request" | "photo-and-logo-request")
            || (has_media_asset_scope
                && title_word_count <= 6
                && normalized_title.ends_with(" logo use")
                && (segment == "logo-use" || segment.ends_with("-logo-use")))
    }) {
        return true;
    }
    if semantic_terminal.is_some_and(|segment| {
        matches!(
            segment,
            "articles"
                | "blog"
                | "blogs"
                | "insights"
                | "latest-news"
                | "news"
                | "news-releases"
                | "newsroom"
                | "press-releases"
                | "resources"
                | "stories"
                | "updates"
        ) && normalized_title == segment.replace(['-', '_'], " ")
    }) {
        return true;
    }
    if semantic_terminal == Some("timeline") && normalized_title == "timeline" {
        return true;
    }

    path_segments.last().is_some_and(|segment| {
        let segment = strip_page_extension(segment);
        let terminal_legal_or_subscription_utility = [
            "cookie-notice",
            "cookie-policy",
            "legal-notice",
            "legal-notices",
            "newsletter",
            "privacy",
            "privacy-notice",
            "privacy-policy",
            "terms",
            "terms-and-conditions",
            "terms-conditions",
            "terms-of-service",
            "terms-of-use",
        ]
        .iter()
        .any(|utility| {
            segment == *utility
                || segment
                    .strip_suffix(&format!("-{utility}"))
                    .is_some_and(|prefix| {
                        !prefix.is_empty()
                            && prefix
                                .trim_end_matches(['-', '_'])
                                .bytes()
                                .all(|byte| byte.is_ascii_digit())
                    })
        });
        matches!(
            segment,
            "all-rss-feeds"
                | "advertising-practices"
                | "b-roll"
                | "brand-assets"
                | "brand-articles"
                | "community-articles"
                | "company-articles"
                | "comment-policy"
                | "corporate-actions"
                | "corporate-financial"
                | "corporate-fact-sheet"
                | "cookie-policy"
                | "cookies-policy"
                | "corporate-video"
                | "employee-stories"
                | "glossary"
                | "historical-photos"
                | "home"
                | "insights-blog"
                | "latest-articles"
                | "management-photos"
                | "materials-for-media"
                | "market-notices"
                | "media-contact"
                | "media-contacts"
                | "media-and-news"
                | "media-materials"
                | "mediacontact"
                | "mediacontacts"
                | "newsletter-sign-up"
                | "newsletter-signup"
                | "personal-data"
                | "press-assets"
                | "press-office-contacts"
                | "privacy-policy"
                | "prime-rate-information"
                | "product-status"
                | "rss"
                | "rss-feed"
                | "rss-feeds"
                | "social-channels"
                | "social-media"
                | "socialchannels"
                | "socialmedia"
                | "research-library"
                | "subscribe"
                | "subscription"
                | "terms-of-use"
                | "video"
                | "videos"
                | "west-in-the-news"
                | "who-we-are"
                | "world-events"
                | "write-for-us"
        ) || terminal_legal_or_subscription_utility
            || is_year_named_archive_segment(segment)
            || (segment == "none" && normalized_title.ends_with("coming soon"))
    })
}

fn strip_page_extension(segment: &str) -> &str {
    [".aspx", ".html", ".asp", ".htm", ".php"]
        .iter()
        .find_map(|suffix| segment.strip_suffix(suffix))
        .unwrap_or(segment)
}

fn is_year_named_archive_segment(segment: &str) -> bool {
    let is_year = |value: &str| value.len() == 4 && value.bytes().all(|byte| byte.is_ascii_digit());
    segment.strip_suffix("-news-archive").is_some_and(is_year)
        || [
            "news-release-",
            "news-releases-",
            "press-release-",
            "press-releases-",
        ]
        .iter()
        .any(|prefix| segment.strip_prefix(prefix).is_some_and(is_year))
        || [
            "-news-release",
            "-news-releases",
            "-press-release",
            "-press-releases",
        ]
        .iter()
        .any(|suffix| segment.strip_suffix(suffix).is_some_and(is_year))
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::{is_cms_placeholder_article, is_non_editorial_utility_article};

    #[test]
    fn identifies_short_multilingual_cms_starter_posts() {
        assert!(is_cms_placeholder_article(
            "Hello world!",
            "Welcome to WordPress. This is your first post. Edit or delete it."
        ));
        assert!(is_cms_placeholder_article(
            "Olá, mundo!",
            "Boas-vindas ao WP Network. Esse é o seu primeiro post. Edite-o ou exclua-o."
        ));
    }

    #[test]
    fn preserves_substantive_hello_world_launch_articles() {
        assert!(!is_cms_placeholder_article(
            "Hello World!",
            "We are launching a new data platform that makes governed analytics accessible to every team."
        ));
        assert!(!is_cms_placeholder_article(
            "Our first post",
            "Welcome to WordPress. This is your first post."
        ));
    }

    #[test]
    fn identifies_static_cms_fixtures_without_blocking_real_test_articles() {
        for title in [
            "Test Post 003",
            "Test Article 1",
            "test in the news 2",
            "Test Post — Please Ignore",
            "Another item to trigger pagination",
            "Carousel Display of Multiple Assets",
        ] {
            assert!(is_cms_placeholder_article(
                title,
                "This fixture may contain arbitrary copied content."
            ));
        }
        assert!(is_cms_placeholder_article(
            "Blog Title 3",
            "A copied placeholder body."
        ));
        assert!(is_cms_placeholder_article(
            "Coming soon",
            "This publication has not launched yet."
        ));
        assert!(is_cms_placeholder_article("storie-1", "test"));
        assert!(is_cms_placeholder_article(
            "group_61b09d332e2f3 dd",
            "placeholder"
        ));
        assert!(is_cms_placeholder_article(
            "test Independent Auditors",
            "Independent Auditors item desc"
        ));
        assert!(!is_cms_placeholder_article(
            "Test 2FA Using Stably",
            "A concise product update."
        ));
        assert!(!is_cms_placeholder_article(
            "Test Coverage vs Test Priority",
            "A substantive guide to optimizing a testing strategy."
        ));
        assert!(!is_cms_placeholder_article(
            "Coming soon",
            &"A substantive product launch analysis. ".repeat(20)
        ));
    }

    #[test]
    fn identifies_utility_pages_without_blocking_editorial_subscription_news() {
        for title in [
            "Corporate Backgrounder",
            "Leadership Team",
            "Events & Financial Calendar",
            "Events and Financial Calendar",
            "Legal Notes",
            "Financial Highlights",
            "Listings",
            "Share monitor tool",
            "Shareholder Remuneration",
            "History and Profile",
            "Legal Notice",
            "Ownership Breakdown",
            "Property Portfolio",
            "Videos",
        ] {
            assert!(
                is_non_editorial_utility_article(
                    title,
                    &Url::parse("https://example.com/press/static-page").expect("valid URL")
                ),
                "expected exact utility title to be rejected: {title}"
            );
        }
        assert!(is_non_editorial_utility_article(
            "Newsletter Sign-up",
            &Url::parse("https://example.com/news/newsletter-sign-up").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Supermicro Newsroom",
            &Url::parse("https://example.com/news/newsletter-sign-up").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Feb 21, 2019 Coming Soon",
            &Url::parse("https://example.com/news/releases/none").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "No title",
            &Url::parse("https://example.com/news/placeholder").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "About Acme",
            &Url::parse("https://example.com/news/about-acme").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Guides & Articles",
            &Url::parse("https://example.com/blog/guides").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Gen Blogs | Impact",
            &Url::parse("https://example.com/blog/impact").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Gen Blogs | Family of Brands",
            &Url::parse("https://example.com/blog/family-of-brands").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Categories",
            &Url::parse("https://example.com/blog/guides").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "General Information",
            &Url::parse("https://example.com/news/performance").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Guide for writing community posts",
            &Url::parse("https://developer.example.com/discuss/t/writing-posts/10277")
                .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Machine Learning Terms: Complete Machine Learning & AI Glossary",
            &Url::parse("https://example.com/resources/glossary/").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Privacy Policy",
            &Url::parse(
                "https://example.com/component/content/article/23-privacy-policy.html?catid=2"
            )
            .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Videos | News and media | Example",
            &Url::parse("https://example.com/en/news-and-media/videos").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Video Library: Product and Brand Videos",
            &Url::parse("https://example.com/newsroom/videos").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Terms for using this publication",
            &Url::parse("https://example.com/en/legal-notice/").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "What types of cookies do we use?",
            &Url::parse("https://docs.example.com/en/articles/6497426-cookie-policy")
                .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Newsroom Terms & Conditions",
            &Url::parse("https://news.example.com/terms-and-conditions").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Privacy Policy - updated July 14, 2026",
            &Url::parse("https://example.com/legal/privacy").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Newsletter",
            &Url::parse("https://example.com/newsletter/").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "2024 News Archive",
            &Url::parse("https://example.com/news/2024-news-archive/").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "2020 News Release",
            &Url::parse("https://example.com/news/press-releases-2020/").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Employee interviews",
            &Url::parse("https://example.com/blog/employee-stories/").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Advertising Content Policies Overview",
            &Url::parse(
                "https://www.taboola.com/help/en/articles/3878198-advertising-content-policies-overview"
            )
            .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "How to find warranty documentation",
            &Url::parse("https://help.example.com/hc/en-us/articles/12345-warranty")
                .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Media Contacts",
            &Url::parse("https://example.com/newsroom/media-contact").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "PSEG's Social Media Channels",
            &Url::parse("https://example.com/newsroom/socialmedia").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Photo & Logo Request",
            &Url::parse(
                "https://investor.example.com/news-releases/photo-logo-request/default.aspx"
            )
            .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Acme Logo Use",
            &Url::parse("https://example.com/media/media-library/acme-logo-use")
                .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Atlassian Press Kit | Atlassian",
            &Url::parse("https://www.atlassian.com/company/news/press-kit").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Mediakit",
            &Url::parse("https://www.iconplc.com/news-events/mediakit").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Amer Sports fact sheet, logo, and images",
            &Url::parse("https://www.amersports.com/newsroom/materials-for-media/")
                .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Press assets | Pinterest Newsroom",
            &Url::parse("https://newsroom.pinterest.com/press-assets/").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Google Developers news and updates | Google Blog",
            &Url::parse("https://blog.google/innovation-and-ai/technology/developers-tools/")
                .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Braking System Archives - In The Garage with CarParts.com",
            &Url::parse("https://example.com/blog/auto-repair/braking-system/").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Grid Dynamics acquires Ekumen",
            &Url::parse("https://example.com/blog/media-and-news").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Explore topics",
            &Url::parse("https://example.com/blog/evs").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "USA media coverage.",
            &Url::parse("https://example.com/newsroom/media-coverage/usa").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Photos & Videos",
            &Url::parse("https://example.com/press/photos").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Product Tour Hub | Acme",
            &Url::parse("https://example.com/our-insights/resources/product-tour-hub")
                .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Timeline",
            &Url::parse("https://example.com/press/timeline").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "View more from Safety & Security",
            &Url::parse("https://example.com/blog/safety-security/").expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "Timeline of our 2026 product launch",
            &Url::parse("https://example.com/press/product-launch-timeline").expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "Company publishes its 2026 financial highlights",
            &Url::parse("https://example.com/news/2026-financial-highlights").expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "Carrier unveils a new building platform",
            &Url::parse(
                "https://example.com/media-resources/news/news-articles/carrier-unveils-platform"
            )
            .expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "Company announces product news and updates for July | Acme Blog",
            &Url::parse("https://example.com/blog/product-update").expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "Medicare Options for Employees and Retirees",
            &Url::parse("https://example.com/blog/medicare-options-for-employees-and-retirees")
                .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "monday.com press room",
            &Url::parse("https://monday.com/p/news/press-kit/").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Additional Resources",
            &Url::parse("https://www.veea.com/news/press-kit").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Taylor Morrison Home Corp. - Mediaroom - RSS Feeds",
            &Url::parse("https://newsroom.taylormorrison.com/all-rss-feeds?rsspage=20295")
                .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "RSS Feeds -",
            &Url::parse("https://example.com/blog/rss-feeds").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Advertising practices",
            &Url::parse("https://example.com/newsroom/advertising-practices.html")
                .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Prime Rate Information",
            &Url::parse("https://example.com/newsroom/prime-rate-information").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Nordic Markets - Corporate Actions",
            &Url::parse("https://www.nasdaq.com/european-market-activity/news/corporate-actions")
                .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Market Notices",
            &Url::parse("https://www.nasdaq.com/european-market-activity/news/market-notices")
                .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Subscribe for News Updates Via RSS | McKesson",
            &Url::parse("https://www.mckesson.com/about-us/newsroom/subscribe-to-mckesson-news/")
                .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Acme Instagram feed 12 May 2026",
            &Url::parse("https://example.com/instagram-feed/acme-instagram-feed-12-may-2026")
                .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Marketing Project Manager",
            &Url::parse("https://example.com/career_ca_marketing-project-manager/")
                .expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Field Application Engineer",
            &Url::parse("https://example.com/jobs/field-application-engineer").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "Insights",
            &Url::parse("https://example.com/insights").expect("valid URL")
        ));
        assert!(is_non_editorial_utility_article(
            "News Releases",
            &Url::parse("https://example.com/insights/news-releases").expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "Company launches a new subscription service",
            &Url::parse("https://example.com/news/new-subscription-service").expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "About our 2026 product launch",
            &Url::parse("https://example.com/news/about-our-product-launch").expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "Reducing GPU cold-start time",
            &Url::parse("https://example.com/docs/blogs/reducing-gpu-cold-start")
                .expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "Validated Cloud release notes",
            &Url::parse("https://support.example.com/en/article/release-notes-app-updates")
                .expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "Releases for June 2026",
            &Url::parse(
                "https://support.example.com/hc/en-us/articles/46376121877901-releases-for-june-2026"
            )
            .expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "The story behind Acme's new logo",
            &Url::parse("https://example.com/blog/the-story-behind-our-new-logo")
                .expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "Acme launches a new press kit for partners",
            &Url::parse("https://example.com/news/acme-launches-new-press-kit").expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "Acme launches RSS support for customers",
            &Url::parse("https://example.com/news/acme-launches-rss-support").expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "Customers subscribe to Acme after its RSS product launch",
            &Url::parse("https://example.com/news/rss-product-launch").expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "How to evaluate engineering job queues",
            &Url::parse("https://example.com/blog/jobs/engineering-job-queues").expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "Employee Story: Danielle",
            &Url::parse("https://blog.example.com/career-story-danielle").expect("valid URL")
        ));
    }

    #[test]
    fn identifies_non_editorial_subsections_inside_broad_content_hubs() {
        for url in [
            "https://example.com/resources/events/company-conference",
            "https://example.com/resources/testimonials/customer-profile",
            "https://example.com/resources/trainings/operator-course",
            "https://example.com/library/webinars/cloud-security",
            "https://example.com/resource-center/videos/product-demo",
            "https://example.com/our-insights/resources/demo-series/product-demo",
            "https://example.com/our-insights/resources/product-tour-hub",
        ] {
            assert!(
                is_non_editorial_utility_article(
                    "A substantial page with a plausible editorial title",
                    &Url::parse(url).expect("valid URL")
                ),
                "expected broad hub subsection to be non-editorial: {url}"
            );
        }

        for url in [
            "https://example.com/resources/blog/events-industry-trends",
            "https://example.com/resources/news/events/company-conference",
            "https://example.com/news-events/events/company-conference",
            "https://example.com/our-insights/resources/research-reports/industry-outlook",
        ] {
            assert!(
                !is_non_editorial_utility_article(
                    "Company publishes a substantive editorial update",
                    &Url::parse(url).expect("valid URL")
                ),
                "expected explicit editorial scope to remain eligible: {url}"
            );
        }
    }

    #[test]
    fn identifies_static_investor_relations_sections_without_blocking_press_releases() {
        for url in [
            "https://ir.example.com/en/overview/company-profile/",
            "https://ri.example.com/en/governance/board/",
            "https://investor.example.com/investor-relations/stock-information/quote/",
            "https://investors.example.com/en/financial-information/reports/",
        ] {
            assert!(
                is_non_editorial_utility_article(
                    "A plausible static investor page",
                    &Url::parse(url).expect("valid URL")
                ),
                "expected investor utility section to be rejected: {url}"
            );
        }
        assert!(is_non_editorial_utility_article(
            "Why invest in Acme?",
            &Url::parse("https://ir.example.com/en/why-invest-in-acme/").expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "Why invest in resilient infrastructure now?",
            &Url::parse("https://example.com/blog/why-invest-in-infrastructure")
                .expect("valid URL")
        ));
        assert!(!is_non_editorial_utility_article(
            "Acme announces a new independent director",
            &Url::parse(
                "https://ir.example.com/investor-relations/press-releases/new-independent-director"
            )
            .expect("valid URL")
        ));
    }

    #[test]
    fn identifies_market_quote_profiles_without_blocking_market_editorial_articles() {
        for (title, url) in [
            (
                "Planet Image International Ltd Stock Price Today | NASDAQ: YIBO Live - Investing.com",
                "https://www.investing.com/equities/planet-image-international",
            ),
            (
                "Acme Corporation (ACME) Stock Price, News, Quote & History - Yahoo Finance",
                "https://finance.yahoo.com/quote/ACME/",
            ),
            (
                "ACME Stock Quote: Price, Chart and Market News",
                "https://www.nasdaq.com/market-activity/stocks/acme",
            ),
        ] {
            assert!(
                is_non_editorial_utility_article(title, &Url::parse(url).expect("valid URL")),
                "expected market quote profile to be rejected: {url}"
            );
        }

        for (title, url) in [
            (
                "Rethinking diversification in an AI-driven world",
                "https://example.com/insights/equities/rethinking-diversification",
            ),
            (
                "Acme launches a new manufacturing platform",
                "https://finance.yahoo.com/quote/ACME/news/acme-launches-platform",
            ),
            (
                "Acme explains what moved its stock price after earnings",
                "https://example.com/news/stock-price-after-earnings",
            ),
        ] {
            assert!(
                !is_non_editorial_utility_article(title, &Url::parse(url).expect("valid URL")),
                "expected substantive market article to remain eligible: {url}"
            );
        }
    }
}
