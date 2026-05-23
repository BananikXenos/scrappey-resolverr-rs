use anyhow::Result;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use thirtyfour::{Proxy, prelude::*};

use crate::logging::{LogContext, TimingLogger, log_error_with_context};

use crate::challenge::{
    ChallengeHandler, cloudflare::CloudflareHandler, ddos_guard::DdosGuardHandler,
};
use crate::config::BrowserConfig;

/// Default local proxy bridge address for browser connections
const LOCAL_PROXY_ADDR: &str = "127.0.0.1:8080";

/// Default HTTP status code when the real status cannot be determined.
/// Used as a fallback if the PerformanceNavigationTiming entry is missing.
const DEFAULT_HTTP_STATUS: u16 = 200;

/// JS snippet that returns the HTTP status of the current document navigation
/// via the Performance API (PerformanceNavigationTiming.responseStatus, Chrome 102+).
/// Returns null when unavailable (e.g. cross-origin same-document or older Chrome).
const NAVIGATION_STATUS_JS: &str = r#"
    var nav = performance.getEntriesByType('navigation')[0];
    return nav && typeof nav.responseStatus === 'number' ? nav.responseStatus : null;
"#;

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
///
/// Holds a pooled `WebDriver` that lives across requests on the same session.
/// Lazy-initialized on first navigation and quit on `shutdown()`.
pub struct Browser {
    pub data: BrowserData,
    pub config: BrowserConfig,
    driver: Option<WebDriver>,
}

impl Browser {
    /// Create a new browser instance with default config and data.
    pub fn new() -> Self {
        Browser {
            data: BrowserData::default(),
            config: BrowserConfig::default(),
            driver: None,
        }
    }

    /// Set a custom configuration for the browser.
    pub fn with_config(mut self, config: BrowserConfig) -> Self {
        self.config = config;
        self
    }

    /// Quit the pooled WebDriver if one is alive. Called on session destroy
    /// or expiry. Safe to call when no driver exists.
    pub async fn shutdown(&mut self) {
        if let Some(driver) = self.driver.take()
            && let Err(e) = driver.quit().await
        {
            warn!("WebDriver quit failed during shutdown: {}", e);
        }
    }

    /// Load browser session data (user agent, cookies) from a JSON file.
    pub fn load_data(&mut self, path: impl AsRef<std::path::Path>) -> Result<()> {
        let file = std::fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);
        self.data = serde_json::from_reader(reader)?;
        Ok(())
    }

    /// Save browser session data (user agent, cookies) to a JSON file.
    pub fn save_data(&self, path: impl AsRef<std::path::Path>) -> Result<()> {
        let file = std::fs::File::create(path)?;
        serde_json::to_writer_pretty(file, &self.data)?;
        Ok(())
    }

    /// Main navigation method: reuses the pooled WebDriver across requests on
    /// the same session, lazy-creating it on first call. The driver is kept
    /// alive between requests so cookie/UA state and any solved challenge
    /// continue to apply without paying the Chrome cold-start cost.
    pub async fn get(&mut self, url: &str, timeout: u64) -> Result<Response> {
        let ctx = LogContext::new().with_url(url);
        let timing = TimingLogger::new("browser.get").with_url(url);

        ctx.debug(&format!("Starting navigation (timeout: {}s)", timeout));

        // Ensure a live driver. If creation fails we don't cache anything.
        if self.driver.is_none() {
            match self.setup_driver().await {
                Ok(d) => {
                    ctx.debug("WebDriver instance created");
                    self.driver = Some(d);
                }
                Err(e) => {
                    ctx.error(&format!("Failed to create WebDriver: {}", e));
                    return Err(e);
                }
            }
        }

        // Drive the navigation. On a fatal error we tear down the cached
        // driver so the next request can recreate it.
        let result = self.navigate_once(url, timeout, &ctx).await;

        if let Err(ref e) = result {
            // Screenshot before any driver teardown so we still have a window.
            if let (true, Some(driver)) = (
                self.config.screenshots.capture_failure_screenshots,
                self.driver.as_ref(),
            ) {
                match self.capture_failure_screenshot(driver, url).await {
                    Ok(()) => ctx.debug("Failure screenshot captured"),
                    Err(screenshot_err) => ctx.warn(&format!(
                        "Failed to capture failure screenshot: {}",
                        screenshot_err
                    )),
                }
            }

            if is_driver_lost(e) {
                ctx.warn("Driver appears lost; tearing down for next request");
                self.shutdown().await;
            }
        }

        let duration = timing.finish_silent();
        match result {
            Ok(response) => {
                ctx.info(&format!(
                    "Navigation successful (status: {}, {} bytes, {:.2}s)",
                    response.status,
                    response.body.len(),
                    duration.as_secs_f64()
                ));
                Ok(response)
            }
            Err(e) => {
                log_error_with_context(&ctx, &e);
                Err(e)
            }
        }
    }

    /// Drive a single navigation against the pooled WebDriver.
    /// Pulled out so `get` can wrap it with screenshot + recovery handling
    /// without juggling the borrow on `self.driver`.
    async fn navigate_once(
        &mut self,
        url: &str,
        timeout: u64,
        ctx: &LogContext,
    ) -> Result<Response> {
        // Re-apply UA in case it changed (e.g. Scrappey fallback last request)
        // and re-set cookies. Both are idempotent.
        self.apply_user_agent().await?;

        ctx.debug(&format!(
            "Configuring cookies ({} cookies)",
            self.data.cookies.len()
        ));
        self.configure_cookies().await?;

        ctx.debug("Navigating to URL");
        let driver = self
            .driver
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("WebDriver missing during navigation"))?;
        driver.get(url).await?;

        // SAFETY of split borrow: handle_challenges needs &mut WebDriver while
        // also touching self.data; take the driver out, work on it, put it back.
        let mut driver = self
            .driver
            .take()
            .ok_or_else(|| anyhow::anyhow!("WebDriver vanished mid-request"))?;
        let challenge_outcome = self.handle_challenges(&mut driver, url, timeout).await;
        // Put the driver back even on error so we keep the session alive
        // (or so `shutdown` later finds it and quits cleanly).
        self.driver = Some(driver);

        if let Some(response) = challenge_outcome? {
            ctx.info("Challenge resolved via fallback");
            return Ok(response);
        }

        ctx.debug("Extracting response");
        let driver = self
            .driver
            .take()
            .ok_or_else(|| anyhow::anyhow!("WebDriver missing after challenges"))?;
        let response = self.extract_response(&driver, url).await;
        self.driver = Some(driver);
        response
    }

    /// Apply the current session UA to the live driver via CDP override.
    /// Lets us rotate UA (e.g. after a Scrappey fallback) without restarting
    /// Chrome.
    async fn apply_user_agent(&self) -> Result<()> {
        let Some(driver) = self.driver.as_ref() else {
            return Ok(());
        };
        driver
            .cdp()
            .emulation()
            .set_user_agent_override(&self.data.user_agent)
            .await?;
        Ok(())
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
        caps.set_browser_option("excludeSwitches", ["enable-automation"])?;

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
    async fn configure_cookies(&mut self) -> Result<()> {
        self.clean_expired_cookies();

        let Some(driver) = self.driver.as_ref() else {
            return Ok(());
        };

        let cdp = driver.cdp();
        cdp.network().enable().await?;

        for cookie in &self.data.cookies {
            let cookie_value = serde_json::to_value(cookie)
                .map_err(|e| anyhow::anyhow!("Failed to serialize cookie: {}", e))?;
            cdp.send_raw("Network.setCookie", cookie_value).await?;
        }

        Ok(())
    }

    /// Remove expired cookies from the session data.
    fn clean_expired_cookies(&mut self) {
        let now = chrono::Utc::now().timestamp();
        self.data.cookies.retain(|cookie| {
            let expired = cookie.expiry.is_some_and(|expiry| expiry <= now);
            if expired {
                debug!("Removing expired cookie: {cookie:?}");
            }
            !expired
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
        let cloudflare_handler = CloudflareHandler::new();
        if cloudflare_handler.is_protected(driver).await {
            let challenge_timing = TimingLogger::new("cloudflare_challenge").with_url(url);
            ctx.info("Cloudflare challenge detected, handling...");

            let result = cloudflare_handler
                .handle_with_fallback(
                    driver,
                    timeout,
                    Some(&self.config.scrappey),
                    Some(&self.config.proxy),
                    url,
                    &mut self.data.cookies,
                    &mut self.data.user_agent,
                )
                .await?;
            challenge_timing.finish();

            if result.is_some() {
                ctx.info("Cloudflare challenge resolved via fallback");
                return Ok(result);
            } else {
                ctx.info("Cloudflare challenge resolved");
            }
        }

        Ok(None)
    }

    /// Extract the final response from the browser, including cookies and page source.
    async fn extract_response(&mut self, driver: &WebDriver, url: &str) -> Result<Response> {
        // Extract cookies using WebDriver API (more reliable than DevTools)
        let cookies = driver.get_all_cookies().await?;
        self.data.cookies = cookies.clone();

        let body = driver.source().await?;
        let status = read_navigation_status(driver)
            .await
            .unwrap_or(DEFAULT_HTTP_STATUS);

        Ok(Response {
            url: url.to_string(),
            status,
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
        screenshot_files.sort_by_key(|entry| std::cmp::Reverse(entry.1));

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

/// Heuristic: did this error indicate the cached WebDriver session is dead
/// (chromedriver crashed, session id invalidated, connection refused)?
/// On true we drop the cached driver and let the next request recreate it.
/// On false we keep the driver (challenge timeouts and similar are not
/// reasons to throw away a working browser).
fn is_driver_lost(err: &anyhow::Error) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("invalid session id")
        || msg.contains("session not created")
        || msg.contains("connection refused")
        || msg.contains("session deleted")
        || msg.contains("disconnected: not connected to devtools")
        || msg.contains("chrome not reachable")
}

/// Read the HTTP status of the current navigation via the Performance API.
/// Returns None when the navigation entry is missing or doesn't expose
/// `responseStatus` (e.g. cross-origin redirects, pre-Chrome-102 fallback).
async fn read_navigation_status(driver: &WebDriver) -> Option<u16> {
    let value = driver
        .execute(NAVIGATION_STATUS_JS, vec![])
        .await
        .ok()?
        .json()
        .clone();
    value.as_u64().and_then(|n| u16::try_from(n).ok())
}
