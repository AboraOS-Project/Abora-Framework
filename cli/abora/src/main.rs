//! `abora` — the Abora Framework command line client.
//!
//! The CLI talks only to the loopback daemon API. It contains no privileged
//! operations of its own and never runs commands on the host.

use std::path::PathBuf;
use std::process::ExitCode;

use abora_config::Config;
use clap::{Args, Parser, Subcommand};
use serde_json::Value;
use sha2::{Digest, Sha256};

use abora_core::{
    API_VERSION, CLI_NAME, DAEMON_NAME, DEFAULT_CONFIG_PATH, DEFAULT_LISTEN_ADDR, FRAMEWORK_NAME,
    FRAMEWORK_VERSION,
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
        /// Bearer token for authenticated daemons. Defaults to $ABORA_DAEMON_TOKEN.
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
    },
    /// Show update status, or apply updates (`abora updates apply`).
    Updates(UpdatesArgs),
    /// Validate configuration files (no daemon required).
    Config(ConfigArgs),
    /// Manage daemon bearer tokens (no daemon required).
    Auth(AuthArgs),
}

#[derive(Args)]
struct UpdatesArgs {
    /// Daemon base URL, e.g. `http://127.0.0.1:7360`.
    #[arg(long, value_name = "URL", global = true)]
    url: Option<String>,
    /// Bearer token. Defaults to $ABORA_DAEMON_TOKEN. Applying needs a token that grants `manage:updates`.
    #[arg(long, value_name = "TOKEN", global = true)]
    token: Option<String>,
    /// Print the raw JSON from the daemon instead of a summary.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<UpdatesCommand>,
}

#[derive(Subcommand)]
enum UpdatesCommand {
    /// Ask the daemon to upgrade the packages the last check listed. Shows the plan and asks first.
    Apply {
        /// Only show what would happen; request nothing.
        #[arg(long)]
        dry_run: bool,
        /// Do not ask for confirmation (required when not running in a terminal).
        #[arg(long, short = 'y')]
        yes: bool,
    },
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

#[derive(Args)]
struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Generate a bearer token and the `secret_hash` line to add to the
    /// daemon's token file ([security].token_file).
    GenerateToken {
        /// Human-readable label for the token (shows in logs).
        #[arg(long, default_value = "unnamed")]
        name: String,
        /// Permission id to grant, e.g. `--permission read:health`. Repeatable.
        /// Default: `read_all` (grants every permission).
        #[arg(long = "permission", value_name = "ID")]
        permissions: Vec<String>,
        /// Random secret length in bytes (16..=128).
        #[arg(long, default_value_t = 32)]
        secret_bytes: usize,
        /// Use an explicit secret instead of generating a random one.
        #[arg(long, value_name = "SECRET")]
        secret: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Version => cmd_version(),
        Command::Status { url, token } => cmd_status(url, token),
        Command::Updates(args) => cmd_updates(args),
        Command::Config(cfg) => match cfg.command {
            ConfigCommand::Check { path } => cmd_config_check(path),
        },
        Command::Auth(auth) => match auth.command {
            AuthCommand::GenerateToken {
                name,
                permissions,
                secret_bytes,
                secret,
            } => cmd_auth_generate_token(name, permissions, secret_bytes, secret),
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

fn cmd_status(url: Option<String>, token: Option<String>) -> ExitCode {
    let base = match resolve_daemon_url(url) {
        Ok(base) => base,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::FAILURE;
        }
    };
    let token = token.or_else(|| std::env::var("ABORA_DAEMON_TOKEN").ok());

    let health = match get_json(&format!("{base}/api/v1/health"), token.as_deref()) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("error: could not reach {DAEMON_NAME} at {base}\n  {msg}");
            eprintln!(
                "hint: is the daemon running? try `cargo run -p aborad` or start the systemd unit."
            );
            if msg.contains("bearer token") {
                eprintln!("hint: the daemon requires authentication; pass --token or set $ABORA_DAEMON_TOKEN.");
            }
            return ExitCode::FAILURE;
        }
    };
    let version = get_json(&format!("{base}/api/v1/version"), token.as_deref());
    let system = get_json(&format!("{base}/api/v1/system"), token.as_deref());

    let daemon = &health["daemon"];
    println!(
        "daemon:    {} {}",
        daemon["name"].as_str().unwrap_or("-"),
        version_str(&daemon["version"])
    );
    match &version {
        Ok(v) => println!("framework: {}", version_str(&v["framework_version"])),
        Err(msg) => println!("framework: unavailable ({msg})"),
    }
    println!(
        "status:    {}",
        health["status"].as_str().unwrap_or("unknown")
    );
    println!(
        "api:       {} ({})",
        health["api"]["name"].as_str().unwrap_or("-"),
        health["api"]["path"].as_str().unwrap_or("-")
    );
    println!(
        "uptime:    {}s (daemon since {})",
        health["uptime_seconds"].as_u64().unwrap_or(0),
        health["started_at"].as_str().unwrap_or("-")
    );

    match system {
        Ok(sys) => {
            println!("hostname:  {}", sys["hostname"].as_str().unwrap_or("-"));
            println!("os:        {}", os_pretty(&sys));
            println!(
                "kernel:    {}",
                sys["kernel"]["release"].as_str().unwrap_or("-")
            );
            println!(
                "arch:      {} ({})",
                sys["architecture"].as_str().unwrap_or("-"),
                sys["machine"].as_str().unwrap_or("-")
            );
            println!(
                "memory:    {}/{} bytes used",
                sys["memory"]["used_bytes"].as_u64().unwrap_or(0),
                sys["memory"]["total_bytes"].as_u64().unwrap_or(0)
            );
            println!(
                "uptime:    {}s (host)",
                sys["uptime_seconds"].as_u64().unwrap_or(0)
            );
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

/// How many available updates / history entries `abora updates` lists before summarising.
const SHOW_AVAILABLE: usize = 10;
const SHOW_HISTORY: usize = 5;

/// Human-readable lines for a `GET /api/v1/updates` body. Pure, so it can be tested.
fn format_updates(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let available = v["available"].as_array().map_or(&[][..], Vec::as_slice);

    let status = match v["status"]["state"].as_str() {
        None => "unknown (no check has finished yet)".to_owned(),
        Some("update_available") => format!("{} update(s) available", available.len()),
        Some("up_to_date") => "up to date".to_owned(),
        Some("installing") => "installing an update now".to_owned(),
        Some("error") => format!(
            "error: {}",
            v["status"]["message"].as_str().unwrap_or("unknown")
        ),
        Some(other) => other.to_owned(),
    };
    out.push(format!("status:     {status}"));
    out.push(format!(
        "last check: {}",
        v["last_check"].as_str().unwrap_or("never")
    ));

    if let Some(s) = v.get("schedule").filter(|s| s.is_object()) {
        let window = match s["in_maintenance_window"].as_bool() {
            Some(true) => "open",
            Some(false) => "closed",
            None => "unknown",
        };
        out.push(format!(
            "policy:     maintenance window {window}; applying is {}; checks every {}s",
            if s["apply_permitted"].as_bool() == Some(true) {
                "permitted now"
            } else {
                "not permitted now"
            },
            s["check_interval_seconds"].as_u64().unwrap_or(0)
        ));
    }

    if v["reboot"]["required"].as_bool() == Some(true) {
        out.push(format!(
            "reboot:     REQUIRED{}{}",
            v["reboot"]["pending_since"]
                .as_str()
                .map(|t| format!(" (since {t})"))
                .unwrap_or_default(),
            v["reboot"]["reason"]
                .as_str()
                .map(|r| format!(": {r}"))
                .unwrap_or_default()
        ));
    } else {
        out.push("reboot:     not required".to_owned());
    }

    if !available.is_empty() {
        out.push(String::new());
        out.push("available:".to_owned());
        for u in available.iter().take(SHOW_AVAILABLE) {
            out.push(format!(
                "  {}",
                u["summary"]
                    .as_str()
                    .or_else(|| u["component"].as_str())
                    .unwrap_or("-")
            ));
        }
        if available.len() > SHOW_AVAILABLE {
            out.push(format!(
                "  ... and {} more (use --json for the full list)",
                available.len() - SHOW_AVAILABLE
            ));
        }
    }

    let history = v["history"].as_array().map_or(&[][..], Vec::as_slice);
    if !history.is_empty() {
        out.push(String::new());
        out.push("history (newest first):".to_owned());
        for h in history.iter().take(SHOW_HISTORY) {
            out.push(format!(
                "  {} {} {}",
                h["applied_at"].as_str().unwrap_or("-"),
                if h["succeeded"].as_bool() == Some(true) {
                    "ok    "
                } else {
                    "FAILED"
                },
                h["note"].as_str().unwrap_or("")
            ));
        }
    }
    out
}

fn cmd_updates(args: UpdatesArgs) -> ExitCode {
    let base = match resolve_daemon_url(args.url) {
        Ok(base) => base,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::FAILURE;
        }
    };
    let token = args
        .token
        .or_else(|| std::env::var("ABORA_DAEMON_TOKEN").ok());

    match args.command {
        None => {
            let body = match get_json(&format!("{base}/api/v1/updates"), token.as_deref()) {
                Ok(v) => v,
                Err(msg) => return updates_error(&base, &msg),
            };
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&body).unwrap_or_default()
                );
            } else {
                for line in format_updates(&body) {
                    println!("{line}");
                }
            }
            ExitCode::SUCCESS
        }
        Some(UpdatesCommand::Apply { dry_run, yes }) => {
            apply_updates(&base, token.as_deref(), dry_run, yes, args.json)
        }
    }
}

fn updates_error(base: &str, msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    if msg.contains("HTTP 403") {
        eprintln!(
            "hint: this token does not grant the needed permission (`read:updates` to look, `manage:updates` to apply)."
        );
    } else if msg.contains("transport error") {
        eprintln!("hint: is the daemon running at {base}?");
    } else if msg.contains("bearer token") {
        eprintln!("hint: pass --token or set $ABORA_DAEMON_TOKEN.");
    }
    ExitCode::FAILURE
}

/// The plan lines for an apply dry run. Pure, so it can be tested.
fn format_plan(plan: &Value) -> Vec<String> {
    let packages: Vec<&str> = plan["packages"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let mut out = vec![format!("{} package(s) would be upgraded:", packages.len())];
    for chunk in packages.chunks(6) {
        out.push(format!("  {}", chunk.join(" ")));
    }
    out
}

fn apply_updates(
    base: &str,
    token: Option<&str>,
    dry_run: bool,
    yes: bool,
    json: bool,
) -> ExitCode {
    use std::io::{BufRead, IsTerminal, Write};

    let plan = match call_json(
        "POST",
        &format!("{base}/api/v1/updates/apply?dry_run=true"),
        token,
    ) {
        Ok(v) => v,
        Err(msg) => return updates_error(base, &msg),
    };
    if json && dry_run {
        println!(
            "{}",
            serde_json::to_string_pretty(&plan).unwrap_or_default()
        );
    }
    if plan["permitted"].as_bool() != Some(true) {
        eprintln!(
            "cannot apply now: {}",
            plan["reason"].as_str().unwrap_or("not permitted")
        );
        return ExitCode::FAILURE;
    }
    if !json || !dry_run {
        for line in format_plan(&plan) {
            println!("{line}");
        }
    }
    if dry_run {
        return ExitCode::SUCCESS;
    }

    if !yes {
        if !std::io::stdin().is_terminal() {
            eprintln!("refusing to apply without confirmation: run in a terminal, or pass --yes");
            return ExitCode::FAILURE;
        }
        print!("Apply these updates now? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
            println!("cancelled; nothing was requested");
            return ExitCode::FAILURE;
        }
    }

    match call_json("POST", &format!("{base}/api/v1/updates/apply"), token) {
        Ok(v) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else {
                println!(
                    "accepted (request {}). Follow it with `abora updates`: status shows `installing`, then a history entry appears.",
                    v["id"].as_str().unwrap_or("-")
                );
            }
            ExitCode::SUCCESS
        }
        Err(msg) => updates_error(base, &msg),
    }
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
fn get_json(url: &str, token: Option<&str>) -> Result<Value, String> {
    call_json("GET", url, token)
}

/// Perform a request (`GET`, or `POST` with no body) and return the JSON body, with useful errors.
fn call_json(method: &str, url: &str, token: Option<&str>) -> Result<Value, String> {
    // Return non-2xx responses instead of raising so we can read the API
    // error envelope out of the body.
    let config = ureq::config::Config::builder()
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let bearer = token.map(|t| format!("Bearer {t}"));
    let mut response = if method == "POST" {
        let mut request = agent.post(url);
        if let Some(bearer) = &bearer {
            request = request.header("Authorization", bearer);
        }
        request.send_empty()
    } else {
        let mut request = agent.get(url);
        if let Some(bearer) = &bearer {
            request = request.header("Authorization", bearer);
        }
        request.call()
    }
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

/// `abora auth generate-token` — produce a secret and its SHA-256 hash for
/// the daemon token file. The daemon only ever stores the hash.
fn cmd_auth_generate_token(
    name: String,
    permissions: Vec<String>,
    secret_bytes: usize,
    explicit_secret: Option<String>,
) -> ExitCode {
    let secret = match explicit_secret {
        Some(s) => s,
        None => match random_secret(secret_bytes) {
            Ok(s) => s,
            Err(msg) => {
                eprintln!("error: {msg}");
                return ExitCode::FAILURE;
            }
        },
    };
    let permissions = if permissions.is_empty() {
        vec!["read_all".to_owned()]
    } else {
        permissions
    };
    let hash = sha256_hex(&secret);

    println!("secret:      {secret}");
    println!("secret_hash: {hash}");
    println!();
    println!(
        "Append this to the file referenced by [security].token_file (e.g. /etc/abora/auth.toml):"
    );
    let perms = permissions
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    println!("  [[tokens]]");
    println!("  name = \"{name}\"");
    println!("  permissions = [{perms}]");
    println!("  secret_hash = \"{hash}\"");
    println!();
    println!("note: the secret is shown only once. The daemon stores hashes only.");
    ExitCode::SUCCESS
}

/// Uniform SHA-256 of a token secret, `sha256:<hex>` — same format the
/// daemon's token store uses.
fn sha256_hex(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    format!("sha256:{}", hex_encode(&digest))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Cryptographically random hex secret from `/dev/urandom`.
fn random_secret(byte_len: usize) -> Result<String, String> {
    if !(16..=128).contains(&byte_len) {
        return Err("secret length must be between 16 and 128 bytes".to_owned());
    }
    let mut bytes = vec![0u8; byte_len];
    {
        use std::io::Read;
        let mut source = std::fs::File::open("/dev/urandom")
            .map_err(|e| format!("could not open /dev/urandom: {e}"))?;
        source
            .read_exact(&mut bytes)
            .map_err(|e| format!("could not read /dev/urandom: {e}"))?;
    }
    Ok(hex_encode(&bytes))
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

    #[test]
    fn sha256_hex_matches_known_vector() {
        // sha256("test")
        assert_eq!(
            sha256_hex("test"),
            "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
    }

    #[test]
    fn hex_encode_is_lowercase_and_padded() {
        assert_eq!(hex_encode(&[0x0f, 0xaa, 0x01]), "0faa01");
        assert_eq!(hex_encode(&[0u8; 0]), "");
    }

    #[test]
    fn random_secret_is_expected_length_and_readable() {
        let secret = random_secret(32).unwrap();
        assert_eq!(secret.len(), 64);
        assert!(secret.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(random_secret(8).is_err());
    }

    fn sample() -> Value {
        serde_json::json!({
            "status": { "state": "update_available", "versions": ["a 1"] },
            "last_check": "2026-09-21T01:00:00Z",
            "reboot": { "required": true, "reason": "kernel", "pending_since": "2026-09-21T00:30:00Z" },
            "schedule": { "check_interval_seconds": 21600, "in_maintenance_window": true, "apply_permitted": true,
                          "installs_permitted": false, "reboot_permitted": true },
            "available": [ { "summary": "openssl: 1 -> 2 (x)", "component": "openssl" } ],
            "history": [ { "applied_at": "2026-09-20T03:00:00Z", "succeeded": false, "note": "apply x: 0 package(s); boom" } ]
        })
    }

    #[test]
    fn updates_summary_shows_status_policy_reboot_available_and_history() {
        let text = format_updates(&sample()).join("\n");
        assert!(text.contains("status:     1 update(s) available"), "{text}");
        assert!(text.contains("last check: 2026-09-21T01:00:00Z"));
        assert!(text
            .contains("maintenance window open; applying is permitted now; checks every 21600s"));
        assert!(text.contains("reboot:     REQUIRED (since 2026-09-21T00:30:00Z): kernel"));
        assert!(text.contains("  openssl: 1 -> 2 (x)"));
        assert!(text.contains("FAILED apply x: 0 package(s); boom"));
    }

    #[test]
    fn updates_summary_is_honest_before_the_first_check() {
        let v =
            serde_json::json!({ "reboot": { "required": false }, "available": [], "history": [] });
        let text = format_updates(&v).join("\n");
        assert!(
            text.contains("unknown (no check has finished yet)"),
            "{text}"
        );
        assert!(text.contains("last check: never"));
        assert!(text.contains("reboot:     not required"));
        assert!(
            !text.contains("policy:"),
            "no schedule yet, so no policy line"
        );
    }

    #[test]
    fn long_lists_are_cut_with_a_pointer_to_json() {
        let many: Vec<Value> = (0..25)
            .map(|i| serde_json::json!({ "summary": format!("pkg{i}") }))
            .collect();
        let v = serde_json::json!({ "status": { "state": "update_available" }, "available": many, "reboot": {}, "history": [] });
        let text = format_updates(&v).join("\n");
        assert!(text.contains("pkg9") && !text.contains("pkg10"));
        assert!(text.contains("and 15 more (use --json"));
    }

    #[test]
    fn the_plan_lists_packages_in_rows() {
        let plan = serde_json::json!({ "packages": ["a", "b", "c", "d", "e", "f", "g"] });
        assert_eq!(
            format_plan(&plan),
            ["7 package(s) would be upgraded:", "  a b c d e f", "  g"]
        );
    }
}
