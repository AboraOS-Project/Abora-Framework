//! `aborad` — the Abora Framework system daemon.
//!
//! Secure foundations, in this milestone:
//!
//! * Loopback-only by default; refuses to bind a remote address.
//! * Read-only API (`health`, `version`, `system`, `services`) with a
//!   per-operation permission model.
//! * No arbitrary remote shell/command execution endpoint, ever.
//! * Structured logs to stderr (captured by systemd/journald).
//!
//! Run `aborad --help` for usage.

use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::ExitCode;

use abora_config::{Config, ConfigError};
use abora_core::{API_VERSION, DAEMON_NAME, DEFAULT_CONFIG_PATH, FRAMEWORK_NAME, FRAMEWORK_VERSION};
use abora_log::{info, warn, Logger};

use crate::state::AppState;

mod auth;
mod errors;
mod handlers;
mod server;
mod state;

const USAGE: &str = "Usage: aborad [--config <path>] [--version] [--help]";

fn main() -> ExitCode {
    match parse_args() {
        Ok(Action::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Action::Version) => {
            println!(
                "{FRAMEWORK_NAME} {FRAMEWORK_VERSION} ({DAEMON_NAME} {}, api {API_VERSION})",
                abora_core::Version::current()
            );
            ExitCode::SUCCESS
        }
        Ok(Action::Run { config_path }) => match run(config_path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("error: {msg}");
                ExitCode::FAILURE
            }
        },
        Err(msg) => {
            eprintln!("error: {msg}\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

enum Action {
    Help,
    Version,
    Run { config_path: Option<PathBuf> },
}

fn parse_args() -> Result<Action, String> {
    let mut config_path = None;
    let mut help = false;
    let mut version = false;
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => help = true,
            "--version" | "-V" => version = true,
            "--config" => {
                let value = iter.next().ok_or_else(|| "--config requires a path argument".to_owned())?;
                config_path = Some(PathBuf::from(value));
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if help {
        return Ok(Action::Help);
    }
    if version {
        return Ok(Action::Version);
    }
    Ok(Action::Run { config_path })
}

struct LoadedConfig {
    config: Config,
    source: ConfigSource,
}

enum ConfigSource {
    Explicit(PathBuf),
    Environment(PathBuf),
    // The default path did not exist; compiled-in defaults were used.
    BuiltinDefaults,
}

fn resolve_config(explicit: Option<PathBuf>, logger: &Logger) -> Result<LoadedConfig, String> {
    if let Some(path) = explicit {
        let config = Config::load(&path).map_err(|e| format!("{e}"))?;
        return Ok(LoadedConfig { config, source: ConfigSource::Explicit(path) });
    }

    if let Ok(path) = std::env::var("ABORA_CONFIG").map(PathBuf::from) {
        let config = Config::load(&path).map_err(|e| format!("{e}"))?;
        return Ok(LoadedConfig { config, source: ConfigSource::Environment(path) });
    }

    let default = PathBuf::from(DEFAULT_CONFIG_PATH);
    match Config::load(&default) {
        Ok(config) => Ok(LoadedConfig { config, source: ConfigSource::Explicit(default) }),
        Err(ConfigError::Io { path, .. }) if path == default && !default.exists() => {
            warn!(
                logger,
                "no configuration file at {}; using compiled-in defaults",
                DEFAULT_CONFIG_PATH
            );
            Ok(LoadedConfig { config: Config::default(), source: ConfigSource::BuiltinDefaults })
        }
        Err(e) => Err(format!("{e}")),
    }
}

fn check_bind_addr(config: &Config) -> Result<SocketAddr, String> {
    let addr: SocketAddr = config
        .remote
        .listen_addr
        .parse()
        .map_err(|_| format!("[remote] listen_addr `{}` is not a valid socket address", config.remote.listen_addr))?;

    if !addr.ip().is_loopback() {
        return Err(format!(
            "refusing to bind non-loopback address `{}`: remote management is not implemented \
             in this milestone. Keep [remote].listen_addr on 127.0.0.1/::1; see docs/security.md.",
            config.remote.listen_addr
        ));
    }
    Ok(addr)
}

fn run(config_path: Option<PathBuf>) -> Result<(), String> {
    // Bootstrap a minimal logger for the config-resolution phase; the real
    // logger is configured below from the loaded config.
    let bootstrap_logger = Logger::builder().build();

    let loaded = resolve_config(config_path, &bootstrap_logger)?;
    let config = loaded.config;

    // Validate the (possibly compiled-in) config before starting anything.
    config
        .validate()
        .map_err(|errors| format!("invalid configuration:\n{}", errors.join("\n")))?;

    let bind_addr = check_bind_addr(&config)?;

    let logger = Logger::builder()
        .level(config.logging.level)
        .format(config.logging.format)
        .build();
    if let Err(first) = abora_log::set_global(logger.clone()) {
        // Only one logger can be global per process; not fatal for the daemon.
        let _ = first;
    }

    info!(logger, "starting {DAEMON_NAME} {FRAMEWORK_VERSION} (api {API_VERSION})");
    match &loaded.source {
        ConfigSource::Explicit(p) => info!(logger, "configuration loaded from {}", p.display()),
        ConfigSource::Environment(p) => info!(logger, "configuration loaded from {} (ABORA_CONFIG)", p.display()),
        ConfigSource::BuiltinDefaults => {}
    }
    for line in config.summary() {
        info!(logger, "{line}");
    }
    if config.remote.enabled {
        warn!(
            logger,
            "[remote].enabled is set but remote management is not implemented; ignoring it"
        );
    }
    if config.security.require_authentication {
        info!(
            logger,
            "authentication required: loopback-only mode (remote clients are refused)"
        );
    }
    if !config.security.allow_loopback_unauthenticated {
        warn!(
            logger,
            "[security].allow_loopback_unauthenticated = false is not yet supported; all API \
             requests will be refused until token authentication is implemented"
        );
    }

    // A synchronous, pre-bound listener: errors surface before we log that
    // we are running, and the socket never accepts connections from anyone
    // before axum attaches.
    let listener = TcpListener::bind(bind_addr).map_err(|e| {
        format!(
            "could not bind {}: {e} (is another aborad already running?)",
            bind_addr
        )
    })?;
    // std sockets are blocking by default; tokio refuses to register them as-is.
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("could not configure listener: {e}"))?;
    info!(logger, "listening on {bind_addr} (loopback only)");

    let state = AppState::new(config, logger);
    let app = server::build(state);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("could not start async runtime: {e}"))?;

    runtime.block_on(async {
        let listener = tokio::net::TcpListener::from_std(listener)
            .map_err(|e| format!("could not register listener with async runtime: {e}"))?;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| format!("server error: {e}"))
    })
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}