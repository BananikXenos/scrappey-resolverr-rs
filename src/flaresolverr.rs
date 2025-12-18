use anyhow::Result;
use axum::{
    Router,
    extract::Json,
    http::StatusCode,
    response::Json as ResponseJson,
    routing::{get, post},
};
use log::{debug, warn};

use crate::logging::{LogContext, TimingLogger};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thirtyfour::Cookie;

use crate::config::ServerConfig;
use crate::session::SessionManager;

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

/// Main API struct for FlareSolverr-compatible server.
pub struct FlareSolverrAPI {
    config: ServerConfig,
    session_manager: SessionManager,
}

impl FlareSolverrAPI {
    /// Create a new API instance with the given config.
    pub fn new(config: ServerConfig) -> Self {
        let browser_config = config.to_browser_config();
        let data_dir = std::path::Path::new(&config.data_path)
            .parent()
            .unwrap_or_else(|| std::path::Path::new("/data"))
            .join("sessions");

        let session_manager = SessionManager::new(browser_config, data_dir);

        Self {
            config,
            session_manager,
        }
    }

    /// Build the Axum router with all endpoints.
    pub fn create_router(&self) -> Router {
        let config = self.config.clone();
        let session_manager = self.session_manager.clone();

        Router::new()
            .route("/", get(index))
            .route("/health", get(health))
            .route(
                "/v1",
                post(move |request| v1_handler(request, config.clone(), session_manager.clone())),
            )
    }
}

/// Handler for the index page ("/").
async fn index() -> ResponseJson<IndexResponse> {
    debug!("Index endpoint accessed");
    ResponseJson(IndexResponse {
        msg: "FlareSolverr is ready!".to_string(),
        version: FLARESOLVERR_VERSION.to_string(),
        user_agent: get_user_agent(),
    })
}

/// Handler for health check ("/health").
async fn health() -> ResponseJson<HealthResponse> {
    debug!("Health check endpoint accessed");
    ResponseJson(HealthResponse {
        status: STATUS_OK.to_string(),
    })
}

/// Main handler for the v1 API endpoint ("/v1").
/// Handles all challenge-solving and session commands.
async fn v1_handler(
    Json(request): Json<V1Request>,
    config: ServerConfig,
    session_manager: SessionManager,
) -> Result<ResponseJson<V1Response>, (StatusCode, ResponseJson<ErrorResponse>)> {
    let start_timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let timing = TimingLogger::new("v1_request");
    let session_id_clone = request.session.clone();
    let session_id = session_id_clone.as_deref();
    let url_clone = request.url.clone();
    let url = url_clone.as_deref();
    let cmd = request.cmd.clone();

    let mut ctx = LogContext::new().with_operation(&cmd);
    if let Some(sid) = session_id {
        ctx = ctx.with_session(sid);
    }
    if let Some(u) = url {
        ctx = ctx.with_url(u);
    }

    ctx.info("Incoming API request");

    let result = handle_v1_request(request, config, session_manager).await;

    let end_timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let duration_ms = (end_timestamp - start_timestamp) as f64 / 1000.0;

    match result {
        Ok(mut response) => {
            response.start_timestamp = start_timestamp;
            response.end_timestamp = end_timestamp;
            response.version = FLARESOLVERR_VERSION.to_string();

            let mut timing_logger = timing.with_session(session_id.unwrap_or("none"));
            if let Some(u) = url {
                timing_logger = timing_logger.with_url(u);
            }
            timing_logger.finish();

            ctx.info(&format!(
                "Request completed successfully in {:.2}s",
                duration_ms
            ));
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

            ctx.error(&format!(
                "Request failed after {:.2}s: {}",
                duration_ms, error_msg
            ));
            Ok(ResponseJson(error_response))
        }
    }
}

/// Dispatches the v1 API command to the appropriate handler.
async fn handle_v1_request(
    req: V1Request,
    config: ServerConfig,
    session_manager: SessionManager,
) -> Result<V1Response, String> {
    // Validate required fields
    if req.cmd.is_empty() {
        return Err("Request parameter 'cmd' is mandatory.".to_string());
    }

    let ctx = LogContext::new().with_operation(&req.cmd);

    // Warn about deprecated parameters for compatibility
    if req.headers.is_some() {
        ctx.warn("Deprecated parameter 'headers' was removed in FlareSolverr v2");
    }
    if req.user_agent.is_some() {
        ctx.warn("Deprecated parameter 'userAgent' was removed in FlareSolverr v2");
    }

    // Set default timeout (ms to seconds)
    const DEFAULT_TIMEOUT_MS: u32 = 60000;
    const MS_TO_SECONDS: u32 = 1000;
    let max_timeout = req.max_timeout.unwrap_or(DEFAULT_TIMEOUT_MS) / MS_TO_SECONDS;

    match req.cmd.as_str() {
        "request.get" => handle_request_get(req, max_timeout, config, session_manager).await,
        "request.post" => handle_request_post(req, max_timeout, config, session_manager).await,
        "sessions.create" => handle_sessions_create(req, config, session_manager).await,
        "sessions.list" => handle_sessions_list(req, session_manager).await,
        "sessions.destroy" => handle_sessions_destroy(req, session_manager).await,
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
    config: ServerConfig,
    session_manager: SessionManager,
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

    // Use session if provided, otherwise create a temporary one
    let session_id = if let Some(ref session_id) = req.session {
        session_id.clone()
    } else {
        // Create a temporary session for this request
        session_manager
            .create_session(req.session_ttl_minutes)
            .await
            .map_err(|e| format!("Failed to create session: {e}"))?
    };

    let ctx = LogContext::new().with_session(&session_id).with_url(&url);

    // Execute request with session
    let response_result = session_manager
        .with_session(&session_id, |session| {
            session.browser.config.webdriver.window_size = (1280, 720);

            // Load session data if available
            if let Err(e) = session.load() {
                ctx.warn(&format!("Failed to load session data: {}", e));
            } else {
                ctx.debug(&format!(
                    "Session data loaded ({} cookies)",
                    session.browser.data.cookies.len()
                ));
            }

            // Return session for async operation
            (session.browser.clone(), session.id.clone())
        })
        .await;

    let (mut browser, session_id_clone) = match response_result {
        Some((browser, sid)) => (browser, sid),
        None => {
            return Err(format!("Session not found: {}", session_id));
        }
    };

    // Navigate to the URL and solve challenges
    match browser.get(&url, u64::from(max_timeout)).await {
        Ok(response) => {
            // Save session data after navigation
            if let Some(Err(e)) = session_manager
                .with_session(&session_id_clone, |session| {
                    session.browser.data = browser.data.clone();
                    session.save()
                })
                .await
            {
                ctx.warn(&format!("Failed to save session data: {}", e));
            } else {
                ctx.debug(&format!(
                    "Session data saved ({} cookies)",
                    browser.data.cookies.len()
                ));
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
                session: Some(session_id_clone),
                sessions: None,
            })
        }
        Err(e) => {
            // Save browser data even on error
            let _ = session_manager
                .with_session(&session_id_clone, |session| {
                    session.browser.data = browser.data.clone();
                    session.save()
                })
                .await;

            Err(format!("Error solving the challenge: {e}"))
        }
    }
}

/// Handles POST challenge-solving requests (not implemented).
async fn handle_request_post(
    req: V1Request,
    _max_timeout: u32,
    _config: ServerConfig,
    _session_manager: SessionManager,
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

/// Handler for session creation.
async fn handle_sessions_create(
    req: V1Request,
    _config: ServerConfig,
    session_manager: SessionManager,
) -> Result<V1Response, String> {
    let session_id = session_manager
        .create_session(req.session_ttl_minutes)
        .await
        .map_err(|e| format!("Failed to create session: {e}"))?;

    Ok(V1Response {
        status: STATUS_OK.to_string(),
        message: "Session created successfully.".to_string(),
        start_timestamp: 0,
        end_timestamp: 0,
        version: FLARESOLVERR_VERSION.to_string(),
        solution: None,
        session: Some(session_id),
        sessions: None,
    })
}

/// Handler for session listing.
async fn handle_sessions_list(
    _req: V1Request,
    session_manager: SessionManager,
) -> Result<V1Response, String> {
    let sessions = session_manager.list_sessions().await;

    Ok(V1Response {
        status: STATUS_OK.to_string(),
        message: format!("Found {} active session(s).", sessions.len()),
        start_timestamp: 0,
        end_timestamp: 0,
        version: FLARESOLVERR_VERSION.to_string(),
        solution: None,
        session: None,
        sessions: Some(sessions),
    })
}

/// Handler for session destruction.
async fn handle_sessions_destroy(
    req: V1Request,
    session_manager: SessionManager,
) -> Result<V1Response, String> {
    let session_id = req.session.ok_or_else(|| {
        "Request parameter 'session' is mandatory in 'sessions.destroy' command.".to_string()
    })?;

    session_manager
        .destroy_session(&session_id)
        .await
        .map_err(|e| format!("Failed to destroy session: {e}"))?;

    Ok(V1Response {
        status: STATUS_OK.to_string(),
        message: "Session destroyed successfully.".to_string(),
        start_timestamp: 0,
        end_timestamp: 0,
        version: FLARESOLVERR_VERSION.to_string(),
        solution: None,
        session: Some(session_id),
        sessions: None,
    })
}

/// Returns a placeholder user agent string for the index endpoint.
fn get_user_agent() -> String {
    "That's a secret :)".to_string()
}
