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
| GET    | `/api/v1/updates`     | implemented (read-only, apt) |
| POST   | `/api/v1/updates/apply` | implemented (token with `manage:updates` only) |

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
  "components": [
    { "name": "updates", "status": "ok" }
  ]
}
```

`status` is `ok`, `degraded`, or `maintenance`. Overall `status` is `degraded`
when any component is degraded. `components` grows as subsystems register a
health check; today that is `updates` (degraded when the last update check
failed, with the error in `detail`).

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

### Filters and pagination

All parameters are optional; unknown parameters and bad values are
`400 invalid_request` (a typo never silently returns everything).

| Parameter | Values | Meaning |
|-----------|--------|---------|
| `state`   | `running`, `stopped`, `failed`, `activating`, `deactivating`, `unknown` | Only units in that state |
| `enabled` | `true`, `false` | Only units with that `enabled` hint (units with no hint never match) |
| `q`       | text | Case-insensitive substring of name or description |
| `limit`   | 1 to 1000 | Page size (default: everything) |
| `offset`  | number | Matches to skip before the page |

The body stays a plain array. The number of matches **before** `limit`/`offset`
is returned in the `X-Total-Count` response header.

```
GET /api/v1/services?state=failed&limit=20&offset=0
```

## `GET /api/v1/services/{name}`

Detail for one unit from `systemctl show`. `{name}` must be a unit name ending
in `.service` (`400` otherwise); an unknown unit is `404 not_found`. Requires the
same `read:services` permission as the list.

```json
{ "name": "cron.service", "load_state": "loaded", "active_state": "active",
  "sub_state": "running", "enabled": true, "description": "Regular background program processing daemon",
  "fragment_path": "/usr/lib/systemd/system/cron.service",
  "exec_start": "/usr/sbin/cron -f -P", "main_pid": 812, "memory_bytes": 2412544 }
```

`load_state`, `active_state` and `sub_state` are systemd's raw values. Every
other field is omitted when systemd does not report it.

## `GET /api/v1/updates`

Read-only update information. Nothing is installed, and this call runs no check: the daemon
checks (with `apt-get -s`, a simulation) at startup and then every `[updates] check_interval`, and this reports
the result. See [docs/update.md](update.md).

```json
{
  "status": { "state": "update_available", "versions": ["openssl 3.0.13-0ubuntu3.5"] },
  "last_check": "2026-09-21T00:56:07.391Z",
  "reboot": { "required": false },
  "schedule": { "check_interval_seconds": 21600, "in_maintenance_window": false,
                "installs_permitted": false, "reboot_permitted": false },
  "available": [
    { "channel": "stable", "version": { "major": 3, "minor": 0, "patch": 13, "prerelease": null, "build": null },
      "component": "openssl",
      "summary": "openssl: 3.0.13-0ubuntu3.4 -> 3.0.13-0ubuntu3.5 (Ubuntu:24.04/noble-updates [amd64])",
      "size_bytes": 0 }
  ],
  "history": []
}
```

* `status` and `last_check` are **omitted until the first check has finished**, and are never
  guessed. `status.state` is `up_to_date`, `update_available`, `installing` or `error`
  (`error` carries a `message`, for example when `apt-get` is missing).
* `schedule` is what the update policy permits at the moment it was last evaluated (about every 30 s; omitted
  until the first evaluation). `apply_permitted` says a requested apply would be accepted now; `installs_permitted`
  is about *automatic* installs, which are not implemented, so nothing acts on it. See
  [docs/update.md](update.md#the-scheduler).
* `reboot.required` reflects `/var/run/reboot-required`; `pending_since` and `reason` are set when known.
* `available` is what the last check found (`size_bytes: 0` means unknown; the exact Debian
  version is in `summary`). `history` is applied updates, newest first: one entry per
  apply run, with the packages counted and any note in `note`.
* State is kept in `[updates] state_file`. If that cannot be opened the daemon warns and keeps
  it in memory only.

## `POST /api/v1/updates/apply`

Ask for the packages the last check listed to be upgraded. **Changes the system**, so it needs a
bearer token that grants `manage:updates`; it is refused in preview mode (no token file) and for
`read_all` tokens (see [docs/security.md](security.md#mutating-permissions)).

| Query | Meaning |
|-------|---------|
| `dry_run=true` | Only report what would happen. Nothing is requested. |

```json
{ "dry_run": false, "permitted": true, "id": "apply-a4f0f0137dfbdcc9",
  "packages": ["libc6", "openssl"] }
```

* A real request answers **`202 Accepted`**: the work is done by a separate root helper
  (`abora-apply`, see [docs/apply-design.md](apply-design.md)). Follow it in `GET /api/v1/updates`:
  `status` is `installing` while it runs, then a `history` entry with the same `id` in its `note`
  appears (about 30 s after it finishes).
* A dry run answers `200` with `permitted` and, if not permitted, a `reason`.
* **`409 conflict`** when nothing is available (check first), an apply is already running, or the
  local time is outside a maintenance window (`[maintenance]` must be enabled and a window open;
  `[updates] automatic` is not required).
* `400` for an unknown query parameter or a bad `dry_run` value.
* The request contains only package names taken from the last check; there is no way to pass a
  command, path or option.

## Errors

Every non-2xx response uses the same envelope:

```json
{
  "error": {
    "code": "not_implemented",
    "message": "the update system is planned but not implemented in this milestone; see docs/update.md",
    "request_id": "4f2d99b17c0e8a51"
  }
}
```

`code` is one of `invalid_request`, `unauthorized`, `forbidden`, `not_found`,
`conflict`, `not_implemented`, `service_unavailable`, `internal`, mapped 1:1
to the conventional HTTP status.

## Correlation ids

Every response carries an `X-Request-ID` header. If the caller sends a
`X-Request-ID` themselves it is echoed back unchanged (so downstream/tracing
tools can correlate); otherwise a random 16-hex-character id is generated
once per request. Error envelopes additionally embed the same id as
`error.request_id`, and the daemon prefixes its request log lines with it —
report all three when filing a bug. Caller-supplied ids must be 1–128
characters of `[A-Za-z0-9._:-]`; anything else is ignored and replaced.
Unknown routes (HTTP 404) return the same envelope with
`code: "not_found"`, so every non-2xx response is consistent machine-readable
JSON.

## Configuration reload

The daemon re-reads its configuration file without a restart in two ways:

* **`SIGHUP`** — explicit, immediate reload.
* **File watching** — every 2 seconds the config file (and the token file,
  when set) are checked for modification and reloaded automatically.

A reload applies immediately: the log threshold,
`[system] hostname`/`description`, `[updates]*`, `[maintenance]`,
`[security] require_authentication` **and** `[security] token_file` (a new
or edited token file is picked up; a broken one keeps the previous store so
the daemon never locks itself out). The `abora` CLI's `auth generate-token`
writes the token file, so a reload activates new tokens promptly.

Two settings require a **restart** and are only warned about if changed:
`[api] base_path` (the router is built at startup) and
`[remote] listen_addr` (the socket is bound at startup), plus the log
`format`. A failed reload (unparseable file, invalid values) never stops the
daemon — the previous configuration keeps serving and the error is logged.
With compiled-in defaults (no config file) there is nothing to reload;
`SIGHUP` logs that fact.

## Implementation notes

* Axum handler functions; authorization lives in a per-route middleware
  layer, never inside handlers.
* `request_id` is populated by the outermost middleware layer on every
  non-2xx `application/json` response, after the handler has produced the
  envelope.
* `[api] max_payload_bytes` and `request_timeout_secs` are enforced by the
  server skeleton (limits validated at config load).

## Authentication examples

With a token file configured:

```sh
curl -H "Authorization: Bearer <secret>" http://127.0.0.1:7360/api/v1/health
ABORA_DAEMON_TOKEN=<secret> abora status
```