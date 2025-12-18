//! Session management for concurrent browser instances.

use anyhow::Result;
use chrono::{DateTime, Utc};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
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

/// Thread-safe session manager.
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
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
        sessions.insert(DEFAULT_SESSION_ID.to_string(), default_session);
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

        self.sessions.write().await.insert(id.clone(), session);
        info!(
            "[session={}] Created (TTL: {})",
            id,
            ttl_minutes.map_or("unlimited".into(), |t| format!("{}m", t))
        );
        Ok(id)
    }

    /// Execute a function with a mutable session reference.
    pub async fn with_session<F, R>(&self, id: &str, f: F) -> Option<R>
    where
        F: FnOnce(&mut Session) -> R,
    {
        let mut sessions = self.sessions.write().await;
        sessions.get_mut(id).map(|session| {
            session.touch();
            f(session)
        })
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

        let mut sessions = self.sessions.write().await;
        match sessions.remove(id) {
            Some(session) => {
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

    /// Save all sessions to disk.
    #[allow(dead_code)]
    pub async fn save_all(&self) {
        for session in self.sessions.read().await.values() {
            if let Err(e) = session.save() {
                warn!("Failed to save session {}: {e}", session.id);
            }
        }
    }
}

/// Clean up expired sessions (excluding default).
async fn cleanup_expired(sessions: &Arc<RwLock<HashMap<String, Session>>>) {
    let mut sessions = sessions.write().await;
    let expired: Vec<_> = sessions
        .iter()
        .filter(|(id, s)| *id != DEFAULT_SESSION_ID && s.is_expired())
        .map(|(id, _)| id.clone())
        .collect();

    for id in expired {
        if let Some(session) = sessions.remove(&id) {
            session.save().ok();
            if session.data_path.exists() {
                std::fs::remove_file(&session.data_path).ok();
            }
            debug!("[session={}] Expired and cleaned up", id);
        }
    }
}
