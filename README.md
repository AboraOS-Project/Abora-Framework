# Abora Framework

The shared server foundation behind **Abora Cloud** and **Abora Atlas**.

A single reliable base for system services, configuration, logging, security
defaults, a versioned HTTP API, update infrastructure, and installer
components — so both platforms build on the same foundation instead of
reinventing it.

## Used By

* Abora Cloud
* Abora Atlas

## Status

Early development for Abora v5 — **milestone 0: foundation**.

## What exists today

| Piece              | State                                                        |
|--------------------|--------------------------------------------------------------|
| Workspace          | 7 library crates + 2 binaries                                |
| `aborad` daemon    | Loopback-only, read-only API: health, version, system, services |
| Authentication     | Per-permission bearer tokens (SHA-256 hashed, constant-time)  |
| Service registry   | systemd discovery via `abora-services` (read-only, `503` honest) |
| `abora` CLI        | `version`, `status`, `config check`, `auth generate-token`   |
| Configuration      | Strongly-typed TOML, validated, Cloud/Atlas-extensible        |
| Logging            | Structured JSON/text to stderr (journald-friendly), redaction |
| System info        | Linux backend from `/proc` + `/etc/os-release`               |
| Update model       | Channels, windows, reboot policy, provider trait (no delivery yet) |
| Docs & packaging   | docs/, SECURITY.md, systemd unit, dev/check scripts          |
| Nix / NixOS        | flake: packages, devShell, hardened NixOS module, overlay    |

## Security posture

Deny-by-default. Non-loopback binds are refused; `401`/`403` bearer-token
authentication is enforced whenever a token file is configured (hash-stored,
constant-time compared), and there is **no arbitrary command-execution
endpoint now or planned**. Loopback-only preview fallback is documented and
logged. See [SECURITY.md](SECURITY.md) and [docs/security.md](docs/security.md).

## Quick start

Requirements: Rust edition 2021 toolchain (`cargo`; rustup-managed preferred).
On NixOS / Nix machines, prefer the flake instead (see
[docs/nix.md](docs/nix.md)).

```sh
# Build and test
cargo build --workspace
cargo test --workspace
./scripts/check.sh

# Start the daemon with the shipped default config
cargo run -p aborad -- --config config/abora.toml.default

# Point a CLI at it
cargo run -p abora -- status

# Validate a config without starting the daemon
cargo run -p abora -- config check config/abora.toml.default

# Talk to it directly
curl http://127.0.0.1:7360/api/v1/health
```

### Enforce token authentication

1. Generate a token (daemon stores only the hash):
   ```sh
   cargo run -p abora -- auth generate-token --name=operator --permission=read_all
   ```
2. Paste the printed `[[tokens]]` block into a root-owned, `0600` file and
   point `[security] token_file` at it (template:
   `config/auth.toml.example`).
3. Restart `aborad`; every request now needs `Authorization: Bearer <secret>`
   — the CLI reads it from `--token` or `$ABORA_DAEMON_TOKEN`.

Install as a service (root):

```sh
sudo install -Dm644 config/abora.toml.default /etc/abora/abora.toml
sudo install -Dm644 installer/systemd/aborad.service /etc/systemd/system/aborad.service
sudo systemctl daemon-reload && sudo systemctl enable --now aborad
journalctl -u aborad -f
```

## Repository layout

```
crates/abora-core    Shared constants, Version type        (leaf)
crates/abora-config  TOML config + validation + extensions
crates/abora-log     Structured logging (Redacted support)
crates/abora-sysinfo Read-only system information (Linux)
crates/abora-api     Versioned API types + error envelope   (pure data)
crates/abora-update  Update domain model + provider trait
crates/abora-services Read-only service registry (systemd)   (new)
services/aborad      The daemon (axum, read-only API)
cli/abora            Command line client (loopback only)
nix/                 Flake package derivation + NixOS module
config/              Shipped default configuration + auth.toml.example
docs/                Architecture, config, API, security, update, nix
installer/           systemd unit + install notes
scripts/             check.sh / dev.sh helpers
```

See [docs/architecture.md](docs/architecture.md) for the dependency model
and runtime flow.

## Documentation

* [docs/architecture.md](docs/architecture.md) — layout, runtime, auth model
* [docs/configuration.md](docs/configuration.md) — every config section
* [docs/daemon-api.md](docs/daemon-api.md) — HTTP API contract
* [docs/security.md](docs/security.md) — threat model and guarantees
* [docs/update.md](docs/update.md) — update design (not yet delivered)
* [docs/nix.md](docs/nix.md) — Nix / NixOS builds and the `services.aborad` module
* [docs/roadmap.md](docs/roadmap.md) — what comes next
* [SECURITY.md](SECURITY.md) — vulnerability reporting

## Reporting vulnerabilities

Report privately, see [SECURITY.md](SECURITY.md). Do not open public issues
for active exploits.

## License

Apache-2.0.