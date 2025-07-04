/// DDoS-Guard challenge detection and handling logic.
pub mod ddos_guard {
    use anyhow::Result;

    /// Returns true if the current page is protected by DDoS-Guard.
    pub async fn is_protected(driver: &mut thirtyfour::WebDriver) -> bool {
        driver
            .title()
            .await
            .is_ok_and(|title| title.contains("DDoS-Guard"))
    }

    /// Waits for the DDoS-Guard challenge to be solved, or times out.
    pub async fn handle_challenge(driver: &mut thirtyfour::WebDriver, timeout: u64) -> Result<()> {
        let start_time = std::time::Instant::now();
        while is_protected(driver).await {
            if start_time.elapsed().as_secs() > timeout {
                return Err(anyhow::anyhow!("DDoS Guard challenge timed out"));
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }

        Ok(())
    }
}

/// Cloudflare challenge detection, handling, and fallback logic.
pub mod cloudflare {

    use anyhow::Result;
    use thirtyfour::prelude::*;

    use crate::scrappey::{ScrappeyClient, ScrappeyGetRequest, ScrappeyResponse};

    /// Returns true if the current page is protected by a Cloudflare challenge.
    pub async fn is_protected(driver: &mut WebDriver) -> bool {
        driver
            .title()
            .await
            .is_ok_and(|title| title.contains("Just a moment..."))
    }

    /// Waits for the Cloudflare challenge to be solved, or times out.
    pub async fn handle_challenge(driver: &mut WebDriver, timeout: u64) -> Result<()> {
        let start_time = std::time::Instant::now();
        while is_protected(driver).await {
            if start_time.elapsed().as_secs() > timeout {
                return Err(anyhow::anyhow!("Cloudflare challenge timed out"));
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }

        Ok(())
    }

    /// Fallback: Use Scrappey API to resolve Cloudflare challenge if browser automation fails.
    pub async fn scrappey_resolve(
        url: String,
        api_key: String,
        proxy: &str,
        timeout: u64,
    ) -> Result<ScrappeyResponse> {
        // If we reach here, the challenge was not solved in time, we need to use a third-party service
        let client = ScrappeyClient::new(api_key);
        let request = ScrappeyGetRequest {
            url,
            proxy: Some(proxy.to_string()),
            ..Default::default()
        };
        client.get(request, timeout).await
    }
}
