//! FlareSolverr-compatible API server.

use axum::{
    Router,
    extract::Json,
    http::StatusCode,
    response::{IntoResponse, Json as ResponseJson},
    routing::{get, post},
};
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thirtyfour::{Cookie, SameSite};

use crate::config::ServerConfig;
use crate::logging::LogContext;
use crate::session::{DEFAULT_SESSION_ID, SessionManager};

const STATUS_OK: &str = "ok";
const STATUS_ERROR: &str = "error";
const VERSION: &str = "3.3.21";
const DEFAULT_TIMEOUT_MS: u32 = 60000;

/// FlareSolverr cookie format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlaresolverrCookie {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    pub expires: f64,
    #[serde(rename = "httpOnly")]
    pub http_only: bool,
    pub secure: Option<bool>,
    #[serde(rename = "sameSite")]
    pub same_site: Option<String>,
}

impl From<Cookie> for FlaresolverrCookie {
    fn from(c: Cookie) -> Self {
        Self {
            name: c.name,
            value: c.value,
            domain: c.domain,
            path: c.path,
            expires: c.expiry.map_or(-1.0, |e| e as f64 / 1000.0),
            http_only: false,
            secure: c.secure,
            same_site: c.same_site.map(|s| format!("{:?}", s)),
        }
    }
}

impl From<FlaresolverrCookie> for Cookie {
    fn from(c: FlaresolverrCookie) -> Self {
        let same_site = c.same_site.and_then(|s| match s.to_lowercase().as_str() {
            "lax" => Some(SameSite::Lax),
            "strict" => Some(SameSite::Strict),
            "none" => Some(SameSite::None),
            _ => None,
        });
        let expiry = if c.expires < 0.0 {
            None
        } else {
            // FlareSolverr exposes `expires` in seconds since epoch (with
            // fractional precision); thirtyfour wants i64 seconds.
            Some(c.expires as i64)
        };
        Cookie {
            name: c.name,
            value: c.value,
            domain: c.domain,
            path: c.path,
            expiry,
            secure: c.secure,
            same_site,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Solution {
    pub url: String,
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub response: String,
    pub cookies: Vec<FlaresolverrCookie>,
    #[serde(rename = "userAgent")]
    pub user_agent: String,
}

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
    // Deprecated
    pub headers: Option<Vec<HashMap<String, String>>>,
    #[serde(rename = "userAgent")]
    pub user_agent: Option<String>,
    pub download: Option<bool>,
    #[serde(rename = "returnRawHtml")]
    pub return_raw_html: Option<bool>,
}

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
    pub solution: Option<Solution>,
    pub session: Option<String>,
    pub sessions: Option<Vec<String>>,
}

impl V1Response {
    fn ok(message: &str) -> Self {
        Self {
            status: STATUS_OK.to_string(),
            message: message.to_string(),
            start_timestamp: 0,
            end_timestamp: 0,
            version: VERSION.to_string(),
            solution: None,
            session: None,
            sessions: None,
        }
    }

    fn error(message: &str) -> Self {
        Self {
            status: STATUS_ERROR.to_string(),
            message: format!("Error: {}", message),
            start_timestamp: 0,
            end_timestamp: 0,
            version: VERSION.to_string(),
            solution: None,
            session: None,
            sessions: None,
        }
    }

    fn with_solution(mut self, solution: Solution) -> Self {
        self.solution = Some(solution);
        self
    }

    fn with_session(mut self, session: String) -> Self {
        self.session = Some(session);
        self
    }

    fn with_sessions(mut self, sessions: Vec<String>) -> Self {
        self.sessions = Some(sessions);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexResponse {
    pub msg: String,
    pub version: String,
    #[serde(rename = "userAgent")]
    pub user_agent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
}

/// URL of the local chromedriver instance probed by the health endpoint.
const CHROMEDRIVER_STATUS_URL: &str = "http://127.0.0.1:9515/status";

/// FlareSolverr API server.
pub struct FlareSolverrAPI {
    session_manager: SessionManager,
}

impl FlareSolverrAPI {
    pub fn new(config: ServerConfig) -> Self {
        let data_dir = std::path::Path::new(&config.data_path).join("sessions");
        let session_manager = SessionManager::new(config.to_browser_config(), data_dir);
        Self { session_manager }
    }

    pub fn create_router(&self) -> Router {
        let sm = self.session_manager.clone();
        Router::new()
            .route("/", get(index))
            .route("/health", get(health))
            .route("/v1", post(move |req| v1_handler(req, sm.clone())))
    }
}

async fn index() -> ResponseJson<IndexResponse> {
    debug!("Index endpoint accessed");
    ResponseJson(IndexResponse {
        msg: "FlareSolverr is ready!".to_string(),
        version: VERSION.to_string(),
        user_agent: "That's a secret :)".to_string(),
    })
}

async fn health() -> impl IntoResponse {
    debug!("Health check");
    match probe_chromedriver().await {
        Ok(()) => (
            StatusCode::OK,
            ResponseJson(HealthResponse {
                status: STATUS_OK.to_string(),
            }),
        ),
        Err(reason) => {
            warn!("Health check failed: {}", reason);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                ResponseJson(HealthResponse {
                    status: format!("chromedriver unhealthy: {}", reason),
                }),
            )
        }
    }
}

/// Probe chromedriver's /status and confirm it reports ready=true.
async fn probe_chromedriver() -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .map_err(|e| format!("client: {}", e))?;

    let resp = client
        .get(CHROMEDRIVER_STATUS_URL)
        .send()
        .await
        .map_err(|e| format!("connect: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("status: {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse: {}", e))?;

    let ready = body
        .get("value")
        .and_then(|v| v.get("ready"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !ready {
        return Err("chromedriver reports not ready".to_string());
    }

    Ok(())
}

async fn v1_handler(Json(req): Json<V1Request>, sm: SessionManager) -> ResponseJson<V1Response> {
    let start = now_ms();
    let mut ctx = LogContext::new()
        .with_operation(&req.cmd)
        .with_session(req.session.as_deref().unwrap_or("default"));

    if let Some(ref url) = req.url {
        ctx = ctx.with_url(url);
    }

    ctx.info("Incoming request");

    let mut response = match req.cmd.as_str() {
        "request.get" => handle_get(req, &sm).await,
        "request.post" => handle_post(req).await,
        "sessions.create" => handle_create(req, &sm).await,
        "sessions.list" => handle_list(&sm).await,
        "sessions.destroy" => handle_destroy(req, &sm).await,
        "" => V1Response::error("Parameter 'cmd' is mandatory"),
        cmd => V1Response::error(&format!("Unknown command: {}", cmd)),
    };

    response.start_timestamp = start;
    response.end_timestamp = now_ms();

    let duration = (response.end_timestamp - start) as f64 / 1000.0;
    if response.status == STATUS_OK {
        ctx.info(&format!("Completed in {:.2}s", duration));
    } else {
        ctx.error(&format!(
            "Failed after {:.2}s: {}",
            duration, response.message
        ));
    }

    ResponseJson(response)
}

async fn handle_get(req: V1Request, sm: &SessionManager) -> V1Response {
    // Validate
    let url = match req.url {
        Some(ref u) => u.clone(),
        None => return V1Response::error("Parameter 'url' is mandatory for request.get"),
    };
    if req.post_data.is_some() {
        return V1Response::error("Cannot use 'postData' with GET request");
    }

    // Warn about deprecated params
    warn_deprecated(&req);

    // Per-request `proxy` would require rebuilding the proxy bridge per
    // request and is not supported by this server. The upstream proxy is
    // fixed at startup via env. Warn-and-ignore stays FlareSolverr-compatible.
    if req.proxy.is_some() {
        warn!(
            "Per-request 'proxy' is ignored; upstream proxy is configured globally via env"
        );
    }
    // `session_ttl_minutes` only has meaning at sessions.create time.
    if req.session_ttl_minutes.is_some() {
        warn!("'session_ttl_minutes' on request.get is ignored; set it on sessions.create");
    }

    let timeout = req.max_timeout.unwrap_or(DEFAULT_TIMEOUT_MS) / 1000;
    let session_id = sm.resolve_session_id(req.session.as_deref());
    let is_default = session_id == DEFAULT_SESSION_ID;

    let handle = match sm.get_session(&session_id).await {
        Some(h) => h,
        None => return V1Response::error(&format!("Session not found: {}", session_id)),
    };

    // Hold the per-session lock for the whole request so concurrent calls
    // on the same session id serialize cleanly (no cookie/UA clobber).
    let mut session = handle.lock().await;
    session.touch();
    session.browser.config.webdriver.window_size = (1280, 720);

    // Only load from disk for non-default sessions; default was preloaded.
    if !is_default {
        session.load().ok();
    }

    // Merge per-request cookies into the session before navigation.
    // Upsert on (name, domain, path) so clients can overwrite stale entries.
    if let Some(extra) = req.cookies.clone() {
        let added = extra.len();
        for incoming in extra {
            let incoming = Cookie::from(incoming);
            if let Some(existing) = session.browser.data.cookies.iter_mut().find(|c| {
                c.name == incoming.name
                    && c.domain == incoming.domain
                    && c.path == incoming.path
            }) {
                *existing = incoming;
            } else {
                session.browser.data.cookies.push(incoming);
            }
        }
        debug!(
            "[session={}] Merged {} client-supplied cookie(s)",
            session_id, added
        );
    }

    debug!(
        "[session={}] Using session ({} cookies)",
        session_id,
        session.browser.data.cookies.len()
    );

    let nav_result = session.browser.get(&url, timeout.into()).await;

    // Save session state regardless of success/failure so partial cookies
    // from a failed challenge attempt are not lost.
    if let Err(e) = session.save() {
        warn!("[session={}] Failed to save: {}", session_id, e);
    }

    match nav_result {
        Ok(response) => {
            let solution = Solution {
                url: response.url,
                status: response.status,
                headers: HashMap::new(),
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

            V1Response::ok("Challenge solved!")
                .with_solution(solution)
                .with_session(session_id)
        }
        Err(e) => V1Response::error(&format!("Challenge failed: {}", e)),
    }
}

async fn handle_post(req: V1Request) -> V1Response {
    if req.post_data.is_none() {
        return V1Response::error("Parameter 'postData' is mandatory for request.post");
    }
    warn_deprecated(&req);
    // Explicit, loud, and unmistakable — Prowlarr will surface this in its UI
    // instead of treating it as a transient indexer failure.
    V1Response::error(
        "request.post is not implemented by scrappey-resolverr-rs; \
         configure your indexer to use GET or route POSTs through a different solver",
    )
}

async fn handle_create(req: V1Request, sm: &SessionManager) -> V1Response {
    match sm.create_session(req.session_ttl_minutes).await {
        Ok(id) => V1Response::ok("Session created").with_session(id),
        Err(e) => V1Response::error(&format!("Failed to create session: {}", e)),
    }
}

async fn handle_list(sm: &SessionManager) -> V1Response {
    let sessions = sm.list_sessions().await;
    let count = sessions.len();
    V1Response::ok(&format!("Found {} session(s)", count)).with_sessions(sessions)
}

async fn handle_destroy(req: V1Request, sm: &SessionManager) -> V1Response {
    let id = match req.session {
        Some(ref id) => id.clone(),
        None => return V1Response::error("Parameter 'session' is mandatory for sessions.destroy"),
    };

    match sm.destroy_session(&id).await {
        Ok(()) => V1Response::ok("Session destroyed").with_session(id),
        Err(e) => V1Response::error(&format!("{}", e)),
    }
}

fn warn_deprecated(req: &V1Request) {
    if req.headers.is_some() {
        warn!("Deprecated: 'headers' was removed in FlareSolverr v2");
    }
    if req.user_agent.is_some() {
        warn!("Deprecated: 'userAgent' was removed in FlareSolverr v2");
    }
    if req.return_raw_html.is_some() {
        warn!("Deprecated: 'returnRawHtml' was removed in FlareSolverr v2");
    }
    if req.download.is_some() {
        warn!("Deprecated: 'download' was removed in FlareSolverr v2");
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
