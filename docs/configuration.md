# Configuration

Abora Framework reads one TOML file: `/etc/abora/abora.toml` by default,
overridable with `--config <path>` (daemon) / a positional path (CLI) or the
`ABORA_CONFIG` environment variable.

The shipped defaults live in [`config/abora.toml.default`](../config/abora.toml.default)
and are guaranteed (by a parity test) to match the compiled-in defaults.

## Resolution order

1. Explicit CLI flag / positional argument.
2. `$ABORA_CONFIG`.
3. `/etc/abora/abora.toml`.
4. Nothing — compiled-in defaults (the daemon logs this).

The human-readable summary of the *applied* configuration is printed by
`abora config check` and logged at daemon startup, so operators can see what
actually took effect.

## Reload

The daemon re-reads the configuration file without a restart on `SIGHUP` and
on file change (config + token files are watched and polled every 2s). Most
settings apply immediately; `[api] base_path`, `[remote] listen_addr` and
the log `format` require a restart (the daemon warns if they changed). A
reload that fails to parse/validate keeps the previous configuration.
See [docs/daemon-api.md](daemon-api.md#configuration-reload).

## Validation

Loading always parses *and* validates. Invalid config is rejected loudly:

* unknown keys inside framework sections (typo protection),
* non-socket `listen_addr`,
* malformed `base_path`,
* zero `max_payload_bytes` / `request_timeout_secs`,
* invalid `check_interval` durations and maintenance windows,
* empty/whitespace hostname overrides.

`abora config check <path>` validates a file without starting the daemon.

## Sections

### `[system]` — identity

| Key           | Type   | Default | Meaning                          |
|---------------|--------|---------|----------------------------------|
| `hostname`    | string | none    | Override shown by `GET /api/v1/system` (default: OS hostname) |
| `description` | string | none    | Free-form description            |

### `[updates]` — automatic update behaviour

| Key             | Type            | Default  | Meaning                        |
|-----------------|-----------------|----------|--------------------------------|
| `channel`       | string / table  | `stable` | `stable`, `beta`, `nightly`, or `{ custom = "name" }` |
| `automatic`     | bool            | `true`   | Install available updates automatically (inside windows) |
| `check_interval`| string          | `6h`     | Poll interval: `<n>s`, `<n>m`, `<n>h`, or `<n>d` |
| `reboot_policy` | string          | `ask`    | Reboot outside a window: `ask`, `always`, `never` |

Not yet implemented end-to-end; see [docs/update.md](update.md).

### `[maintenance]` — scheduled windows

| Key      | Type  | Default | Meaning                         |
|----------|-------|---------|---------------------------------|
| `enabled`| bool  | `true`  | Enforce maintenance windows     |
| `windows`| array | `[]`    | List of windows (below)         |

Each window:

| Field                | Type    | Meaning                             |
|----------------------|---------|-------------------------------------|
| `days`               | array   | `monday` … `sunday` (lowercase)     |
| `start`              | string  | Local-time `HH:MM` (inclusive start) |
| `end`                | string  | Local-time `HH:MM` (exclusive end)   |
| `max_duration_minutes`| int    | Upper bound for one update run      |

```toml
[maintenance]
windows = [
  { days = ["saturday", "sunday"], start = "02:00", end = "04:00", max_duration_minutes = 90 },
]
```

Windows are half-open: work may start at `start` and must finish before `end`.

### `[remote]` — remote management (reserved)

| Key                 | Type   | Default            | Meaning                  |
|---------------------|--------|--------------------|--------------------------|
| `enabled`           | bool   | `false`             | Reserved; ignored        |
| `listen_addr`       | string | `127.0.0.1:7360`    | Socket to bind. Loopback only, for now |
| `advertise_hostname`| string | none               | Optional discovery FQDN  |

The daemon **refuses to start** if `listen_addr` is not a loopback address.
Remote management is not implemented; see [docs/security.md](security.md).

### `[security]`

| Key                      | Type   | Default | Meaning                                |
|--------------------------|--------|---------|----------------------------------------|
| `require_authentication` | bool   | `true`  | Require a valid bearer token for every API operation |
| `token_file`             | path   | none    | Bearer-token store (`0600`, hashes only) |

When `token_file` is set, tokens are enforced for all clients (loopback
included). Generate entries with:

```sh
abora auth generate-token --name=operator --permission=read_all
# -> paste the [[tokens]] block into the token file
```

See `config/auth.toml.example` and [docs/security.md](security.md). Without
a token file, loopback clients are trusted as a preview fallback and remote
clients are refused; the daemon logs a warning. A configured-but-unreadable
token file refuses startup.

### `[logging]`

| Key     | Type   | Default | Meaning                           |
|---------|--------|---------|-----------------------------------|
| `level` | string | `info`  | `off`, `error`, `warn`, `info`, `debug`, `trace` |
| `format`| string | `json`  | `json` (one object per line) or `text` |

Logs go to **stderr**, which systemd captures into journald (`journalctl -u aborad`).

### `[api]` — daemon HTTP API limits

| Key                    | Type    | Default    | Meaning                    |
|------------------------|---------|------------|----------------------------|
| `base_path`            | string  | `/api/v1`  | API version path prefix   |
| `max_payload_bytes`    | integer | `1048576`  | Max request body size      |
| `request_timeout_secs` | integer | `30`       | Per-request timeout        |

### `[features]` — dynamic flags

Any `name = true/false` pairs. Framework reserves nothing here; read with
`Config::feature("name")`.

## Extensions for Cloud / Atlas

Framework owns the sections above. Any *other top-level table* is preserved
verbatim and accessible by dotted name, so both platforms configure
themselves in the same file without touching framework source:

```toml
[cloud.telemetry]
enabled = true
```

Rust-side:

```rust
let cfg = abora_config::Config::load("/etc/abora/abora.toml")?;
let telemetry: Telemetry = cfg.typed_extension("cloud.telemetry")?.unwrap_or_default();
```

Unknown keys inside framework sections are still rejected — extension tables
must sit at the top level.