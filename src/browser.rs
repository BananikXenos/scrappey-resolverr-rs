use anyhow::Result;
use serde::{Deserialize, Serialize};
use thirtyfour::{extensions::cdp::ChromeDevTools, prelude::*};

use crate::challenge::{self, ddos_guard};

// Configuration extracted to eliminate hard-coded values
#[derive(Debug, Clone)]
pub struct BrowserConfig {
    pub webdriver_url: String,
    pub window_size: (u32, u32),
    pub challenge_timeout: u64,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            webdriver_url: "http://localhost:9515".to_string(),
            window_size: (1920, 1080),
            challenge_timeout: 30,
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
    pub async fn navigate(&mut self, url: &str) -> Result<Response> {
        let mut driver = self.setup_driver().await?;

        self.configure_cookies(&driver).await?;

        driver.get(url).await?;

        self.handle_challenges(&mut driver, url).await?;

        let response = self.extract_response(&driver, url).await?;

        driver.quit().await?;

        Ok(response)
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
            if let Some(expiry) = cookie.expiry {
                if expiry <= now {
                    println!("Removing expired cookie: {:?}", cookie);
                    return false;
                }
            }
            true
        });
    }

    async fn handle_challenges(
        &mut self,
        driver: &mut WebDriver,
        url: &str,
    ) -> Result<Option<Response>> {
        // Handle DDoS Guard challenge
        if ddos_guard::is_protected(driver).await {
            println!("DDoS Guard challenge detected, handling...");
            ddos_guard::handle_challenge(driver, self.config.challenge_timeout).await?;
        }

        // Handle Cloudflare challenge
        if challenge::cloudflare::is_protected(driver).await {
            println!("Cloudflare challenge detected, handling...");
            if let Some(response) = self.handle_cloudflare_challenge(driver, url).await? {
                return Ok(Some(response));
            }
        }

        Ok(None)
    }

    async fn handle_cloudflare_challenge(
        &mut self,
        driver: &mut WebDriver,
        url: &str,
    ) -> Result<Option<Response>> {
        match challenge::cloudflare::handle_challenge(driver, self.config.challenge_timeout).await {
            Ok(_) => {
                println!("Cloudflare challenge handled successfully.");
                Ok(None)
            }
            Err(e) => {
                println!("Failed to handle Cloudflare challenge: {}", e);
                self.fallback_to_scrappey(url).await
            }
        }
    }

    async fn fallback_to_scrappey(&mut self, url: &str) -> Result<Option<Response>> {
        let scrappey_api_key = std::env::var("SCRAPPEY_API_KEY")
            .map_err(|_| anyhow::anyhow!("SCRAPPEY_API_KEY environment variable not set"))?;
        let proxy = std::env::var("SCRAPPEY_PROXY").unwrap_or_default();

        println!("Attempting to resolve challenge with Scrappey...");

        let response =
            challenge::cloudflare::scrappey_resolve(url.to_string(), scrappey_api_key, &proxy)
                .await?;

        println!("Scrappey resolved the challenge successfully.");

        // Update cookies from Scrappey response
        if let Some(cookies) = response.solution.cookies {
            for cookie in cookies {
                self.data.cookies.push(cookie.into());
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

        self.data.cookies.extend(new_cookies);

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
