# Security

This document is for operators and security reviewers. The full threat
model lives in [`docs/security.md`](docs/security.md).

## Security model

Abora Framework is **deny-by-default**. At this milestone:

* `aborad` binds **loopback only** and refuses at startup to bind a
  non-loopback address (remote management is not implemented).
* With `[security] token_file` configured, **every** API request needs a
  valid `Authorization: Bearer` token (SHA-256 hash-compared in constant
  time); per-token permissions decide `401` vs `403`.
* The HTTP API is **read-only** (`health`, `version`, `system`, `services`).
* **Non-loopback peers are rejected** (`403`) regardless of tokens.
* `GET /api/v1/updates` returns `501` — the update machinery is not faked.
* There is **no endpoint that executes commands or arbitrary code**, now or
  planned.
* Without a token file, loopback is trusted as a documented preview fallback.

## Hardening in place

* `#![forbid(unsafe_code)]` across the workspace.
* Token secrets are stored as SHA-256 hashes only (never plaintext) in a
  `0600` file; comparison is constant-time.
* Listener bound synchronously before the runtime serves traffic.
* Config validated and refused loudly before binding; a configured-but-broken
  token file refuses startup.
* Structured logs to stderr (journald); sensitive values serialized as
  `"<redacted>"` via `Redacted<T>`.
* Graceful shutdown on SIGINT/SIGTERM.

## Operator checklist

1. Keep `[security] require_authentication = true`.
2. Configure `[security] token_file` and generate tokens with
   `abora auth generate-token` (`config/auth.toml.example` shows the format);
   keep the file root-owned, mode `0600`.
3. Hand the plaintext secrets to consumers (e.g. `ABORA_DAEMON_TOKEN`) and
   never commit them.
4. Keep `[remote] listen_addr` on `127.0.0.1`/`::1`.
5. Restrict who can read `/etc/abora/abora.toml` and who can run the CLI.
6. Run the daemon under a dedicated, unprivileged account once packaged
   (see `installer/systemd/aborad.service`).

## Reporting a vulnerability

Report privately; do **not** open a public issue for an active or
unpatched vulnerability.

* Email the maintainers (contact listed on the GitHub repository).
* Include: affected version, affected endpoints/crates, a minimal
  reproduction, impact, and your suggested fix if any.
* Expect an acknowledgement within 72 hours and a coordinated disclosure
  timeline.

Prefer a private GitHub security advisory if you have repository access, or
create one at https://github.com/AboraOS-Project/Abora-Framework/security/advisories