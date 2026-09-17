//! `abora` — the Abora Framework command line client.
//!
//! The CLI talks only to the loopback daemon API. It contains no privileged
//! operations of its own and never runs commands on the host.

use std::path::PathBuf;
use std::process::ExitCode;

use abora_config::Config;
use clap::{Args, Parser, Subcommand};
use serde_json::Value;

use abora_core::{
    CLI_NAME, DAEMON_NAME, DEFAULT_LISTEN_ADDR, DEFAULT_CONFIG_PATH, FRAMEWORK_NAME,
    FRAMEWORK_VERSION, API_VERSION,
};

#[derive(Parser)]
#[command(
    name = CLI_NAME,
    version = FRAMEWORK_VERSION,
    about = "Abora Framework command line client"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print framework and CLI version information (no daemon required).
    Version,
    /// Query a running `aborad` daemon for status and version.
    Status {
        /// Daemon base URL, e.g. `http://127.0.0.1:7360`.
        #[arg(long, value_name = "URL")]
        url: Option<String>,
    },
    /// Validate configuration files (no daemon required).
    Config(ConfigArgs),
}

#[derive(Args)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Parse and validate a configuration file.
    Check {
        /// Path to the configuration file. Defaults to `$ABORA_CONFIG` or
        /// `/etc/abora/abora.toml`.
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Version => cmd_version(),
        Command::Status { url } => cmd_status(url),
        Command::Config(cfg) => match cfg.command {
            ConfigCommand::Check { path } => cmd_config_check(path),
        },
    }
}

fn cmd_version() -> ExitCode {
    println!("{FRAMEWORK_NAME} {FRAMEWORK_VERSION}");
    println!("client: {CLI_NAME} {}", abora_core::Version::current());
    println!("daemon: {DAEMON_NAME} (query `abora status` for the running version)");
    println!("api:    {API_VERSION}");
    ExitCode::SUCCESS
}

fn cmd_status(url: Option<String>) -> ExitCode {
    let base = match resolve_daemon_url(url) {
        Ok(base) => base,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::FAILURE;
        }
    };

    let health = match get_json(&format!("{base}/api/v1/health")) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("error: could not reach {DAEMON_NAME} at {base}\n  {msg}");
            eprintln!("hint: is the daemon running? try `cargo run -p aborad` or start the systemd unit.");
            return ExitCode::FAILURE;
        }
    };
    let version = get_json(&format!("{base}/api/v1/version"));
    let system = get_json(&format!("{base}/api/v1/system"));

    let daemon = &health["daemon"];
    println!("daemon:    {} {}", daemon["name"].as_str().unwrap_or("-"), version_str(&daemon["version"]));
    match &version {
        Ok(v) => println!("framework: {}", version_str(&v["framework_version"])),
        Err(msg) => println!("framework: unavailable ({msg})"),
    }
    println!("status:    {}", health["status"].as_str().unwrap_or("unknown"));
    println!("api:       {} ({})", health["api"]["name"].as_str().unwrap_or("-"), health["api"]["path"].as_str().unwrap_or("-"));
    println!("uptime:    {}s (daemon since {})", health["uptime_seconds"].as_u64().unwrap_or(0), health["started_at"].as_str().unwrap_or("-"));

    match system {
        Ok(sys) => {
            println!("hostname:  {}", sys["hostname"].as_str().unwrap_or("-"));
            println!("os:        {}", os_pretty(&sys));
            println!("kernel:    {}", sys["kernel"]["release"].as_str().unwrap_or("-"));
            println!("arch:      {} ({})", sys["architecture"].as_str().unwrap_or("-"), sys["machine"].as_str().unwrap_or("-"));
            println!("memory:    {}/{} bytes used", sys["memory"]["used_bytes"].as_u64().unwrap_or(0), sys["memory"]["total_bytes"].as_u64().unwrap_or(0));
            println!("uptime:    {}s (host)", sys["uptime_seconds"].as_u64().unwrap_or(0));
        }
        Err(msg) => println!("system:    unavailable ({msg})"),
    }

    ExitCode::SUCCESS
}

fn os_pretty(sys: &Value) -> String {
    let os = &sys["os"];
    let pretty = os["pretty_name"].as_str();
    let name = os["name"].as_str();
    let version = os["version"].as_str();
    match (pretty, name, version) {
        (Some(p), _, _) => p.to_owned(),
        (None, Some(n), Some(v)) => format!("{n} {v}"),
        (None, Some(n), None) => n.to_owned(),
        _ => os["id"].as_str().unwrap_or("unknown").to_owned(),
    }
}

/// Render the structured API `Version` object (or any string fallback) as
/// `major.minor.patch[-prerelease][+build]`.
fn version_str(v: &Value) -> String {
    let Some(major) = v["major"].as_u64() else {
        return v.as_str().unwrap_or("-").to_owned();
    };
    let Some(minor) = v["minor"].as_u64() else {
        return v.as_str().unwrap_or("-").to_owned();
    };
    let Some(patch) = v["patch"].as_u64() else {
        return v.as_str().unwrap_or("-").to_owned();
    };
    let mut s = format!("{major}.{minor}.{patch}");
    if let Some(p) = v["prerelease"].as_str() {
        s.push('-');
        s.push_str(p);
    }
    if let Some(b) = v["build"].as_str() {
        s.push('+');
        s.push_str(b);
    }
    s
}

fn cmd_config_check(path: Option<PathBuf>) -> ExitCode {
    let config = match load_config(path) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("config check failed: {msg}");
            return ExitCode::FAILURE;
        }
    };

    match config.validate() {
        Ok(()) => {
            println!("config OK");
            for line in config.summary() {
                println!("  {line}");
            }
            ExitCode::SUCCESS
        }
        Err(errors) => {
            eprintln!("config invalid:");
            for e in errors {
                eprintln!("  - {e}");
            }
            ExitCode::FAILURE
        }
    }
}

fn load_config(explicit: Option<PathBuf>) -> Result<Config, String> {
    if let Some(path) = explicit {
        return Config::load(path).map_err(|e| e.to_string());
    }
    if let Ok(path) = std::env::var("ABORA_CONFIG").map(PathBuf::from) {
        return Config::load(path).map_err(|e| e.to_string());
    }
    let default = PathBuf::from(DEFAULT_CONFIG_PATH);
    if default.exists() {
        return Config::load(default).map_err(|e| e.to_string());
    }
    Err(format!(
        "no configuration file found at {DEFAULT_CONFIG_PATH}; pass a path or set $ABORA_CONFIG"
    ))
}

fn resolve_daemon_url(explicit: Option<String>) -> Result<String, String> {
    if let Some(u) = explicit {
        return Ok(u.trim_end_matches('/').to_owned());
    }
    if let Ok(u) = std::env::var("ABORA_DAEMON_URL") {
        return Ok(u.trim_end_matches('/').to_owned());
    }

    let addr = match load_config(None) {
        Ok(cfg) => cfg.remote.listen_addr,
        Err(_) => DEFAULT_LISTEN_ADDR.to_owned(),
    };
    if addr.starts_with("http://") || addr.starts_with("https://") {
        return Ok(addr.trim_end_matches('/').to_owned());
    }
    Ok(format!("http://{addr}"))
}

/// Perform a GET request and return the JSON body, with useful errors.
fn get_json(url: &str) -> Result<Value, String> {
    // Return non-2xx responses instead of raising so we can read the API
    // error envelope out of the body.
    let config = ureq::config::Config::builder()
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| format!("transport error: {e}"))?;
    let status = response.status().as_u16();

    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("could not read response body: {e}"))?;

    if status >= 400 {
        if let Ok(v) = serde_json::from_str::<Value>(&body) {
            if let Some(err) = v.get("error") {
                return Err(format!(
                    "HTTP {status}: {} ({})",
                    err["message"].as_str().unwrap_or("unknown error"),
                    err["code"].as_str().unwrap_or("unknown")
                ));
            }
        }
        return Err(format!("HTTP {status}: {body}"));
    }

    serde_json::from_str(&body).map_err(|e| format!("invalid JSON from {url}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_url_resolution_defaults_to_loopback() {
        let base = resolve_daemon_url(Some("http://127.0.0.1:7360/".into())).unwrap();
        assert_eq!(base, "http://127.0.0.1:7360");
    }

    #[test]
    fn os_pretty_prefers_pretty_name() {
        let v = serde_json::json!({
            "os": { "pretty_name": "Pop!_OS 24.04 LTS", "id": "pop" }
        });
        assert_eq!(os_pretty(&v), "Pop!_OS 24.04 LTS");
        let v = serde_json::json!({ "os": { "id": "unknown" } });
        assert_eq!(os_pretty(&v), "unknown");
    }
}