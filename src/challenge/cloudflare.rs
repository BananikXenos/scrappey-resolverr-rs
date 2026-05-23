//! Cloudflare challenge detection, handling, and fallback logic.

use anyhow::Result;
use log::{debug, info, warn};
use thirtyfour::{Cookie, prelude::*};

use crate::browser::Response;
use crate::config::{ProxyConfig, ScrappeyConfig};
use crate::logging::{LogContext, TimingLogger};
use crate::scrappey::{ScrappeyClient, ScrappeyGetRequest};

use super::{ChallengeHandler, title_contains};

/// Fraction of timeout allocated to initial Cloudflare challenge attempt
const CLOUDFLARE_TIMEOUT_FRACTION: u64 = 3;

/// Cloudflare challenge handler implementation.
pub struct CloudflareHandler;

impl CloudflareHandler {
    /// Create a new Cloudflare challenge handler.
    pub fn new() -> Self {
        Self
    }

    /// Handle Cloudflare challenge with optional Scrappey fallback.
    pub async fn handle_with_fallback(
        &self,
        driver: &mut WebDriver,
        timeout: u64,
        scrappey_config: Option<&ScrappeyConfig>,
        proxy_config: Option<&ProxyConfig>,
        url: &str,
        browser_cookies: &mut Vec<Cookie>,
        browser_user_agent: &mut String,
    ) -> Result<Option<Response>> {
        let cloudflare_timeout = timeout / CLOUDFLARE_TIMEOUT_FRACTION;
        let scrappey_timeout = timeout - cloudflare_timeout;

        // Try browser-based challenge handling first
        let start_time = std::time::Instant::now();
        while self.is_protected(driver).await {
            if start_time.elapsed().as_secs() > cloudflare_timeout {
                // Browser handling timed out, try Scrappey fallback if configured
                if let (Some(scrappey), Some(proxy)) = (scrappey_config, proxy_config) {
                    warn!("Cloudflare challenge timed out, falling back to Scrappey");
                    return Self::fallback_to_scrappey(
                        scrappey,
                        proxy,
                        url,
                        browser_cookies,
                        browser_user_agent,
                        scrappey_timeout,
                    )
                    .await
                    .map(Some);
                } else {
                    return Err(anyhow::anyhow!(
                        "Cloudflare challenge timed out after {} seconds",
                        cloudflare_timeout
                    ));
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }

        info!("Cloudflare challenge handled successfully");
        Ok(None)
    }

    /// Use Scrappey API as a fallback to solve Cloudflare challenges.
    async fn fallback_to_scrappey(
        scrappey_config: &ScrappeyConfig,
        proxy_config: &ProxyConfig,
        url: &str,
        browser_cookies: &mut Vec<Cookie>,
        browser_user_agent: &mut String,
        timeout: u64,
    ) -> Result<Response> {
        if !scrappey_config.is_configured() {
            return Err(anyhow::anyhow!("Scrappey API key not configured"));
        }

        let ctx = LogContext::new().with_url(url);
        let proxy = proxy_config.to_url();

        ctx.info(&format!(
            "Attempting to resolve challenge with Scrappey API (timeout: {}s, estimated: 20-40s)",
            timeout
        ));

        let scrappey_timing = TimingLogger::new("scrappey_resolve").with_url(url);
        let client = ScrappeyClient::new(scrappey_config.api_key.clone());
        let request = ScrappeyGetRequest {
            url: url.to_string(),
            proxy: Some(proxy),
            ..Default::default()
        };
        let response = client.get(request, timeout).await?;
        let duration = scrappey_timing.finish_silent();

        ctx.info(&format!(
            "Scrappey resolved challenge successfully in {:.2}s",
            duration.as_secs_f64()
        ));
        debug!(
            "Scrappey response: status={:?}, cookies={:?}",
            response.solution.status_code,
            response.solution.cookies.as_ref().map(|c| c.len())
        );

        // Merge Scrappey cookies into the session, upserting on
        // (name, domain, path) so a cookie returned by Scrappey replaces
        // any existing one with the same identity tuple instead of
        // adding a duplicate that Chrome would pick from at random.
        if let Some(cookies) = response.solution.cookies {
            for incoming in cookies {
                let incoming = Cookie::from(incoming);
                if let Some(existing) = browser_cookies.iter_mut().find(|c| {
                    c.name == incoming.name
                        && c.domain == incoming.domain
                        && c.path == incoming.path
                }) {
                    *existing = incoming;
                } else {
                    browser_cookies.push(incoming);
                }
            }
        }
        if let Some(ua) = response.solution.user_agent {
            *browser_user_agent = ua;
        }

        Ok(Response {
            url: response
                .solution
                .current_url
                .unwrap_or_else(|| url.to_string()),
            status: response.solution.status_code.unwrap_or(200),
            body: response.solution.response.unwrap_or_default(),
            cookies: browser_cookies.clone(),
            user_agent: browser_user_agent.clone(),
        })
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
