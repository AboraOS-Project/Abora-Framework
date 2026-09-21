//! Shared state held by the daemon across requests.
//!
//! `config` and `tokens` are behind [`std::sync::RwLock`] so the config
//! reload path (SIGHUP / file watching) can swap in a fresh configuration
//! and token store without replacing the whole [`AppState`] (which requests
//! and the server task hold concurrently).

use std::sync::{Arc, RwLock};
use std::time::Instant;

use abora_api::{AvailableUpdate, Channel, UpdatesResponse};
use abora_config::Config;
use abora_log::Logger;
#[cfg(test)]
use abora_update::host::CommandRunner;
use abora_update::host::HostPackageProvider;
use abora_update::{UpdateProvider, UpdateStore};

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
    pub updates: UpdatesState,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    /// State with update checking switched off (no `apt`, nothing on disk). Used by tests.
    #[cfg(test)]
    pub fn new(config: Config, logger: Logger, tokens: Option<TokenStore>) -> Arc<Self> {
        Self::with_updates(config, logger, tokens, UpdatesState::disabled())
    }

    pub fn with_updates(config: Config, logger: Logger, tokens: Option<TokenStore>, updates: UpdatesState) -> Arc<Self> {
        Arc::new(Self {
            updates,
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

/// Update information served by `GET /api/v1/updates`: the provider that checks for
/// updates, the persistent store, and the list found by the last check.
pub struct UpdatesState {
    store: Arc<UpdateStore>,
    provider: Box<dyn UpdateProvider>,
    available: RwLock<Vec<AvailableUpdate>>,
}

/// A runner that always fails, for [`UpdatesState::disabled`].
#[cfg(test)]
struct NoChecks;

#[cfg(test)]
impl CommandRunner for NoChecks {
    fn run(&self, _program: &str, _args: &[&str]) -> Result<String, String> {
        Err("update checks are disabled".into())
    }
}

impl UpdatesState {
    /// The real thing: host packages (apt) with the given store.
    pub fn host(store: Arc<UpdateStore>) -> Self {
        Self { provider: Box::new(HostPackageProvider::new(store.clone())), store, available: RwLock::new(Vec::new()) }
    }

    /// Never runs a command and never touches the disk or `/var/run`. Used by tests.
    #[cfg(test)]
    pub fn disabled() -> Self {
        let store = Arc::new(UpdateStore::in_memory());
        let provider = HostPackageProvider::with_parts(
            store.clone(),
            Box::new(NoChecks),
            std::path::PathBuf::from("/nonexistent/abora-reboot-required"),
            abora_log::rfc3339_now,
        );
        Self { store, provider: Box::new(provider), available: RwLock::new(Vec::new()) }
    }

    /// Check for updates now (this runs `apt-get`, so call it off the async threads).
    /// Returns how many updates were found.
    pub fn refresh(&self, channel: &Channel) -> Result<usize, String> {
        let found = self.provider.check(channel).map_err(|e| e.to_string())?;
        let count = found.updates.len();
        *self.available.write().expect("updates lock poisoned") = found.updates;
        Ok(count)
    }

    /// Everything the endpoint reports, from what is already known (no commands are run
    /// except reading the reboot marker).
    pub fn response(&self) -> UpdatesResponse {
        UpdatesResponse {
            status: self.provider.status().ok(),
            last_check: self.store.last_check(),
            reboot: self.provider.reboot_required().unwrap_or_else(|_| self.store.reboot()),
            available: self.available.read().expect("updates lock poisoned").clone(),
            history: self.store.history(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use abora_update::UpdateStatus;

    struct Canned;
    impl CommandRunner for Canned {
        fn run(&self, _program: &str, _args: &[&str]) -> Result<String, String> {
            Ok("Inst openssl [3.0.13-0ubuntu3.4] (3.0.13-0ubuntu3.5 Ubuntu:24.04/noble-updates [amd64])\n".into())
        }
    }

    #[test]
    fn a_refresh_fills_in_the_response() {
        let store = Arc::new(UpdateStore::in_memory());
        let provider = HostPackageProvider::with_parts(
            store.clone(),
            Box::new(Canned),
            std::path::PathBuf::from("/nonexistent/abora-reboot-required"),
            || "2026-09-21T05:00:00Z".to_owned(),
        );
        let updates = UpdatesState { store, provider: Box::new(provider), available: RwLock::new(Vec::new()) };

        assert!(updates.response().status.is_none(), "nothing claimed before the first check");
        assert_eq!(updates.refresh(&Channel::Stable), Ok(1));

        let r = updates.response();
        assert_eq!(r.last_check.as_deref(), Some("2026-09-21T05:00:00Z"));
        assert_eq!(r.available.len(), 1);
        assert_eq!(r.available[0].component, "openssl");
        assert_eq!(r.status, Some(UpdateStatus::UpdateAvailable { versions: vec!["openssl 3.0.13-0ubuntu3.5".into()] }));
    }

    #[test]
    fn a_failed_refresh_reports_an_error_status_and_keeps_the_old_list() {
        let updates = UpdatesState::disabled();
        assert!(updates.refresh(&Channel::Stable).is_err());
        assert!(matches!(updates.response().status, Some(UpdateStatus::Error { .. })));
        assert!(updates.response().available.is_empty());
    }
}
