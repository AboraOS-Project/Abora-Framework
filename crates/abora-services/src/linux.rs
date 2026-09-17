//! systemd backend for the service registry.
//!
//! Two `systemctl` calls cover everything we need, both read-only and invoked
//! with a hard-coded argument list (no paths, names, or flags come from
//! input):
//!
//! * `systemctl list-units --all --type=service` — load/active/sub state and
//!   the unit description, one line per unit.
//! * `systemctl list-unit-files --type=service` — per-unit file state
//!   (`enabled`, `disabled`, `static`, `masked`, ...) which becomes the
//!   `enabled` hint.
//!
//! Parsing is pure (strings in, values out) so it can be unit-tested with
//! captured real-world output.

#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::process::Command;

use abora_api::ServiceStatus;

use super::{ServiceCollector, ServicesError};

/// systemd unit's `ActiveState`, mapped onto the coarse API state. Unknown
/// values map to `Unknown` rather than guessing.
#[cfg(target_os = "linux")]
fn map_active_state(active: &str) -> abora_api::ServiceState {
    use abora_api::ServiceState;
    match active {
        "active" => ServiceState::Running,
        "inactive" => ServiceState::Stopped,
        "failed" => ServiceState::Failed,
        "activating" => ServiceState::Activating,
        "deactivating" => ServiceState::Deactivating,
        _ => ServiceState::Unknown,
    }
}

/// A unit's `UnitFileState`, turned into the two-valued `enabled` hint.
/// `static`/`indirect`/`generated`/`transient` units are started on demand
/// by dependencies rather than enabled directly, so they report `None`.
#[cfg(target_os = "linux")]
fn file_state_enabled(state: &str) -> Option<bool> {
    match state {
        "enabled" | "enabled-runtime" => Some(true),
        "disabled" | "masked" | "masked-runtime" => Some(false),
        _ => None,
    }
}

/// One `systemctl list-units` row.
#[cfg(target_os = "linux")]
struct RawUnit {
    name: String,
    state: abora_api::ServiceState,
    description: Option<String>,
}

#[cfg(target_os = "linux")]
pub struct SystemdServiceManager;

#[cfg(target_os = "linux")]
impl ServiceCollector for SystemdServiceManager {
    fn collect(&self) -> Result<Vec<ServiceStatus>, ServicesError> {
        let units_output = run_systemctl(&["list-units", "--all", "--type=service"])?;
        let files_output = run_systemctl(&["list-unit-files", "--type=service"])?;

        let units = parse_list_units(&units_output)?;
        let file_states = parse_list_unit_files(&files_output)?;

        let mut services: Vec<ServiceStatus> = units
            .into_iter()
            .map(|u| ServiceStatus {
                enabled: file_states.get(&u.name).copied().flatten(),
                name: u.name,
                state: u.state,
                description: u.description,
            })
            .collect();
        services.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(services)
    }
}

/// Run `systemctl` with the shared read-only flag set and the given
/// positional arguments.
#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) -> Result<String, ServicesError> {
    let output = Command::new("systemctl")
        .args(["--no-legend", "--no-pager", "--no-ask-password", "--plain"])
        .args(args)
        .output()
        .map_err(|e| ServicesError::Systemd(format!("could not execute `systemctl`: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ServicesError::Systemd(format!(
            "`systemctl {}` exited with {}{}",
            args.join(" "),
            output.status,
            if stderr.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", stderr.trim())
            }
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse `systemctl list-units --type=service` output. Lines are
/// `UNIT LOAD ACTIVE SUB DESCRIPTION`; the description may contain spaces.
#[cfg(target_os = "linux")]
fn parse_list_units(text: &str) -> Result<Vec<RawUnit>, ServicesError> {
    let mut units = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("UNIT ") {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 4 {
            continue; // malformed or truncated row
        }
        let name = tokens[0];
        let load = tokens[1];
        let active = tokens[2];
        if !name.ends_with(".service") || load.is_empty() || active.is_empty() {
            continue;
        }
        let description = tokens
            .get(4..)
            .filter(|rest| !rest.is_empty())
            .map(|rest| rest.join(" "));
        units.push(RawUnit {
            name: name.to_owned(),
            state: map_active_state(active),
            description,
        });
    }
    Ok(units)
}

/// Parse `systemctl list-unit-files --type=service` output: `UNIT FILE STATE`.
#[cfg(target_os = "linux")]
fn parse_list_unit_files(text: &str) -> Result<HashMap<String, Option<bool>>, ServicesError> {
    let mut states = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("UNIT FILE") {
            continue;
        }
        let mut it = line.split_whitespace();
        let name = it.next().unwrap_or_default();
        let state = it.next().unwrap_or_default();
        if name.is_empty() || !name.ends_with(".service") || state.is_empty() {
            continue;
        }
        states.insert(name.to_owned(), file_state_enabled(state));
    }
    Ok(states)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use abora_api::ServiceState;

    const LIST_UNITS_SAMPLE: &str = r#"accounts-daemon.service                          loaded    active   running Accounts Service
acpid.service                                    loaded    inactive dead    ACPI event daemon
apparmor.service                                 loaded    active   exited  Load AppArmor profiles
ssh.service                                      loaded    failed   failed  OpenBSD Secure Shell server
snapd.seeded.service                             loaded    active   exited  
broken.service                                   loaded    active   
"#;

    const LIST_UNIT_FILES_SAMPLE: &str = r#"accounts-daemon.service                          enabled         enabled
acpid.service                                    disabled        enabled
apparmor.service                                 enabled         enabled
alsa-utils.service                               masked          enabled
ssh.service                                      enabled         enabled
dbus.service                                     static          -
"#;

    #[test]
    fn list_units_parses_name_state_and_description() {
        let units = parse_list_units(LIST_UNITS_SAMPLE).unwrap();
        assert_eq!(units.len(), 5);

        let accounts = &units[0];
        assert_eq!(accounts.name, "accounts-daemon.service");
        assert_eq!(accounts.state, ServiceState::Running);
        assert_eq!(accounts.description.as_deref(), Some("Accounts Service"));

        let acpi = &units[1];
        assert_eq!(acpi.state, ServiceState::Stopped);

        let apparmor = &units[2];
        assert_eq!(apparmor.state, ServiceState::Running); // active + exited counts as running

        let ssh = &units[3];
        assert_eq!(ssh.state, ServiceState::Failed);

        // Trailing whitespace-only description and truncated rows are tolerated.
        let snapd = &units[4];
        assert_eq!(snapd.description, None);
    }

    #[test]
    fn list_units_preserves_empty_description() {
        let units = parse_list_units("foo.service loaded active running").unwrap();
        assert_eq!(units[0].description, None);
    }

    #[test]
    fn list_unit_files_maps_states_to_enabled_hint() {
        let states = parse_list_unit_files(LIST_UNIT_FILES_SAMPLE).unwrap();
        assert_eq!(states.get("accounts-daemon.service").copied().flatten(), Some(true));
        assert_eq!(states.get("acpid.service").copied().flatten(), Some(false));
        assert_eq!(states.get("alsa-utils.service").copied().flatten(), Some(false)); // masked
        assert_eq!(states.get("dbus.service").copied().flatten(), None); // static
    }
}