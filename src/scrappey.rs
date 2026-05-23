//! Scrappey API client and data structures for integrating with the Scrappey challenge-solving service.
//! Provides GET/POST request wrappers, balance checking, and conversion utilities for cookies.

use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use thirtyfour::{Cookie, SameSite};

/// Client for interacting with the Scrappey API.
#[derive(Debug, Clone)]
pub struct ScrappeyClient {
    api_key: String,
    client: Client,
    endpoint: String,
}

impl Default for ScrappeyClient {
    /// Unconfigured client — `is_configured()` returns false. Used as the
    /// `BrowserConfig::default()` value; real instances come from
    /// `ScrappeyClient::new(api_key)` once at server startup.
    fn default() -> Self {
        Self::new(String::new())
    }
}

impl ScrappeyClient {
    /// Create a new ScrappeyClient with the given API key. The internal
    /// reqwest::Client is built once (with its own connection pool) and
    /// the whole struct is Clone, so callers should construct one instance
    /// at startup and clone it into long-lived state instead of calling
    /// `new` per request.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: Client::new(),
            endpoint: "https://publisher.scrappey.com/api/v1".to_string(),
        }
    }

    /// True when an API key is set and the client is usable.
    pub fn is_configured(&self) -> bool {
        !self.api_key.is_empty()
    }

    /// Check remaining balance (number of requests left) on the Scrappey account.
    pub async fn get_balance(&self, timeout: u64) -> Result<ScrappeyBalance> {
        let resp = self
            .client
            .get(format!("{}/balance?key={}", self.endpoint, self.api_key))
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(timeout))
            .send()
            .await?;

        resp.json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse balance response: {}", e))
    }

    /// Make a GET request via Scrappey, using the provided parameters and timeout.
    pub async fn get(&self, req: ScrappeyGetRequest, timeout: u64) -> Result<ScrappeyResponse> {
        let mut payload = serde_json::to_value(&req)?
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Failed to convert request to JSON object"))?
            .clone();
        payload.insert("cmd".to_string(), Value::String("request.get".to_string()));
        let resp = self
            .client
            .post(format!("{}?key={}", self.endpoint, self.api_key))
            .header("Content-Type", "application/json")
            .json(&payload)
            .timeout(std::time::Duration::from_secs(timeout))
            .send()
            .await?;
        resp.json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse Scrappey response: {}", e))
    }

}

/// Balance response from Scrappey API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrappeyBalance {
    /// Number of requests remaining in your balance
    pub balance: f64,
}

/// Parameters for Scrappey GET requests.
/// Most fields are optional and allow fine-tuning of the request.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScrappeyGetRequest {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cookiejar: Option<Vec<ScrappeyCookie>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cookies: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    #[serde(rename = "proxyCountry", skip_serializing_if = "Option::is_none")]
    pub proxy_country: Option<String>,
    #[serde(rename = "customHeaders", skip_serializing_if = "Option::is_none")]
    pub custom_headers: Option<HashMap<String, String>>,
    #[serde(rename = "includeImages", skip_serializing_if = "Option::is_none")]
    pub include_images: Option<bool>,
    #[serde(rename = "includeLinks", skip_serializing_if = "Option::is_none")]
    pub include_links: Option<bool>,
    #[serde(rename = "requestType", skip_serializing_if = "Option::is_none")]
    pub request_type: Option<String>,
    #[serde(rename = "localStorage", skip_serializing_if = "Option::is_none")]
    pub local_storage: Option<HashMap<String, String>>,
}

/// Cookie object for Scrappey requests and responses.
/// Used for cookiejar and response cookies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrappeyCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<i64>,
    #[serde(rename = "httpOnly", skip_serializing_if = "Option::is_none")]
    pub http_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
    #[serde(rename = "sameSite", skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
}

/// Convert a ScrappeyCookie to a thirtyfour::Cookie for browser automation.
impl From<ScrappeyCookie> for Cookie {
    fn from(scr: ScrappeyCookie) -> Self {
        Cookie {
            name: scr.name,
            value: scr.value,
            path: Some(scr.path),
            domain: Some(scr.domain),
            secure: scr.secure,
            expiry: scr.expires,
            same_site: scr.same_site.and_then(|s| match s.to_lowercase().as_str() {
                "lax" => Some(SameSite::Lax),
                "strict" => Some(SameSite::Strict),
                "none" => Some(SameSite::None),
                _ => None,
            }),
        }
    }
}

/// Scrappey API response for challenge-solving requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrappeyResponse {
    pub solution: ScrappeySolution,
    #[serde(rename = "timeElapsed")]
    pub time_elapsed: Option<u64>,
    pub data: Option<String>,
    pub session: Option<String>,
}

/// Solution object returned by Scrappey for a challenge-solving request.
/// Contains cookies, user agent, response body, and other metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrappeySolution {
    pub verified: Option<bool>,
    #[serde(rename = "currentUrl")]
    pub current_url: Option<String>,
    #[serde(rename = "statusCode")]
    pub status_code: Option<u16>,
    #[serde(rename = "userAgent")]
    pub user_agent: Option<String>,
    #[serde(rename = "innerText")]
    pub inner_text: Option<String>,
    #[serde(rename = "localStorageData")]
    pub local_storage_data: Option<HashMap<String, String>>,
    pub cookies: Option<Vec<ScrappeyCookie>>,
    #[serde(rename = "cookieString")]
    pub cookie_string: Option<String>,
    pub response: Option<String>,
    #[serde(rename = "responseHeaders")]
    pub response_headers: Option<HashMap<String, Value>>,
    #[serde(rename = "requestHeaders")]
    pub request_headers: Option<HashMap<String, Value>>,
    #[serde(rename = "requestBody")]
    pub request_body: Option<String>,
    #[serde(rename = "ipInfo")]
    pub ip_info: Option<HashMap<String, Value>>,
    pub method: Option<String>,
    #[serde(rename = "type")]
    pub r#type: Option<String>,
}
