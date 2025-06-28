use anyhow::Result;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use thirtyfour::{Proxy, extensions::cdp::ChromeDevTools, prelude::*};

use crate::challenge::{self, ddos_guard};

// Configuration extracted to eliminate hard-coded values
#[derive(Debug, Clone)]
pub struct BrowserConfig {
    pub webdriver_url: String,
    pub window_size: (u32, u32),
    pub proxy_host: String,
    pub proxy_port: u16,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
    pub scrappey_api_key: String,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            webdriver_url: "http://localhost:9515".to_string(),
            window_size: (1920, 1080),
            proxy_host: "127.0.0.1".to_string(),
            proxy_port: 1080,
            proxy_username: None,
            proxy_password: None,
            scrappey_api_key: String::new(),
        }
    }
}

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

pub struct Response {
    pub url: String,
    pub status: u16,
    pub body: String,
    pub cookies: Vec<Cookie>,
    pub user_agent: String,
}

pub struct Browser {
    pub data: BrowserData,
    pub config: BrowserConfig,
}

impl Browser {
    pub fn new() -> Self {
        Browser {
            data: BrowserData::default(),
            config: BrowserConfig::default(),
        }
    }

    pub fn with_config(mut self, config: BrowserConfig) -> Self {
        self.config = config;
        self
    }

    pub fn load_data(&mut self, path: &str) -> Result<()> {
        let file = std::fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);
        self.data = serde_json::from_reader(reader)?;
        Ok(())
    }

    pub fn save_data(&self, path: &str) -> Result<()> {
        let file = std::fs::File::create(path)?;
        serde_json::to_writer_pretty(file, &self.data)?;
        Ok(())
    }

    // Main navigation method - now much cleaner and focused
    pub async fn get(&mut self, url: &str, timeout: u64) -> Result<Response> {
        let mut driver = self.setup_driver().await?;

        // Use a closure to ensure driver.quit() is always called
        let result = async {
            self.configure_cookies(&driver).await?;
            driver.get(url).await?;

            if let Some(response) = self.handle_challenges(&mut driver, url, timeout).await? {
                return Ok(response);
            }

            let response = self.extract_response(&driver, url).await?;
            Ok(response)
        }
        .await;

        // Always attempt to quit the driver, even if result is Err
        let quit_result = driver.quit().await;

        // If the main logic errored, return that error
        // Otherwise, if quitting the driver errored, return that error
        // Otherwise, return the successful response
        match (result, quit_result) {
            (Ok(response), Ok(_)) => Ok(response),
            (Err(e), _) => Err(e),
            (_, Err(e)) => Err(e.into()),
        }
    }

    // Broken out methods for specific responsibilities
    async fn setup_driver(&self) -> Result<WebDriver> {
        let mut caps = DesiredCapabilities::chrome();
        caps.set_no_sandbox()?;
        caps.set_disable_dev_shm_usage()?;
        caps.add_arg("--disable-blink-features=AutomationControlled")?;
        caps.add_arg(&format!(
            "--window-size={},{}",
            self.config.window_size.0, self.config.window_size.1
        ))?;
        caps.add_arg(&format!("--user-agent={}", self.data.user_agent))?;
        caps.add_arg("--disable-infobars")?;
        caps.insert_browser_option("excludeSwitches", ["enable-automation"])?;

        // Configure proxy to our local noauth http to auth http bridge
        caps.set_proxy(Proxy::Manual {
            ftp_proxy: None,
            http_proxy: Some("127.0.0.1:8080".to_string()),
            ssl_proxy: None,
            socks_proxy: None,
            socks_version: None,
            socks_username: None, // unsupported in chromedriver
            socks_password: None, // unsupported in chromedriver
            no_proxy: None,
        })?;

        let driver = WebDriver::new(&self.config.webdriver_url, caps).await?;
        Ok(driver)
    }

    async fn configure_cookies(&mut self, driver: &WebDriver) -> Result<()> {
        // Clean expired cookies first
        self.clean_expired_cookies();

        // Set cookies using Chrome DevTools
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

    fn clean_expired_cookies(&mut self) {
        let now = chrono::Utc::now().timestamp();
        self.data.cookies.retain(|cookie| {
            if let Some(expiry) = cookie.expiry
                && expiry <= now
            {
                debug!("Removing expired cookie: {cookie:?}");
                return false;
            }
            true
        });
    }

    async fn handle_challenges(
        &mut self,
        driver: &mut WebDriver,
        url: &str,
        timeout: u64,
    ) -> Result<Option<Response>> {
        // Handle DDoS Guard challenge
        if ddos_guard::is_protected(driver).await {
            info!("DDoS Guard challenge detected, handling...");
            ddos_guard::handle_challenge(driver, timeout).await?;
        }

        // Handle Cloudflare challenge
        if challenge::cloudflare::is_protected(driver).await {
            info!("Cloudflare challenge detected, handling...");
            if let Some(response) = self
                .handle_cloudflare_challenge(driver, url, timeout)
                .await?
            {
                return Ok(Some(response));
            }
        }

        Ok(None)
    }

    async fn handle_cloudflare_challenge(
        &mut self,
        driver: &mut WebDriver,
        url: &str,
        timeout: u64,
    ) -> Result<Option<Response>> {
        match challenge::cloudflare::handle_challenge(driver, timeout).await {
            Ok(_) => {
                info!("Cloudflare challenge handled successfully.");
                Ok(None)
            }
            Err(e) => {
                warn!("Failed to handle Cloudflare challenge: {e}");
                // we can close the driver here, but we will try to resolve with Scrappey
                driver.clone().quit().await?;
                self.fallback_to_scrappey(url, timeout).await
            }
        }
    }

    async fn fallback_to_scrappey(&mut self, url: &str, timeout: u64) -> Result<Option<Response>> {
        if self.config.scrappey_api_key.is_empty() {
            return Err(anyhow::anyhow!("Scrappey API key not configured"));
        }

        // create a proxy string for Scrappey, if username and password are provided also include them
        let proxy = if let (Some(username), Some(password)) =
            (&self.config.proxy_username, &self.config.proxy_password)
        {
            format!(
                "http://{}:{}@{}:{}",
                username, password, self.config.proxy_host, self.config.proxy_port
            )
        } else {
            format!(
                "http://{}:{}",
                self.config.proxy_host, self.config.proxy_port
            )
        };

        info!("Attempting to resolve challenge with Scrappey...");

        let response = challenge::cloudflare::scrappey_resolve(
            url.to_string(),
            self.config.scrappey_api_key.clone(),
            &proxy,
            timeout,
        )
        .await?;

        info!("Scrappey resolved the challenge successfully.");

        // Print debug information
        debug!("Scrappey response: {response:?}");

        // Update cookies from Scrappey response
        for cookie in response.solution.cookies.unwrap() {
            self.data.cookies.push(cookie.into());
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

    async fn extract_response(&mut self, driver: &WebDriver, url: &str) -> Result<Response> {
        let dev_tools = ChromeDevTools::new(driver.handle.clone());

        // Extract cookies using Chrome DevTools
        let new_cookies = dev_tools
            .execute_cdp("Storage.getCookies")
            .await?
            .get("cookies")
            .and_then(|c| c.as_array())
            .map_or(Vec::new(), |arr| {
                arr.iter()
                    .filter_map(|c| serde_json::from_value(c.clone()).ok())
                    .collect::<Vec<Cookie>>()
            });

        self.data.cookies = new_cookies;

        let body = driver.source().await?;
        let cookies = driver.get_all_cookies().await?;

        Ok(Response {
            url: url.to_string(),
            status: 200, // thirtyfour doesn't provide status, assuming success
            body,
            cookies,
            user_agent: self.data.user_agent.clone(),
        })
    }
}
