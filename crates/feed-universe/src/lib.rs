use std::{collections::BTreeMap, fs, path::Path};

use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use url::Url;

pub const COMPANY_UNIVERSE_SCHEMA_VERSION: &str = "company-universe.v2";
pub const MAX_UNIVERSE_COMPANIES: usize = 100_000;
const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_HINTS: usize = 50;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanyUniverseDocument {
    pub schema_version: String,
    pub source: UniverseSource,
    pub companies: Vec<UniverseCompany>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UniverseSource {
    pub name: String,
    #[serde(default)]
    pub revision: Option<String>,
    pub generated_at: DateTime<Utc>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UniverseCompany {
    pub source_company_id: String,
    pub company_key: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub ownership_status: OwnershipStatus,
    #[serde(default)]
    pub lifecycle_status: LifecycleStatus,
    #[serde(default)]
    pub listings: Vec<UniverseListing>,
    #[serde(default)]
    pub market_cap: Option<i64>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub ipo_year: Option<i32>,
    #[serde(default)]
    pub sector: Option<String>,
    #[serde(default)]
    pub industry: Option<String>,
    #[serde(default)]
    pub identifiers: BTreeMap<String, String>,
    #[serde(default)]
    pub homepage_url: Option<Url>,
    #[serde(default)]
    pub investor_relations_url: Option<Url>,
    #[serde(default)]
    pub newsroom_url: Option<Url>,
    #[serde(default)]
    pub blog_url: Option<Url>,
    #[serde(default)]
    pub hints: Vec<Url>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipStatus {
    Public,
    Private,
    StateOwned,
    Cooperative,
    #[default]
    Unknown,
}

impl OwnershipStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
            Self::StateOwned => "state_owned",
            Self::Cooperative => "cooperative",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStatus {
    Active,
    Acquired,
    Inactive,
    #[default]
    Unknown,
}

impl LifecycleStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Acquired => "acquired",
            Self::Inactive => "inactive",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UniverseListing {
    pub ticker: String,
    #[serde(default)]
    pub exchange: Option<String>,
    #[serde(default)]
    pub is_primary: bool,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedUniverse {
    pub document: CompanyUniverseDocument,
    pub input_sha256: String,
    pub input_bytes: usize,
}

impl ValidatedUniverse {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, UniverseError> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|source| UniverseError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, UniverseError> {
        if bytes.is_empty() {
            return Err(UniverseError::Validation(
                "company universe input cannot be empty".to_owned(),
            ));
        }
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(UniverseError::Validation(format!(
                "company universe input exceeds {MAX_INPUT_BYTES} bytes"
            )));
        }
        let document: CompanyUniverseDocument = serde_json::from_slice(bytes)?;
        validate_document(&document)?;
        Ok(Self {
            document,
            input_sha256: hex::encode(Sha256::digest(bytes)),
            input_bytes: bytes.len(),
        })
    }
}

fn validate_document(document: &CompanyUniverseDocument) -> Result<(), UniverseError> {
    if document.schema_version != COMPANY_UNIVERSE_SCHEMA_VERSION {
        return Err(UniverseError::Validation(format!(
            "unsupported schema version {}",
            document.schema_version
        )));
    }
    validate_text("source name", &document.source.name, 128)?;
    if let Some(revision) = document.source.revision.as_deref() {
        validate_text("source revision", revision, 256)?;
    }
    validate_object("source metadata", &document.source.metadata)?;
    if document.companies.is_empty() {
        return Err(UniverseError::Validation(
            "company universe must contain at least one company".to_owned(),
        ));
    }
    if document.companies.len() > MAX_UNIVERSE_COMPANIES {
        return Err(UniverseError::Validation(format!(
            "company universe exceeds {MAX_UNIVERSE_COMPANIES} companies"
        )));
    }

    let mut source_ids = std::collections::HashSet::with_capacity(document.companies.len());
    let mut company_keys = std::collections::HashSet::with_capacity(document.companies.len());
    for (index, company) in document.companies.iter().enumerate() {
        validate_company(index + 1, company)?;
        if !source_ids.insert(company.source_company_id.as_str()) {
            return Err(UniverseError::Validation(format!(
                "duplicate source_company_id {} at company row {}",
                company.source_company_id,
                index + 1
            )));
        }
        if !company_keys.insert(company.company_key.as_str()) {
            return Err(UniverseError::Validation(format!(
                "duplicate company_key {} at company row {}",
                company.company_key,
                index + 1
            )));
        }
    }
    Ok(())
}

fn validate_company(row: usize, company: &UniverseCompany) -> Result<(), UniverseError> {
    validate_text(
        &format!("company row {row} source_company_id"),
        &company.source_company_id,
        256,
    )?;
    validate_company_key(row, &company.company_key)?;
    validate_text(&format!("company row {row} name"), &company.name, 512)?;
    let mut aliases = std::collections::HashSet::with_capacity(company.aliases.len());
    for alias in &company.aliases {
        validate_text(&format!("company row {row} alias"), alias, 512)?;
        if !aliases.insert(alias.trim().to_lowercase()) {
            return Err(UniverseError::Validation(format!(
                "company row {row} contains duplicate alias {alias}"
            )));
        }
    }
    for (field, value) in [
        ("country", company.country.as_deref()),
        ("sector", company.sector.as_deref()),
        ("industry", company.industry.as_deref()),
    ] {
        if let Some(value) = value {
            validate_text(&format!("company row {row} {field}"), value, 512)?;
        }
    }
    if company.market_cap.is_some_and(|value| value < 0) {
        return Err(UniverseError::Validation(format!(
            "company row {row} market_cap cannot be negative"
        )));
    }
    if company
        .ipo_year
        .is_some_and(|year| !(1600..=Utc::now().year() + 1).contains(&year))
    {
        return Err(UniverseError::Validation(format!(
            "company row {row} has an invalid ipo_year"
        )));
    }
    let mut listings = std::collections::HashSet::with_capacity(company.listings.len());
    let mut primary_listings = 0;
    for listing in &company.listings {
        validate_ticker(row, &listing.ticker)?;
        if let Some(exchange) = listing.exchange.as_deref() {
            validate_text(
                &format!("company row {row} listing exchange"),
                exchange,
                128,
            )?;
        }
        validate_object(
            &format!("company row {row} listing metadata"),
            &listing.metadata,
        )?;
        let listing_key = (
            listing.ticker.as_str(),
            listing.exchange.as_deref().unwrap_or_default(),
        );
        if !listings.insert(listing_key) {
            return Err(UniverseError::Validation(format!(
                "company row {row} contains duplicate listing {}",
                listing.ticker
            )));
        }
        primary_listings += usize::from(listing.is_primary);
    }
    if primary_listings > 1 {
        return Err(UniverseError::Validation(format!(
            "company row {row} has more than one primary listing"
        )));
    }
    if company.hints.len() > MAX_HINTS {
        return Err(UniverseError::Validation(format!(
            "company row {row} exceeds {MAX_HINTS} hints"
        )));
    }
    for url in [
        company.homepage_url.as_ref(),
        company.investor_relations_url.as_ref(),
        company.newsroom_url.as_ref(),
        company.blog_url.as_ref(),
    ]
    .into_iter()
    .flatten()
    .chain(company.hints.iter())
    {
        validate_public_url(row, url)?;
    }
    for (kind, value) in &company.identifiers {
        validate_text(&format!("company row {row} identifier kind"), kind, 64)?;
        validate_text(&format!("company row {row} identifier {kind}"), value, 256)?;
    }
    validate_object(&format!("company row {row} metadata"), &company.metadata)?;
    Ok(())
}

fn validate_company_key(row: usize, company_key: &str) -> Result<(), UniverseError> {
    if company_key.is_empty() || company_key.len() > 160 {
        return Err(UniverseError::Validation(format!(
            "company row {row} company_key must contain 1 to 160 characters"
        )));
    }
    let valid = company_key.split('-').all(|part| {
        !part.is_empty()
            && part
                .chars()
                .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
    });
    if !valid {
        return Err(UniverseError::Validation(format!(
            "company row {row} company_key must be a lowercase ASCII slug"
        )));
    }
    Ok(())
}

fn validate_ticker(row: usize, ticker: &str) -> Result<(), UniverseError> {
    if ticker.is_empty() || ticker.len() > 32 {
        return Err(UniverseError::Validation(format!(
            "company row {row} ticker must contain 1 to 32 characters"
        )));
    }
    if ticker != ticker.to_ascii_uppercase() {
        return Err(UniverseError::Validation(format!(
            "company row {row} ticker {ticker} must be uppercase"
        )));
    }
    if !ticker.chars().all(|character| {
        character.is_ascii_uppercase()
            || character.is_ascii_digit()
            || matches!(character, '.' | '-' | '/' | '^')
    }) {
        return Err(UniverseError::Validation(format!(
            "company row {row} ticker {ticker} contains unsupported characters"
        )));
    }
    Ok(())
}

fn validate_text(field: &str, value: &str, max_len: usize) -> Result<(), UniverseError> {
    if value.trim().is_empty() {
        return Err(UniverseError::Validation(format!(
            "{field} cannot be empty"
        )));
    }
    if value.len() > max_len {
        return Err(UniverseError::Validation(format!(
            "{field} exceeds {max_len} bytes"
        )));
    }
    Ok(())
}

fn validate_object(field: &str, value: &Value) -> Result<(), UniverseError> {
    if !value.is_object() {
        return Err(UniverseError::Validation(format!(
            "{field} must be a JSON object"
        )));
    }
    Ok(())
}

fn validate_public_url(row: usize, url: &Url) -> Result<(), UniverseError> {
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(UniverseError::Validation(format!(
            "company row {row} URL {url} must be an HTTP(S) URL without credentials"
        )));
    }
    Ok(())
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

#[derive(Debug, thiserror::Error)]
pub enum UniverseError {
    #[error("failed to read company universe file {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid company universe JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid company universe: {0}")]
    Validation(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> serde_json::Value {
        serde_json::json!({
            "schema_version": COMPANY_UNIVERSE_SCHEMA_VERSION,
            "source": {
                "name": "fixture",
                "revision": "2026-07-19",
                "generated_at": "2026-07-19T00:00:00Z",
                "metadata": {}
            },
            "companies": [{
                "source_company_id": "berkshire-hathaway",
                "company_key": "berkshire-hathaway",
                "name": "Berkshire Hathaway Inc.",
                "aliases": ["Berkshire Hathaway"],
                "ownership_status": "public",
                "lifecycle_status": "active",
                "listings": [{
                    "ticker": "BRK-B",
                    "exchange": "NYSE",
                    "is_primary": true,
                    "metadata": {}
                }],
                "market_cap": 1000000000,
                "country": "United States",
                "ipo_year": 1980,
                "sector": "Financials",
                "industry": "Insurance",
                "identifiers": {},
                "homepage_url": "https://www.berkshirehathaway.com/",
                "investor_relations_url": null,
                "newsroom_url": null,
                "blog_url": null,
                "hints": [],
                "metadata": {}
            }]
        })
    }

    #[test]
    fn parses_and_hashes_a_valid_document() {
        let bytes = serde_json::to_vec(&fixture()).expect("serialize fixture");
        let validated = ValidatedUniverse::parse(&bytes).expect("valid universe");
        assert_eq!(
            validated.document.companies[0].company_key,
            "berkshire-hathaway"
        );
        assert_eq!(validated.document.companies[0].listings[0].ticker, "BRK-B");
        assert_eq!(validated.input_sha256.len(), 64);
        assert_eq!(validated.input_bytes, bytes.len());
    }

    #[test]
    fn rejects_duplicate_company_keys() {
        let mut value = fixture();
        let mut duplicate = value["companies"][0].clone();
        duplicate["source_company_id"] = serde_json::json!("another-source-id");
        value["companies"]
            .as_array_mut()
            .expect("companies")
            .push(duplicate);
        let error = ValidatedUniverse::parse(
            &serde_json::to_vec(&value).expect("serialize duplicate fixture"),
        )
        .expect_err("duplicate must fail");
        assert!(
            error
                .to_string()
                .contains("duplicate company_key berkshire-hathaway")
        );
    }

    #[test]
    fn accepts_a_private_company_without_a_listing() {
        let mut value = fixture();
        value["companies"][0]["source_company_id"] = serde_json::json!("openai");
        value["companies"][0]["company_key"] = serde_json::json!("openai");
        value["companies"][0]["name"] = serde_json::json!("OpenAI");
        value["companies"][0]["ownership_status"] = serde_json::json!("private");
        value["companies"][0]["listings"] = serde_json::json!([]);
        let validated = ValidatedUniverse::parse(
            &serde_json::to_vec(&value).expect("serialize private-company fixture"),
        )
        .expect("private company should be valid");
        assert!(validated.document.companies[0].listings.is_empty());
    }

    #[test]
    fn rejects_non_http_urls_and_non_object_metadata() {
        let mut value = fixture();
        value["companies"][0]["homepage_url"] = serde_json::json!("file:///etc/passwd");
        value["companies"][0]["metadata"] = serde_json::json!([]);
        let error = ValidatedUniverse::parse(
            &serde_json::to_vec(&value).expect("serialize invalid fixture"),
        )
        .expect_err("invalid document must fail");
        assert!(error.to_string().contains("HTTP(S)"));
    }
}
