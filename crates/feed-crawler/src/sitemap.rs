//! Sitemap parsing + news-scope classification for the sitemap discovery tier.
//!
//! A newsroom sitemap is a rate-limit-friendly, structurally-stable article-URL
//! inventory. This module turns sitemap XML into typed entries and derives a
//! publish date from `<lastmod>`, `<news:publication_date>`, or a date embedded
//! in the URL path (`/YYYY/MM/` or `/YYYY-MM-DD/`) -- the last case is required
//! because plenty of real newsroom sitemaps (verified: Apple) carry neither date
//! tag. See docs/design/2026-08-24-sitemap-discovery-tier.md.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
pub use feed_core::{is_news_scoped_url, news_score, url_is_sitemap};
use quick_xml::Reader;
use quick_xml::events::Event;
use url::Url;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SitemapEntry {
    pub loc: Url,
    pub lastmod: Option<DateTime<Utc>>,
    pub news_date: Option<DateTime<Utc>>,
}

impl SitemapEntry {
    /// Best available publish date: explicit news tag, else `<lastmod>`, else a
    /// date parsed out of the URL path. `None` means the crawler must fetch the
    /// article page to date it.
    pub fn published_at(&self) -> Option<DateTime<Utc>> {
        self.news_date
            .or(self.lastmod)
            .or_else(|| date_from_url_path(&self.loc))
    }

    /// The MUTABLE freshness signal: a `<news:publication_date>` or `<lastmod>`
    /// that a publisher bumps when the article changes. `None` for sitemaps that
    /// only carry an immutable URL-path date (e.g. Apple `/2026/08/slug`) — those
    /// cannot signal a re-edit, so incremental crawls fall back to dedup for them.
    pub fn change_signal(&self) -> Option<DateTime<Utc>> {
        self.news_date.or(self.lastmod)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SitemapDoc {
    /// `<sitemapindex>` -- points at child sitemaps.
    Index(Vec<Url>),
    /// `<urlset>` -- points at pages.
    UrlSet(Vec<SitemapEntry>),
}

#[derive(Debug, thiserror::Error)]
pub enum SitemapError {
    #[error("sitemap xml parse error: {0}")]
    Xml(String),
    #[error("document is neither a <urlset> nor a <sitemapindex>")]
    NotASitemap,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Leaf {
    None,
    Loc,
    Lastmod,
    NewsDate,
}

/// Parse sitemap XML bytes into a typed document.
pub fn parse_sitemap(bytes: &[u8]) -> Result<SitemapDoc, SitemapError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut saw_urlset = false;
    let mut saw_index = false;

    // urlset accumulation
    let mut entries: Vec<SitemapEntry> = Vec::new();
    let mut cur_loc = String::new();
    let mut cur_lastmod = String::new();
    let mut cur_newsdate = String::new();

    // sitemapindex accumulation
    let mut child_sitemaps: Vec<Url> = Vec::new();
    let mut in_child = false;
    let mut cur_child_loc = String::new();

    let mut leaf = Leaf::None;

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|error| SitemapError::Xml(error.to_string()))?
        {
            Event::Start(element) => {
                let local = element.local_name();
                match local.as_ref() {
                    b"urlset" => saw_urlset = true,
                    b"sitemapindex" => saw_index = true,
                    b"url" => {
                        cur_loc.clear();
                        cur_lastmod.clear();
                        cur_newsdate.clear();
                    }
                    b"sitemap" => {
                        in_child = true;
                        cur_child_loc.clear();
                    }
                    b"loc" => leaf = Leaf::Loc,
                    b"lastmod" => leaf = Leaf::Lastmod,
                    name if name.ends_with(b"publication_date") => leaf = Leaf::NewsDate,
                    _ => {}
                }
            }
            Event::Text(text) => {
                if leaf != Leaf::None {
                    let value = text
                        .unescape()
                        .map_err(|error| SitemapError::Xml(error.to_string()))?;
                    let target = match leaf {
                        Leaf::Loc if in_child => &mut cur_child_loc,
                        Leaf::Loc => &mut cur_loc,
                        Leaf::Lastmod => &mut cur_lastmod,
                        Leaf::NewsDate => &mut cur_newsdate,
                        Leaf::None => unreachable!("leaf None handled by outer guard"),
                    };
                    target.push_str(value.as_ref());
                }
            }
            Event::End(element) => {
                let local = element.local_name();
                match local.as_ref() {
                    b"loc" | b"lastmod" => leaf = Leaf::None,
                    name if name.ends_with(b"publication_date") => leaf = Leaf::None,
                    b"url" => {
                        if let Ok(loc) = Url::parse(cur_loc.trim()) {
                            entries.push(SitemapEntry {
                                loc,
                                lastmod: parse_flexible_date(&cur_lastmod),
                                news_date: parse_flexible_date(&cur_newsdate),
                            });
                        }
                    }
                    b"sitemap" => {
                        if let Ok(loc) = Url::parse(cur_child_loc.trim()) {
                            child_sitemaps.push(loc);
                        }
                        in_child = false;
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    if saw_index || (!saw_urlset && !child_sitemaps.is_empty()) {
        Ok(SitemapDoc::Index(child_sitemaps))
    } else if saw_urlset || !entries.is_empty() {
        Ok(SitemapDoc::UrlSet(entries))
    } else {
        Err(SitemapError::NotASitemap)
    }
}

/// Parse a `<lastmod>` / `<news:publication_date>` value. Accepts RFC3339
/// (`2026-08-04T12:00:00Z`) and bare `YYYY-MM-DD`.
fn parse_flexible_date(raw: &str) -> Option<DateTime<Utc>> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return date
            .and_hms_opt(0, 0, 0)
            .map(|naive| Utc.from_utc_datetime(&naive));
    }
    None
}

/// Derive a publish date from a date embedded in the URL path, e.g.
/// `/newsroom/2026/08/slug/` or `/2026-08-04/slug`. Returns midnight UTC.
pub fn date_from_url_path(url: &Url) -> Option<DateTime<Utc>> {
    let segments: Vec<&str> = url
        .path_segments()
        .map(|parts| parts.filter(|part| !part.is_empty()).collect())
        .unwrap_or_default();

    // Case A: a single `YYYY-MM-DD` segment.
    for segment in &segments {
        if let Ok(date) = NaiveDate::parse_from_str(segment, "%Y-%m-%d") {
            return date
                .and_hms_opt(0, 0, 0)
                .map(|naive| Utc.from_utc_datetime(&naive));
        }
    }

    // Case B: consecutive `YYYY`, `MM`, optional `DD` segments.
    for window in segments.windows(2) {
        let year = parse_year(window[0]);
        let month = parse_month(window[1]);
        if let (Some(year), Some(month)) = (year, month) {
            // Look for an optional day segment immediately after.
            let day = segments
                .iter()
                .position(|part| *part == window[1])
                .and_then(|index| segments.get(index + 1))
                .and_then(|part| parse_day(part))
                .unwrap_or(1);
            if let Some(date) = NaiveDate::from_ymd_opt(year, month, day) {
                return date
                    .and_hms_opt(0, 0, 0)
                    .map(|naive| Utc.from_utc_datetime(&naive));
            }
        }
    }
    None
}

fn parse_year(segment: &str) -> Option<i32> {
    if segment.len() == 4 && segment.bytes().all(|byte| byte.is_ascii_digit()) {
        let year: i32 = segment.parse().ok()?;
        if (1990..=2099).contains(&year) {
            return Some(year);
        }
    }
    None
}

fn parse_month(segment: &str) -> Option<u32> {
    if segment.len() == 2 && segment.bytes().all(|byte| byte.is_ascii_digit()) {
        let month: u32 = segment.parse().ok()?;
        if (1..=12).contains(&month) {
            return Some(month);
        }
    }
    None
}

fn parse_day(segment: &str) -> Option<u32> {
    if segment.len() == 2 && segment.bytes().all(|byte| byte.is_ascii_digit()) {
        let day: u32 = segment.parse().ok()?;
        if (1..=31).contains(&day) {
            return Some(day);
        }
    }
    None
}

/// Acceptance gate for a parsed `<urlset>`: it must yield at least
/// `min_dated` entries that are both date-derivable and editorial-scoped.
pub fn news_acceptance(entries: &[SitemapEntry]) -> NewsAcceptance {
    let mut dated = 0_usize;
    let mut editorial = 0_usize;
    let mut dated_editorial = 0_usize;
    for entry in entries {
        let has_date = entry.published_at().is_some();
        let is_editorial = news_score(&entry.loc) >= 0;
        if has_date {
            dated += 1;
        }
        if is_editorial {
            editorial += 1;
        }
        if has_date && is_editorial {
            dated_editorial += 1;
        }
    }
    NewsAcceptance {
        total: entries.len(),
        dated,
        editorial,
        dated_editorial,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NewsAcceptance {
    pub total: usize,
    pub dated: usize,
    pub editorial: usize,
    pub dated_editorial: usize,
}

impl NewsAcceptance {
    /// Accept when a plurality of entries are dated editorial pages.
    pub fn accepted(&self) -> bool {
        self.dated_editorial > 0 && self.dated_editorial * 2 >= self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(value: &str) -> Url {
        Url::parse(value).expect("valid test url")
    }

    #[test]
    fn parses_flat_urlset() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
        <urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
          <url><loc>https://www.apple.com/newsroom/2026/08/apple-reports-third-quarter-results/</loc></url>
          <url><loc>https://www.apple.com/newsroom/2026/07/apple-upgrade-launches/</loc></url>
        </urlset>"#;
        let doc = parse_sitemap(xml).expect("parses");
        match doc {
            SitemapDoc::UrlSet(entries) => {
                assert_eq!(entries.len(), 2);
                // No date tags -> date must come from the URL path.
                let date = entries[0].published_at().expect("date from path");
                assert_eq!(date.format("%Y-%m").to_string(), "2026-08");
            }
            SitemapDoc::Index(_) => panic!("expected urlset"),
        }
    }

    #[test]
    fn parses_sitemapindex() {
        let xml = br#"<?xml version="1.0"?>
        <sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
          <sitemap><loc>https://x.com/news-sitemap.xml</loc></sitemap>
          <sitemap><loc>https://x.com/shop-sitemap.xml</loc></sitemap>
        </sitemapindex>"#;
        let doc = parse_sitemap(xml).expect("parses");
        match doc {
            SitemapDoc::Index(children) => {
                assert_eq!(children.len(), 2);
                assert!(is_news_scoped_url(&children[0]));
                assert!(!is_news_scoped_url(&children[1]));
            }
            SitemapDoc::UrlSet(_) => panic!("expected index"),
        }
    }

    #[test]
    fn parses_google_news_publication_date() {
        let xml = br#"<?xml version="1.0"?>
        <urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"
                xmlns:news="http://www.google.com/schemas/sitemap-news/0.9">
          <url>
            <loc>https://ir.example.com/press/some-release</loc>
            <news:news><news:publication_date>2026-08-04T09:30:00Z</news:publication_date></news:news>
          </url>
        </urlset>"#;
        let doc = parse_sitemap(xml).expect("parses");
        let SitemapDoc::UrlSet(entries) = doc else {
            panic!("expected urlset");
        };
        let date = entries[0].news_date.expect("news date");
        assert_eq!(date.format("%Y-%m-%dT%H:%M").to_string(), "2026-08-04T09:30");
    }

    #[test]
    fn lastmod_used_when_no_path_date() {
        let xml = br#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
          <url><loc>https://x.com/press/story-slug</loc><lastmod>2026-08-01</lastmod></url>
        </urlset>"#;
        let SitemapDoc::UrlSet(entries) = parse_sitemap(xml).expect("parses") else {
            panic!("expected urlset");
        };
        assert_eq!(
            entries[0].published_at().unwrap().format("%Y-%m-%d").to_string(),
            "2026-08-01"
        );
    }

    #[test]
    fn date_from_path_variants() {
        assert!(date_from_url_path(&url("https://x.com/2026/08/04/slug")).is_some());
        assert!(date_from_url_path(&url("https://x.com/news/2026/08/slug")).is_some());
        assert!(date_from_url_path(&url("https://x.com/2026-08-04/slug")).is_some());
        assert!(date_from_url_path(&url("https://x.com/about/team")).is_none());
        // Year out of range is ignored.
        assert!(date_from_url_path(&url("https://x.com/1200/08/slug")).is_none());
    }

    #[test]
    fn change_signal_only_from_mutable_dates() {
        // URL-path date is immutable -> no change signal (dedup handles novelty).
        let path_only = SitemapEntry {
            loc: url("https://www.apple.com/newsroom/2026/08/x"),
            lastmod: None,
            news_date: None,
        };
        assert!(path_only.published_at().is_some());
        assert!(path_only.change_signal().is_none());

        // lastmod is mutable -> it IS a change signal.
        let with_lastmod = SitemapEntry {
            loc: url("https://x.com/press/y"),
            lastmod: parse_flexible_date("2026-08-01"),
            news_date: None,
        };
        assert!(with_lastmod.change_signal().is_some());
    }

    #[test]
    fn url_is_sitemap_detects_nested_children() {
        // Broadcom-style: a urlset entry that is itself a child sitemap.
        assert!(url_is_sitemap(&url("https://www.broadcom.com/blog/sitemap.xml")));
        assert!(url_is_sitemap(&url("https://x.com/sitemap_index.xml")));
        assert!(!url_is_sitemap(&url(
            "https://www.apple.com/newsroom/2026/08/apple-reports"
        )));
    }

    #[test]
    fn news_scoping_prefers_editorial() {
        assert!(news_score(&url("https://www.apple.com/newsroom/sitemap.xml")) > 0);
        assert!(news_score(&url("https://x.com/press/2026/08/x")) > 0);
        assert!(news_score(&url("https://www.apple.com/shop/sitemap.xml")) < 0);
        assert!(news_score(&url("https://www.apple.com/retail/sitemap.xml")) < 0);
    }

    #[test]
    fn acceptance_requires_dated_editorial_plurality() {
        let entries = vec![
            SitemapEntry {
                loc: url("https://x.com/news/2026/08/a"),
                lastmod: None,
                news_date: None,
            },
            SitemapEntry {
                loc: url("https://x.com/news/2026/08/b"),
                lastmod: None,
                news_date: None,
            },
        ];
        assert!(news_acceptance(&entries).accepted());

        let inventory = vec![SitemapEntry {
            loc: url("https://x.com/shop/product-42"),
            lastmod: None,
            news_date: None,
        }];
        assert!(!news_acceptance(&inventory).accepted());
    }
}
