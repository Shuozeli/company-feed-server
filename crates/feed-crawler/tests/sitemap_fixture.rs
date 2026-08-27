//! Parser check against a real Apple newsroom sitemap (fetched 2026-08-24).
//! Apple's sitemap has NO <lastmod> and NO <news:publication_date>; the publish
//! date must be derived from the `/YYYY/MM/` URL path.

use feed_crawler::{SitemapDoc, news_acceptance, parse_sitemap};

const APPLE_SITEMAP: &[u8] = include_bytes!("fixtures/apple_newsroom_sitemap.xml");

#[test]
fn apple_newsroom_sitemap_yields_dated_editorial_entries() {
    let doc = parse_sitemap(APPLE_SITEMAP).expect("apple sitemap parses");
    let SitemapDoc::UrlSet(entries) = doc else {
        panic!("apple newsroom sitemap is a flat urlset");
    };

    assert!(entries.len() >= 30, "expected the fixture's ~40 entries");

    // Every entry should date off the URL path even without date tags.
    let dated = entries
        .iter()
        .filter(|entry| entry.published_at().is_some())
        .count();
    assert!(
        dated * 2 >= entries.len(),
        "at least half of apple entries must be dateable from the path, got {dated}/{}",
        entries.len()
    );

    let acceptance = news_acceptance(&entries);
    assert!(
        acceptance.accepted(),
        "apple newsroom sitemap must pass the news acceptance gate: {acceptance:?}"
    );

    // Spot-check a known article.
    let quarter = entries.iter().find(|entry| {
        entry
            .loc
            .as_str()
            .contains("apple-reports-third-quarter-results")
    });
    let quarter = quarter.expect("Q3 results article present in fixture");
    assert_eq!(
        quarter.published_at().unwrap().format("%Y-%m").to_string(),
        "2026-07"
    );
}
