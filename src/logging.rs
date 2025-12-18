//! Structured logging utilities for better observability.

use log::{debug, error, info, warn};
use std::time::{Duration, Instant};

/// Logging context for tracking operations with metadata.
#[derive(Clone, Debug)]
pub struct LogContext {
    pub session_id: Option<String>,
    pub url: Option<String>,
    pub operation: Option<String>,
}

impl LogContext {
    /// Create a new logging context.
    pub fn new() -> Self {
        Self {
            session_id: None,
            url: None,
            operation: None,
        }
    }

    /// Add session ID to context.
    pub fn with_session(mut self, session_id: &str) -> Self {
        self.session_id = Some(session_id.to_string());
        self
    }

    /// Add URL to context.
    pub fn with_url(mut self, url: &str) -> Self {
        self.url = Some(url.to_string());
        self
    }

    /// Add operation name to context.
    pub fn with_operation(mut self, operation: &str) -> Self {
        self.operation = Some(operation.to_string());
        self
    }

    /// Format context for log messages.
    fn format(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ref op) = self.operation {
            parts.push(format!("op={}", op));
        }
        if let Some(ref sid) = self.session_id {
            parts.push(format!("session={}", sid));
        }
        if let Some(ref url) = self.url {
            // Truncate long URLs for readability
            let display_url = if url.len() > 80 {
                format!("{}...", &url[..77])
            } else {
                url.clone()
            };
            parts.push(format!("url={}", display_url));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("[{}] ", parts.join(" "))
        }
    }

    /// Log an info message with context.
    pub fn info(&self, message: &str) {
        info!("{}{}", self.format(), message);
    }

    /// Log a debug message with context.
    pub fn debug(&self, message: &str) {
        debug!("{}{}", self.format(), message);
    }

    /// Log a warning message with context.
    pub fn warn(&self, message: &str) {
        warn!("{}{}", self.format(), message);
    }

    /// Log an error message with context.
    pub fn error(&self, message: &str) {
        error!("{}{}", self.format(), message);
    }
}

impl Default for LogContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Timing logger for tracking operation duration.
pub struct TimingLogger {
    context: LogContext,
    start: Instant,
}

impl TimingLogger {
    /// Create a new timing logger.
    pub fn new(operation: &str) -> Self {
        Self {
            context: LogContext::new().with_operation(operation),
            start: Instant::now(),
        }
    }

    /// Add session ID to timing context.
    pub fn with_session(mut self, session_id: &str) -> Self {
        self.context = self.context.with_session(session_id);
        self
    }

    /// Add URL to timing context.
    pub fn with_url(mut self, url: &str) -> Self {
        self.context = self.context.with_url(url);
        self
    }

    /// Add operation name to timing context.
    pub fn with_operation(mut self, operation: &str) -> Self {
        self.context = self.context.with_operation(operation);
        self
    }

    /// Finish timing and log the duration.
    pub fn finish(self) -> Duration {
        let duration = self.start.elapsed();
        self.context
            .info(&format!("Completed in {:.2}s", duration.as_secs_f64()));
        duration
    }

    /// Finish timing and return duration without logging.
    pub fn finish_silent(self) -> Duration {
        self.start.elapsed()
    }
}

/// Log an error with full context and chain.
pub fn log_error_with_context(context: &LogContext, error: &anyhow::Error) {
    let mut message = format!("Error: {}", error);
    let mut source = error.source();
    let mut depth = 0;
    while let Some(err) = source {
        depth += 1;
        message.push_str(&format!(" (caused by: {})", err));
        source = err.source();
        if depth > 5 {
            // Prevent infinite chains
            break;
        }
    }
    context.error(&message);
}

/// Log an error with a simple message.
pub fn log_error_simple(context: &LogContext, message: &str, error: &dyn std::error::Error) {
    context.error(&format!("{}: {}", message, error));
}
