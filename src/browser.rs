use anyhow::Result;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use thirtyfour::{Proxy, extensions::cdp::ChromeDevTools, prelude::*};

use crate::logging::{LogContext, TimingLogger, log_error_with_context};

use crate::challenge::{
    ChallengeHandler,
    cloudflare::{self, scrappey_resolve},
    ddos_guard::DdosGuardHandler,
};
use crate::config::BrowserConfig;

/// Default local proxy bridge address for browser connections
const LOCAL_PROXY_ADDR: &str = "127.0.0.1:8080";

/// Fraction of timeout allocated to initial Cloudflare challenge attempt
const CLOUDFLARE_TIMEOUT_FRACTION: u64 = 3;

/// Default HTTP status code when status cannot be determined
const DEFAULT_HTTP_STATUS: u16 = 200;

/// Stores browser session data such as user agent and cookies.
/// This struct is serializable for persistence between runs.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BrowserData {
    pub user_agent: String,
    pub cookies: Vec<Cookie>,
}

impl Default for BrowserData {
    fn default() -> Self {
        BrowserData {
            user_agent: ua_generator::ua::spoof_ua().to_string(),
            cookies: Vec::new(),
        }
    }
}

/// Represents the result of a browser navigation, including page content and cookies.
pub struct Response {
    pub url: String,
    pub status: u16,
    pub body: String,
    pub cookies: Vec<Cookie>,
    pub user_agent: String,
}

/// Main browser automation struct, encapsulating session data and configuration.
#[derive(Clone)]
pub struct Browser {
    pub data: BrowserData,
    pub config: BrowserConfig,
}

impl Browser {
    /// Create a new browser instance with default config and data.
    pub fn new() -> Self {
        Browser {
            data: BrowserData::default(),
            config: BrowserConfig::default(),
        }
    }

    /// Set a custom configuration for the browser.
    pub fn with_config(mut self, config: BrowserConfig) -> Self {
        self.config = config;
        self
    }

    /// Load browser session data (user agent, cookies) from a JSON file.
    pub fn load_data(&mut self, path: &str) -> Result<()> {
        let file = std::fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);
        self.data = serde_json::from_reader(reader)?;
        Ok(())
    }

    /// Save browser session data (user agent, cookies) to a JSON file.
    pub fn save_data(&self, path: &str) -> Result<()> {
        let file = std::fs::File::create(path)?;
        serde_json::to_writer_pretty(file, &self.data)?;
        Ok(())
    }

    /// Main navigation method: launches a browser, navigates to the URL, handles challenges, and extracts the response.
    /// Ensures the driver is always quit, even on error.
    pub async fn get(&mut self, url: &str, timeout: u64) -> Result<Response> {
        let ctx = LogContext::new().with_url(url);
        let timing = TimingLogger::new("browser.get").with_url(url);

        ctx.debug(&format!("Starting navigation (timeout: {}s)", timeout));

        let mut driver = match self.setup_driver().await {
            Ok(d) => {
                ctx.debug("WebDriver instance created");
                d
            }
            Err(e) => {
                ctx.error(&format!("Failed to create WebDriver: {}", e));
                return Err(e);
            }
        };

        // Use a closure to ensure driver.quit() is always called
        let result = async {
            ctx.debug(&format!(
                "Configuring cookies ({} cookies)",
                self.data.cookies.len()
            ));
            self.configure_cookies(&driver).await?;

            ctx.debug("Navigating to URL");
            driver.get(url).await?;

            // Handle anti-bot challenges if present
            if let Some(response) = self.handle_challenges(&mut driver, url, timeout).await? {
                ctx.info("Challenge resolved via fallback");
                return Ok(response);
            }

            ctx.debug("Extracting response");
            let response = self.extract_response(&driver, url).await?;
            Ok(response)
        }
        .await;

        // Take screenshot on failure if enabled
        if result.is_err() && self.config.screenshots.capture_failure_screenshots {
            if let Err(screenshot_err) = self.capture_failure_screenshot(&driver, url).await {
                ctx.warn(&format!(
                    "Failed to capture failure screenshot: {}",
                    screenshot_err
                ));
            } else {
                ctx.debug("Failure screenshot captured");
            }
        }

        // Always attempt to quit the driver, even if result is Err
        let quit_result = driver.quit().await;
        if quit_result.is_err() {
            ctx.warn("Failed to quit WebDriver cleanly");
        }

        // Log timing and return result
        let duration = timing.finish_silent();
        match (result, quit_result) {
            (Ok(response), Ok(_)) => {
                ctx.info(&format!(
                    "Navigation successful (status: {}, {} bytes, {:.2}s)",
                    response.status,
                    response.body.len(),
                    duration.as_secs_f64()
                ));
                Ok(response)
            }
            (Err(e), _) => {
                log_error_with_context(&ctx, &e);
                Err(e)
            }
            (_, Err(e)) => {
                ctx.error(&format!("WebDriver quit failed: {}", e));
                Err(e.into())
            }
        }
    }

    /// Set up a new Chrome WebDriver instance with configured capabilities and proxy.
    async fn setup_driver(&self) -> Result<WebDriver> {
        debug!(
            "Setting up WebDriver (window: {}x{}, WebDriver: {})",
            self.config.webdriver.window_size.0,
            self.config.webdriver.window_size.1,
            self.config.webdriver.url
        );

        let mut caps = DesiredCapabilities::chrome();
        caps.set_no_sandbox()?;
        caps.set_disable_dev_shm_usage()?;
        caps.add_arg("--disable-blink-features=AutomationControlled")?;
        caps.add_arg(&format!(
            "--window-size={},{}",
            self.config.webdriver.window_size.0, self.config.webdriver.window_size.1
        ))?;
        caps.add_arg(&format!("--user-agent={}", self.data.user_agent))?;
        caps.add_arg("--disable-infobars")?;
        caps.insert_browser_option("excludeSwitches", ["enable-automation"])?;

        // Always use the local proxy bridge (noauth) for outgoing requests
        debug!("Configuring proxy: {}", LOCAL_PROXY_ADDR);
        caps.set_proxy(Proxy::Manual {
            ftp_proxy: None,
            http_proxy: Some(LOCAL_PROXY_ADDR.to_string()),
            ssl_proxy: None,
            socks_proxy: None,
            socks_version: None,
            socks_username: None, // unsupported in chromedriver
            socks_password: None, // unsupported in chromedriver
            no_proxy: None,
        })?;

        debug!("Connecting to WebDriver");
        let driver = WebDriver::new(&self.config.webdriver.url, caps).await?;
        debug!("WebDriver connected successfully");
        Ok(driver)
    }

    /// Set cookies in the browser using Chrome DevTools Protocol.
    /// Cleans expired cookies before setting.
    async fn configure_cookies(&mut self, driver: &WebDriver) -> Result<()> {
        self.clean_expired_cookies();

        let dev_tools = ChromeDevTools::new(driver.handle.clone());
        dev_tools.execute_cdp("Network.enable").await?;

        for cookie in &self.data.cookies {
            let cookie_value = serde_json::to_value(cookie)
                .map_err(|e| anyhow::anyhow!("Failed to serialize cookie: {}", e))?;
            dev_tools
                .execute_cdp_with_params("Network.setCookie", cookie_value)
                .await?;
        }

        Ok(())
    }

    /// Remove expired cookies from the session data.
    fn clean_expired_cookies(&mut self) {
        let now = chrono::Utc::now().timestamp();
        self.data.cookies.retain(|cookie| {
            if let Some(expiry) = cookie.expiry {
                if expiry <= now {
                    debug!("Removing expired cookie: {cookie:?}");
                    return false;
                }
            }
            true
        });
    }

    /// Detect and handle anti-bot challenges (DDoS Guard, Cloudflare).
    /// Returns a Response if solved by fallback, otherwise None.
    async fn handle_challenges(
        &mut self,
        driver: &mut WebDriver,
        url: &str,
        timeout: u64,
    ) -> Result<Option<Response>> {
        let ctx = LogContext::new().with_url(url);

        // Handle DDoS Guard challenge if detected
        let ddos_guard_handler = DdosGuardHandler::new();
        if ddos_guard_handler.is_protected(driver).await {
            let challenge_timing = TimingLogger::new("ddos_guard_challenge").with_url(url);
            ctx.info("DDoS Guard challenge detected, handling...");
            ddos_guard_handler.handle_challenge(driver, timeout).await?;
            challenge_timing.finish();
            ctx.info("DDoS Guard challenge resolved");
        }

        // Handle Cloudflare challenge if detected
        let cloudflare_handler = cloudflare::CloudflareHandler::new();
        if cloudflare_handler.is_protected(driver).await {
            let challenge_timing = TimingLogger::new("cloudflare_challenge").with_url(url);
            ctx.info("Cloudflare challenge detected, handling...");
            let result = self
                .handle_cloudflare_challenge(driver, url, timeout)
                .await?;
            challenge_timing.finish();
            if result.is_some() {
                ctx.info("Cloudflare challenge resolved via fallback");
            } else {
                ctx.info("Cloudflare challenge resolved");
            }
            if let Some(response) = result {
                return Ok(Some(response));
            }
        }

        Ok(None)
    }

    /// Attempt to solve Cloudflare challenge, falling back to Scrappey if needed.
    async fn handle_cloudflare_challenge(
        &mut self,
        driver: &mut WebDriver,
        url: &str,
        timeout: u64,
    ) -> Result<Option<Response>> {
        let cloudflare_timeout = timeout / CLOUDFLARE_TIMEOUT_FRACTION;
        let scrappey_timeout = timeout - cloudflare_timeout;

        let cloudflare_handler = cloudflare::CloudflareHandler::new();
        match cloudflare_handler
            .handle_challenge(driver, cloudflare_timeout)
            .await
        {
            Ok(_) => {
                info!("Cloudflare challenge handled successfully.");
                Ok(None)
            }
            Err(e) => {
                warn!("Failed to handle Cloudflare challenge: {e}");
                self.fallback_to_scrappey(url, scrappey_timeout).await
            }
        }
    }

    /// Use Scrappey API as a fallback to solve anti-bot challenges.
    /// Updates cookies and user agent from Scrappey response.
    async fn fallback_to_scrappey(&mut self, url: &str, timeout: u64) -> Result<Option<Response>> {
        if !self.config.scrappey.is_configured() {
            return Err(anyhow::anyhow!("Scrappey API key not configured"));
        }

        let ctx = LogContext::new().with_url(url);

        // Build proxy string for Scrappey
        let proxy = self.config.proxy.to_url();

        ctx.info(&format!(
            "Attempting to resolve challenge with Scrappey API (timeout: {}s, estimated: 20-40s)",
            timeout
        ));

        let scrappey_timing = TimingLogger::new("scrappey_resolve").with_url(url);
        let response = scrappey_resolve(
            url.to_string(),
            self.config.scrappey.api_key.clone(),
            &proxy,
            timeout,
        )
        .await?;
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

        // Update cookies from Scrappey response
        if let Some(cookies) = response.solution.cookies {
            for cookie in cookies {
                self.data.cookies.push(Cookie::from(cookie));
            }
        }

        // Update user agent from Scrappey response
        if let Some(ua) = response.solution.user_agent {
            self.data.user_agent = ua;
        }

        Ok(Some(Response {
            url: response
                .solution
                .current_url
                .unwrap_or_else(|| url.to_string()),
            status: response.solution.status_code.unwrap_or(200),
            body: response.solution.response.unwrap_or_default(),
            cookies: self.data.cookies.clone(),
            user_agent: self.data.user_agent.clone(),
        }))
    }

    /// Extract the final response from the browser, including cookies and page source.
    async fn extract_response(&mut self, driver: &WebDriver, url: &str) -> Result<Response> {
        // Extract cookies using WebDriver API (more reliable than DevTools)
        let cookies = driver.get_all_cookies().await?;
        self.data.cookies = cookies.clone();

        let body = driver.source().await?;

        Ok(Response {
            url: url.to_string(),
            status: DEFAULT_HTTP_STATUS, // thirtyfour doesn't provide status, assuming success
            body,
            cookies,
            user_agent: self.data.user_agent.clone(),
        })
    }

    /// Capture a screenshot when challenge resolution fails for debugging purposes.
    async fn capture_failure_screenshot(&self, driver: &WebDriver, url: &str) -> Result<()> {
        // Create screenshot directory if it doesn't exist
        std::fs::create_dir_all(&self.config.screenshots.screenshot_dir)?;

        // Clean up old screenshots first
        self.cleanup_old_screenshots()?;

        // Generate filename with timestamp and domain
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let domain = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown".to_string());
        let filename = format!("failure_{}_{}.png", domain, timestamp);
        let filepath = std::path::Path::new(&self.config.screenshots.screenshot_dir).join(filename);

        // Take screenshot
        let screenshot_data = driver.screenshot_as_png().await?;
        std::fs::write(&filepath, screenshot_data)?;

        info!("Failure screenshot saved to: {}", filepath.display());
        Ok(())
    }

    /// Clean up old failure screenshots, keeping only the N most recent ones.
    fn cleanup_old_screenshots(&self) -> Result<()> {
        let screenshot_dir = std::path::Path::new(&self.config.screenshots.screenshot_dir);

        // If directory doesn't exist, nothing to clean up
        if !screenshot_dir.exists() {
            return Ok(());
        }

        // Get all failure screenshot files
        let mut screenshot_files: Vec<_> = std::fs::read_dir(screenshot_dir)?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();

                // Only consider PNG files that start with "failure_"
                if path.is_file()
                    && path.extension().and_then(|s| s.to_str()) == Some("png")
                    && path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(|name| name.starts_with("failure_"))
                {
                    // Get the modification time for sorting
                    let metadata = entry.metadata().ok()?;
                    let modified = metadata.modified().ok()?;
                    Some((path, modified))
                } else {
                    None
                }
            })
            .collect();

        // Sort by modification time (newest first)
        screenshot_files.sort_by(|a, b| b.1.cmp(&a.1));

        // Remove old screenshots if we exceed the limit
        if screenshot_files.len() > self.config.screenshots.max_failure_screenshots {
            let files_to_remove =
                &screenshot_files[self.config.screenshots.max_failure_screenshots..];

            for (file_path, _) in files_to_remove {
                if let Err(e) = std::fs::remove_file(file_path) {
                    warn!(
                        "Failed to remove old screenshot {}: {}",
                        file_path.display(),
                        e
                    );
                } else {
                    debug!("Removed old screenshot: {}", file_path.display());
                }
            }

            if !files_to_remove.is_empty() {
                info!(
                    "Cleaned up {} old failure screenshots, keeping {} most recent",
                    files_to_remove.len(),
                    self.config.screenshots.max_failure_screenshots
                );
            }
        }

        Ok(())
    }
}
