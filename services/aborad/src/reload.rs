//! Runtime configuration reload for `aborad`.
//!
//! Config can be refreshed without a restart via `SIGHUP` or by an
//! automatic file watch. A reload:
//!
//! * re-parses and re-validates the configuration file;
//! * applies the log threshold immediately (`Logger::set_level`);
//! * swaps the bearer-token store when `[security] token_file` changes or a
//!   new file appears *that is valid* (bad token files keep the previous
//!   store — the daemon never locks itself out on reload);
//! * installs the new `Config` for handlers;
//! * warns about fields that need a restart (`[api] base_path`,
//!   `[remote] listen_addr`, log format).
//!
//! Unlike startup, a reload failure never brings the daemon down: the old
//! configuration keeps serving and the problem is logged.

use std::path::{Path, PathBuf};

use abora_config::Config;
use abora_log::{info, warn};

use crate::state::SharedState;
use crate::tokens::TokenStore;

/// Where the running configuration came from — used to re-read it on reload.
#[derive(Debug, Clone)]
pub enum ReloadSource {
    /// A concrete file we (re)load from.
    File(PathBuf),
    /// Compiled-in defaults; there is no file to reload.
    Builtin,
}

impl ReloadSource {
    /// The file to watch and reread, if there is one.
    pub fn file_path(&self) -> Option<&Path> {
        match self {
            ReloadSource::File(path) => Some(path),
            ReloadSource::Builtin => None,
        }
    }
}

/// Re-read and apply the configuration and token store. Never panics; logs
/// the outcome. Returns the new log threshold so callers can react.
pub fn reload(state: &SharedState, source: &ReloadSource) {
    match source {
        ReloadSource::Builtin => {
            info!(
                state.logger,
                "config reload requested but no configuration file exists (compiled-in defaults in use)"
            );
        }
        ReloadSource::File(path) => {
            let fresh = match Config::load(path) {
                Ok(cfg) => cfg,
                Err(e) => {
                    warn!(
                        state.logger,
                        "config reload from {} failed: {e}; keeping the previous configuration",
                        path.display()
                    );
                    return;
                }
            };
            apply_reload(state, fresh);
        }
    }
}

/// Install a freshly loaded configuration, logging what changed.
fn apply_reload(state: &SharedState, fresh: Config) {
    let logger = &state.logger;
    let previous = state.config.read().expect("config lock poisoned");

    // Live-applicable fields.
    let level_changed = previous.logging.level != fresh.logging.level;
    if level_changed {
        logger.set_level(fresh.logging.level);
    }

    // Fields that are fixed when the router/listener is built.
    let mut restart_required = Vec::new();
    if previous.remote.listen_addr != fresh.remote.listen_addr {
        restart_required.push(format!(
            "[remote] listen_addr {} -> {} (restart required)",
            previous.remote.listen_addr, fresh.remote.listen_addr
        ));
    }
    if previous.api.base_path != fresh.api.base_path {
        restart_required.push(format!(
            "[api] base_path {} -> {} (restart required)",
            previous.api.base_path, fresh.api.base_path
        ));
    }
    if previous.logging.format != fresh.logging.format {
        restart_required.push("log format change requires a restart".to_owned());
    }

    let changes = changed_settings(&previous, &fresh);

    // Swap the new config in. The old `RwLock` guard drops here.
    drop(previous);
    *state.config.write().expect("config lock poisoned") = fresh;

    refresh_tokens(state, level_changed);

    if changes.is_empty() && restart_required.is_empty() {
        info!(logger, "config reload applied; configuration unchanged");
        return;
    }
    for change in &changes {
        info!(logger, "config reload: {change}");
    }
    for note in &restart_required {
        warn!(logger, "config reload: {note}");
    }
    let new_level = state
        .config
        .read()
        .expect("config lock poisoned")
        .logging
        .level;
    info!(
        logger,
        "config reload applied (log level {}{})",
        new_level,
        if level_changed { " changed" } else { " unchanged" }
    );
}

/// Refresh the token store if `[security] token_file` is set. A valid store
/// replaces the previous one; an invalid one is logged and keeps the old
/// store so the daemon cannot lose its own credentials on a broken file.
fn refresh_tokens(state: &SharedState, _level_changed: bool) {
    let (token_file, previous_not_empty) = {
        let config = state.config.read().expect("config lock poisoned");
        let tokens = state.tokens.read().expect("tokens lock poisoned");
        let previous_not_empty = tokens
            .as_ref()
            .map(|store| !store.is_empty())
            .unwrap_or(false);
        (config.security.token_file.clone(), previous_not_empty)
    };

    let Some(path) = token_file else {
        // No token file configured in the new state.
        let tokens = state.tokens.read().expect("tokens lock poisoned");
        if tokens.is_some() {
            drop(tokens);
            *state.tokens.write().expect("tokens lock poisoned") = None;
            info!(state.logger, "config reload: token authentication disabled (no token_file)");
        }
        return;
    };

    match TokenStore::load(&path) {
        Ok(store) => {
            let size = store.len();
            *state.tokens.write().expect("tokens lock poisoned") = Some(store);
            let note = if size == 0 {
                "token file contains no tokens; all requests will be rejected with 401"
                    .to_owned()
            } else if previous_not_empty {
                format!("{size} token(s) reloaded from {}", path.display())
            } else {
                format!("token authentication enabled ({size} token(s) from {})", path.display())
            };
            info!(state.logger, "config reload: {note}");
        }
        Err(e) => {
            warn!(
                state.logger,
                "config reload: token file {} could not be loaded ({e}); keeping the previous token store",
                path.display()
            );
        }
    }
}

/// Human-readable list of settings that differ between old and new config.
fn changed_settings(previous: &Config, fresh: &Config) -> Vec<String> {
    let mut changes = Vec::new();

    if previous.system.hostname != fresh.system.hostname {
        changes.push(format!("[system] hostname = {}", fresh.system.hostname.as_deref().unwrap_or("-")));
    }
    if previous.system.description != fresh.system.description {
        changes.push("system description changed".to_owned());
    }
    if previous.updates.channel != fresh.updates.channel {
        changes.push(format!("[updates] channel = {}", fresh.updates.channel));
    }
    if previous.updates.automatic != fresh.updates.automatic {
        changes.push(format!("[updates] automatic = {}", fresh.updates.automatic));
    }
    if previous.updates.check_interval != fresh.updates.check_interval {
        changes.push(format!("[updates] check_interval = {}", fresh.updates.check_interval));
    }
    if previous.updates.reboot_policy != fresh.updates.reboot_policy {
        changes.push(format!("[updates] reboot_policy = {:?}", fresh.updates.reboot_policy));
    }
    if previous.maintenance.enabled != fresh.maintenance.enabled {
        changes.push(format!("[maintenance] enabled = {}", fresh.maintenance.enabled));
    }
    if previous.maintenance.windows != fresh.maintenance.windows {
        changes.push(format!(
            "[maintenance] {} window(s)",
            fresh.maintenance.windows.len()
        ));
    }
    if previous.security.require_authentication != fresh.security.require_authentication {
        changes.push(format!(
            "[security] require_authentication = {}",
            fresh.security.require_authentication
        ));
    }
    if previous.security.token_file != fresh.security.token_file {
        changes.push(format!(
            "[security] token_file = {}",
            fresh.security.token_file.as_deref().map(|p| p.display().to_string()).unwrap_or_else(|| "unset".to_owned())
        ));
    }
    if previous.logging.level != fresh.logging.level {
        changes.push(format!("[logging] level = {}", fresh.logging.level));
    }
    if previous.api.max_payload_bytes != fresh.api.max_payload_bytes {
        changes.push(format!(
            "[api] max_payload_bytes = {}",
            fresh.api.max_payload_bytes
        ));
    }
    if previous.api.request_timeout_secs != fresh.api.request_timeout_secs {
        changes.push(format!(
            "[api] request_timeout_secs = {}",
            fresh.api.request_timeout_secs
        ));
    }

    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::TokenStore;
    use abora_log::{Format, Level};

    fn test_state_with(config: Config) -> SharedState {
        let logger = abora_log::Logger::new(Level::Trace, Format::Json);
        crate::state::AppState::new(config, logger, None)
    }

    fn store(secret: &str) -> TokenStore {
        let hash = crate::tokens::sha256_hex(secret);
        TokenStore::from_str(&format!(
            "[[tokens]]\nname = \"t\"\npermissions = [\"read_all\"]\nsecret_hash = \"{hash}\"\n"
        ))
        .unwrap()
    }

    #[test]
    fn changed_settings_reports_modified_fields() {
        let previous = Config::default();
        let mut fresh = Config::default();
        fresh.system.hostname = Some("node-7".to_owned());
        fresh.security.require_authentication = false;
        fresh.logging.level = Level::Debug;

        let changes = changed_settings(&previous, &fresh);
        assert!(changes.iter().any(|c| c.contains("hostname = node-7")));
        assert!(
            changes
                .iter()
                .any(|c| c.contains("require_authentication = false"))
        );
        assert!(changes.iter().any(|c| c.contains("log level") || c.contains("[logging] level = debug")));
        assert!(!changes.iter().any(|c| c.contains("listen_addr")));
    }

    #[test]
    fn reload_from_missing_file_keeps_previous_config() {
        let state = test_state_with(Config::default());
        let missing = tempfile_path("abora-reload-missing.toml");
        if missing.exists() {
            let _ = std::fs::remove_file(&missing);
        }
        reload(&state, &ReloadSource::File(missing.clone()));
        // Still the compiled-in defaults (hostname None).
        assert_eq!(
            state.config.read().unwrap().system.hostname.as_deref(),
            None
        );
    }

    #[test]
    fn reload_applies_new_config_and_log_level() {
        let state = test_state_with(Config::default());
        let path = tempfile_path("abora-reload-ok.toml");
        std::fs::write(
            &path,
            "[system]\nhostname = \"reloaded\"\n\n[logging]\nlevel = \"warn\"\n",
        )
        .unwrap();

        reload(&state, &ReloadSource::File(path.clone()));
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            state.config.read().unwrap().system.hostname.as_deref(),
            Some("reloaded")
        );
        assert_eq!(state.logger.level(), Level::Warn);
    }

    #[test]
    fn reload_swaps_token_store() {
        use abora_log::Level as Lv;
        let logger = abora_log::Logger::new(Lv::Off, Format::Json);
        let state = crate::state::AppState::new(Config::default(), logger, None);

        let auth = tempfile_path("abora-reload-auth.toml");
        let hash = crate::tokens::sha256_hex("op-secret");
        std::fs::write(
            &auth,
            format!(
                "[[tokens]]\nname = \"op\"\npermissions = [\"read_all\"]\nsecret_hash = \"{hash}\"\n"
            ),
        )
        .unwrap();
        let config_file = tempfile_path("abora-reload-with-auth.toml");
        std::fs::write(
            &config_file,
            format!("[security]\ntoken_file = \"{}\"\n", auth.display()),
        )
        .unwrap();

        reload(&state, &ReloadSource::File(config_file.clone()));
        let _ = std::fs::remove_file(&config_file);
        let _ = std::fs::remove_file(&auth);

        let tokens = state.tokens.read().unwrap();
        assert_eq!(
            tokens.as_ref().map(|t| t.len()),
            Some(1),
            "token store should have been loaded from the new config"
        );
    }

    #[test]
    fn refresh_tokens_keeps_previous_store_on_bad_file() {
        let logger = abora_log::Logger::new(Level::Off, Format::Json);
        let state = crate::state::AppState::new(Config::default(), logger, Some(store("keep-me")));
        {
            let mut config = state.config.write().unwrap();
            config.security.token_file = None; // empty file path -> skipped, store kept? no: unset disables
        }
        // With token_file unset, refresh disables auth.
        refresh_tokens(&state, false);
        assert!(state.tokens.read().unwrap().is_none());

        // Now point at a corrupt file and ensure the previous store survives.
        let bad = tempfile_path("abora-reload-bad.toml");
        std::fs::write(&bad, "[[tokens]]\nnot a valid store\n").unwrap();
        {
            let mut config = state.config.write().unwrap();
            config.security.token_file = Some(bad.clone());
        }
        *state.tokens.write().unwrap() = Some(store("keep-me"));
        refresh_tokens(&state, false);
        let _ = std::fs::remove_file(&bad);
        let tokens = state.tokens.read().unwrap();
        assert!(tokens.as_ref().is_some(), "previous store must survive a bad file");
    }

    fn tempfile_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("abora-reload-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }
}