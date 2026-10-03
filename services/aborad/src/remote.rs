//! The TLS listener for remote management (`[remote] enabled = true`).
//!
//! It serves the same routes as the loopback listener, but every request is
//! tagged [`crate::server::RemoteListener`], and the authorization layer then
//! demands a valid bearer token for everything: there is no preview mode and
//! `require_authentication = false` does not apply. The daemon refuses to
//! start remote management without a token store, a certificate and a key
//! (see `abora_config::Config::validate`), and only ever speaks TLS here.

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::time::Duration;

use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use axum_server::Handle;

/// How long in-flight remote requests get to finish on shutdown.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Load the certificate chain and private key (PEM). Fails closed: a missing
/// or unreadable file stops the daemon rather than serving without TLS.
pub async fn load_tls(cert: &Path, key: &Path) -> Result<RustlsConfig, String> {
    install_crypto_provider();
    RustlsConfig::from_pem_file(cert, key).await.map_err(|e| {
        format!(
            "could not load the remote TLS certificate {} / key {}: {e}",
            cert.display(),
            key.display()
        )
    })
}

/// rustls needs one process-wide crypto provider. Installing twice is
/// harmless (the second attempt is ignored).
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Bind the remote address synchronously, so a port clash surfaces before
/// the daemon says it is running.
pub fn bind(addr: SocketAddr) -> Result<TcpListener, String> {
    let listener = TcpListener::bind(addr)
        .map_err(|e| format!("could not bind the remote TLS listener on {addr}: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("could not configure the remote listener: {e}"))?;
    Ok(listener)
}

/// Serve `app` over TLS on `listener` until `stop` turns true.
pub async fn serve(
    listener: TcpListener,
    tls: RustlsConfig,
    app: Router,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    let handle = Handle::new();
    let shutdown = handle.clone();
    tokio::spawn(async move {
        let _ = stop.wait_for(|stopped| *stopped).await;
        shutdown.graceful_shutdown(Some(SHUTDOWN_GRACE));
    });
    axum_server::from_tcp_rustls(listener, tls)
        .handle(handle)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .map_err(|e| format!("remote listener error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use crate::tokens::{sha256_hex, TokenStore};
    use abora_config::Config;

    struct Fixture {
        dir: std::path::PathBuf,
        cert_pem: String,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A throwaway self-signed certificate for `localhost`, written to a temp dir.
    fn fixture() -> Fixture {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "aborad-remote-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cert_pem = certified.cert.pem();
        std::fs::write(dir.join("cert.pem"), &cert_pem).unwrap();
        std::fs::write(dir.join("key.pem"), certified.key_pair.serialize_pem()).unwrap();
        Fixture { dir, cert_pem }
    }

    fn app(tokens: Option<TokenStore>) -> Router {
        let logger = abora_log::Logger::builder()
            .level(abora_log::Level::Off)
            .writer(Box::new(std::io::sink()))
            .build();
        crate::server::build_remote(AppState::new(Config::default(), logger, tokens))
    }

    fn store(secret: &str) -> TokenStore {
        TokenStore::from_str(&format!(
            "[[tokens]]\nname = \"remote-test\"\npermissions = [\"read:health\"]\nsecret_hash = \"{}\"\n",
            sha256_hex(secret)
        ))
        .unwrap()
    }

    /// GET over real TLS, trusting only the fixture's certificate.
    fn get(port: u16, cert_pem: &str, token: Option<&str>) -> Result<u16, String> {
        let cert =
            ureq::tls::Certificate::from_pem(cert_pem.as_bytes()).map_err(|e| e.to_string())?;
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::new_with_certs(&[cert.to_owned()]))
            .build();
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder()
                .tls_config(tls)
                .http_status_as_error(false)
                .build(),
        );
        let mut request = agent.get(format!("https://localhost:{port}/api/v1/health"));
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        request
            .call()
            .map(|r| r.status().as_u16())
            .map_err(|e| e.to_string())
    }

    /// Start a remote listener on an ephemeral port; returns the port and the stop switch.
    async fn start(
        tokens: Option<TokenStore>,
        fx: &Fixture,
    ) -> (u16, tokio::sync::watch::Sender<bool>) {
        let listener = bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let port = listener.local_addr().unwrap().port();
        let tls = load_tls(&fx.dir.join("cert.pem"), &fx.dir.join("key.pem"))
            .await
            .unwrap();
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let app = app(tokens);
        tokio::spawn(async move { serve(listener, tls, app, stop_rx).await });
        (port, stop_tx)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remote_listener_requires_a_token_over_tls() {
        let fx = fixture();
        let (port, stop) = start(Some(store("s3cret")), &fx).await;
        let cert = fx.cert_pem.clone();
        let results = tokio::task::spawn_blocking(move || {
            (
                get(port, &cert, None),
                get(port, &cert, Some("wrong")),
                get(port, &cert, Some("s3cret")),
            )
        })
        .await
        .unwrap();
        assert_eq!(results.0, Ok(401), "no token is refused");
        assert_eq!(results.1, Ok(401), "an unknown token is refused");
        assert_eq!(results.2, Ok(200), "a valid token is admitted over TLS");
        let _ = stop.send(true);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remote_listener_without_a_token_store_refuses_everything() {
        let fx = fixture();
        let (port, stop) = start(None, &fx).await;
        let cert = fx.cert_pem.clone();
        let status = tokio::task::spawn_blocking(move || get(port, &cert, Some("anything")))
            .await
            .unwrap();
        assert_eq!(status, Ok(403), "no preview mode on the remote listener");
        let _ = stop.send(true);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn plain_http_is_not_served_on_the_remote_port() {
        let fx = fixture();
        let (port, stop) = start(Some(store("s3cret")), &fx).await;
        let status = tokio::task::spawn_blocking(move || {
            ureq::get(format!("http://localhost:{port}/api/v1/health"))
                .header("Authorization", "Bearer s3cret")
                .call()
                .map(|r| r.status().as_u16())
        })
        .await
        .unwrap();
        assert!(
            status.is_err(),
            "plain HTTP must not get an API response: {status:?}"
        );
        let _ = stop.send(true);
    }

    #[tokio::test]
    async fn missing_certificate_files_fail_closed() {
        let err = load_tls(
            Path::new("/nonexistent/cert.pem"),
            Path::new("/nonexistent/key.pem"),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("could not load the remote TLS certificate"),
            "{err}"
        );
    }
}
