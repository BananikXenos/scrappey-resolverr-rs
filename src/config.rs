use anyhow::Result;
use serde::{Deserialize, Serialize};

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

/// Scrappey API configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScrappeyConfig {
    pub api_key: String,
}

impl ScrappeyConfig {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    pub fn is_configured(&self) -> bool {
        !self.api_key.is_empty()
    }
}

/// Screenshot configuration for debugging and failure capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotConfig {
    pub capture_failure_screenshots: bool,
    pub screenshot_dir: String,
}

#[allow(dead_code)]
impl ScreenshotConfig {
    pub fn new(capture_failure_screenshots: bool, screenshot_dir: String) -> Self {
        Self {
            capture_failure_screenshots,
            screenshot_dir,
        }
    }

    pub fn disabled() -> Self {
        Self {
            capture_failure_screenshots: false,
            screenshot_dir: "/tmp".to_string(),
        }
    }
}

impl Default for ScreenshotConfig {
    fn default() -> Self {
        Self {
            capture_failure_screenshots: true,
            screenshot_dir: "/data/screenshots".to_string(),
        }
    }
}

/// WebDriver configuration for browser automation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebDriverConfig {
    pub url: String,
    pub window_size: (u32, u32),
}

#[allow(dead_code)]
impl WebDriverConfig {
    pub fn new(url: String, window_size: (u32, u32)) -> Self {
        Self { url, window_size }
    }
}

impl Default for WebDriverConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:9515".to_string(),
            window_size: (1920, 1080),
        }
    }
}

/// Browser automation configuration.
/// Combines all the configuration components needed for browser operations.
#[derive(Debug, Clone, Default)]
pub struct BrowserConfig {
    pub webdriver: WebDriverConfig,
    pub proxy: ProxyConfig,
    pub scrappey: ScrappeyConfig,
    pub screenshots: ScreenshotConfig,
}

#[allow(dead_code)]
impl BrowserConfig {
    pub fn new(
        webdriver: WebDriverConfig,
        proxy: ProxyConfig,
        scrappey: ScrappeyConfig,
        screenshots: ScreenshotConfig,
    ) -> Self {
        Self {
            webdriver,
            proxy,
            scrappey,
            screenshots,
        }
    }
}

/// API server configuration for the FlareSolverr-compatible server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub proxy: ProxyConfig,
    pub scrappey: ScrappeyConfig,
    pub screenshots: ScreenshotConfig,
    pub data_path: String,
    pub host: String,
    pub port: u16,
}

impl ServerConfig {
    pub fn new(
        proxy: ProxyConfig,
        scrappey: ScrappeyConfig,
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
            scrappey: ScrappeyConfig::default(),
            screenshots: ScreenshotConfig::default(),
            data_path: "/data/persistent.json".to_string(),
            host: "0.0.0.0".to_string(),
            port: 8191,
        }
    }
}

/// Load configuration from environment variables.
pub fn load_from_env() -> Result<ServerConfig> {
    let scrappey_api_key = std::env::var("SCRAPPEY_API_KEY")?;
    let proxy_host = std::env::var("PROXY_HOST")?;
    let proxy_port = std::env::var("PROXY_PORT")?
        .parse::<u16>()
        .map_err(|_| anyhow::anyhow!("Invalid PROXY_PORT"))?;
    let proxy_username = std::env::var("PROXY_USERNAME").ok();
    let proxy_password = std::env::var("PROXY_PASSWORD").ok();
    let data_path =
        std::env::var("DATA_PATH").unwrap_or_else(|_| "/data/persistent.json".to_string());
    let capture_failure_screenshots = std::env::var("CAPTURE_FAILURE_SCREENSHOTS")
        .unwrap_or_else(|_| "true".to_string())
        .parse::<bool>()
        .unwrap_or(true);
    let screenshot_dir =
        std::env::var("SCREENSHOT_DIR").unwrap_or_else(|_| "/data/screenshots".to_string());
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = std::env::var("PORT")
        .unwrap_or_else(|_| "8191".to_string())
        .parse::<u16>()
        .unwrap_or(8191);

    let proxy = if let (Some(username), Some(password)) = (proxy_username, proxy_password) {
        ProxyConfig::with_auth(proxy_host, proxy_port, username, password)
    } else {
        ProxyConfig::new(proxy_host, proxy_port)
    };

    let scrappey = ScrappeyConfig::new(scrappey_api_key);
    let screenshots = ScreenshotConfig::new(capture_failure_screenshots, screenshot_dir);

    Ok(ServerConfig::new(
        proxy,
        scrappey,
        screenshots,
        data_path,
        host,
        port,
    ))
}
