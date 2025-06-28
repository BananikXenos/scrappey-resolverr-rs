use anyhow::Result;
use serde::{Deserialize, Serialize};
use thirtyfour::{extensions::cdp::ChromeDevTools, prelude::*};

use crate::challenge::{self, ddos_guard};

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
}

impl Browser {
    pub fn new() -> Self {
        Browser {
            data: BrowserData::default(),
        }
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

    pub async fn navigate(&mut self, url: &str) -> Result<Response> {
        let mut caps = DesiredCapabilities::chrome();
        caps.set_no_sandbox()?;
        caps.set_disable_dev_shm_usage()?;
        caps.add_arg("--disable-blink-features=AutomationControlled")?;
        caps.add_arg("window-size=1920,1080")?;
        caps.add_arg(format!("user-agent={}", self.data.user_agent).as_str())?;
        caps.add_arg("disable-infobars")?;
        caps.insert_browser_option("excludeSwitches", ["enable-automation"])?;

        let mut driver = WebDriver::new("http://localhost:9515", caps).await?;

        // Handle expired cookies
        self.data.cookies.retain(|cookie| {
            if let Some(expiry) = cookie.expiry {
                if expiry <= chrono::Utc::now().timestamp() {
                    println!("Removing expired cookie: {:?}", cookie);
                    return false;
                }
            }
            true
        });

        // A weird workaround to set cookies before requesting the URL
        let dev_tools = ChromeDevTools::new(driver.handle.clone());
        dev_tools.execute_cdp("Network.enable").await?;
        for cookie in &self.data.cookies {
            dev_tools
                .execute_cdp_with_params("Network.setCookie", serde_json::to_value(cookie).unwrap())
                .await?;
        }

        driver.get(url).await?;

        if ddos_guard::is_protected(&mut driver).await {
            println!("DDoS Guard challenge detected, handling...");
            ddos_guard::handle_challenge(&mut driver, 30).await?;
        }

        if challenge::cloudflare::is_protected(&mut driver).await {
            println!("Cloudflare challenge detected, handling...");
            match challenge::cloudflare::handle_challenge(&mut driver, 30).await {
                Ok(_) => println!("Cloudflare challenge handled successfully."),
                Err(e) => {
                    println!("Failed to handle Cloudflare challenge: {}", e);
                    let scrappey_api_key = std::env::var("SCRAPPEY_API_KEY")
                        .expect("SCRAPPEY_API_KEY environment variable not set");
                    let proxy = std::env::var("SCRAPPEY_PROXY").unwrap_or_default();
                    let response = challenge::cloudflare::scrappey_resolve(
                        &mut driver,
                        scrappey_api_key,
                        &proxy,
                    );

                    println!("Attempting to resolve challenge with Scrappey...");
                    match response.await {
                        Ok(scrappey_response) => {
                            println!("Scrappey resolved the challenge successfully.");
                            // Update cookies from Scrappey response
                            for cookie in scrappey_response.solution.cookies.unwrap() {
                                self.data.cookies.push(cookie.into());
                            }
                            // Update user agent from Scrappey response
                            self.data.user_agent =
                                scrappey_response.solution.user_agent.unwrap_or_default();

                            // return the Scrappey response
                            return Ok(Response {
                                url: scrappey_response
                                    .solution
                                    .current_url
                                    .unwrap_or(url.to_string()),
                                status: scrappey_response.solution.status_code.unwrap_or(200),
                                body: scrappey_response.solution.response.unwrap_or_default(),
                                cookies: self.data.cookies.clone(),
                                user_agent: self.data.user_agent.clone(),
                            });
                        }
                        Err(e) => {
                            println!("Failed to resolve challenge with Scrappey: {}", e);
                        }
                    }
                }
            }
        }

        // Use Network.getAllCookies (deprecated in favor of Storage.getCookies)
        let cookies = dev_tools
            .execute_cdp("Storage.getCookies")
            .await?
            .get("cookies")
            .and_then(|c| c.as_array())
            .map_or(Vec::new(), |arr| {
                arr.iter()
                    .filter_map(|c| serde_json::from_value(c.clone()).ok())
                    .collect::<Vec<Cookie>>()
            });
        self.data.cookies.extend(cookies.clone());

        let status = 200; // not provided by thirtyfour, so we assume success
        let cookies = driver.get_all_cookies().await?;
        let body = driver.source().await?;
        let user_agent = self.data.user_agent.clone();

        driver.quit().await?;

        Ok(Response {
            url: url.to_string(),
            status,
            body,
            cookies,
            user_agent,
        })
    }
}
