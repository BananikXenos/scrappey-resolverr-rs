#![allow(dead_code)]

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

impl ScrappeyClient {
    /// Create a new ScrappeyClient with the given API key.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: Client::new(),
            endpoint: "https://publisher.scrappey.com/api/v1".to_string(),
        }
    }

    /// Check remaining balance (number of requests left) on the Scrappey account.
    pub async fn get_balance(&self, timeout: u64) -> Result<ScrappeyBalance> {
        let resp = self
            .client
            .get(format!("{}/balance?key={}", self.endpoint, self.api_key))
            .timeout(std::time::Duration::from_secs(timeout))
            .send()
            .await?;

        resp.json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse balance response: {}", e))
    }

    /// Make a GET request via Scrappey, using the provided parameters and timeout.
    pub async fn get(&self, req: ScrappeyGetRequest, timeout: u64) -> Result<ScrappeyResponse> {
        let mut payload = serde_json::to_value(&req)?.as_object().unwrap().clone();
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

    /// Make a POST request via Scrappey, using the provided parameters and timeout.
    pub async fn post(&self, req: ScrappeyPostRequest, timeout: u64) -> Result<ScrappeyResponse> {
        let mut payload = serde_json::to_value(&req)?.as_object().unwrap().clone();
        payload.insert("cmd".to_string(), Value::String("request.post".to_string()));
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

/// Balance response from Scrappey API
/// Balance response from Scrappey API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrappeyBalance {
    /// Number of requests remaining in your balance
    pub balance: u64,
}

/// Parameters for Scrappey GET requests
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

/// Parameters for Scrappey POST requests
/// Parameters for Scrappey POST requests.
/// Accepts post_data as either string or object, plus all GET options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrappeyPostRequest {
    pub url: String,
    #[serde(rename = "postData", skip_serializing_if = "Option::is_none")]
    pub post_data: Option<Value>, // Accepts either string or object
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

/// Cookie object for cookiejar and response cookies
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

/// Scrappey API response
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
