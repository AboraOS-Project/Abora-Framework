# Architecture

This document describes how the Abora Framework is organised and how its
parts fit together.

## Goals

* One shared foundation for Abora Cloud and Abora Atlas.
* Secure by default: loopback-only, authenticated, read-only for now.
* No arbitrary remote command execution, ever.
* Versioned, additive API that both platforms build on.
* Structured, journald-friendly logging.
* First-class, tested configuration.

## Workspace layout

Cargo workspace with library and binary crates:

```
crates/
  abora-core     Shared constants and the `Version` type. No platform code.
  abora-config   Strongly-typed TOML configuration + validation + extensions.
  abora-log      Structured logging (JSON/text) to stderr, lazy records.
  abora-sysinfo  Read-only system information (Linux, /proc + os-release).
  abora-api      Versioned API data types + error envelope. No HTTP dependency.
  abora-update   Update domain model (channels, windows, policy) + provider trait.
  abora-services Read-only service registry (systemd discovery).
services/
  aborad         The framework daemon: HTTP API + auth, correlation-id, tracing layers.
cli/
  abora          The command line client (talks to the loopback daemon only).
config/          Shipped default configuration file.
docs/            This documentation set.
scripts/         Developer / CI helpers.
installer/       Packaging (systemd unit) and install documentation.
```

Dependency direction is outward: `aborad` (or a platform) depends on the
crates; crates never depend on `aborad`. `abora-core` is the leaf every
crate may depend on.

## Runtime flow (`aborad`)

1. Parse CLI args: `--config`, `--version`, `--help`.
2. Resolve configuration:
   `--config` flag → `$ABORA_CONFIG` → `/etc/abora/abora.toml` → compiled-in
   defaults. The source is logged.
3. Validate the configuration; refuse to start on invalid values.
4. Enforce the security posture: the bind address must be a loopback
   address. A non-loopback bind is refused — remote management is not
   implemented, and the daemon will not pretend otherwise.
5. Build the logger (level/format from config), set it global. The
   *level* is mutable at runtime (config reload); the *format* is not.
6. Bind the listener *before* starting the async runtime, so bind errors
   surface synchronously and the socket never accepts before being handled.
7. Start a multi-thread tokio runtime and serve axum on
   `/api/v1`, shutting down gracefully on SIGINT/SIGTERM.
8. A background task reloads configuration on `SIGHUP` and on file-watch
   changes (config + token file, 2s poll): a fresh `Config` and `TokenStore`
   are swapped in under `RwLock`s (handlers and the auth middleware read the
   live copy), and the log threshold is updated. Invalid reloads keep the
   previous configuration. `base_path`/`listen_addr`/log-format changes
   require a restart and are flagged in the logs.

## HTTP API

See [docs/daemon-api.md](daemon-api.md) for the full reference.

* Path prefix `/api/v1` (configurable via `[api] base_path`).
* Read-only endpoints: `health`, `version`, `system`, `services`.
* `updates` intentionally returns `501 Not Implemented` — the model exists
  (`abora-update`) but the machinery does not, and we won't fake it.
* Every route is guarded by a per-operation permission enforced in a
  middleware layer. Today only loopback clients are admitted.

## Service registry

`abora-services` isolates all knowledge of the local init manager. Its
`ServiceCollector` trait plus a Linux systemd backend (`systemctl` with a
fixed argument list — no user input, no injection) produce the `ServiceStatus`
values served by `GET /api/v1/services`. Discovery is strictly read-only and
reports `503` honestly on hosts without systemd. Swapping in another init
manager means adding a sibling backend, not touching handlers or API types.

## Authorization model

`crates/…/aborad/src/auth.rs` defines a small `Permission` enum and the pure
`authorize()` decision function: peer address + bearer token + token store →
allow/deny. Handlers never check authorization themselves; the server
middleware does, per route. Keeping the decision pure makes it directly
unit-testable.

Tokens live in a separate store (`src/tokens.rs`) whose file references
SHA-256 hashes only; secrets are compared in constant time and never
persisted in plaintext. Each token grants an explicit set of permission ids
or the `read_all` wildcard. Non-loopback peers are refused unconditionally;
loopback is trusted only when no token store is configured (preview). See
[docs/security.md](security.md).

## Configuration model

`abora-config` owns the framework sections (`system`, `updates`,
`maintenance`, `remote`, `security`, `logging`, `api`, `features`). Unknown
keys inside these sections are rejected (`deny_unknown_fields`), while
*unknown top-level sections* are preserved verbatim under
`Config::extension` so Abora Cloud / Atlas can hang their own `[cloud.*]` /
`[atlas.*]` tables off the same file. See [docs/configuration.md](configuration.md).

## Logging

`abora-log` writes one structured record per line to stderr, which systemd
captures into journald. JSON is the default format. Sensitive values are
wrapped in `Redacted<T>`, which always serializes as `<redacted>`. See
[docs/configuration.md](configuration.md#logging).

Panics are logged the same way: `aborad` installs `abora_log::install_panic_hook`, so a
panic is one JSON error record (`component: "panic"`, with `thread`, `location` and, when
`RUST_BACKTRACE` is set, `backtrace`) rather than free text that a log parser would miss.

## Security

Threat model and guarantees are in [docs/security.md](security.md) and the
root [`SECURITY.md`](../SECURITY.md).

## Update infrastructure

The domain model (channels, maintenance windows, reboot policy, status,
history, provider trait) lives in `abora-update`. Delivery mechanics are
not implemented yet; see [docs/update.md](update.md).