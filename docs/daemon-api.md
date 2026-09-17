# Daemon API

The `aborad` daemon exposes a small, versioned, read-only HTTP API. Full
machine-readable reference: [`crates/abora-api`](../crates/abora-api/src/lib.rs).

## Versioning policy

* Current version is `v1`, served under `/api/v1` (`[api] base_path`).
* Adding optional fields or new endpoints stays **in** the current version.
* Removing/renaming fields, changing semantics, or changing status codes for
  existing conditions requires a new version.
* Clients must ignore unknown fields.

## Endpoints

| Method | Path                  | Status          |
|--------|-----------------------|-----------------|
| GET    | `/api/v1/health`      | implemented     |
| GET    | `/api/v1/version`     | implemented     |
| GET    | `/api/v1/system`      | implemented     |
| GET    | `/api/v1/services`    | implemented (systemd) |
| GET    | `/api/v1/updates`     | `501` planned   |

All endpoints require loopback source addresses today (see
[docs/security.md](security.md)). When `[security] token_file` is
configured, every request must also carry
`Authorization: Bearer <secret>` (`401` without a valid token, `403` without
the route's permission). With no token file configured, loopback is trusted
(preview mode).

## `GET /api/v1/health`

```json
{
  "status": "ok",
  "daemon": { "name": "aborad", "version": {"major":0,"minor":1,"patch":0} },
  "api": { "name": "v1", "path": "/api/v1" },
  "uptime_seconds": 12,
  "started_at": "2026-09-16T10:00:00.000Z",
  "components": []
}
```

`status` is `ok`, `degraded`, or `maintenance`. `components` grows as
subsystems register a health check.

## `GET /api/v1/version`

```json
{
  "framework_name": "Abora Framework",
  "framework_version": { "major": 0, "minor": 1, "patch": 0 },
  "daemon": { "name": "aborad", "version": { "major": 0, "minor": 1, "patch": 0 } },
  "api": { "name": "v1", "path": "/api/v1" },
  "git_commit": null
}
```

`git_commit` is populated when CI builds with `ABORA_BUILD_COMMIT` set.

## `GET /api/v1/system`

Reads the live host (`/proc`, `/etc/os-release`). No host actions.

```json
{
  "hostname": "node-01",
  "architecture": "x86_64",
  "machine": "x86_64",
  "uptime_seconds": 123456,
  "os": { "pretty_name": "Pop!_OS 24.04 LTS", "id": "pop", "version_id": "24.04" },
  "kernel": { "name": "Linux", "release": "6.10.5", "version": "#1 SMP PREEMPT" },
  "memory": { "total_bytes": 33554432000, "available_bytes": 10000000000, "used_bytes": 23554432000 },
  "cpu": { "model": "AMD Ryzen 9 ...", "cores": 16, "speed_mhz": 3400 },
  "framework_version": { "major": 0, "minor": 1, "patch": 0 }
}
```

Optional fields (`description`, `hostname_override`, parts of `os`/`cpu`)
are omitted (not `null`) when unknown.

## `GET /api/v1/services`

Read-only list of managed services, discovered through the local init
manager — systemd on Linux (`systemctl`, a fixed, read-only command set; no
user input ever reaches the shell). Sorted by unit name.

```json
[
  { "name": "acpid.service", "state": "stopped", "enabled": false,
    "description": "ACPI event daemon" },
  { "name": "accounts-daemon.service", "state": "running", "enabled": true,
    "description": "Accounts Service" },
  { "name": "dbus.service", "state": "running",
    "description": "D-Bus System Message Bus" }
]
```

* `state`: `running`, `stopped`, `failed`, `activating`, `deactivating`,
  `unknown` (mapped from systemd's `ActiveState`).
* `enabled`: `true` for `enabled`/`enabled-runtime`, `false` for
  `disabled`/`masked`, `null` (omitted) for `static`/`indirect`/generated
  units that are pulled in by dependencies rather than enabled directly.
* `description`: the unit's Description, omitted when absent.

On a host without a compatible init manager the endpoint returns
**`503 ServiceUnavailable`** (with `code: "service_unavailable"`) rather
than an empty list that would imply success.

## `GET /api/v1/updates`

Returns **`501 Not Implemented`** — the update model is designed
(`crates/abora-update`), the delivery machinery is not. We will not fake
status, see [docs/update.md](update.md).

## Errors

Every non-2xx response uses the same envelope:

```json
{
  "error": {
    "code": "not_implemented",
    "message": "the update system is planned but not implemented in this milestone; see docs/update.md",
    "request_id": null
  }
}
```

`code` is one of `invalid_request`, `unauthorized`, `forbidden`, `not_found`,
`conflict`, `not_implemented`, `service_unavailable`, `internal`, mapped 1:1
to the conventional HTTP status.

## Implementation notes

* Axum handler functions; authorization lives in a per-route middleware
  layer, never inside handlers.
* `request_id` is reserved for correlation; not yet generated.
* `[api] max_payload_bytes` and `request_timeout_secs` are enforced by the
  server skeleton (limits validated at config load).

## Authentication examples

With a token file configured:

```sh
curl -H "Authorization: Bearer <secret>" http://127.0.0.1:7360/api/v1/health
ABORA_DAEMON_TOKEN=<secret> abora status
```