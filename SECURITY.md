# Security

This document is for operators and security reviewers. The full threat
model lives in [`docs/security.md`](docs/security.md).

## Security model

Abora Framework is **deny-by-default**. At this milestone:

* `aborad` binds **loopback only** and refuses at startup to bind a
  non-loopback address (remote management is not implemented).
* The HTTP API is **read-only** (`health`, `version`, `system`, `services`).
* **Non-loopback peers are rejected** (`403`); token authentication lands in
  the next milestone.
* `GET /api/v1/updates` returns `501` — the update machinery is not faked.
* There is **no endpoint that executes commands or arbitrary code**, now or
  planned.

## Hardening in place

* `#![forbid(unsafe_code)]` across the workspace.
* Listener bound synchronously before the runtime serves traffic.
* Config validated and refused loudly before binding.
* Structured logs to stderr (journald); sensitive values serialized as
  `"<redacted>"` via `Redacted<T>`.
* Graceful shutdown on SIGINT/SIGTERM.

## Operator checklist

1. Keep `[security] require_authentication = true`.
2. Keep `[remote] listen_addr` on `127.0.0.1`/`::1`.
3. Do not set `[security] allow_loopback_unauthenticated = false` until token
   auth exists — the daemon will refuse all requests (by design).
4. Restrict who can read `/etc/abora/abora.toml` and who can run the CLI.
5. Run the daemon under a dedicated, unprivileged account once packaged
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