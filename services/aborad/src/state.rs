//! Shared state held by the daemon across requests.
//!
//! `config` and `tokens` are behind [`std::sync::RwLock`] so the config
//! reload path (SIGHUP / file watching) can swap in a fresh configuration
//! and token store without replacing the whole [`AppState`] (which requests
//! and the server task hold concurrently).

use std::sync::{Arc, RwLock};
use std::time::Instant;

use abora_config::Config;
use abora_log::Logger;

use crate::tokens::TokenStore;

/// Process-wide state passed to every handler via axum's `State`.
pub struct AppState {
    pub config: RwLock<Config>,
    pub logger: Logger,
    /// Loaded bearer-token store, when `[security] token_file` is set.
    /// Replaced wholesale on reload.
    pub tokens: RwLock<Option<TokenStore>>,
    pub started_at: Instant,
    /// RFC 3339 timestamp captured at startup (stable across requests).
    pub started_at_rfc3339: String,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    pub fn new(config: Config, logger: Logger, tokens: Option<TokenStore>) -> Arc<Self> {
        Arc::new(Self {
            started_at_rfc3339: abora_log::rfc3339_now(),
            config: RwLock::new(config),
            logger,
            tokens: RwLock::new(tokens),
            started_at: Instant::now(),
        })
    }

    /// Seconds since the daemon started.
    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}