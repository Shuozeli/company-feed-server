//! Dark-launch probe for the sitemap crawl tier.
//!
//! Runs the real `SitemapCrawler` against one or more newsroom sitemap URLs and
//! prints the resulting batch (item count, metadata metrics, and a sample of
//! emitted URLs + derived publish dates). Proves the new crawl path on live data
//! without touching the job queue or database.
//!
//! Usage:
//!   cargo run -p feed-crawler --example sitemap_probe -- <sitemap_url> [more_urls...]

use chrono::Utc;
use feed_core::{Source, SourceKind, SourceStatus};
use feed_crawler::{SitemapCrawler, SitemapCrawlerConfig};
use url::Url;
use uuid::Uuid;

fn make_source(url: Url) -> Source {
    Source {
        id: Uuid::new_v4(),
        source_id: "example:sitemap".to_owned(),
        company_id: Uuid::new_v4(),
        kind: SourceKind::Sitemap,
        url,
        status: SourceStatus::Approved,
        freshness_slo_seconds: 86_400,
        browser_required: false,
        public_export_allowed: true,
        discovery_confidence: Some(0.85),
        metadata: serde_json::json!({}),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[tokio::main]
async fn main() {
    let urls: Vec<String> = std::env::args().skip(1).collect();
    if urls.is_empty() {
        eprintln!("usage: sitemap_probe <sitemap_url> [more_urls...]");
        std::process::exit(2);
    }

    let crawler = SitemapCrawler::new(SitemapCrawlerConfig::default())
        .expect("build sitemap crawler");

    for raw in urls {
        let Ok(url) = Url::parse(&raw) else {
            println!("[{raw}] INVALID URL");
            continue;
        };
        let source = make_source(url.clone());
        println!("\n==== {url} ====");
        match crawler.crawl(&source, None).await {
            Ok(batch) => {
                println!("metrics: {}", batch.metadata);
                println!("emitted {} items", batch.items.len());
                let dated = batch
                    .items
                    .iter()
                    .filter(|item| item.published_at.is_some())
                    .count();
                println!("with derived date: {dated}/{}", batch.items.len());
                for item in batch.items.iter().take(5) {
                    let date_source = item
                        .payload
                        .get("date_source")
                        .and_then(|value| value.as_str())
                        .unwrap_or("?");
                    println!(
                        "  {} | {} | {}",
                        item.published_at
                            .map(|dt| dt.format("%Y-%m-%d").to_string())
                            .unwrap_or_else(|| "----------".to_owned()),
                        date_source,
                        item.url
                    );
                }
            }
            Err(error) => println!("CRAWL ERROR: {error}"),
        }
    }
}
