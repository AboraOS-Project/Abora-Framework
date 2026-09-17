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
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use abora_config::{Config, ConfigError};
use abora_core::{API_VERSION, DAEMON_NAME, DEFAULT_CONFIG_PATH, FRAMEWORK_NAME, FRAMEWORK_VERSION};
use abora_log::{info, warn, Logger};

use crate::reload::ReloadSource;
use crate::state::{AppState, SharedState};
use crate::tokens::TokenStore;

mod auth;
mod errors;
mod handlers;
mod reload;
mod server;
mod state;
mod tokens;

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

/// Load the bearer-token store when configured. Authentication fails closed:
/// if `[security] token_file` is set but the file cannot be read or parsed,
/// the daemon refuses to start.
fn load_token_store(config: &Config, logger: &Logger) -> Result<Option<TokenStore>, String> {
    let Some(path) = &config.security.token_file else {
        warn!(
            logger,
            "[security].token_file not configured: loopback clients are trusted, remote \
             clients are refused. Generate tokens with `abora auth generate-token`; \
             see docs/security.md."
        );
        return Ok(None);
    };

    let store = TokenStore::load(path).map_err(|e| format!("{e}"))?;
    if !config.security.require_authentication {
        warn!(
            logger,
            "[security].require_authentication = false: tokens in {} are loaded but NOT enforced",
            path.display()
        );
    } else if store.is_empty() {
        warn!(
            logger,
            "token file {} contains no tokens; all API requests will be rejected with 401 \
             until tokens are added (see `abora auth generate-token`)",
            path.display()
        );
    } else {
        info!(
            logger,
            "token authentication enabled ({} token(s) from {})",
            store.len(),
            path.display()
        );
    }
    Ok(Some(store))
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

/// How often the background task checks watched files for changes.
const WATCH_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// `(mtime, length)` of a watched file — enough to detect edits.
type FileFingerprint = Option<(SystemTime, u64)>;

fn fingerprint(path: &Path) -> FileFingerprint {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok().map(|t| (t, m.len())))
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

    let reload_source = match &loaded.source {
        ConfigSource::Explicit(p) => ReloadSource::File(p.clone()),
        ConfigSource::Environment(p) => ReloadSource::File(p.clone()),
        ConfigSource::BuiltinDefaults => ReloadSource::Builtin,
    };

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

    let tokens = load_token_store(&config, &logger)?;

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

    let state = AppState::new(config, logger, tokens);
    let app = server::build(state.clone());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("could not start async runtime: {e}"))?;

    runtime.block_on(async {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(signal_and_watch_loop(state.clone(), reload_source, shutdown_tx));

        let listener = tokio::net::TcpListener::from_std(listener)
            .map_err(|e| format!("could not register listener with async runtime: {e}"))?;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await
        .map_err(|e| format!("server error: {e}"))
    })
}

/// Runs for the lifetime of the daemon: reloads configuration on `SIGHUP`
/// (and when watched files change on disk) and triggers a graceful shutdown
/// on `SIGTERM`/`SIGINT`.
async fn signal_and_watch_loop(state: SharedState, source: ReloadSource, shutdown: tokio::sync::oneshot::Sender<()>) {
    // Seed with the current fingerprint so the first tick is not a "change".
    let mut config_seen = source.file_path().and_then(fingerprint);
    let mut token_seen = fingerprint_of_token(state.as_ref());

    let mut watch = tokio::time::interval(WATCH_POLL_INTERVAL);
    watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        // Resolves exactly once per `SIGTERM` (graceful shutdown).
        let terminate = async {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                let mut sig = signal(SignalKind::terminate()).ok();
                loop {
                    if let Some(s) = sig.as_mut() {
                        if s.recv().await.is_some() {
                            break;
                        }
                    }
                }
            }
            #[cfg(not(unix))]
            std::future::pending::<()>().await;
        };

        // Resolves once per `SIGHUP`, then keeps the branch parked.
        let hangup = async {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                let mut sig = signal(SignalKind::hangup()).ok();
                if let Some(s) = sig.as_mut() {
                    let _ = s.recv().await;
                }
            }
            std::future::pending::<()>().await;
        };

        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate => break,
            _ = hangup => reload::reload(&state, &source),
            _ = watch.tick() => {
                let config_now = source.file_path().and_then(fingerprint);
                let token_now = fingerprint_of_token(state.as_ref());
                let config_changed = config_now != config_seen;
                let token_changed = token_now != token_seen;
                if config_changed || token_changed {
                    reload::reload(&state, &source);
                }
                // Re-seed regardless; a temporary read failure settles
                // back naturally on the next change.
                config_seen = config_now.or(config_seen);
                token_seen = token_now.or(token_seen);
            }
        }
    }

    let _ = shutdown.send(());
}

fn fingerprint_of_token(state: &AppState) -> FileFingerprint {
    let config = state.config.read().expect("config lock poisoned");
    config.security.token_file.as_deref().and_then(fingerprint)
}