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
   handlers never self-authorize. Tokens grant exactly the permissions they
   list.
5. **Surface before you bind.** Config errors, missing token files, and
   unfixable postures refuse startup loudly, before the socket listens.
6. **Secrets stay hashed.** The token file stores SHA-256 hashes only; the
   secrets never round-trip through the daemon and cannot be recovered from
   disk.

## Current posture (milestone 1)

| Property               | Guarantee                                             |
|------------------------|-------------------------------------------------------|
| Bind address           | Loopback only (`127.0.0.1` / `::1`). Non-loopback binds are **refused at startup**. |
| Admission              | With `[security] token_file` configured: every request needs a valid bearer token (`401` otherwise), and remote peers are still **refused unconditionally**. Without a token file: loopback is trusted (preview), remote is refused. |
| Authentication         | `Authorization: Bearer <secret>`, SHA-256 hash-compared in constant time. |
| Authorization          | Per-token permission grants; `read_all` or matching `read:*` ids. Missing grant → `403`. |
| Operations             | Read-only: `health`, `version`, `system`, `services`. |
| Updates                | `501` — not faked.                                    |
| Remote management      | Not implemented; `[remote].enabled` is logged as ignored. |
| Logging                | Structured to stderr (journald); sensitive values are `Redacted`. |
| Arbitrary execution    | No such endpoint exists or is planned.                |

## Authentication and authorization

### When is a token needed?

`[security] token_file` points at the token store. When set, **every** API
request (loopback included) must carry `Authorization: Bearer <secret>`.

* Missing header or unknown secret → `401` (never reveals whether a token
  exists).
* Known secret without the route's permission → `403`.
* A malformed or unreadable token file **fails daemon startup** (fail closed),
  as does a token file that exists but contains no tokens — that would
  otherwise brick the API with `401`s accidentally.

### Generating tokens

```sh
abora auth generate-token --name=operator --permission=read_all
```

prints a random secret (64 hex chars, 32 bytes from `/dev/urandom`), its
`secret_hash`, and the `[[tokens]]` block to paste into the token file.
Example file: [`config/auth.toml.example`](../config/auth.toml.example).

The daemon sees only the hash. Keep the generated secret in a secrets
manager, a `0600` file, or the platform agent's environment; never in
configs or logs.

### The preview fallback

If `require_authentication = true` (default) but **no** token file is
configured, loopback clients are treated as trusted — a stated preview
stopgap that keeps out-of-the-box local provisioning usable. It is logged
loudly at startup and non-loopback clients are still refused. It is removed
once token provisioning is mandatory in packaging.

### Non-loopback peers

Regardless of tokens, non-loopback connections are refused with `403`:
authenticated remote management does not exist yet and tokens are not a
backdoor around the bind boundary.

## What we never do

* Execute shell commands or arbitrary code from API input.
* Bind non-loopback addresses while unauthenticated-by-token.
* Leak secrets, tokens, or config values (see `Redacted` in `abora-log`).
* Store token secrets on disk in plaintext.
* Log request bodies or full error chains that could contain secrets.

## Startup checks

1. Config parses and validates.
2. Bind address is loopback (else: refuse).
3. Token file, when configured, loads and is valid (else: refuse).
4. Listener binds **before** the runtime accepts requests.
5. Shutdown on SIGINT/SIGTERM drains gracefully.

## Permissions reference

| Permission id  | Endpoint              |
|----------------|-----------------------|
| `read:health`  | `GET /api/v1/health`  |
| `read:version` | `GET /api/v1/version` |
| `read:system`  | `GET /api/v1/system`  |
| `read:services`| `GET /api/v1/services`|
| `read:updates` | `GET /api/v1/updates` |
| `read_all`     | all of the above      |

## Reporting a vulnerability

See `SECURITY.md` and `docs/contributing.md`. Report privately; do not open
public issues for active exploits.