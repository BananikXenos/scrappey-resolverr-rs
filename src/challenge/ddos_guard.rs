//! DDoS-Guard challenge detection and handling logic.

use thirtyfour::prelude::*;

use super::{ChallengeHandler, title_contains};

/// DDoS Guard challenge handler implementation.
pub struct DdosGuardHandler;

impl DdosGuardHandler {
    /// Create a new DDoS Guard challenge handler.
    pub fn new() -> Self {
        Self
    }
}

impl Default for DdosGuardHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ChallengeHandler for DdosGuardHandler {
    fn name(&self) -> &'static str {
        "DDoS Guard"
    }

    async fn is_protected(&self, driver: &mut WebDriver) -> bool {
        title_contains(driver, "DDoS-Guard").await
    }
}
