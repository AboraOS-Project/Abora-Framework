//! HTTP handlers for `aborad`. Each handler is small, read-only, and
//! declares its required [`Permission`] in [`super::server`].

use axum::extract::State;
use axum::Json;

use abora_api::{
    ApiErrorBody, ApiVersionInfo, DaemonInfo, ErrorCode, HealthResponse, HealthStatus,
    ServiceStatus, SystemResponse, VersionResponse,
};
use abora_core::{DAEMON_NAME, FRAMEWORK_NAME, Version};

use crate::errors::ApiError;
use crate::state::SharedState;

/// `GET /api/v1/health`
pub async fn health(State(state): State<SharedState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: HealthStatus::Ok,
        daemon: DaemonInfo {
            name: DAEMON_NAME.to_owned(),
            version: Version::current(),
        },
        api: ApiVersionInfo::current(),
        uptime_seconds: state.uptime_seconds(),
        started_at: state.started_at_rfc3339.clone(),
        // Components register as subsystems are implemented; empty today is
        // honest, not a lie.
        components: Vec::new(),
    })
}

/// `GET /api/v1/version`
pub async fn version() -> Json<VersionResponse> {
    Json(VersionResponse {
        framework_name: FRAMEWORK_NAME.to_owned(),
        framework_version: Version::current(),
        daemon: DaemonInfo {
            name: DAEMON_NAME.to_owned(),
            version: Version::current(),
        },
        api: ApiVersionInfo::current(),
        git_commit: option_env!("ABORA_BUILD_COMMIT").map(str::to_owned),
    })
}

/// `GET /api/v1/system`
pub async fn system(State(state): State<SharedState>) -> Result<Json<SystemResponse>, ApiError> {
    let info = abora_sysinfo::SystemInfo::collect().map_err(|e| {
        state
            .logger
            .error(format!("system info collection failed: {e}"));
        ApiError::internal(format!("could not collect system information: {e}"))
    })?;

    let (hostname_override, description) = {
        let config = state.config.read().expect("config lock poisoned");
        (config.system.hostname.clone(), config.system.description.clone())
    };
    let hostname = hostname_override
        .clone()
        .unwrap_or_else(|| info.hostname.clone());

    Ok(Json(SystemResponse {
        hostname,
        description,
        hostname_override,
        architecture: info.architecture,
        machine: info.machine,
        uptime_seconds: info.uptime.as_secs(),
        os: info.os,
        kernel: info.kernel,
        memory: info.memory,
        cpu: info.cpu,
        framework_version: Version::current(),
    }))
}

/// `GET /api/v1/services`
///
/// Read-only list of managed services, discovered through the local init
/// manager (systemd on Linux). If discovery is unavailable (non-systemd host,
/// sandbox, ...) this is `503` rather than an honest-looking empty list.
pub async fn services(State(state): State<SharedState>) -> Result<Json<Vec<ServiceStatus>>, ApiError> {
    match abora_services::collect() {
        Ok(units) => Ok(Json(units)),
        Err(e) => {
            state.logger.warn(format!("service discovery failed: {e}"));
            Err(ApiError(ApiErrorBody::new(
                ErrorCode::ServiceUnavailable,
                format!("could not query the service registry: {e}"),
            )))
        }
    }
}

/// `GET /api/v1/updates`
///
/// Update infrastructure is designed (see `crates/abora-update`) but not
/// implemented. Return `501` honestly instead of inventing status.
pub async fn updates() -> ApiError {
    ApiError(ApiErrorBody::new(
        ErrorCode::NotImplemented,
        "the update system is planned but not implemented in this milestone; see docs/update.md",
    ))
}