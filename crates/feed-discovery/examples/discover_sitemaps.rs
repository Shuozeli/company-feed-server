//! Phase-2 dark-launch: run discovery with `probe_sitemaps=true` for a few real
//! companies and print the discovered sitemap candidates (kind, confidence,
//! evidence). Proves the discovery half of the sitemap tier on live robots.txt.

use chrono::Utc;
use feed_core::{Company, LifecycleStatus, OwnershipStatus, SourceKind};
use feed_discovery::{DiscoveryClient, DiscoveryConfig};
use url::Url;
use uuid::Uuid;

fn company(key: &str, name: &str, homepage: &str, newsroom: Option<&str>) -> Company {
    Company {
        id: Uuid::new_v4(),
        company_key: key.to_owned(),
        name: name.to_owned(),
        aliases: Vec::new(),
        ownership_status: OwnershipStatus::Public,
        lifecycle_status: LifecycleStatus::Active,
        listings: Vec::new(),
        homepage_url: Url::parse(homepage).ok(),
        investor_relations_url: None,
        newsroom_url: newsroom.and_then(|value| Url::parse(value).ok()),
        blog_url: None,
        hints: Vec::new(),
        discovery_enabled: true,
        discovery_not_before: Utc::now(),
        discovery_cadence_seconds: 86_400,
        metadata: serde_json::json!({}),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[tokio::main]
async fn main() {
    let client = DiscoveryClient::new(DiscoveryConfig {
        probe_sitemaps: true,
        ..DiscoveryConfig::default()
    })
    .expect("discovery client");

    let companies = vec![
        company("apple", "Apple", "https://www.apple.com", Some("https://www.apple.com/newsroom/")),
        company("amazon", "Amazon", "https://www.aboutamazon.com", None),
        company("broadcom", "Broadcom", "https://www.broadcom.com", None),
        company("tesla", "Tesla", "https://www.tesla.com", None),
    ];

    for company in companies {
        println!("\n==== {} ====", company.name);
        match client.discover_company(&company).await {
            Ok(report) => {
                let sitemaps: Vec<_> = report
                    .candidates
                    .iter()
                    .filter(|candidate| candidate.candidate_kind == SourceKind::Sitemap)
                    .collect();
                if sitemaps.is_empty() {
                    println!("  no sitemap candidate (falls through to RSS/HTML tier)");
                }
                for candidate in sitemaps {
                    println!(
                        "  SITEMAP conf={:.2} {}",
                        candidate.confidence, candidate.candidate_url
                    );
                }
            }
            Err(error) => println!("  discovery error: {error}"),
        }
    }
}
