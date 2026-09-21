//! Versioned API definitions for Abora Framework.
//!
//! This crate contains **pure data types** shared by the daemon, the CLI, and
//! (later) Abora Cloud and Abora Atlas. It must never depend on a web
//! framework so it stays usable from any consumer.
//!
//! # Versioning policy
//!
//! * The current version is `v1`, served under `/api/v1`.
//! * Adding new optional fields and new endpoints is a backwards-compatible
//!   change and stays in the same version.
//! * Removing or renaming fields, changing semantics, or changing status
//!   codes for existing conditions requires a new version (`/api/v2`).
//! * Clients must ignore unknown fields.
//!
//! # Endpoints
//!
//! | Method | Path                  | Status              |
//! |--------|-----------------------|---------------------|
//! | GET    | `/api/v1/health`      | implemented         |
//! | GET    | `/api/v1/version`     | implemented         |
//! | GET    | `/api/v1/system`      | implemented         |
//! | GET    | `/api/v1/services`    | implemented (systemd) |
//! | GET    | `/api/v1/services/{name}` | implemented (systemd) |
//! | GET    | `/api/v1/updates`     | implemented (read-only, apt) |
//!
//! See `docs/daemon-api.md` for the full contract.

#![forbid(unsafe_code)]

use abora_core::{Version, API_PATH_PREFIX, API_VERSION};

pub use abora_sysinfo::{Architecture, CpuInfo, KernelInfo, MemoryInfo, OsInfo};
pub use abora_update::{
    AvailableUpdate, AvailableUpdates, Channel, MaintenanceWindow, RebootStatus,
    UpdateHistoryEntry, UpdateStatus,
};

/// Description of the active API version.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ApiVersionInfo {
    /// Version identifier, e.g. `v1`.
    pub name: String,
    /// Path prefix the version is served from, e.g. `/api/v1`.
    pub path: String,
}

impl ApiVersionInfo {
    /// The version compiled into this crate.
    pub fn current() -> Self {
        Self {
            name: API_VERSION.to_owned(),
            path: API_PATH_PREFIX.to_owned(),
        }
    }
}

impl Default for ApiVersionInfo {
    fn default() -> Self {
        Self::current()
    }
}

/// Identifying information about a running daemon instance.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DaemonInfo {
    pub name: String,
    pub version: Version,
}

impl DaemonInfo {
    pub fn current() -> Self {
        Self {
            name: abora_core::DAEMON_NAME.to_owned(),
            version: Version::current(),
        }
    }
}

/// Coarse health state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    /// Everything is operating normally.
    Ok,
    /// Running, but something is degraded (see per-component detail).
    Degraded,
    /// Deliberately in maintenance mode.
    Maintenance,
}

/// Health of one internal component.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComponentHealth {
    pub name: String,
    pub status: HealthStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Response for `GET /api/v1/health`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HealthResponse {
    pub status: HealthStatus,
    pub daemon: DaemonInfo,
    pub api: ApiVersionInfo,
    /// Seconds since the daemon started.
    pub uptime_seconds: u64,
    /// RFC 3339 timestamp of daemon start.
    pub started_at: String,
    /// Per-component health. Empty until components register.
    #[serde(default)]
    pub components: Vec<ComponentHealth>,
}

/// Response for `GET /api/v1/version`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VersionResponse {
    pub framework_name: String,
    pub framework_version: Version,
    pub daemon: DaemonInfo,
    pub api: ApiVersionInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
}

/// Response for `GET /api/v1/system`.
///
/// Uses the platform-agnostic [`abora_sysinfo`] types so consumers do not
/// need to know anything about Linux internals.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SystemResponse {
    pub hostname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname_override: Option<String>,
    pub architecture: Architecture,
    pub machine: String,
    pub uptime_seconds: u64,
    pub os: OsInfo,
    pub kernel: KernelInfo,
    pub memory: MemoryInfo,
    pub cpu: CpuInfo,
    pub framework_version: Version,
}

/// State of a managed system service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Running,
    Stopped,
    Failed,
    Activating,
    Deactivating,
    Unknown,
}

/// One managed system service. Read-only for now.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServiceStatus {
    pub name: String,
    pub state: ServiceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Detailed view of a single unit (`systemctl show`). Raw systemd values are
/// kept as strings so we never guess at their meaning.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServiceDetail {
    pub name: String,
    /// `LoadState` (`"loaded"`, `"error"`, ...).
    pub load_state: String,
    /// `ActiveState` (`"active"`, `"inactive"`, `"failed"`, ...).
    pub active_state: String,
    /// `SubState` (`"running"`, `"dead"`, `"exited"`, ...).
    pub sub_state: String,
    /// `UnitFileState` (`"enabled"`, `"disabled"`, `"static"`, ...),
    /// collapsed into the two-valued `enabled` hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// `Description`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Absolute path to the unit's fragment file (`FragmentPath`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment_path: Option<String>,
    /// First `ExecStart` command line (`argv[]` portion), e.g.
    /// `/usr/sbin/cron -f -P $EXTRA_OPTS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_start: Option<String>,
    /// PID of the main process when the unit is active (`MainPID`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_pid: Option<u64>,
    /// Current memory usage in bytes (`MemoryCurrent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
}

/// What the update policy allows at the moment it was last evaluated (about every 30 seconds).
/// Installing updates is not implemented yet, so these say what policy *would* permit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScheduleInfo {
    /// `[updates] check_interval` in seconds.
    pub check_interval_seconds: u64,
    /// Whether the server's local time is inside a maintenance window. Omitted if the time is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_maintenance_window: Option<bool>,
    /// A requested apply (`POST /api/v1/updates/apply`) would be accepted now.
    pub apply_permitted: bool,
    pub installs_permitted: bool,
    pub reboot_permitted: bool,
}

/// Body of `POST /api/v1/updates/apply`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ApplyResponse {
    /// `true` when nothing was requested and this only describes what would happen.
    pub dry_run: bool,
    /// The request is (or, for a dry run, would be) accepted.
    pub permitted: bool,
    /// Why not, when `permitted` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Id of the accepted request (absent for a dry run). Its outcome appears in `history`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The packages the last check listed, which is what would be (or was requested to be) upgraded.
    pub packages: Vec<String>,
}

/// Body of `GET /api/v1/updates`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UpdatesResponse {
    /// Result of the last check. Omitted until the first check has run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<UpdateStatus>,
    /// When the update source was last checked (RFC 3339), if ever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<String>,
    pub reboot: RebootStatus,
    /// Policy as last evaluated. Omitted until the scheduler has run once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<ScheduleInfo>,
    /// Updates found by the last check.
    pub available: Vec<AvailableUpdate>,
    /// Applied updates, newest first.
    pub history: Vec<UpdateHistoryEntry>,
}

/// Machine-readable error classification used by all API versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    NotImplemented,
    ServiceUnavailable,
    Internal,
}

impl ErrorCode {
    /// Conventional HTTP status for this code.
    pub const fn http_status(self) -> u16 {
        match self {
            ErrorCode::InvalidRequest => 400,
            ErrorCode::Unauthorized => 401,
            ErrorCode::Forbidden => 403,
            ErrorCode::NotFound => 404,
            ErrorCode::Conflict => 409,
            ErrorCode::NotImplemented => 501,
            ErrorCode::ServiceUnavailable => 503,
            ErrorCode::Internal => 500,
        }
    }
}

/// Error detail embedded in every non-2xx response body.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ApiErrorDetail {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// Envelope for API errors: `{ "error": { ... } }`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ApiErrorBody {
    pub error: ApiErrorDetail,
}

impl ApiErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            error: ApiErrorDetail {
                code,
                message: message.into(),
                request_id: None,
            },
        }
    }

    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.error.request_id = Some(request_id.into());
        self
    }
}

/// Well-known endpoint paths for the current API version using the default
/// prefix. Callers with a custom `[api] base_path` should build paths with
/// [`endpoint`].
pub mod paths {
    pub const HEALTH: &str = "/api/v1/health";
    pub const VERSION: &str = "/api/v1/version";
    pub const SYSTEM: &str = "/api/v1/system";
    pub const SERVICES: &str = "/api/v1/services";
    pub const UPDATES: &str = "/api/v1/updates";
}

/// Join a configured base path with a resource name.
///
/// ```rust
/// # use abora_api::endpoint;
/// assert_eq!(endpoint("/api/v1", "health"), "/api/v1/health");
/// assert_eq!(endpoint("/api/v1/", "health"), "/api/v1/health");
/// ```
pub fn endpoint(base_path: &str, resource: &str) -> String {
    format!(
        "{}/{}",
        base_path.trim_end_matches('/'),
        resource.trim_start_matches('/')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_have_expected_statuses() {
        assert_eq!(ErrorCode::InvalidRequest.http_status(), 400);
        assert_eq!(ErrorCode::Unauthorized.http_status(), 401);
        assert_eq!(ErrorCode::Forbidden.http_status(), 403);
        assert_eq!(ErrorCode::NotFound.http_status(), 404);
        assert_eq!(ErrorCode::NotImplemented.http_status(), 501);
        assert_eq!(ErrorCode::Internal.http_status(), 500);
    }

    #[test]
    fn error_envelope_serializes_as_spec() {
        let body = ApiErrorBody::new(ErrorCode::NotImplemented, "planned").with_request_id("req-1");
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["error"]["code"], "not_implemented");
        assert_eq!(json["error"]["message"], "planned");
        assert_eq!(json["error"]["request_id"], "req-1");
    }

    #[test]
    fn endpoint_join_normalizes_slashes() {
        assert_eq!(endpoint("/api/v1", "health"), "/api/v1/health");
        assert_eq!(endpoint("/api/v1/", "/health"), "/api/v1/health");
        assert_eq!(endpoint("/custom/base", "system"), "/custom/base/system");
    }

    #[test]
    fn health_response_omits_absent_optional_fields() {
        let health = HealthResponse {
            status: HealthStatus::Ok,
            daemon: DaemonInfo::current(),
            api: ApiVersionInfo::current(),
            uptime_seconds: 12,
            started_at: "2026-09-16T00:00:00.000Z".to_owned(),
            components: vec![],
        };
        let json = serde_json::to_value(&health).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["api"]["name"], "v1");
        assert_eq!(json["components"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn system_response_can_embed_sysinfo_types() {
        let os = OsInfo::unknown();
        let response = SystemResponse {
            hostname: "node-01".into(),
            description: None,
            hostname_override: Some("node-01".into()),
            architecture: Architecture::X86_64,
            machine: "x86_64".into(),
            uptime_seconds: 5,
            os,
            kernel: KernelInfo {
                name: "Linux".into(),
                release: "6.10.0".into(),
                version: "#1 SMP".into(),
            },
            memory: MemoryInfo::from_fields(1000, 500, 200, 0, 0),
            cpu: CpuInfo::default(),
            framework_version: Version::current(),
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["architecture"], "x86_64");
        assert!(json.get("description").is_none());
        assert_eq!(json["hostname_override"], "node-01");
    }
}
