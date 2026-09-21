//! Router construction for `aborad`.
//!
//! Every route is paired with a [`Permission`]; the auth middleware enforces
//! the current policy (loopback clients are trusted, remote clients are
//! refused) before the handler runs. An outermost layer assigns every
//! request a correlation id (`X-Request-ID`) that is echoed back and injected
//! into error envelopes.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Request};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, MethodRouter};
use axum::Router;
use http_body_util::BodyExt;

use abora_api::{endpoint, ApiErrorBody, ErrorCode};
use abora_log::{info, warn, Redacted};

use crate::auth::{self, Decision, Permission};
use crate::errors::ApiError;
use crate::handlers;
use crate::state::SharedState;

/// Response/request header carrying the per-request correlation id.
pub const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Exploration-visible correlation id stored in request extensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

/// Build the full router for a daemon instance. Pass the loopback boundary
/// explicitly so that serving and testing use the same path.
pub fn build(state: SharedState) -> Router {
    let base_path = state
        .config
        .read()
        .expect("config lock poisoned")
        .api
        .base_path
        .clone();

    Router::new()
        .route(
            &endpoint(&base_path, "health"),
            with_auth(get(handlers::health), state.clone(), Permission::ReadHealth),
        )
        .route(
            &endpoint(&base_path, "version"),
            with_auth(
                get(handlers::version),
                state.clone(),
                Permission::ReadVersion,
            ),
        )
        .route(
            &endpoint(&base_path, "system"),
            with_auth(get(handlers::system), state.clone(), Permission::ReadSystem),
        )
        .route(
            &endpoint(&base_path, "services"),
            with_auth(
                get(handlers::services),
                state.clone(),
                Permission::ReadServices,
            ),
        )
        .route(
            &endpoint(&base_path, "services/{name}"),
            with_auth(
                get(handlers::service_detail),
                state.clone(),
                Permission::ReadServices,
            ),
        )
        .route(
            &endpoint(&base_path, "updates"),
            with_auth(
                get(handlers::updates),
                state.clone(),
                Permission::ReadUpdates,
            ),
        )
        .fallback(fallback)
        // Outermost: assign/carry a request id for every request, then
        // correlate it into error envelopes and log lines.
        .layer(middleware::from_fn(request_id_layer))
        .with_state(state)
}

/// Unknown routes get the same error envelope as everything else, so every
/// non-2xx response carries `code` and (after the request-id layer)
/// `request_id`.
async fn fallback(uri: axum::http::Uri) -> (StatusCode, axum::Json<ApiErrorBody>) {
    (
        StatusCode::NOT_FOUND,
        axum::Json(ApiErrorBody::new(
            ErrorCode::NotFound,
            format!("no API route matches `{uri}`"),
        )),
    )
}

/// Accept a caller-supplied `X-Request-ID` (a correlation handle propagated
/// across components) or generate one. Restricted to a conservative charset
/// so it can never smuggle headers or break logs.
fn incoming_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// Generate a fresh correlation id: 16 hex chars from `/dev/urandom`. If that
/// fails (sandbox), fall back to a time/pid/counter mix — still unique in
/// practice, just not crypto-random.
fn generate_request_id() -> String {
    let mut bytes = [0u8; 8];
    let random = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_ok();
    if random {
        return hex_lower(&bytes);
    }
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let mixed = nanos ^ pid.rotate_left(32) ^ counter.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    hex_lower(&mixed.to_le_bytes())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Per-request correlation middleware. Runs outside the auth/route layers.
async fn request_id_layer(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .filter(|s| incoming_request_id(s))
        .map(str::to_owned)
        .unwrap_or_else(generate_request_id);

    req.extensions_mut().insert(RequestId(id.clone()));

    let response = next.run(req).await;

    let mut response = response;
    response.headers_mut().insert(
        X_REQUEST_ID,
        HeaderValue::from_str(&id).unwrap_or_else(|_| HeaderValue::from_static("unknown")),
    );

    if response.status().is_client_error() || response.status().is_server_error() {
        inject_request_id(response, &id).await
    } else {
        response
    }
}

/// Put the correlation id into the `error.request_id` field of any JSON error
/// envelope that does not already carry one.
async fn inject_request_id(response: Response, id: &str) -> Response {
    let is_json = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.starts_with("application/json"))
        .unwrap_or(false);
    if !is_json {
        return response;
    }

    let (parts, body) = response.into_parts();
    let bytes = match BodyExt::collect(body).await {
        Ok(collected) => collected.to_bytes(),
        // Body::collect consumes the body; a failure here is effectively
        // unreachable for the tiny JSON envelopes we rewrite.
        Err(_) => return (parts.status, parts.headers, axum::body::Body::empty()).into_response(),
    };

    let mut value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return response_from_bytes(parts, bytes.to_vec()),
    };

    let injected = value
        .get_mut("error")
        .and_then(|e| e.as_object_mut())
        .map(|error| {
            if error.contains_key("request_id") {
                false
            } else {
                error.insert(
                    "request_id".into(),
                    serde_json::Value::String(id.to_owned()),
                );
                true
            }
        })
        .unwrap_or(false);

    let payload = if injected {
        serde_json::to_vec(&value).unwrap_or_else(|_| bytes.to_vec())
    } else {
        bytes.to_vec()
    };
    response_from_bytes(parts, payload)
}

/// Rebuild a JSON response from its collected body, preserving status and
/// headers. `Body::collect` consumes the original body, so we re-emit it.
fn response_from_bytes(parts: axum::http::response::Parts, payload: Vec<u8>) -> Response {
    let mut headers = parts.headers.clone();
    headers.remove(axum::http::header::CONTENT_LENGTH);
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    (parts.status, headers, axum::body::Bytes::from(payload)).into_response()
}

/// Attach the per-operation authorization middleware to one route.
///
/// Generic over the router state `S` and its error type so axum can prove the
/// middleware service bounds at this call site.
fn with_auth<S>(
    router: MethodRouter<S>,
    state: SharedState,
    permission: Permission,
) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    router.layer(middleware::from_fn::<_, (Request,)>(
        move |req: Request, next: Next| {
            let state = state.clone();
            async move { authorize(req, next, state, permission).await }
        },
    ))
}

/// Enforce per-operation authorization. Runs before every handler.
async fn authorize(
    req: Request,
    next: Next,
    state: SharedState,
    permission: Permission,
) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));

    // Tokens are enforced only when the operator configured a store AND
    // authentication is required; otherwise the pure preview rule applies.
    // The authorization decision is computed (and the locks dropped) inside
    // the block so the token borrow never outlives its guard.
    let bearer = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_value);

    let decision = {
        let config = state.config.read().expect("config lock poisoned");
        let tokens = state.tokens.read().expect("tokens lock poisoned");
        let store = if config.security.require_authentication {
            tokens.as_ref()
        } else {
            None
        };
        auth::authorize(peer, permission, bearer, store)
    };

    match decision {
        Decision::Allowed => {
            if state.logger.level() >= abora_log::Level::Debug {
                let method = req.method().clone();
                let uri = req.uri().clone();
                let request_id = req
                    .extensions()
                    .get::<RequestId>()
                    .map(|r| r.0.as_str())
                    .unwrap_or("-");
                info!(
                    state.logger,
                    "{request_id} {method} {uri} allowed (permission: {})",
                    permission.permission_id()
                );
            }
            next.run(req).await
        }
        denied => {
            if let Some(body) = denied.into_denial(permission) {
                let request_id = req
                    .extensions()
                    .get::<RequestId>()
                    .map(|r| r.0.as_str())
                    .unwrap_or("-");
                warn!(
                    state.logger,
                    "{request_id} denied {} {} (permission: {}, peer: {})",
                    req.method(),
                    req.uri(),
                    permission.permission_id(),
                    Redacted(peer)
                );
                ApiError(body).into_response()
            } else {
                ApiError::internal("unexpected authorization failure").into_response()
            }
        }
    }
}

/// Parse the token out of an `Authorization: Bearer <secret>` header value.
/// The `Bearer` scheme is accepted case-insensitively (RFC 6750).
fn bearer_value(header: &str) -> Option<&str> {
    let (scheme, rest) = header.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    let token = rest.trim();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use crate::tokens::TokenStore;
    use abora_config::Config;

    pub(crate) fn test_app(config: Config) -> Router {
        test_app_with_tokens(config, None)
    }

    pub(crate) fn test_app_with_tokens(config: Config, tokens: Option<TokenStore>) -> Router {
        use abora_log::{Format, Level};
        let logger = abora_log::Logger::builder()
            .level(Level::Off)
            .format(Format::Json)
            .writer(Box::new(std::io::sink()))
            .build();
        build(AppState::new(config, logger, tokens))
    }

    fn request_with_peer(uri: &str, ip: IpAddr) -> Request {
        request_with_peer_and_token(uri, ip, None)
    }

    fn request_with_peer_and_token(uri: &str, ip: IpAddr, token: Option<&str>) -> Request {
        let mut builder = Request::builder().uri(uri);
        if let Some(token) = token {
            builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        let mut req = builder.body(axum::body::Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(ip, 0)));
        req
    }

    fn loopback_request(uri: &str) -> Request {
        request_with_peer(uri, IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    fn loopback_request_with_token(uri: &str, token: &str) -> Request {
        request_with_peer_and_token(uri, IpAddr::V4(Ipv4Addr::LOCALHOST), Some(token))
    }

    /// True when `systemctl` is runnable from the test environment (i.e. not
    /// a Nix-build sandbox). Used to branch assertions on `/api/v1/services`.
    fn systemctl_available() -> bool {
        std::process::Command::new("systemctl")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn token_store(secret: &str, permissions: &[&str]) -> TokenStore {
        let perms = permissions
            .iter()
            .map(|p| format!("\"{p}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let hash = crate::tokens::sha256_hex(secret);
        TokenStore::from_str(&format!(
            "[[tokens]]\nname = \"test\"\npermissions = [{perms}]\nsecret_hash = \"{hash}\"\n"
        ))
        .unwrap()
    }

    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_returns_ok() {
        let app = test_app(Config::default());
        let resp = app
            .oneshot(loopback_request("/api/v1/health"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["daemon"]["name"], "aborad");
        assert_eq!(v["api"]["name"], "v1");
        assert_eq!(v["components"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn version_returns_framework_and_daemon_versions() {
        let app = test_app(Config::default());
        let resp = app
            .oneshot(loopback_request("/api/v1/version"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["framework_version"].get("major").is_some(), "got: {v}");
        assert!(v["framework_version"]["patch"].as_u64().is_some());
        assert_eq!(v["daemon"]["name"], "aborad");
        assert_eq!(v["api"]["path"], "/api/v1");
    }

    #[tokio::test]
    async fn system_returns_real_machine_facts() {
        let app = test_app(Config::default());
        let resp = app
            .oneshot(loopback_request("/api/v1/system"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(!v["hostname"].as_str().unwrap().is_empty());
        assert!(!v["kernel"]["release"].as_str().unwrap().is_empty());
        assert!(v["memory"]["total_bytes"].as_u64().unwrap() > 0);
        assert_eq!(v["architecture"].as_str().unwrap(), std::env::consts::ARCH);
    }

    #[tokio::test]
    async fn services_lists_systemd_units_else_503() {
        let app = test_app(Config::default());
        let resp = app
            .oneshot(loopback_request("/api/v1/services"))
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        if systemctl_available() {
            assert_eq!(status, 200);
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let list = v.as_array().expect("services must be an array");
            assert!(!list.is_empty(), "a systemd host should have services");
            for entry in list {
                assert!(entry["name"].as_str().unwrap().ends_with(".service"));
                assert!(entry["state"].is_string());
            }
        } else {
            // Sandboxes (e.g. Nix build) have no systemd: 503, not fake data.
            assert_eq!(status, 503);
        }
    }

    async fn get_json(uri: &str) -> (u16, axum::http::HeaderMap, serde_json::Value) {
        let resp = test_app(Config::default())
            .oneshot(loopback_request(uri))
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            headers,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn services_query_rejects_bad_input_with_the_error_envelope() {
        for uri in [
            "/api/v1/services?state=sleeping",
            "/api/v1/services?enabled=maybe",
            "/api/v1/services?limit=0",
            "/api/v1/services?limit=100000",
            "/api/v1/services?limit=abc",
            "/api/v1/services?offset=-1",
            "/api/v1/services?stat=failed",
        ] {
            let (status, _, v) = get_json(uri).await;
            assert_eq!(status, 400, "{uri}");
            assert_eq!(v["error"]["code"], "invalid_request", "{uri}");
        }
    }

    #[tokio::test]
    async fn services_pagination_and_total_count() {
        if !systemctl_available() {
            return;
        }
        let (status, headers, all) = get_json("/api/v1/services").await;
        assert_eq!(status, 200);
        let total: usize = headers["x-total-count"].to_str().unwrap().parse().unwrap();
        assert_eq!(total, all.as_array().unwrap().len());

        let (_, headers, page) = get_json("/api/v1/services?limit=1&offset=1").await;
        assert_eq!(
            page.as_array().unwrap().len(),
            1.min(total.saturating_sub(1))
        );
        assert_eq!(
            headers["x-total-count"]
                .to_str()
                .unwrap()
                .parse::<usize>()
                .unwrap(),
            total,
            "count ignores paging"
        );

        let (_, _, none) = get_json("/api/v1/services?q=zzz-no-such-unit-zzz").await;
        assert!(none.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn service_detail_validates_and_reports_missing_units() {
        let (status, _, v) = get_json("/api/v1/services/not-a-unit").await;
        assert_eq!(status, 400);
        assert_eq!(v["error"]["code"], "invalid_request");
        let (status, _, _) = get_json("/api/v1/services/-evil.service").await;
        assert_eq!(status, 400);
        if systemctl_available() {
            let (status, _, v) = get_json("/api/v1/services/zzz-no-such-unit-zzz.service").await;
            assert_eq!(status, 404);
            assert_eq!(v["error"]["code"], "not_found");
        }
    }

    #[tokio::test]
    async fn updates_before_any_check_reports_nothing_known() {
        let (status, _, v) = get_json("/api/v1/updates").await;
        assert_eq!(status, 200);
        assert!(
            v.get("status").is_none(),
            "no status claimed before a check: {v}"
        );
        assert!(v.get("last_check").is_none());
        assert_eq!(v["reboot"]["required"], false);
        assert_eq!(v["available"], serde_json::json!([]));
        assert_eq!(v["history"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn unknown_routes_are_404() {
        let app = test_app(Config::default());
        let resp = app.oneshot(loopback_request("/api/v1/nope")).await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[test]
    fn request_id_validation() {
        assert!(incoming_request_id("abc-123_DEF.9:0"));
        assert!(incoming_request_id("a")); // 1 char
        assert!(incoming_request_id(&"x".repeat(128)));
        assert!(!incoming_request_id("")); // empty
        assert!(!incoming_request_id(&"x".repeat(129))); // too long
        assert!(!incoming_request_id("has space"));
        assert!(!incoming_request_id("tab\there"));
        assert!(!incoming_request_id("quote\"here"));
    }

    #[test]
    fn generate_request_id_is_16_hex_chars() {
        let id = generate_request_id();
        assert_eq!(id.len(), 16, "got: {id}");
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        let id2 = generate_request_id();
        assert_ne!(id, id2);
    }

    #[tokio::test]
    async fn success_echoes_generated_request_id() {
        let app = test_app(Config::default());
        let resp = app
            .clone()
            .oneshot(loopback_request("/api/v1/health"))
            .await
            .unwrap();
        let id = resp
            .headers()
            .get(X_REQUEST_ID)
            .expect("success should carry a request id")
            .to_str()
            .unwrap();
        assert_eq!(id.len(), 16);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn caller_supplied_request_id_is_honored() {
        let app = test_app(Config::default());
        let mut req = loopback_request("/api/v1/health");
        req.headers_mut()
            .insert(X_REQUEST_ID, HeaderValue::from_static("client-123"));
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.headers().get(X_REQUEST_ID).unwrap(), "client-123");
    }

    #[tokio::test]
    async fn invalid_request_id_is_rejected() {
        let app = test_app(Config::default());
        let mut req = loopback_request("/api/v1/health");
        let too_long: String = "x".repeat(200);
        req.headers_mut()
            .insert(X_REQUEST_ID, HeaderValue::from_str(&too_long).unwrap());
        let resp = app.oneshot(req).await.unwrap();
        let echoed = resp.headers().get(X_REQUEST_ID).unwrap().to_str().unwrap();
        assert_eq!(echoed.len(), 16, "an over-long id must be replaced");
    }

    #[tokio::test]
    async fn error_envelope_carries_request_id() {
        let app = test_app(Config::default());
        // A bad query is a 400 with the normal envelope.
        let mut req = loopback_request("/api/v1/services?limit=0");
        req.headers_mut()
            .insert(X_REQUEST_ID, HeaderValue::from_static("trace-42"));
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 400);
        assert_eq!(resp.headers().get(X_REQUEST_ID).unwrap(), "trace-42");
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], "invalid_request");
        assert_eq!(v["error"]["request_id"], "trace-42");
    }

    #[tokio::test]
    async fn unknown_route_returns_envelope_with_request_id() {
        let app = test_app(Config::default());
        let mut req = loopback_request("/api/v1/nope");
        req.headers_mut()
            .insert(X_REQUEST_ID, HeaderValue::from_static("t-1"));
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 404);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], "not_found");
        assert_eq!(v["error"]["request_id"], "t-1");
    }

    #[tokio::test]
    async fn auth_denial_envelope_carries_request_id() {
        let app = test_app_with_tokens(
            Config::default(),
            Some(token_store("ops", &["read:health"])),
        );
        let mut req = loopback_request_with_token("/api/v1/system", "ops");
        req.headers_mut()
            .insert(X_REQUEST_ID, HeaderValue::from_static("deny-1"));
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 403);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], "forbidden");
        assert_eq!(v["error"]["request_id"], "deny-1");
    }

    #[tokio::test]
    async fn remote_peer_is_forbidden() {
        let app = test_app(Config::default());
        let resp = app
            .oneshot(request_with_peer(
                "/api/v1/health",
                IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], "forbidden");
    }

    #[tokio::test]
    async fn custom_base_path_is_honored() {
        let mut cfg = Config::default();
        cfg.api.base_path = "/api/v2".to_owned();
        let app = test_app(cfg);

        let old = app
            .clone()
            .oneshot(loopback_request("/api/v1/health"))
            .await
            .unwrap();
        assert_eq!(old.status(), 404);

        let resp = app
            .oneshot(loopback_request("/api/v2/health"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn token_required_when_store_configured() {
        let app = test_app_with_tokens(
            Config::default(),
            Some(token_store("ops", &["read:health"])),
        );

        // No header.
        let resp = app
            .clone()
            .oneshot(loopback_request("/api/v1/health"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        // Wrong secret.
        let resp = app
            .clone()
            .oneshot(loopback_request_with_token("/api/v1/health", "wrong"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        // Valid secret, missing permission for this route.
        let resp = app
            .clone()
            .oneshot(loopback_request_with_token("/api/v1/system", "ops"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);

        // Valid secret, correct permission.
        let resp = app
            .oneshot(loopback_request_with_token("/api/v1/health", "ops"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn remote_peer_is_refused_even_with_a_valid_token() {
        let app = test_app_with_tokens(Config::default(), Some(token_store("ops", &["read_all"])));
        let resp = app
            .oneshot(request_with_peer_and_token(
                "/api/v1/health",
                IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)),
                Some("ops"),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], "forbidden");
    }

    #[tokio::test]
    async fn token_missing_permission_returns_error_envelope() {
        let app = test_app_with_tokens(
            Config::default(),
            Some(token_store("ops", &["read:health"])),
        );
        let resp = app
            .oneshot(loopback_request_with_token("/api/v1/system", "ops"))
            .await
            .unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], "forbidden");
        assert!(v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("read:system"));
    }

    #[tokio::test]
    async fn read_all_token_accesses_every_route() {
        let app =
            test_app_with_tokens(Config::default(), Some(token_store("admin", &["read_all"])));
        for path in [
            "/api/v1/health",
            "/api/v1/version",
            "/api/v1/system",
            "/api/v1/services",
        ] {
            let resp = app
                .clone()
                .oneshot(loopback_request_with_token(path, "admin"))
                .await
                .unwrap();
            let status = resp.status();
            if path == "/api/v1/services" && !systemctl_available() {
                // Auth is enforced regardless of backend availability.
                assert_ne!(status, 401, "path: {path}");
                assert_eq!(status, 503, "path: {path}");
            } else {
                assert_eq!(status, 200, "path: {path}");
            }
        }
    }
}
