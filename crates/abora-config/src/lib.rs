//! Strongly typed, extensible configuration for Abora Framework.
//!
//! # Why TOML
//!
//! TOML is human-readable, widely understood by operators, and fully
//! lossless with `serde`. It gives us strongly typed configuration with
//! validation without writing a parser.
//!
//! # Extending for Cloud / Atlas
//!
//! Framework owns the sections documented below (`system`, `updates`,
//! `maintenance`, `remote`, `security`, `logging`, `api`, `features`).
//! Unknown *top-level* tables are preserved untouched and exposed through
//! [`Config::extensions`], so Cloud and Atlas can add their own sections
//! (e.g. `[cloud.*]`, `[atlas.*]`) **without modifying Framework source**:
//!
//! ```toml
//! [cloud.telemetry]
//! enabled = true
//! ```
//!
//! ```rust,ignore
//! let cfg = Config::load("/etc/abora/abora.toml")?;
//! let telemetry: Telemetry = cfg
//!     .typed_extension("cloud.telemetry")
//!     .unwrap()  // Option<Telemetry>
//!     .unwrap_or_default();
//! ```
//!
//! Cloud/Atlas validate their own sections; Framework validates its own and
//! never rejects configuration it does not understand.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use abora_core::DEFAULT_LISTEN_ADDR;
use abora_log::{Format, Level};
use abora_update::{Channel, MaintenanceWindow, RebootPolicy};

pub mod schedule;

pub use abora_log::{Format as LogFormat, Level as LogLevel};

/// The full framework configuration. All values have sane, secure defaults.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Config {
    #[serde(default)]
    pub system: SystemConfig,
    #[serde(default)]
    pub updates: UpdatesConfig,
    #[serde(default)]
    pub maintenance: MaintenanceConfig,
    #[serde(default)]
    pub remote: RemoteConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub api: ApiConfig,
    #[serde(default)]
    pub features: FeaturesConfig,
    /// Unknown top-level tables, preserved for Cloud/Atlas extensions.
    #[serde(flatten)]
    extensions: toml::Table,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            system: SystemConfig::default(),
            updates: UpdatesConfig::default(),
            maintenance: MaintenanceConfig::default(),
            remote: RemoteConfig::default(),
            security: SecurityConfig::default(),
            logging: LoggingConfig::default(),
            api: ApiConfig::default(),
            features: FeaturesConfig::default(),
            extensions: toml::Table::new(),
        }
    }
}

impl Config {
    /// Load, parse and validate a configuration file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        Self::from_str(&raw)
    }

    /// Parse and validate configuration from a TOML string.
    pub fn from_str(raw: &str) -> Result<Self, ConfigError> {
        let cfg: Self = toml::from_str(raw).map_err(ConfigError::Toml)?;
        cfg.validate()
            .map_err(|errors| ConfigError::Invalid(errors.join("\n")))?;
        Ok(cfg)
    }

    /// Parse and validate configuration from raw bytes.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, ConfigError> {
        let text = std::str::from_utf8(bytes).map_err(|e| ConfigError::NotUtf8 { source: e })?;
        Self::from_str(text)
    }

    /// Render the current configuration (including defaults) as TOML.
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Access preserved extension tables (Cloud/Atlas sections).
    pub fn extensions(&self) -> &toml::Table {
        &self.extensions
    }

    /// Access a single extension table by dotted name, e.g. `"cloud.telemetry"`.
    pub fn extension(&self, name: &str) -> Option<&toml::Value> {
        let mut current: &toml::Table = &self.extensions;
        let mut parts = name.split('.');
        let last = parts.next_back()?;
        for part in parts {
            match current.get(part) {
                Some(toml::Value::Table(next)) => current = next,
                _ => return None,
            }
        }
        current.get(last)
    }

    /// Deserialize an extension into a caller-defined typed struct.
    pub fn typed_extension<'de, T: serde::Deserialize<'de>>(
        &'de self,
        name: &str,
    ) -> Result<Option<T>, toml::de::Error> {
        match self.extension(name) {
            None => Ok(None),
            Some(value) => value.clone().try_into().map(Some),
        }
    }

    /// Human-readable summary (used by the CLI and logs). No secrets.
    pub fn summary(&self) -> Vec<String> {
        vec![
            format!("hostname override: {}", self.system.hostname.as_deref().unwrap_or("-")),
            format!("update channel: {}", self.updates.channel),
            format!("automatic updates: {}", self.updates.automatic),
            format!("maintenance enabled: {}", self.maintenance.enabled),
            format!("remote management enabled: {}", self.remote.enabled),
            format!("auth required: {}", self.security.require_authentication),
            format!("listen: {} (loopback-only for now)", self.remote.listen_addr),
            format!("log level: {}, format: {}", self.logging.level, self.logging.format),
            format!("api base path: {}", self.api.base_path),
        ]
    }

    /// Validate the configuration, returning every problem found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors: Vec<String> = Vec::new();

        if let Some(hostname) = &self.system.hostname {
            let h = hostname.trim();
            if h.is_empty() {
                errors.push("[system] hostname must not be empty".to_owned());
            } else if h.chars().any(|c| c.is_whitespace()) {
                errors.push(format!("[system] hostname `{hostname}` must not contain whitespace"));
            }
        }

        if !self.updates.state_file.is_absolute() {
            errors.push(format!(
                "[updates] state_file `{}` must be an absolute path",
                self.updates.state_file.display()
            ));
        }
        if let Err(e) = parse_duration(&self.updates.check_interval) {
            errors.push(format!(
                "[updates] check_interval `{}` is not a valid duration: {e}",
                self.updates.check_interval
            ));
        }

        validate_windows(&self.maintenance.windows, &mut errors);

        if self.remote.listen_addr.parse::<SocketAddr>().is_err() {
            errors.push(format!(
                "[remote] listen_addr `{}` is not a valid socket address",
                self.remote.listen_addr
            ));
        }

        let base_path = self.api.base_path.trim();
        if !base_path.starts_with('/') || base_path.len() < 2 {
            errors.push(format!(
                "[api] base_path `{}` must start with `/` and be non-trivial",
                self.api.base_path
            ));
        } else if self.api.base_path != base_path {
            errors.push("[api] base_path must not contain leading/trailing whitespace".to_owned());
        }

        if self.api.max_payload_bytes == 0 {
            errors.push("[api] max_payload_bytes must be greater than zero".to_owned());
        }
        if self.api.request_timeout_secs == 0 {
            errors.push("[api] request_timeout_secs must be greater than zero".to_owned());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

fn validate_windows(windows: &[MaintenanceWindow], errors: &mut Vec<String>) {
    for (i, window) in windows.iter().enumerate() {
        if window.days.is_empty() {
            errors.push(format!(
                "[maintenance] window #{i} must define at least one day"
            ));
        }
        let start = match parse_clock(&window.start) {
            Ok(m) => m,
            Err(e) => {
                errors.push(format!(
                    "[maintenance] window #{i} start `{}` is not HH:MM: {e}",
                    window.start
                ));
                continue;
            }
        };
        let end = match parse_clock(&window.end) {
            Ok(m) => m,
            Err(e) => {
                errors.push(format!(
                    "[maintenance] window #{i} end `{}` is not HH:MM: {e}",
                    window.end
                ));
                continue;
            }
        };
        if end <= start {
            errors.push(format!(
                "[maintenance] window #{i}: end ({}) must be after start ({})",
                window.end, window.start
            ));
        }
        if window.max_duration_minutes == Some(0) {
            errors.push(format!(
                "[maintenance] window #{i}: max_duration_minutes must be greater than zero"
            ));
        }
    }
}

/// System identity.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SystemConfig {
    /// Optional hostname override shown by the API. Empty = use OS hostname.
    pub hostname: Option<String>,
    /// Free-form description surfaced by `GET /api/v1/system`.
    pub description: Option<String>,
}

impl Default for SystemConfig {
    fn default() -> Self {
        Self {
            hostname: None,
            description: None,
        }
    }
}

/// Automatic update behaviour.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdatesConfig {
    pub channel: Channel,
    pub automatic: bool,
    /// How often to poll for updates, e.g. `"6h"`, `"30m"`, `"1d"`.
    pub check_interval: String,
    pub reboot_policy: RebootPolicy,
    /// Where update history and state are stored (absolute path). Read at startup; changing it needs a restart.
    pub state_file: PathBuf,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self {
            channel: Channel::default(),
            automatic: true,
            check_interval: "6h".to_owned(),
            reboot_policy: RebootPolicy::default(),
            state_file: PathBuf::from("/var/lib/abora/updates.json"),
        }
    }
}

/// Scheduled maintenance windows during which automatic work may happen.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MaintenanceConfig {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<MaintenanceWindow>,
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            windows: Vec::new(),
        }
    }
}

/// Remote management settings.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteConfig {
    /// Reserved for wire-level remote management. **Not supported yet**:
    /// the daemon refuses to bind a non-loopback address until auth is
    /// implemented. See `docs/security.md`.
    pub enabled: bool,
    pub listen_addr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advertise_hostname: Option<String>,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_addr: DEFAULT_LISTEN_ADDR.to_owned(),
            advertise_hostname: None,
        }
    }
}

/// Security settings.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityConfig {
    /// Require a valid bearer token for every API operation.
    ///
    /// When `true` and a token store (`token_file`) is configured, tokens
    /// are enforced for **all** clients (loopback included). When `true`
    /// but no token store exists, loopback clients remain trusted as a
    /// preview fallback and non-loopback clients are refused; the daemon
    /// logs a loud warning. Non-loopback clients are always refused until
    /// authenticated remote management is implemented.
    pub require_authentication: bool,
    /// Path to the bearer-token store. Tokens are stored as SHA-256 hashes
    /// (never plaintext); generate them with `abora auth generate-token`.
    /// The file should be root-owned with mode `0600`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_file: Option<PathBuf>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            require_authentication: true,
            token_file: None,
        }
    }
}

/// Logging settings.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    pub level: Level,
    pub format: Format,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: Level::default(),
            format: Format::default(),
        }
    }
}

/// HTTP API runtime limits.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiConfig {
    /// Path prefix for the current API version, e.g. `/api/v1`.
    pub base_path: String,
    pub max_payload_bytes: usize,
    pub request_timeout_secs: u64,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            base_path: abora_core::API_PATH_PREFIX.to_owned(),
            max_payload_bytes: 1024 * 1024,
            request_timeout_secs: 30,
        }
    }
}

/// Dynamic feature flags; usable by Framework, Cloud and Atlas alike.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FeaturesConfig {
    #[serde(flatten)]
    flags: BTreeMap<String, bool>,
}

impl FeaturesConfig {
    pub fn flag(&self, name: &str) -> bool {
        self.flags.get(name).copied().unwrap_or(false)
    }

    pub fn set_flag(&mut self, name: impl Into<String>, value: bool) {
        self.flags.insert(name.into(), value);
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.flags.keys().map(String::as_str)
    }
}

/// Errors produced while loading or validating configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read configuration file `{path}`: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("could not parse TOML: {_0}")]
    Toml(#[from] toml::de::Error),
    #[error("configuration is not UTF-8: {source}")]
    NotUtf8 {
        source: std::str::Utf8Error,
    },
    #[error("configuration is invalid:\n{0}")]
    Invalid(String),
}

/// Parse a `HH:MM` clock time into minutes-since-midnight.
pub(crate) fn parse_clock(value: &str) -> Result<u32, String> {
    let (hour, minute) = value
        .split_once(':')
        .ok_or_else(|| "expected HH:MM format".to_owned())?;
    if hour.len() != 2 || minute.len() != 2 {
        return Err("expected two-digit hours and minutes".to_owned());
    }
    if !hour.bytes().all(|b| b.is_ascii_digit()) || !minute.bytes().all(|b| b.is_ascii_digit()) {
        return Err("expected digits".to_owned());
    }
    let hour: u32 = hour.parse().map_err(|_| "invalid hour".to_owned())?;
    let minute: u32 = minute.parse().map_err(|_| "invalid minute".to_owned())?;
    if hour > 23 {
        return Err("hour out of range".to_owned());
    }
    if minute > 59 {
        return Err("minute out of range".to_owned());
    }
    Ok(hour * 60 + minute)
}

/// Parse a duration such as `30m`, `6h` or `1d` into seconds.
pub(crate) fn parse_duration(value: &str) -> Result<u64, String> {
    let trimmed = value.trim();
    let value = trimmed;
    let split_at = value
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| "missing unit (s, m, h, d)".to_owned())?;
    let number: u64 = value[..split_at]
        .parse()
        .map_err(|_| "invalid number".to_owned())?;
    let unit = &value[split_at..];
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 60 * 60 * 24,
        other => return Err(format!("unknown unit `{other}` (expected s, m, h, d)")),
    };
    if number == 0 {
        return Err("duration must be greater than zero".to_owned());
    }
    Ok(number * mult)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct TelemetryExt {
        enabled: bool,
    }

    const VALID: &str = r#"
[system]
hostname = "node-01"

[updates]
channel = "beta"
automatic = false
check_interval = "12h"

[maintenance]
enabled = true
windows = [{ days = ["saturday"], start = "02:00", end = "04:00" }]

[remote]
listen_addr = "127.0.0.1:8000"

[security]
require_authentication = false

[features]
experimental_x = true

[cloud.telemetry]
enabled = true
"#;

    #[test]
    fn relative_state_file_is_rejected() {
        let mut cfg = Config::default();
        cfg.updates.state_file = PathBuf::from("updates.json");
        let errors = cfg.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("state_file")), "got: {errors:?}");
    }

    #[test]
    fn parses_a_documented_configuration() {
        let cfg = Config::from_str(VALID).expect("valid config should parse");
        assert_eq!(cfg.system.hostname.as_deref(), Some("node-01"));
        assert_eq!(cfg.updates.channel, Channel::Beta);
        assert!(!cfg.updates.automatic);
        assert_eq!(cfg.updates.check_interval, "12h");
        assert_eq!(cfg.maintenance.windows.len(), 1);
        assert!(!cfg.security.require_authentication);
        assert!(cfg.features.flag("experimental_x"));
        assert!(!cfg.features.flag("missing"));
    }

    #[test]
    fn preserves_unknown_top_level_sections() {
        let cfg = Config::from_str(VALID).unwrap();
        let telemetry = cfg.typed_extension::<TelemetryExt>("cloud.telemetry").unwrap();
        assert_eq!(telemetry, Some(TelemetryExt { enabled: true }));
        assert!(cfg.extension("cloud.telemetry").is_some());
        assert_eq!(cfg.extensions().len(), 1);
    }

    #[test]
    fn defaults_are_secure_and_sensible() {
        let cfg = Config::default();
        assert_eq!(cfg.remote.listen_addr, "127.0.0.1:7360");
        assert!(!cfg.remote.enabled);
        assert!(cfg.security.require_authentication);
        assert!(cfg.updates.automatic);
        assert_eq!(cfg.updates.channel, Channel::Stable);
        assert_eq!(cfg.api.base_path, "/api/v1");
    }

    #[test]
    fn partial_config_overlays_defaults() {
        let cfg = Config::from_str("[updates]\nchannel = \"nightly\"\n").unwrap();
        assert_eq!(cfg.updates.channel, Channel::Nightly);
        assert!(cfg.updates.automatic); // not overridden -> default
        assert_eq!(cfg.system.hostname, None);
    }

    #[test]
    fn rejects_unknown_keys_in_framework_sections() {
        let err = Config::from_str("[system]\nhostnaem = \"typo\"\n").unwrap_err();
        assert!(err.to_string().contains("hostnaem"), "got: {err}");
    }

    #[test]
    fn validation_reports_all_problems() {
        let raw = r#"
[updates]
check_interval = "zz"

[maintenance]
windows = [{ days = [], start = "25:00", end = "02:00" }]

[remote]
listen_addr = "not-an-address"

[api]
base_path = "no-slash"
max_payload_bytes = 0
"#;
        // This configuration is syntactically valid TOML but semantically
        // invalid; `from_str` rejects it during validation.
        let err = Config::from_str(raw).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("check_interval"), "got: {text}");
        assert!(text.contains("window #0"), "got: {text}");
        assert!(text.contains("listen_addr"), "got: {text}");
        assert!(text.contains("base_path"), "got: {text}");
        assert!(text.contains("max_payload_bytes"), "got: {text}");
    }

    #[test]
    fn default_toml_round_trips() {
        let text = Config::default().to_toml().unwrap();
        let back = Config::from_str(&text).unwrap();
        assert_eq!(back, Config::default());
    }

    #[test]
    fn parse_clock_and_duration() {
        assert_eq!(parse_clock("00:00").unwrap(), 0);
        assert_eq!(parse_clock("23:59").unwrap(), 1439);
        assert!(parse_clock("24:00").is_err());
        assert!(parse_clock("2:00").is_err());
        assert!(parse_clock("02:2").is_err());

        assert_eq!(parse_duration("30s").unwrap(), 30);
        assert_eq!(parse_duration("5m").unwrap(), 300);
        assert_eq!(parse_duration("6h").unwrap(), 21600);
        assert_eq!(parse_duration("1d").unwrap(), 86400);
        assert!(parse_duration("6").is_err());
        assert!(parse_duration("x6h").is_err());
        assert!(parse_duration("0h").is_err());
    }
}