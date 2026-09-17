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
//! | GET    | `/api/v1/updates`     | `501` planned       |
//!
//! See `docs/daemon-api.md` for the full contract.

#![forbid(unsafe_code)]

use abora_core::{Version, API_PATH_PREFIX, API_VERSION};

pub use abora_sysinfo::{Architecture, CpuInfo, KernelInfo, MemoryInfo, OsInfo};
pub use abora_update::{
    AvailableUpdate, AvailableUpdates, Channel, MaintenanceWindow, RebootStatus, UpdateHistoryEntry,
    UpdateStatus,
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
        let body = ApiErrorBody::new(ErrorCode::NotImplemented, "planned")
            .with_request_id("req-1");
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