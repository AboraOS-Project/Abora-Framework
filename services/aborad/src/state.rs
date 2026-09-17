//! Shared state held by the daemon across requests.

use std::sync::Arc;
use std::time::Instant;

use abora_config::Config;
use abora_log::Logger;

use crate::tokens::TokenStore;

/// Process-wide state passed to every handler via axum's `State`.
pub struct AppState {
    pub config: Config,
    pub logger: Logger,
    /// Loaded bearer-token store, when `[security] token_file` is set.
    pub tokens: Option<TokenStore>,
    pub started_at: Instant,
    /// RFC 3339 timestamp captured at startup (stable across requests).
    pub started_at_rfc3339: String,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    pub fn new(config: Config, logger: Logger, tokens: Option<TokenStore>) -> Arc<Self> {
        Arc::new(Self {
            started_at_rfc3339: abora_log::rfc3339_now(),
            config,
            logger,
            tokens,
            started_at: Instant::now(),
        })
    }

    /// Seconds since the daemon started.
    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}