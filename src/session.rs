//! Session management for concurrent browser instances.
//! Provides thread-safe session storage and lifecycle management.

use anyhow::Result;
use chrono::{DateTime, Utc};
use log::{debug, warn};

use crate::logging::LogContext;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::browser::{Browser, BrowserData};
use crate::config::BrowserConfig;

/// Represents a browser session with metadata.
pub struct Session {
    /// Unique session identifier
    pub id: String,
    /// Browser instance for this session
    pub browser: Browser,
    /// When the session was created
    pub created_at: DateTime<Utc>,
    /// Last time the session was used
    pub last_used: DateTime<Utc>,
    /// Time-to-live in minutes (None = no expiration)
    pub ttl_minutes: Option<u32>,
    /// Path to session data file
    pub data_path: PathBuf,
}

impl Session {
    /// Create a new session with the given browser configuration.
    pub fn new(config: BrowserConfig, data_dir: &Path, ttl_minutes: Option<u32>) -> Result<Self> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let data_path = data_dir.join(format!("session_{}.json", id));

        // Try to load existing data if file exists (shouldn't for new sessions, but handle gracefully)
        let mut browser = Browser::new().with_config(config);
        if data_path.exists() {
            if let Err(e) = browser.load_data(data_path.to_str().unwrap()) {
                debug!("Could not load session data for new session: {e}");
            }
        }

        Ok(Session {
            id,
            browser,
            created_at: now,
            last_used: now,
            ttl_minutes,
            data_path,
        })
    }

    /// Create a session from existing data (for loading persisted sessions).
    pub fn from_data(
        id: String,
        data: BrowserData,
        config: BrowserConfig,
        data_dir: &Path,
        created_at: DateTime<Utc>,
        ttl_minutes: Option<u32>,
    ) -> Self {
        let data_path = data_dir.join(format!("session_{}.json", id));
        let mut browser = Browser::new().with_config(config);
        browser.data = data;

        Session {
            id,
            browser,
            created_at,
            last_used: Utc::now(),
            ttl_minutes,
            data_path,
        }
    }

    /// Update the last used timestamp.
    pub fn touch(&mut self) {
        self.last_used = Utc::now();
    }

    /// Check if the session has expired based on TTL.
    pub fn is_expired(&self) -> bool {
        if let Some(ttl) = self.ttl_minutes {
            let expiry = self.created_at + chrono::Duration::minutes(ttl as i64);
            Utc::now() > expiry
        } else {
            false
        }
    }

    /// Save session data to disk.
    pub fn save(&self) -> Result<()> {
        // Ensure data directory exists
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

/// Thread-safe session manager for handling multiple concurrent browser sessions.
pub struct SessionManager {
    /// Map of session ID to Session
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    /// Base configuration for new sessions
    base_config: BrowserConfig,
    /// Directory for storing session data
    data_dir: PathBuf,
    /// Background task handle for cleanup
    _cleanup_handle: Arc<tokio::task::JoinHandle<()>>,
}

impl Clone for SessionManager {
    fn clone(&self) -> Self {
        // Clone creates a new manager sharing the same session storage
        // This allows multiple API instances to share the same session pool
        Self {
            sessions: Arc::clone(&self.sessions),
            base_config: self.base_config.clone(),
            data_dir: self.data_dir.clone(),
            _cleanup_handle: Arc::clone(&self._cleanup_handle),
        }
    }
}

impl SessionManager {
    /// Create a new session manager with the given configuration.
    pub fn new(config: BrowserConfig, data_dir: impl AsRef<Path>) -> Self {
        let data_dir = data_dir.as_ref().to_path_buf();
        let sessions = Arc::new(RwLock::new(HashMap::new()));

        // Ensure data directory exists
        if let Err(e) = std::fs::create_dir_all(&data_dir) {
            warn!("Failed to create session data directory: {e}");
        }

        // Spawn background cleanup task
        let sessions_clone = Arc::clone(&sessions);
        let data_dir_clone = data_dir.clone();
        let cleanup_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                Self::cleanup_expired_sessions(&sessions_clone, &data_dir_clone).await;
            }
        });

        Self {
            sessions,
            base_config: config,
            data_dir,
            _cleanup_handle: Arc::new(cleanup_handle),
        }
    }

    /// Create a new session and return its ID.
    pub async fn create_session(&self, ttl_minutes: Option<u32>) -> Result<String> {
        let session = Session::new(self.base_config.clone(), &self.data_dir, ttl_minutes)?;
        let id = session.id.clone();

        let mut sessions = self.sessions.write().await;
        sessions.insert(id.clone(), session);

        let ctx = LogContext::new().with_session(&id);
        ctx.info(&format!(
            "Created new session (TTL: {})",
            ttl_minutes
                .map(|t| format!("{} minutes", t))
                .unwrap_or_else(|| "unlimited".to_string())
        ));
        Ok(id)
    }

    /// Execute a function with a mutable reference to a session.
    /// This is the preferred way to access and modify sessions.
    pub async fn with_session<F, R>(&self, id: &str, f: F) -> Option<R>
    where
        F: FnOnce(&mut Session) -> R,
    {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(id) {
            session.touch();
            Some(f(session))
        } else {
            None
        }
    }

    /// Check if a session exists.
    pub async fn has_session(&self, id: &str) -> bool {
        let sessions = self.sessions.read().await;
        sessions.contains_key(id)
    }

    /// List all active session IDs.
    pub async fn list_sessions(&self) -> Vec<String> {
        let sessions = self.sessions.read().await;
        sessions.keys().cloned().collect()
    }

    /// Destroy a session by ID.
    pub async fn destroy_session(&self, id: &str) -> Result<()> {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.remove(id) {
            let ctx = LogContext::new().with_session(id);

            // Try to save session data before removing
            if let Err(e) = session.save() {
                ctx.warn(&format!(
                    "Failed to save session data before destruction: {}",
                    e
                ));
            }

            // Optionally delete the session data file
            if session.data_path.exists() {
                if let Err(e) = std::fs::remove_file(&session.data_path) {
                    ctx.warn(&format!("Failed to delete session data file: {}", e));
                }
            }

            ctx.info("Session destroyed successfully");
            Ok(())
        } else {
            let ctx = LogContext::new().with_session(id);
            ctx.warn("Attempted to destroy non-existent session");
            Err(anyhow::anyhow!("Session not found: {}", id))
        }
    }

    /// Save all sessions to disk.
    pub async fn save_all(&self) {
        let sessions = self.sessions.read().await;
        for session in sessions.values() {
            if let Err(e) = session.save() {
                warn!("Failed to save session {}: {e}", session.id);
            }
        }
    }

    /// Clean up expired sessions (called by background task).
    async fn cleanup_expired_sessions(
        sessions: &Arc<RwLock<HashMap<String, Session>>>,
        _data_dir: &Path,
    ) {
        let mut sessions_write = sessions.write().await;
        let expired_ids: Vec<String> = sessions_write
            .iter()
            .filter(|(_, session)| session.is_expired())
            .map(|(id, _)| id.clone())
            .collect();

        for id in expired_ids {
            if let Some(session) = sessions_write.remove(&id) {
                let ctx = LogContext::new().with_session(&id);
                ctx.debug("Cleaning up expired session (TTL expired)");
                // Save before removing
                if let Err(e) = session.save() {
                    ctx.warn(&format!("Failed to save expired session data: {}", e));
                }
                // Optionally delete data file
                if session.data_path.exists() {
                    let _ = std::fs::remove_file(&session.data_path);
                }
            }
        }
    }
}
