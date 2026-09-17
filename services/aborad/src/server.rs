//! Router construction for `aborad`.
//!
//! Every route is paired with a [`Permission`]; the auth middleware enforces
//! the current policy (loopback clients are trusted, remote clients are
//! refused) before the handler runs.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::{ConnectInfo, Request};
use axum::http::header::AUTHORIZATION;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, MethodRouter};
use axum::Router;

use abora_api::endpoint;
use abora_log::{info, warn, Redacted};

use crate::auth::{self, Decision, Permission};
use crate::errors::ApiError;
use crate::handlers;
use crate::state::SharedState;

/// Build the full router for a daemon instance. Pass the loopback boundary
/// explicitly so that serving and testing use the same path.
pub fn build(state: SharedState) -> Router {
    let base_path = state.config.api.base_path.clone();

    Router::new()
        .route(
            &endpoint(&base_path, "health"),
            with_auth(get(handlers::health), state.clone(), Permission::ReadHealth),
        )
        .route(
            &endpoint(&base_path, "version"),
            with_auth(get(handlers::version), state.clone(), Permission::ReadVersion),
        )
        .route(
            &endpoint(&base_path, "system"),
            with_auth(get(handlers::system), state.clone(), Permission::ReadSystem),
        )
        .route(
            &endpoint(&base_path, "services"),
            with_auth(get(handlers::services), state.clone(), Permission::ReadServices),
        )
        .route(
            &endpoint(&base_path, "updates"),
            with_auth(get(handlers::updates), state.clone(), Permission::ReadUpdates),
        )
        .with_state(state)
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
async fn authorize(req: Request, next: Next, state: SharedState, permission: Permission) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));

    // Tokens are enforced only when the operator configured a store AND
    // authentication is required; otherwise the pure preview rule applies.
    let store = if state.config.security.require_authentication {
        state.tokens.as_ref()
    } else {
        None
    };
    let bearer = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(bearer_value);

    let decision = auth::authorize(peer, permission, bearer, store);

    match decision {
        Decision::Allowed => {
            if state.config.logging.level >= abora_log::Level::Debug {
                let method = req.method().clone();
                let uri = req.uri().clone();
                info!(
                    state.logger,
                    "{method} {uri} allowed (permission: {})",
                    permission.permission_id()
                );
            }
            next.run(req).await
        }
        denied => {
            if let Some(body) = denied.into_denial(permission) {
                warn!(
                    state.logger,
                    "denied {} {} (permission: {}, peer: {})",
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
        req.extensions_mut().insert(ConnectInfo(SocketAddr::new(ip, 0)));
        req
    }

    fn loopback_request(uri: &str) -> Request {
        request_with_peer(uri, IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    fn loopback_request_with_token(uri: &str, token: &str) -> Request {
        request_with_peer_and_token(uri, IpAddr::V4(Ipv4Addr::LOCALHOST), Some(token))
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
        let resp = app.oneshot(loopback_request("/api/v1/health")).await.unwrap();
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
        let resp = app.oneshot(loopback_request("/api/v1/version")).await.unwrap();
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
        let resp = app.oneshot(loopback_request("/api/v1/system")).await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["hostname"].as_str().unwrap().len() >= 1);
        assert!(v["kernel"]["release"].as_str().unwrap().len() >= 1);
        assert!(v["memory"]["total_bytes"].as_u64().unwrap() > 0);
        assert_eq!(v["architecture"].as_str().unwrap(), std::env::consts::ARCH);
    }

    #[tokio::test]
    async fn services_is_read_only_and_empty() {
        let app = test_app(Config::default());
        let resp = app.oneshot(loopback_request("/api/v1/services")).await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v, serde_json::json!([]));
    }

    #[tokio::test]
    async fn updates_is_honest_501() {
        let app = test_app(Config::default());
        let resp = app.oneshot(loopback_request("/api/v1/updates")).await.unwrap();
        assert_eq!(resp.status(), 501);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], "not_implemented");
    }

    #[tokio::test]
    async fn unknown_routes_are_404() {
        let app = test_app(Config::default());
        let resp = app.oneshot(loopback_request("/api/v1/nope")).await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn remote_peer_is_forbidden() {
        let app = test_app(Config::default());
        let resp = app
            .oneshot(request_with_peer("/api/v1/health", IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))))
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

        let old = app.clone().oneshot(loopback_request("/api/v1/health")).await.unwrap();
        assert_eq!(old.status(), 404);

        let resp = app.oneshot(loopback_request("/api/v2/health")).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn token_required_when_store_configured() {
        let app = test_app_with_tokens(Config::default(), Some(token_store("ops", &["read:health"])));

        // No header.
        let resp = app.clone().oneshot(loopback_request("/api/v1/health")).await.unwrap();
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
        let app = test_app_with_tokens(Config::default(), Some(token_store("ops", &["read:health"])));
        let resp = app
            .oneshot(loopback_request_with_token("/api/v1/system", "ops"))
            .await
            .unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], "forbidden");
        assert!(v["error"]["message"].as_str().unwrap().contains("read:system"));
    }

    #[tokio::test]
    async fn read_all_token_accesses_every_route() {
        let app = test_app_with_tokens(Config::default(), Some(token_store("admin", &["read_all"])));
        for path in ["/api/v1/health", "/api/v1/version", "/api/v1/system", "/api/v1/services"] {
            let resp = app
                .clone()
                .oneshot(loopback_request_with_token(path, "admin"))
                .await
                .unwrap();
            assert_eq!(resp.status(), 200, "path: {path}");
        }
    }
}