use anyhow::Result;
use axum::{
    Router,
    extract::Json,
    http::StatusCode,
    response::Json as ResponseJson,
    routing::{get, post},
};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thirtyfour::Cookie;

use crate::browser::{Browser, BrowserConfig};

/// This module implements the FlareSolverr-compatible API server.
/// It provides endpoints for challenge-solving automation, health checks, and session management.
/// The main entrypoint is FlareSolverrAPI, which wires up the Axum router.
const STATUS_OK: &str = "ok";
const STATUS_ERROR: &str = "error";
const FLARESOLVERR_VERSION: &str = "3.3.21"; // Version string for compatibility

/// FlareSolverr-compatible cookie representation.
/// Used for API serialization/deserialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlaresolverrCookie {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    pub expires: f64, // FlareSolverr uses float for expires
    #[serde(rename = "httpOnly")]
    pub http_only: bool,
    pub secure: Option<bool>,
    #[serde(rename = "sameSite")]
    pub same_site: Option<String>,
}

/// Conversion from thirtyfour::Cookie to FlaresolverrCookie.
impl From<Cookie> for FlaresolverrCookie {
    fn from(cookie: Cookie) -> Self {
        FlaresolverrCookie {
            name: cookie.name,
            value: cookie.value,
            domain: cookie.domain,
            path: cookie.path,
            // If expiry is None, treat as session cookie and set to -1
            expires: cookie
                .expiry
                .map_or(-1.0, |exp| exp as f64 / 1000.0), // Convert ms to seconds
            http_only: /* not provided by chromedriver */ false,
            secure: cookie.secure,
            same_site: cookie.same_site.map(|s| match s {
                thirtyfour::SameSite::Lax => "Lax".to_string(),
                thirtyfour::SameSite::Strict => "Strict".to_string(),
                thirtyfour::SameSite::None => "None".to_string(),
            }),
        }
    }
}

/// Proxy configuration for incoming API requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// The solution/result returned by a challenge-solving request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeResolutionResult {
    pub url: String,
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub response: String,
    pub cookies: Vec<FlaresolverrCookie>,
    #[serde(rename = "userAgent")]
    pub user_agent: String,
}

/// Incoming request format for the FlareSolverr v1 API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct V1Request {
    pub cmd: String,
    pub url: Option<String>,
    #[serde(rename = "postData")]
    pub post_data: Option<String>,
    #[serde(rename = "maxTimeout")]
    pub max_timeout: Option<u32>,
    pub proxy: Option<ProxyConfig>,
    pub session: Option<String>,
    #[serde(rename = "session_ttl_minutes")]
    pub session_ttl_minutes: Option<u32>,
    pub cookies: Option<Vec<FlaresolverrCookie>>,
    #[serde(rename = "returnOnlyCookies")]
    pub return_only_cookies: Option<bool>,
    // Deprecated fields (for compatibility)
    pub headers: Option<Vec<HashMap<String, String>>>,
    #[serde(rename = "userAgent")]
    pub user_agent: Option<String>,
    pub download: Option<bool>,
    #[serde(rename = "returnRawHtml")]
    pub return_raw_html: Option<bool>,
}

/// Outgoing response format for the FlareSolverr v1 API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct V1Response {
    pub status: String,
    pub message: String,
    #[serde(rename = "startTimestamp")]
    pub start_timestamp: u64,
    #[serde(rename = "endTimestamp")]
    pub end_timestamp: u64,
    pub version: String,
    pub solution: Option<ChallengeResolutionResult>,
    pub session: Option<String>,
    pub sessions: Option<Vec<String>>,
}

/// Response for the index endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexResponse {
    pub msg: String,
    pub version: String,
    #[serde(rename = "userAgent")]
    pub user_agent: String,
}

/// Response for the health check endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
}

/// Error response format (not currently used in main API).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    pub status_code: u16,
}

/// Configuration for the FlareSolverr API server and browser automation.
#[derive(Debug, Clone)]
pub struct FlareSolverrConfig {
    pub proxy_host: String,
    pub proxy_port: u16,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
    pub scrappey_api_key: String,
    pub data_path: String,
}

/// Main API struct for FlareSolverr-compatible server.
pub struct FlareSolverrAPI {
    config: FlareSolverrConfig,
}

impl FlareSolverrAPI {
    /// Create a new API instance with the given config.
    pub fn new(config: FlareSolverrConfig) -> Self {
        Self { config }
    }

    /// Build the Axum router with all endpoints.
    pub fn create_router(&self) -> Router {
        let config = self.config.clone();

        Router::new()
            .route("/", get(index))
            .route("/health", get(health))
            .route(
                "/v1",
                post(move |request| v1_handler(request, config.clone())),
            )
    }
}

// Handler for the index page
/// Handler for the index page ("/").
async fn index() -> ResponseJson<IndexResponse> {
    info!("Index endpoint called");
    ResponseJson(IndexResponse {
        msg: "FlareSolverr is ready!".to_string(),
        version: FLARESOLVERR_VERSION.to_string(),
        user_agent: get_user_agent(),
    })
}

// Handler for health check
/// Handler for health check ("/health").
async fn health() -> ResponseJson<HealthResponse> {
    info!("Health endpoint called");
    ResponseJson(HealthResponse {
        status: STATUS_OK.to_string(),
    })
}

// Main V1 API handler
/// Main handler for the v1 API endpoint ("/v1").
/// Handles all challenge-solving and session commands.
async fn v1_handler(
    Json(request): Json<V1Request>,
    config: FlareSolverrConfig,
) -> Result<ResponseJson<V1Response>, (StatusCode, ResponseJson<ErrorResponse>)> {
    let start_timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    info!("Incoming request => POST /v1 body: {request:?}");

    let result = handle_v1_request(request, config).await;

    let end_timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    match result {
        Ok(mut response) => {
            response.start_timestamp = start_timestamp;
            response.end_timestamp = end_timestamp;
            response.version = FLARESOLVERR_VERSION.to_string();

            info!(
                "Response in {} s",
                (end_timestamp - start_timestamp) as f64 / 1000.0
            );
            Ok(ResponseJson(response))
        }
        Err(error_msg) => {
            let error_response = V1Response {
                status: STATUS_ERROR.to_string(),
                message: format!("Error: {error_msg}"),
                start_timestamp,
                end_timestamp,
                version: FLARESOLVERR_VERSION.to_string(),
                solution: None,
                session: None,
                sessions: None,
            };

            error!("Error: {error_msg}");
            Ok(ResponseJson(error_response))
        }
    }
}

/// Dispatches the v1 API command to the appropriate handler.
async fn handle_v1_request(
    req: V1Request,
    config: FlareSolverrConfig,
) -> Result<V1Response, String> {
    // Validate required fields
    if req.cmd.is_empty() {
        return Err("Request parameter 'cmd' is mandatory.".to_string());
    }

    // Warn about deprecated parameters for compatibility
    if req.headers.is_some() {
        warn!("Warning: Request parameter 'headers' was removed in FlareSolverr v2.");
    }
    if req.user_agent.is_some() {
        warn!("Warning: Request parameter 'userAgent' was removed in FlareSolverr v2.");
    }

    // Set default timeout (ms to seconds)
    let max_timeout = req.max_timeout.unwrap_or(60000) / 1000;

    match req.cmd.as_str() {
        "request.get" => handle_request_get(req, max_timeout, config).await,
        "request.post" => handle_request_post(req, max_timeout, config).await,
        "sessions.create" => handle_sessions_create(req).await,
        "sessions.list" => handle_sessions_list(req).await,
        "sessions.destroy" => handle_sessions_destroy(req).await,
        _ => Err(format!(
            "Request parameter 'cmd' = '{}' is invalid.",
            req.cmd
        )),
    }
}

/// Handles GET challenge-solving requests.
async fn handle_request_get(
    req: V1Request,
    max_timeout: u32,
    config: FlareSolverrConfig,
) -> Result<V1Response, String> {
    // Validate GET request
    if req.url.is_none() {
        return Err("Request parameter 'url' is mandatory in 'request.get' command.".to_string());
    }
    if req.post_data.is_some() {
        return Err("Cannot use 'postData' when sending a GET request.".to_string());
    }
    if req.return_raw_html.is_some() {
        warn!("Warning: Request parameter 'returnRawHtml' was removed in FlareSolverr v2.");
    }
    if req.download.is_some() {
        warn!("Warning: Request parameter 'download' was removed in FlareSolverr v2.");
    }

    let url = req.url.unwrap();

    // Create browser instance with config
    let mut browser = Browser::new().with_config(BrowserConfig {
        window_size: (1280, 720),
        proxy_host: config.proxy_host,
        proxy_port: config.proxy_port,
        proxy_username: config.proxy_username.clone(),
        proxy_password: config.proxy_password.clone(),
        scrappey_api_key: config.scrappey_api_key,
        ..Default::default()
    });

    // Try to load browser data if available (for session persistence)
    if let Err(e) = browser.load_data(&config.data_path) {
        warn!("Failed to load browser data, starting fresh: {e}");
    }

    // Navigate to the URL and solve challenges
    match browser.get(&url, u64::from(max_timeout)).await {
        Ok(response) => {
            // Save browser data after navigation
            if let Err(e) = browser.save_data(&config.data_path) {
                warn!("Failed to save browser data: {e}");
            }

            // Convert browser response to FlareSolverr format
            let solution = ChallengeResolutionResult {
                url: response.url,
                status: response.status,
                headers: HashMap::new(), // Not provided by chromedriver
                response: if req.return_only_cookies.unwrap_or(false) {
                    String::new()
                } else {
                    response.body
                },
                cookies: response
                    .cookies
                    .into_iter()
                    .map(FlaresolverrCookie::from)
                    .collect(),
                user_agent: response.user_agent,
            };

            Ok(V1Response {
                status: STATUS_OK.to_string(),
                message: "Challenge solved!".to_string(),
                start_timestamp: 0, // Will be set by caller
                end_timestamp: 0,   // Will be set by caller
                version: FLARESOLVERR_VERSION.to_string(),
                solution: Some(solution),
                session: None,
                sessions: None,
            })
        }
        Err(e) => {
            // Save browser data even on error
            if let Err(save_err) = browser.save_data(&config.data_path) {
                warn!("Failed to save browser data: {save_err}");
            }

            Err(format!("Error solving the challenge: {e}"))
        }
    }
}

/// Handles POST challenge-solving requests (not implemented).
async fn handle_request_post(
    req: V1Request,
    _max_timeout: u32,
    _config: FlareSolverrConfig,
) -> Result<V1Response, String> {
    // Validate POST request
    if req.post_data.is_none() {
        return Err(
            "Request parameter 'postData' is mandatory in 'request.post' command.".to_string(),
        );
    }
    if req.return_raw_html.is_some() {
        warn!("Warning: Request parameter 'returnRawHtml' was removed in FlareSolverr v2.");
    }
    if req.download.is_some() {
        warn!("Warning: Request parameter 'download' was removed in FlareSolverr v2.");
    }

    Err("POST requests are not yet implemented.".to_string())
}

/// Handler for session creation (not implemented).
async fn handle_sessions_create(_req: V1Request) -> Result<V1Response, String> {
    Err("Sessions are not implemented in this version.".to_string())
}

/// Handler for session listing (not implemented).
async fn handle_sessions_list(_req: V1Request) -> Result<V1Response, String> {
    Err("Sessions are not implemented in this version.".to_string())
}

/// Handler for session destruction (not implemented).
async fn handle_sessions_destroy(_req: V1Request) -> Result<V1Response, String> {
    Err("Sessions are not implemented in this version.".to_string())
}

/// Returns a placeholder user agent string for the index endpoint.
fn get_user_agent() -> String {
    "That's a secret :)".to_string()
}
