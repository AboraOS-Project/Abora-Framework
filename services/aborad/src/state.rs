//! Shared state held by the daemon across requests.
//!
//! `config` and `tokens` are behind [`std::sync::RwLock`] so the config
//! reload path (SIGHUP / file watching) can swap in a fresh configuration
//! and token store without replacing the whole [`AppState`] (which requests
//! and the server task hold concurrently).

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use abora_api::{
    ApplyResponse, AvailableUpdate, Channel, ComponentHealth, HealthStatus, ScheduleInfo,
    UpdateHistoryEntry, UpdateStatus, UpdatesResponse,
};
use abora_config::schedule::{local_time, permissions, LocalTime};
use abora_config::Config;
use abora_log::Logger;
use abora_update::apply::{read_regular_file, write_atomic, ApplyRequest, ApplyResult};
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

    pub fn with_updates(
        config: Config,
        logger: Logger,
        tokens: Option<TokenStore>,
        updates: UpdatesState,
    ) -> Arc<Self> {
        Arc::new(Self {
            updates,
            started_at_rfc3339: abora_log::rfc3339_now(),
            config: RwLock::new(config),
            logger,
            tokens: RwLock::new(tokens),
            started_at: Instant::now(),
        })
    }

    /// Health of each subsystem, derived from live state. Add new subsystems here.
    pub fn components_health(&self) -> Vec<ComponentHealth> {
        vec![self.updates.health()]
    }

    /// Seconds since the daemon started.
    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}

/// Why a request to apply updates was not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyRefusal {
    /// Not possible right now (already running, nothing to apply, outside a window).
    Conflict(String),
    /// Could not write the request file.
    Internal(String),
}

/// Update information served by `GET /api/v1/updates`: the provider that checks for
/// updates, the persistent store, the list found by the last check, and the files used to
/// hand an apply request to the root helper (see `docs/apply-design.md`).
pub struct UpdatesState {
    store: Arc<UpdateStore>,
    provider: Box<dyn UpdateProvider>,
    available: RwLock<Vec<AvailableUpdate>>,
    schedule: RwLock<Option<ScheduleInfo>>,
    /// Written by `aborad`, read by the helper.
    apply_request_file: PathBuf,
    /// Written by the helper, read by `aborad`.
    apply_result_file: PathBuf,
    local_time: fn() -> Option<LocalTime>,
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
    fn new(
        store: Arc<UpdateStore>,
        provider: Box<dyn UpdateProvider>,
        apply_request_file: PathBuf,
        apply_result_file: PathBuf,
        local_time: fn() -> Option<LocalTime>,
    ) -> Self {
        Self {
            store,
            provider,
            available: RwLock::new(Vec::new()),
            schedule: RwLock::new(None),
            apply_request_file,
            apply_result_file,
            local_time,
        }
    }

    /// The real thing: host packages (apt) with the given store. `request_file` is where apply
    /// requests are written (next to the state file); results are read from `result_file`.
    pub fn host(store: Arc<UpdateStore>, request_file: PathBuf, result_file: PathBuf) -> Self {
        Self::new(
            store.clone(),
            Box::new(HostPackageProvider::new(store)),
            request_file,
            result_file,
            local_time,
        )
    }

    /// Never runs a command and never touches the disk or `/var/run`. Used by tests.
    #[cfg(test)]
    pub fn disabled() -> Self {
        let dir = std::env::temp_dir().join(format!("abora-disabled-{}", std::process::id()));
        Self::with_runner(Box::new(NoChecks), dir, || None)
    }

    /// Test constructor: a fake command runner, a scratch directory for the apply files, and a
    /// fixed clock.
    #[cfg(test)]
    pub fn with_runner(
        runner: Box<dyn CommandRunner>,
        dir: PathBuf,
        local_time: fn() -> Option<LocalTime>,
    ) -> Self {
        let store = Arc::new(UpdateStore::in_memory());
        let provider = HostPackageProvider::with_parts(
            store.clone(),
            runner,
            PathBuf::from("/nonexistent/abora-reboot-required"),
            abora_log::rfc3339_now,
        );
        Self::new(
            store,
            Box::new(provider),
            dir.join(abora_update::apply::REQUEST_FILE_NAME),
            dir.join("result.json"),
            local_time,
        )
    }

    /// Check for updates now (this runs `apt-get`, so call it off the async threads).
    /// Returns how many updates were found.
    pub fn refresh(&self, channel: &Channel) -> Result<usize, String> {
        let found = self.provider.check(channel).map_err(|e| e.to_string())?;
        let count = found.updates.len();
        *self.available.write().expect("updates lock poisoned") = found.updates;
        Ok(count)
    }

    /// Record what policy permits right now (called by the scheduler).
    pub fn set_schedule(&self, info: ScheduleInfo) {
        *self.schedule.write().expect("updates lock poisoned") = Some(info);
    }

    /// `degraded` when the last update check failed; otherwise `ok` (including before any check).
    pub fn health(&self) -> ComponentHealth {
        let (status, detail) = match self.provider.status() {
            Ok(UpdateStatus::Error { message }) => (HealthStatus::Degraded, Some(message)),
            // Before the first check there is nothing to be unhealthy about.
            Err(e) => (HealthStatus::Ok, Some(e.to_string())),
            Ok(_) => (HealthStatus::Ok, None),
        };
        ComponentHealth {
            name: "updates".to_owned(),
            status,
            detail,
        }
    }

    /// Whether a request is waiting for (or being handled by) the helper.
    fn apply_pending(&self) -> bool {
        std::fs::symlink_metadata(&self.apply_request_file).is_ok()
    }

    /// Everything the endpoint reports, from what is already known (no commands are run
    /// except reading the reboot marker).
    pub fn response(&self) -> UpdatesResponse {
        let status = if self.apply_pending() {
            Some(UpdateStatus::Installing {
                component: "host packages".to_owned(),
                progress_percent: None,
            })
        } else {
            self.provider.status().ok()
        };
        UpdatesResponse {
            status,
            last_check: self.store.last_check(),
            reboot: self
                .provider
                .reboot_required()
                .unwrap_or_else(|_| self.store.reboot()),
            schedule: self.schedule.read().expect("updates lock poisoned").clone(),
            available: self
                .available
                .read()
                .expect("updates lock poisoned")
                .clone(),
            history: self.store.history(),
        }
    }

    /// Whether the last check found anything, and whether an apply is already waiting for the helper.
    /// Used by the scheduler's automatic apply.
    pub fn apply_inputs(&self) -> (bool, bool) {
        let has_available = !self
            .available
            .read()
            .expect("updates lock poisoned")
            .is_empty();
        (has_available, self.apply_pending())
    }

    /// Ask the root helper to apply the packages the last check listed (or, with `dry_run`, say what
    /// would happen). The policy is checked here for a clear early answer; the helper checks it
    /// again itself and is the one that decides.
    pub fn request_apply(
        &self,
        config: &Config,
        caller: &str,
        dry_run: bool,
    ) -> Result<ApplyResponse, ApplyRefusal> {
        let mut packages: Vec<String> = self
            .available
            .read()
            .expect("updates lock poisoned")
            .iter()
            .map(|u| u.component.clone())
            .collect();
        packages.sort();
        packages.dedup();

        let permitted = permissions(config, (self.local_time)()).apply_permitted;
        let block = if self.apply_pending() {
            Some("an update is already being applied")
        } else if packages.is_empty() {
            Some("no updates are available; the last check found nothing")
        } else if !permitted {
            Some(
                "updates may only be applied inside a maintenance window ([maintenance] enabled, and the \
                 local time inside a window)",
            )
        } else {
            None
        };

        if dry_run {
            return Ok(ApplyResponse {
                dry_run: true,
                permitted: block.is_none(),
                reason: block.map(str::to_owned),
                id: None,
                packages,
            });
        }
        if let Some(reason) = block {
            return Err(ApplyRefusal::Conflict(reason.to_owned()));
        }

        let id = new_request_id();
        let request = ApplyRequest {
            id: id.clone(),
            requested_at: abora_log::rfc3339_now(),
            requested_by: caller.to_owned(),
            packages: packages.clone(),
        };
        let json = serde_json::to_vec_pretty(&request).expect("request serializes");
        write_atomic(&self.apply_request_file, &json, 0o600).map_err(|e| {
            ApplyRefusal::Internal(format!("could not write the apply request: {e}"))
        })?;
        Ok(ApplyResponse {
            dry_run: false,
            permitted: true,
            reason: None,
            id: Some(id),
            packages,
        })
    }

    /// Turn the helper's result file into a history entry (once per request id). Returns `true`
    /// when a new result was recorded, so the caller can re-check what is still available.
    pub fn ingest_apply_result(&self, channel: &Channel) -> bool {
        let Ok(bytes) = read_regular_file(&self.apply_result_file, None) else {
            return false;
        };
        let Ok(result) = serde_json::from_slice::<ApplyResult>(&bytes) else {
            return false;
        };
        if !abora_update::apply::valid_request_id(&result.id) && result.id != "invalid" {
            return false;
        }
        let key = format!("apply {}", result.id);
        if self
            .store
            .history()
            .iter()
            .any(|h| h.note.as_deref().is_some_and(|n| n.starts_with(&key)))
        {
            return false;
        }
        let mut note = format!("{key}: {} package(s)", result.packages.len());
        if let Some(extra) = &result.note {
            note.push_str("; ");
            note.push_str(extra);
        }
        if result.reboot_started {
            note.push_str("; reboot started");
        }
        let zero = abora_core::Version::new(0, 0, 0);
        let entry = UpdateHistoryEntry {
            applied_at: result.finished_at.clone(),
            // A run covers many packages, so there is no single from/to version; the packages are counted
            // in the note.
            from_version: zero.clone(),
            to_version: zero,
            channel: channel.clone(),
            component: "host packages".to_owned(),
            succeeded: result.succeeded,
            note: Some(note),
        };
        self.store.record_update(entry).is_ok()
    }
}

/// A unique, non-secret id for an apply request.
fn new_request_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = RandomState::new().build_hasher();
    hasher.write(abora_log::rfc3339_now().as_bytes());
    format!("apply-{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use abora_update::apply::{validate_request, write_atomic, ApplyRequest, REQUEST_FILE_NAME};
    use abora_update::{MaintenanceWindow, Weekday};

    struct Canned;
    impl CommandRunner for Canned {
        fn run(&self, _program: &str, _args: &[&str]) -> Result<String, String> {
            Ok("Inst openssl [3.0.13-0ubuntu3.4] (3.0.13-0ubuntu3.5 Ubuntu:24.04/noble-updates [amd64])\n".into())
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("abora-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sunday_3am() -> Option<LocalTime> {
        Some(LocalTime {
            weekday: Weekday::Sunday,
            minutes: 3 * 60,
        })
    }

    fn window_config() -> Config {
        let mut c = Config::default();
        c.maintenance.enabled = true;
        c.maintenance.windows = vec![MaintenanceWindow {
            days: vec![Weekday::Sunday],
            start: "02:00".into(),
            end: "04:00".into(),
            max_duration_minutes: None,
        }];
        c
    }

    #[test]
    fn a_refresh_fills_in_the_response() {
        let dir = scratch("refresh");
        let updates = UpdatesState::with_runner(Box::new(Canned), dir.clone(), || None);

        assert!(
            updates.response().status.is_none(),
            "nothing claimed before the first check"
        );
        assert_eq!(updates.refresh(&Channel::Stable), Ok(1));

        let r = updates.response();
        assert!(r.last_check.is_some());
        assert_eq!(r.available.len(), 1);
        assert_eq!(r.available[0].component, "openssl");
        assert_eq!(
            r.status,
            Some(UpdateStatus::UpdateAvailable {
                versions: vec!["openssl 3.0.13-0ubuntu3.5".into()]
            })
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_refresh_reports_an_error_status_and_keeps_the_old_list() {
        let updates = UpdatesState::disabled();
        assert!(updates.refresh(&Channel::Stable).is_err());
        assert!(matches!(
            updates.response().status,
            Some(UpdateStatus::Error { .. })
        ));
        assert!(updates.response().available.is_empty());
    }

    #[test]
    fn health_is_ok_before_a_check_and_degraded_after_a_failed_one() {
        let updates = UpdatesState::disabled();
        assert_eq!(updates.health().status, HealthStatus::Ok);
        assert!(updates.refresh(&Channel::Stable).is_err());
        let health = updates.health();
        assert_eq!(health.status, HealthStatus::Degraded);
        assert!(health.detail.is_some());
    }

    #[test]
    fn apply_needs_something_to_apply() {
        let dir = scratch("nothing");
        let updates = UpdatesState::with_runner(Box::new(Canned), dir.clone(), sunday_3am);
        let dry = updates
            .request_apply(&window_config(), "ops", true)
            .unwrap();
        assert!(!dry.permitted && dry.reason.unwrap().contains("no updates"));
        assert!(matches!(
            updates.request_apply(&window_config(), "ops", false),
            Err(ApplyRefusal::Conflict(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_request_names_only_packages_and_passes_the_helpers_own_validation() {
        let dir = scratch("request");
        let updates = UpdatesState::with_runner(Box::new(Canned), dir.clone(), sunday_3am);
        updates.refresh(&Channel::Stable).unwrap();
        let response = updates
            .request_apply(&window_config(), "ops", false)
            .unwrap();
        let request: ApplyRequest =
            serde_json::from_slice(&std::fs::read(dir.join(REQUEST_FILE_NAME)).unwrap()).unwrap();
        assert_eq!(Some(request.id.clone()), response.id);
        assert_eq!(request.requested_by, "ops");
        assert_eq!(request.packages, ["openssl"]);
        assert!(validate_request(&request).is_ok());
        assert!(matches!(
            updates.request_apply(&window_config(), "ops", false),
            Err(ApplyRefusal::Conflict(m)) if m.contains("already")
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_helper_result_becomes_one_history_entry() {
        let dir = scratch("ingest");
        let updates = UpdatesState::with_runner(Box::new(Canned), dir.clone(), || None);
        let result = abora_update::apply::ApplyResult {
            id: "apply-00aa11bb22cc33dd".into(),
            started_at: "2026-09-21T03:00:00Z".into(),
            finished_at: "2026-09-21T03:02:00Z".into(),
            succeeded: true,
            packages: vec!["openssl".into(), "libc6".into()],
            note: Some("a reboot is needed but policy does not allow one now".into()),
            reboot_started: false,
        };
        write_atomic(
            &dir.join("result.json"),
            &serde_json::to_vec(&result).unwrap(),
            0o644,
        )
        .unwrap();

        assert!(
            updates.ingest_apply_result(&Channel::Stable),
            "first time it is recorded"
        );
        assert!(
            !updates.ingest_apply_result(&Channel::Stable),
            "the same id is never recorded twice"
        );

        let history = updates.response().history;
        assert_eq!(history.len(), 1);
        assert!(history[0].succeeded);
        assert_eq!(history[0].applied_at, "2026-09-21T03:02:00Z");
        let note = history[0].note.as_deref().unwrap();
        assert!(
            note.starts_with("apply apply-00aa11bb22cc33dd: 2 package(s)"),
            "{note}"
        );
        assert!(note.contains("policy does not allow"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn garbage_in_the_result_file_is_ignored() {
        let dir = scratch("garbage");
        let updates = UpdatesState::with_runner(Box::new(Canned), dir.clone(), || None);
        std::fs::write(dir.join("result.json"), b"{ nope").unwrap();
        assert!(!updates.ingest_apply_result(&Channel::Stable));
        assert!(updates.response().history.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
