//! Simple logging utilities.

use log::{debug, error, info, warn};
use std::time::Instant;

/// Logging context with optional metadata.
#[derive(Clone, Debug, Default)]
pub struct LogContext {
    pub session_id: Option<String>,
    pub url: Option<String>,
    pub operation: Option<String>,
}

impl LogContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_session(mut self, session_id: &str) -> Self {
        self.session_id = Some(session_id.to_string());
        self
    }

    pub fn with_url(mut self, url: &str) -> Self {
        self.url = Some(url.to_string());
        self
    }

    pub fn with_operation(mut self, operation: &str) -> Self {
        self.operation = Some(operation.to_string());
        self
    }

    fn prefix(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ref op) = self.operation {
            parts.push(format!("op={}", op));
        }
        if let Some(ref sid) = self.session_id {
            parts.push(format!("session={}", sid));
        }
        if let Some(ref url) = self.url {
            let display = if url.len() > 60 { &url[..57] } else { url };
            parts.push(format!("url={}", display));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("[{}] ", parts.join(" "))
        }
    }

    pub fn info(&self, msg: &str) {
        info!("{}{}", self.prefix(), msg);
    }

    pub fn debug(&self, msg: &str) {
        debug!("{}{}", self.prefix(), msg);
    }

    pub fn warn(&self, msg: &str) {
        warn!("{}{}", self.prefix(), msg);
    }

    pub fn error(&self, msg: &str) {
        error!("{}{}", self.prefix(), msg);
    }
}

/// Simple timing logger.
pub struct TimingLogger {
    context: LogContext,
    start: Instant,
}

impl TimingLogger {
    pub fn new(operation: &str) -> Self {
        Self {
            context: LogContext::new().with_operation(operation),
            start: Instant::now(),
        }
    }

    pub fn with_url(mut self, url: &str) -> Self {
        self.context = self.context.with_url(url);
        self
    }

    pub fn finish(self) {
        let secs = self.start.elapsed().as_secs_f64();
        self.context.info(&format!("Completed in {:.2}s", secs));
    }

    pub fn finish_silent(self) -> std::time::Duration {
        self.start.elapsed()
    }
}

/// Log an error with its cause chain.
pub fn log_error_with_context(ctx: &LogContext, error: &anyhow::Error) {
    let mut msg = format!("Error: {}", error);
    let err_ref: &(dyn std::error::Error + 'static) = error.as_ref();
    let mut source = err_ref.source();
    while let Some(err) = source {
        msg.push_str(&format!(" -> {}", err));
        source = err.source();
    }
    ctx.error(&msg);
}
