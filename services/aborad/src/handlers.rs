//! HTTP handlers for `aborad`. Each handler is small, read-only, and
//! declares its required [`Permission`] in [`super::server`].

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderName, HeaderValue};
use axum::response::{AppendHeaders, IntoResponse};
use axum::Json;

use abora_api::{
    ApiErrorBody, ApiVersionInfo, DaemonInfo, ErrorCode, HealthResponse, HealthStatus,
    ServiceDetail, ServiceState, SystemResponse, UpdatesResponse, VersionResponse,
};
use abora_core::{Version, DAEMON_NAME, FRAMEWORK_NAME};

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
        (
            config.system.hostname.clone(),
            config.system.description.clone(),
        )
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

/// Largest page a client may request from the service list.
const MAX_SERVICE_PAGE: usize = 1000;

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError(ApiErrorBody::new(ErrorCode::InvalidRequest, msg))
}

/// Parse the `GET /api/v1/services` query string. Unknown parameters and bad
/// values are `400` with the normal error envelope rather than being ignored,
/// so a typo like `?stat=failed` never silently returns everything.
fn parse_service_query(
    params: &HashMap<String, String>,
) -> Result<abora_services::ServiceQuery, ApiError> {
    let mut query = abora_services::ServiceQuery::default();
    for (key, value) in params {
        match key.as_str() {
            "state" => {
                query.state = Some(match value.as_str() {
                    "running" => ServiceState::Running,
                    "stopped" => ServiceState::Stopped,
                    "failed" => ServiceState::Failed,
                    "activating" => ServiceState::Activating,
                    "deactivating" => ServiceState::Deactivating,
                    "unknown" => ServiceState::Unknown,
                    other => {
                        return Err(bad_request(format!(
                            "`state` must be one of running, stopped, failed, activating, deactivating, unknown (got `{other}`)"
                        )))
                    }
                })
            }
            "enabled" => {
                query.enabled = Some(match value.as_str() {
                    "true" => true,
                    "false" => false,
                    other => return Err(bad_request(format!("`enabled` must be true or false (got `{other}`)"))),
                })
            }
            "q" => {
                if !value.is_empty() {
                    query.text = Some(value.clone());
                }
            }
            "limit" => {
                let n: usize = value
                    .parse()
                    .ok()
                    .filter(|n| (1..=MAX_SERVICE_PAGE).contains(n))
                    .ok_or_else(|| bad_request(format!("`limit` must be a number from 1 to {MAX_SERVICE_PAGE}")))?;
                query.limit = Some(n);
            }
            "offset" => {
                query.offset = value
                    .parse()
                    .map_err(|_| bad_request("`offset` must be a non-negative number"))?;
            }
            other => return Err(bad_request(format!("unknown query parameter `{other}`"))),
        }
    }
    Ok(query)
}

fn services_unavailable(state: &SharedState, e: abora_services::ServicesError) -> ApiError {
    state.logger.warn(format!("service discovery failed: {e}"));
    ApiError(ApiErrorBody::new(
        ErrorCode::ServiceUnavailable,
        format!("could not query the service registry: {e}"),
    ))
}

/// `GET /api/v1/services?state=&enabled=&q=&limit=&offset=`
///
/// Read-only list of managed services, discovered through the local init
/// manager (systemd on Linux). The body stays a plain JSON array; the number
/// of matches before pagination is returned in `X-Total-Count`. If discovery
/// is unavailable (non-systemd host, sandbox, ...) this is `503` rather than
/// an honest-looking empty list.
pub async fn services(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, ApiError> {
    let query = parse_service_query(&params)?;
    let units = abora_services::collect().map_err(|e| services_unavailable(&state, e))?;
    let total = query.total_matching(&units);
    Ok((
        AppendHeaders([(
            HeaderName::from_static("x-total-count"),
            HeaderValue::from(total),
        )]),
        Json(query.apply(&units)),
    ))
}

/// `GET /api/v1/services/{name}`
///
/// Detail for one unit (`systemctl show`). `404` if it does not exist and
/// `400` if the name is not a valid `*.service` unit name.
pub async fn service_detail(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<ServiceDetail>, ApiError> {
    if !abora_services::validate_unit_name(&name) {
        return Err(bad_request(
            "the service name must be a unit name ending in `.service`",
        ));
    }
    match abora_services::detail(&name) {
        Ok(detail) => Ok(Json(detail)),
        Err(abora_services::ServicesError::NotFound(n)) => Err(ApiError(ApiErrorBody::new(
            ErrorCode::NotFound,
            format!("no such service: {n}"),
        ))),
        Err(e) => Err(services_unavailable(&state, e)),
    }
}

/// `GET /api/v1/updates`
///
/// Read-only: status of the last check, when it ran, whether a reboot is pending, the
/// updates that check found, and the history. Nothing is installed and no check is run
/// here; the daemon checks at startup. Before the first check finishes `status` is omitted.
pub async fn updates(State(state): State<SharedState>) -> Json<UpdatesResponse> {
    Json(state.updates.response())
}
