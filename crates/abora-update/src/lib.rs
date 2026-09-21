//! Shared update infrastructure for Abora Framework.
//!
//! This crate defines the *contract* for how updates will work across
//! Abora Cloud and Abora Atlas, but deliberately implements **no real
//! updater** yet. See `docs/update.md` for the full design.
//!
//! # Design goals
//!
//! * Cloud and Atlas express "what is available, what changed, when do we
//!   reboot" in one shared vocabulary.
//! * A `UpdateProvider` is the extension point where the actual update
//!   mechanism (deb/rpm/apk host packages, container images, payload
//!   tarballs, ...) plugs in later.
//! * Maintenance windows and channels are first-class concepts so fleet
//!   scheduling logic never has to re-derive them.
//! * All types are serializable so they can travel over the daemon API.

#![forbid(unsafe_code)]

use std::fmt;

use abora_core::Version;

pub mod store;
pub use store::{Opened, StoreError, UpdateStore};

/// Named update channels in order of stability (stable is the default).
///
/// `"stable"`, `"beta"` and `"nightly"` are first-class values while
/// arbitrary channel names remain possible through `Custom`:
///
/// ```toml
/// channel = "stable"
/// channel = { custom = "my-private-feed" }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Beta,
    Nightly,
    #[serde(rename = "custom")]
    Custom(String),
}

impl Default for Channel {
    fn default() -> Self {
        Channel::Stable
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Channel::Stable => write!(f, "stable"),
            Channel::Beta => write!(f, "beta"),
            Channel::Nightly => write!(f, "nightly"),
            Channel::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

/// What to do when an update requires a reboot outside a maintenance window.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RebootPolicy {
    /// Ask (default): never reboot outside a maintenance window.
    Ask,
    /// Reboot immediately after a successful update.
    Always,
    /// Never reboot automatically; wait for an operator.
    Never,
}

impl Default for RebootPolicy {
    fn default() -> Self {
        RebootPolicy::Ask
    }
}

/// Day of week used for maintenance windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

/// Scheduled window in which automatic updates and reboots are permitted.
///
/// Times use local server time and are given as `HH:MM`-style strings.
/// The window is semantically half-open: updates may start at `start`
/// and must finish before `end`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MaintenanceWindow {
    pub days: Vec<Weekday>,
    pub start: String,
    pub end: String,
    /// Upper bound for a single update run inside this window, in minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration_minutes: Option<u32>,
}

/// A concrete available update.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AvailableUpdate {
    pub channel: Channel,
    pub version: Version,
    pub component: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_notes: Option<String>,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksums: Option<Vec<String>>,
}

/// Collection of updates currently available on a channel.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AvailableUpdates {
    pub channel: Channel,
    #[serde(default)]
    pub updates: Vec<AvailableUpdate>,
}

/// Overall update state of the system.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateStatus {
    UpToDate,
    UpdateAvailable { versions: Vec<String> },
    Installing {
        component: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress_percent: Option<u8>,
    },
    Error { message: String },
}

/// Whether a reboot is required to finish applying updates.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RebootStatus {
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_since: Option<String>,
}

impl Default for RebootStatus {
    fn default() -> Self {
        Self {
            required: false,
            reason: None,
            pending_since: None,
        }
    }
}

/// A single entry from the update history log.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UpdateHistoryEntry {
    pub applied_at: String,
    pub from_version: Version,
    pub to_version: Version,
    pub channel: Channel,
    pub component: String,
    pub succeeded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Errors a provider may raise. Deliberately small; providers must keep
/// secrets out of error messages.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    /// The operation is not implemented by this provider yet.
    #[error("not implemented: {0}")]
    NotImplemented(String),
    /// The operation is unsupported for the current channel/system.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The provider is not in a state that allows the operation (e.g. an
    /// update is already in progress).
    #[error("update conflict: {0}")]
    Conflict(String),
    /// I/O or transport failure while talking to the update source.
    #[error("update source failure: {0}")]
    Source(String),
}

/// Abstraction over a concrete update mechanism.
///
/// This is the seam Cloud/Atlas implement later. Framework ships the
/// interface and the shared scheduling/safety logic that sits on top of it.
///
/// # Safety
///
/// Providers must apply updates atomically where possible, verify checksums,
/// and never bypass documented reboot-policy rules.
pub trait UpdateProvider: Send + Sync {
    /// Check for available updates on a channel.
    fn check(&self, channel: &Channel) -> Result<AvailableUpdates, UpdateError>;

    /// Current update status.
    fn status(&self) -> Result<UpdateStatus, UpdateError>;

    /// Whether a reboot is required.
    fn reboot_required(&self) -> Result<RebootStatus, UpdateError>;

    /// Historical update records, newest first.
    fn history(&self) -> Result<Vec<UpdateHistoryEntry>, UpdateError>;

    /// Apply an update. Implementing this is **not** required for the first
    /// milestone; the default refuses until the provider supports it.
    fn apply(&self, _update: &AvailableUpdate) -> Result<(), UpdateError> {
        Err(UpdateError::NotImplemented(
            "apply is not implemented by this provider".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct TomlWrap {
        channel: Channel,
    }

    #[test]
    fn channel_round_trips_via_toml() {
        for (raw, expected) in [
            ("stable", Channel::Stable),
            ("beta", Channel::Beta),
            ("nightly", Channel::Nightly),
            ("channel = { custom = 'edge' }", Channel::Custom("edge".into())),
        ] {
            let text = if raw.starts_with("channel") {
                raw.to_owned()
            } else {
                format!("channel = {raw:?}")
            };
            let parsed: TomlWrap = toml::from_str(&text).unwrap();
            assert_eq!(parsed.channel, expected, "raw: {raw}");
        }
    }

    #[test]
    fn custom_channel_json_round_trip() {
        let c = Channel::Custom("george".into());
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"custom":"george"}"#);
        let back: Channel = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn maintenance_window_default_is_empty() {
        let w = MaintenanceWindow {
            days: vec![Weekday::Saturday],
            start: "02:00".into(),
            end: "04:00".into(),
            max_duration_minutes: None,
        };
        let json = serde_json::to_value(&w).unwrap();
        assert!(json.get("max_duration_minutes").is_none());
    }

    #[test]
    fn provider_default_apply_refuses() {
        struct Noop;
        impl UpdateProvider for Noop {
            fn check(&self, _c: &Channel) -> Result<AvailableUpdates, UpdateError> {
                Err(UpdateError::Unsupported("test".into()))
            }
            fn status(&self) -> Result<UpdateStatus, UpdateError> {
                Ok(UpdateStatus::UpToDate)
            }
            fn reboot_required(&self) -> Result<RebootStatus, UpdateError> {
                Ok(RebootStatus::default())
            }
            fn history(&self) -> Result<Vec<UpdateHistoryEntry>, UpdateError> {
                Ok(vec![])
            }
        }
        let n = Noop;
        let update = AvailableUpdate {
            channel: Channel::Stable,
            version: Version::new(2, 0, 0),
            component: "framework".into(),
            summary: "test".into(),
            release_notes: None,
            size_bytes: 42,
            released_at: None,
            checksums: None,
        };
        assert!(matches!(n.apply(&update), Err(UpdateError::NotImplemented(_))));
    }
}