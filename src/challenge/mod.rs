//! Challenge detection and handling module.
//! Provides a common trait for different challenge types and implementations for various anti-bot systems.

use anyhow::Result;
use thirtyfour::prelude::*;

pub mod cloudflare;
pub mod ddos_guard;

/// Polling interval for checking challenge status (seconds)
const POLL_INTERVAL_SECS: u64 = 1;

/// Common trait for challenge detection and handling.
/// All challenge types should implement this trait to provide a consistent interface.
#[async_trait::async_trait]
pub trait ChallengeHandler {
    /// Returns the name of the challenge type (e.g., "DDoS Guard", "Cloudflare").
    fn name(&self) -> &'static str;

    /// Checks if the current page is protected by this challenge type.
    /// Returns `true` if the challenge is detected, `false` otherwise.
    async fn is_protected(&self, driver: &mut WebDriver) -> bool;

    /// Attempts to handle the challenge.
    /// Returns `Ok(None)` if the challenge was successfully handled in the browser,
    /// `Ok(Some(response))` if a fallback method was used and returned a response,
    /// or an error if handling failed.
    ///
    /// # Arguments
    /// * `driver` - The WebDriver instance to interact with
    /// * `timeout` - Maximum time to wait for the challenge to be solved (in seconds)
    async fn handle_challenge(
        &self,
        driver: &mut WebDriver,
        timeout: u64,
    ) -> Result<Option<crate::browser::Response>> {
        // Default implementation: just poll until challenge is resolved
        let start_time = std::time::Instant::now();
        while self.is_protected(driver).await {
            if start_time.elapsed().as_secs() > timeout {
                return Err(anyhow::anyhow!(
                    "{} challenge timed out after {} seconds",
                    self.name(),
                    timeout
                ));
            }
            tokio::time::sleep(std::time::Duration::from_secs(POLL_INTERVAL_SECS)).await;
        }
        Ok(None)
    }
}

/// Helper function to check if a page title contains a specific string.
/// Used by challenge implementations to detect protection.
pub(crate) async fn title_contains(driver: &mut WebDriver, text: &str) -> bool {
    driver.title().await.is_ok_and(|title| title.contains(text))
}
