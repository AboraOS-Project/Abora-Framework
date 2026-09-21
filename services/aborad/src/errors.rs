//! Error-to-response plumbing for the daemon.
//!
//! [`ApiErrorBody`] lives in `abora-api` (which must stay free of web-framework
//! dependencies). This module wraps it in a local newtype so `axum`'s
//! [`IntoResponse`] can be implemented without violating the orphan rule.

use axum::response::{IntoResponse, Response};
use axum::Json;

use abora_api::{ApiErrorBody, ErrorCode};
use abora_core::Version;

/// An API error ready to be serialized and returned to a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError(pub ApiErrorBody);

impl From<ApiErrorBody> for ApiError {
    fn from(body: ApiErrorBody) -> Self {
        ApiError(body)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = axum::http::StatusCode::from_u16(self.0.error.code.http_status())
            .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(self.0)).into_response()
    }
}

impl ApiError {
    /// Internal failure, tagged with the framework version for correlation.
    pub fn internal(msg: impl Into<String>) -> Self {
        ApiError(ApiErrorBody::new(
            ErrorCode::Internal,
            format!("{} (framework {})", msg.into(), Version::current()),
        ))
    }
}
