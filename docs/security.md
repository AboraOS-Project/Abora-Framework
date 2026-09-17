# Security

The threat model and the guarantees that hold at this milestone. The short
version for operators lives in the root [`SECURITY.md`](../SECURITY.md).

## Principles

1. **Deny by default.** Nothing is reachable, none is trusted, until proven
   otherwise.
2. **No arbitrary remote execution, ever.** There is and will be no endpoint
   that runs shell commands or executes user-supplied payloads.
3. **Be honest about scope.** Where machinery is not implemented, the API
   says so explicitly (e.g. `501` for updates) instead of faking success.
4. **Least privilege.** Every route declares the single permission it needs;
   handlers never self-authorize.
5. **Surface before you bind.** Config errors and unfixable postures refuse
   startup loudly, before the socket is listening.

## Current posture (milestone 0)

| Property               | Guarantee                                             |
|------------------------|-------------------------------------------------------|
| Bind address           | Loopback only (`127.0.0.1` / `::1`). Non-loopback binds are **refused at startup**. |
| Peer admission         | Non-loopback peers are rejected with a `403`.         |
| Operations             | Read-only: `health`, `version`, `system`, `services`. |
| Updates                | `501` — not faked.                                    |
| Remote management      | Not implemented; `[remote].enabled` is logged as ignored. |
| Logging                | Structured to stderr (journald); sensitive values are `Redacted`. |
| Arbitrary execution    | No such endpoint exists or is planned.                |

## The loopback trust boundary

This milestone treats loopback connections as trusted because token
authentication is not implemented yet (`[security] require_authentication` is
`true`, `allow_loopback_unauthenticated` is the temporary bridge).

* Any local process can already act as that user; the API additionally
  surfaces rich system facts, and we limit blast radius by keeping it
  read-only and loopback-bound.
* Putting this flag to `false` **today** would refuse *everything* — that is
  the honest behaviour until tokens exist, and the config logs a warning if
  you try.

## Token authentication (next milestone)

* `[security] token_file` already exists in the config namespace.
* Loopback-unauthenticated mode will be removed once
  `Authorization: Bearer <token>` is enforced per operation with
  per-permission tokens.
* Tokens will be stored `0600`, hash-compared (never logged), and
  invalidatable without restart.

## What we never do

* Execute shell commands or arbitrary code from API input.
* Bind non-loopback addresses while unauthenticated-by-token.
* Leak secrets, tokens, or config values (see `Redacted` in `abora-log`).
* Log request bodies or full error chains that could contain secrets.

## Intrusion checks carried out at startup

1. Config parses and validates.
2. Bind address is loopback (else: refuse).
3. Listener is bound **before** the runtime accepts requests, so we never
   start degraded and quiet about it.
4. Shutdown on SIGINT/SIGTERM drains gracefully.

## Reporting a vulnerability

See the `SECURITY.md` file and the `docs/contributing.md` guidance. Report
privately; do not open public issues for active exploits.