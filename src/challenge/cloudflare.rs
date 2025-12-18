//! Cloudflare challenge detection, handling, and fallback logic.

use anyhow::Result;
use thirtyfour::prelude::*;

use crate::scrappey::{ScrappeyClient, ScrappeyGetRequest, ScrappeyResponse};

use super::{ChallengeHandler, title_contains};

/// Cloudflare challenge handler implementation.
pub struct CloudflareHandler;

impl CloudflareHandler {
    /// Create a new Cloudflare challenge handler.
    pub fn new() -> Self {
        Self
    }
}

impl Default for CloudflareHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ChallengeHandler for CloudflareHandler {
    fn name(&self) -> &'static str {
        "Cloudflare"
    }

    async fn is_protected(&self, driver: &mut WebDriver) -> bool {
        title_contains(driver, "Just a moment...").await
    }
}

/// Fallback: Use Scrappey API to resolve Cloudflare challenge if browser automation fails.
/// This is a standalone function that can be called when the browser-based challenge handling times out.
///
/// # Arguments
/// * `url` - The URL to resolve
/// * `api_key` - Scrappey API key
/// * `proxy` - Proxy string (e.g., "http://user:pass@host:port")
/// * `timeout` - Request timeout in seconds
pub async fn scrappey_resolve(
    url: String,
    api_key: String,
    proxy: &str,
    timeout: u64,
) -> Result<ScrappeyResponse> {
    let client = ScrappeyClient::new(api_key);
    let request = ScrappeyGetRequest {
        url,
        proxy: Some(proxy.to_string()),
        ..Default::default()
    };
    client.get(request, timeout).await
}
