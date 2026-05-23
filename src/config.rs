use anyhow::{Context, Result};
use log::warn;
use serde::{Deserialize, Serialize};

use crate::scrappey::ScrappeyClient;

/// Read a required env var and return a helpful error if it's not set.
fn require_env(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("Required environment variable {name} not set"))
}

/// Parse a numeric env var, warning (and falling back to default) on bad input.
fn parse_env_with_default<T>(name: &str, default: T) -> T
where
    T: std::str::FromStr + std::fmt::Display + Copy,
    <T as std::str::FromStr>::Err: std::fmt::Display,
{
    match std::env::var(name) {
        Ok(raw) => match raw.parse::<T>() {
            Ok(v) => v,
            Err(e) => {
                warn!("Invalid value for {name} ({raw:?}): {e}; falling back to {default}");
                default
            }
        },
        Err(_) => default,
    }
}

/// Proxy configuration for HTTP/SOCKS proxy settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl ProxyConfig {
    /// Create a new proxy configuration without authentication.
    pub fn new(host: String, port: u16) -> Self {
        Self {
            host,
            port,
            username: None,
            password: None,
        }
    }

    /// Create a new proxy configuration with authentication.
    pub fn with_auth(host: String, port: u16, username: String, password: String) -> Self {
        Self {
            host,
            port,
            username: Some(username),
            password: Some(password),
        }
    }

    /// Get the proxy URL with credentials if available.
    pub fn to_url(&self) -> String {
        if let (Some(username), Some(password)) = (&self.username, &self.password) {
            format!(
                "http://{}:{}@{}:{}",
                username, password, self.host, self.port
            )
        } else {
            format!("http://{}:{}", self.host, self.port)
        }
    }
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 1080,
            username: None,
            password: None,
        }
    }
}

/// Screenshot configuration for debugging and failure capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotConfig {
    pub capture_failure_screenshots: bool,
    pub screenshot_dir: String,
    pub max_failure_screenshots: usize,
}

impl ScreenshotConfig {
    pub fn new(
        capture_failure_screenshots: bool,
        screenshot_dir: String,
        max_failure_screenshots: usize,
    ) -> Self {
        Self {
            capture_failure_screenshots,
            screenshot_dir,
            max_failure_screenshots,
        }
    }
}

impl Default for ScreenshotConfig {
    fn default() -> Self {
        Self {
            capture_failure_screenshots: true,
            screenshot_dir: "/data/screenshots".to_string(),
            max_failure_screenshots: 10,
        }
    }
}

/// WebDriver configuration for browser automation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebDriverConfig {
    pub url: String,
    pub window_size: (u32, u32),
}

impl Default for WebDriverConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:9515".to_string(),
            // 1280x720 is small enough that Cloudflare/JS challenges treat
            // the browser as "normal viewport" and large enough that
            // content lays out as expected. Keep in sync with the previous
            // per-request override in flaresolverr.rs.
            window_size: (1280, 720),
        }
    }
}

/// Browser automation configuration.
/// Combines all the configuration components needed for browser operations.
/// `scrappey` is a pre-built client (not just config) so its reqwest
/// connection pool is shared across all challenge-fallback calls instead of
/// being rebuilt every Cloudflare fallback.
#[derive(Debug, Clone, Default)]
pub struct BrowserConfig {
    pub webdriver: WebDriverConfig,
    pub proxy: ProxyConfig,
    pub scrappey: ScrappeyClient,
    pub screenshots: ScreenshotConfig,
}

/// API server configuration for the FlareSolverr-compatible server.
/// `scrappey` is a fully-constructed client (built once at load time) so
/// both the startup balance check and every Cloudflare fallback share the
/// same reqwest connection pool.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub proxy: ProxyConfig,
    pub scrappey: ScrappeyClient,
    pub screenshots: ScreenshotConfig,
    pub data_path: String,
    pub host: String,
    pub port: u16,
}

impl ServerConfig {
    pub fn new(
        proxy: ProxyConfig,
        scrappey: ScrappeyClient,
        screenshots: ScreenshotConfig,
        data_path: String,
        host: String,
        port: u16,
    ) -> Self {
        Self {
            proxy,
            scrappey,
            screenshots,
            data_path,
            host,
            port,
        }
    }

    pub fn bind_address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Convert this ServerConfig into a BrowserConfig for browser operations.
    pub fn to_browser_config(&self) -> BrowserConfig {
        BrowserConfig {
            webdriver: WebDriverConfig::default(),
            proxy: self.proxy.clone(),
            scrappey: self.scrappey.clone(),
            screenshots: self.screenshots.clone(),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            proxy: ProxyConfig::default(),
            scrappey: ScrappeyClient::default(),
            screenshots: ScreenshotConfig::default(),
            data_path: "/data".to_string(),
            host: "0.0.0.0".to_string(),
            port: 8191,
        }
    }
}

/// Load configuration from environment variables.
pub fn load_from_env() -> Result<ServerConfig> {
    let scrappey_api_key = require_env("SCRAPPEY_API_KEY")?;
    let proxy_host = require_env("PROXY_HOST")?;
    let proxy_port: u16 = require_env("PROXY_PORT")?
        .parse()
        .context("PROXY_PORT must be an integer in 0..=65535")?;
    let proxy_username = std::env::var("PROXY_USERNAME").ok();
    let proxy_password = std::env::var("PROXY_PASSWORD").ok();
    let data_path = std::env::var("DATA_PATH").unwrap_or_else(|_| "/data".to_string());
    let capture_failure_screenshots =
        parse_env_with_default::<bool>("CAPTURE_FAILURE_SCREENSHOTS", true);
    let screenshot_dir =
        std::env::var("SCREENSHOT_DIR").unwrap_or_else(|_| "/data/screenshots".to_string());
    let max_failure_screenshots = parse_env_with_default::<usize>("MAX_FAILURE_SCREENSHOTS", 10);
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = parse_env_with_default::<u16>("PORT", 8191);

    let proxy = match (proxy_username, proxy_password) {
        (Some(username), Some(password)) => {
            ProxyConfig::with_auth(proxy_host, proxy_port, username, password)
        }
        _ => ProxyConfig::new(proxy_host, proxy_port),
    };

    let scrappey = ScrappeyClient::new(scrappey_api_key);
    let screenshots = ScreenshotConfig::new(
        capture_failure_screenshots,
        screenshot_dir,
        max_failure_screenshots,
    );

    Ok(ServerConfig::new(
        proxy,
        scrappey,
        screenshots,
        data_path,
        host,
        port,
    ))
}
