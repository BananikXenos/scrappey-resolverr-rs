//! Session management for concurrent browser instances.

use anyhow::Result;
use chrono::{DateTime, Utc};
use log::{debug, info};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::browser::Browser;
use crate::config::BrowserConfig;

/// Default session ID used when no session is specified
pub const DEFAULT_SESSION_ID: &str = "default";

/// Browser session with metadata.
pub struct Session {
    pub id: String,
    pub browser: Browser,
    pub created_at: DateTime<Utc>,
    pub last_used: DateTime<Utc>,
    pub ttl_minutes: Option<u32>,
    data_path: PathBuf,
}

impl Session {
    /// Create a new session, optionally with a specific ID.
    pub fn new(
        config: BrowserConfig,
        data_dir: &Path,
        ttl_minutes: Option<u32>,
        id: Option<&str>,
    ) -> Result<Self> {
        let id = id
            .map(String::from)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let data_path = data_dir.join(format!("session_{}.json", id));
        let now = Utc::now();

        let mut browser = Browser::new().with_config(config);
        if data_path.exists() {
            if let Err(e) = browser.load_data(data_path.to_str().unwrap()) {
                debug!("Could not load session data for {}: {e}", id);
            }
        }

        Ok(Self {
            id,
            browser,
            created_at: now,
            last_used: now,
            ttl_minutes,
            data_path,
        })
    }

    /// Update last used timestamp.
    pub fn touch(&mut self) {
        self.last_used = Utc::now();
    }

    /// Check if session has expired.
    pub fn is_expired(&self) -> bool {
        self.ttl_minutes.map_or(false, |ttl| {
            Utc::now() > self.created_at + chrono::Duration::minutes(ttl as i64)
        })
    }

    /// Save session data to disk.
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.data_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        self.browser.save_data(self.data_path.to_str().unwrap())
    }

    /// Load session data from disk.
    pub fn load(&mut self) -> Result<()> {
        if self.data_path.exists() {
            self.browser.load_data(self.data_path.to_str().unwrap())
        } else {
            Ok(())
        }
    }
}

/// Type alias for the shared, per-session lock. Holding this serializes
/// concurrent requests against the same session id so cookie/UA updates
/// can't clobber each other.
pub type SessionHandle = Arc<Mutex<Session>>;

/// Thread-safe session manager.
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, SessionHandle>>>,
    config: BrowserConfig,
    data_dir: PathBuf,
    _cleanup_handle: Arc<tokio::task::JoinHandle<()>>,
}

impl Clone for SessionManager {
    fn clone(&self) -> Self {
        Self {
            sessions: Arc::clone(&self.sessions),
            config: self.config.clone(),
            data_dir: self.data_dir.clone(),
            _cleanup_handle: Arc::clone(&self._cleanup_handle),
        }
    }
}

impl SessionManager {
    /// Create a new session manager with preloaded default session.
    pub fn new(config: BrowserConfig, data_dir: impl AsRef<Path>) -> Self {
        let data_dir = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&data_dir).ok();

        // Preload default session
        let default_session =
            Session::new(config.clone(), &data_dir, None, Some(DEFAULT_SESSION_ID))
                .expect("Failed to create default session");

        let mut sessions = HashMap::new();
        sessions.insert(
            DEFAULT_SESSION_ID.to_string(),
            Arc::new(Mutex::new(default_session)),
        );
        let sessions = Arc::new(RwLock::new(sessions));

        info!("[session={}] Default session preloaded", DEFAULT_SESSION_ID);

        // Background cleanup task
        let sessions_clone = Arc::clone(&sessions);
        let cleanup_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                cleanup_expired(&sessions_clone).await;
            }
        });

        Self {
            sessions,
            config,
            data_dir,
            _cleanup_handle: Arc::new(cleanup_handle),
        }
    }

    /// Get session ID to use (provided or default).
    pub fn resolve_session_id(&self, session: Option<&str>) -> String {
        session
            .map(String::from)
            .unwrap_or_else(|| DEFAULT_SESSION_ID.to_string())
    }

    /// Create a new session with a unique ID.
    pub async fn create_session(&self, ttl_minutes: Option<u32>) -> Result<String> {
        let session = Session::new(self.config.clone(), &self.data_dir, ttl_minutes, None)?;
        let id = session.id.clone();

        self.sessions
            .write()
            .await
            .insert(id.clone(), Arc::new(Mutex::new(session)));
        info!(
            "[session={}] Created (TTL: {})",
            id,
            ttl_minutes.map_or("unlimited".into(), |t| format!("{}m", t))
        );
        Ok(id)
    }

    /// Look up a session by id and return its shared handle.
    /// Callers should `.lock().await` the handle for the duration of any work
    /// against the session so concurrent requests on the same id serialize.
    pub async fn get_session(&self, id: &str) -> Option<SessionHandle> {
        self.sessions.read().await.get(id).cloned()
    }

    /// List all active session IDs.
    pub async fn list_sessions(&self) -> Vec<String> {
        self.sessions.read().await.keys().cloned().collect()
    }

    /// Destroy a session by ID.
    pub async fn destroy_session(&self, id: &str) -> Result<()> {
        if id == DEFAULT_SESSION_ID {
            anyhow::bail!("Cannot destroy the default session");
        }

        let handle = {
            let mut sessions = self.sessions.write().await;
            sessions.remove(id)
        };

        match handle {
            Some(handle) => {
                // Wait for any in-flight request on this session to drain before
                // we touch the on-disk state.
                let session = handle.lock().await;
                session.save().ok();
                if session.data_path.exists() {
                    std::fs::remove_file(&session.data_path).ok();
                }
                info!("[session={}] Destroyed", id);
                Ok(())
            }
            None => anyhow::bail!("Session not found: {}", id),
        }
    }
}

/// Clean up expired sessions (excluding default).
async fn cleanup_expired(sessions: &Arc<RwLock<HashMap<String, SessionHandle>>>) {
    // Phase 1: snapshot ids+handles under the read lock so we can probe
    // expiry without blocking new lookups.
    let candidates: Vec<(String, SessionHandle)> = {
        let sessions = sessions.read().await;
        sessions
            .iter()
            .filter(|(id, _)| *id != DEFAULT_SESSION_ID)
            .map(|(id, handle)| (id.clone(), Arc::clone(handle)))
            .collect()
    };

    let mut expired_ids = Vec::new();
    for (id, handle) in candidates {
        // try_lock so we don't block on a session that's mid-request; if
        // it's busy we'll re-check on the next sweep.
        if let Ok(session) = handle.try_lock() {
            if session.is_expired() {
                expired_ids.push(id);
            }
        }
    }

    if expired_ids.is_empty() {
        return;
    }

    // Phase 2: remove from the map.
    let mut sessions = sessions.write().await;
    for id in expired_ids {
        if let Some(handle) = sessions.remove(&id) {
            if let Ok(session) = handle.try_lock() {
                session.save().ok();
                if session.data_path.exists() {
                    std::fs::remove_file(&session.data_path).ok();
                }
            }
            debug!("[session={}] Expired and cleaned up", id);
        }
    }
}
