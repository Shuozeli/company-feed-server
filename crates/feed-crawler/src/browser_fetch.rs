//! Browser-based page fetch via the dragbv2-browser `FetchPage` RPC, used for
//! recipes whose `fetch_profile` is `ResidentialBrowser` — WAF-blocked IR-CMS
//! hosts (Q4/West `ir.*`/`investor.*`) or JS-rendered article feeds that a plain
//! HTTP GET cannot read but a real Chrome over CDP can. The call routes through a
//! residential-egress Chrome (e.g. the alienware CDP) that passes the WAF's JS
//! challenge, waiting for the article-link selector before capturing HTML.

use std::time::Duration;

use tonic::transport::Channel;
use url::Url;

mod pb {
    tonic::include_proto!("dragbv2.browser");
}

use pb::FetchPageRequest;
use pb::browser_client::BrowserClient;

#[derive(Clone, Debug)]
pub struct BrowserFetchConfig {
    /// dragbv2-browser gRPC endpoint, e.g.
    /// `http://dragbv2-browser-k3spod-yuacx.tail8f3b66.ts.net:26420`.
    pub endpoint: String,
    /// WAF-passing residential Chrome CDP endpoint routed per-request, e.g.
    /// `http://alienware-win-yuacx:9222`.
    pub cdp_url: String,
    /// Per-call navigation + connect timeout.
    pub timeout: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserFetchError {
    #[error("browser fetch endpoint is invalid: {0}")]
    Config(String),
    #[error("browser fetch transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("browser fetch rpc error: {0}")]
    Rpc(#[from] tonic::Status),
    #[error("browser fetch returned an invalid final url: {0}")]
    InvalidFinalUrl(String),
}

/// Thin client over dragbv2-browser `FetchPage`. Cheap to clone; opens a fresh
/// channel per fetch (browser fetches are slow and rare, so connection reuse is
/// not worth the shared-state complexity).
#[derive(Clone, Debug)]
pub struct BrowserFetcher {
    config: BrowserFetchConfig,
}

impl BrowserFetcher {
    pub fn new(config: BrowserFetchConfig) -> Self {
        Self { config }
    }

    /// Fetch a fully-rendered page. `wait_for_selector` is the recipe's
    /// article-link selector, so the call waits until the article list is present
    /// (past any WAF interstitial / client-side render) before capturing HTML.
    /// Returns `(final_url, html)` to match the plain-HTTP listing fetch.
    pub async fn fetch(
        &self,
        url: &Url,
        wait_for_selector: &str,
    ) -> Result<(Url, String), BrowserFetchError> {
        let channel = Channel::from_shared(self.config.endpoint.clone())
            .map_err(|error| BrowserFetchError::Config(error.to_string()))?
            .connect_timeout(self.config.timeout)
            .timeout(self.config.timeout)
            .connect()
            .await?;
        let mut client = BrowserClient::new(channel);

        let request = FetchPageRequest {
            url: url.to_string(),
            wait_for_selector: wait_for_selector.to_owned(),
            timeout_ms: u64::try_from(self.config.timeout.as_millis()).unwrap_or(u64::MAX),
            cdp_url: self.config.cdp_url.clone(),
        };
        let response = client.fetch_page(request).await?.into_inner();

        let final_url = if response.final_url.is_empty() {
            url.clone()
        } else {
            Url::parse(&response.final_url)
                .map_err(|_| BrowserFetchError::InvalidFinalUrl(response.final_url.clone()))?
        };
        Ok((final_url, response.html))
    }
}
