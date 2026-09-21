# Update infrastructure

This document explains the *design* of update handling. Today only the
domain model exists (`crates/abora-update`); no real updater is implemented,
and the API endpoint returns `501`.

## Design goals

* One shared vocabulary for Cloud and Atlas: channels, maintenance windows,
  reboot policy, status, history.
* A single extension point — the `UpdateProvider` trait — where the actual
  mechanism (host packages, container images, tarballs, …) plugs in.
* Safety rules owned by framework, not reinvented per platform:
  * updates only inside maintenance windows,
  * reboot policy enforced (never reboot outside a window without consent),
  * updates applied atomically where possible, checksums verified,
  * no secrets in error messages.

## Domain model (`crates/abora-update`)

| Type                 | Purpose                                        |
|----------------------|------------------------------------------------|
| `Channel`            | `stable` / `beta` / `nightly` / custom         |
| `RebootPolicy`       | `ask` / `always` / `never`                     |
| `Weekday`            | Day names for windows                          |
| `MaintenanceWindow`  | Local-time `HH:MM`, half-open, per-run cap     |
| `AvailableUpdate`    | Version, component, size, checksums, notes     |
| `AvailableUpdates`   | What a provider found on a channel             |
| `UpdateStatus`       | `up_to_date`, `update_available`, `installing`, `error` |
| `RebootStatus`       | Whether a reboot is required and why           |
| `UpdateHistoryEntry` | applied_at / from / to / channel / success     |
| `UpdateError`        | not_implemented / unsupported / conflict / source |
| `UpdateProvider`     | The trait providers implement                  |

All types serialize, so they can travel over the daemon API once endpoints
exist.

## Persistent state (`UpdateStore`)

`abora_update::UpdateStore` keeps what must survive a restart: the update history (newest
500 entries), when the source was last checked, and whether a reboot is pending. It is one
JSON file (the caller picks the path; the intended default is `/var/lib/abora/updates.json`).

* Writes are atomic (temp file, `fsync`, rename) and the file is mode `0600`.
* A change only becomes visible in memory after it is safely on disk; a failed save leaves
  the state as it was and returns the error.
* A file that does not parse is moved to `updates.json.corrupt` and the store starts empty
  (`Opened::Recovered`). A file from a newer schema is refused, never overwritten.
* Timestamps come from the caller, so the store has no clock.

It is not connected to the daemon yet: the provider and scheduler (next roadmap items) will
own one and `GET /api/v1/updates` will read from it.

## The `UpdateProvider` trait

```rust
pub trait UpdateProvider: Send + Sync {
    fn check(&self, channel: &Channel) -> Result<AvailableUpdates, UpdateError>;
    fn status(&self) -> Result<UpdateStatus, UpdateError>;
    fn reboot_required(&self) -> Result<RebootStatus, UpdateError>;
    fn history(&self) -> Result<Vec<UpdateHistoryEntry>, UpdateError>;
    fn apply(&self, update: &AvailableUpdate) -> Result<(), UpdateError> { /* refuses by default */ }
}
```

`apply()` is *not* required for this milestone — the default implementation
returns `NotImplemented` until a provider opts in. This keeps the interface
implementable now while guaranteeing no accidental updates.

## Configuration flow

`[updates]` (channel, automatic, check_interval, reboot_policy) and
`[maintenance]` (enabled, windows) live in the shared config (see
[docs/configuration.md](configuration.md)). The scheduler will:

1. Poll `provider.check(channel)` at `check_interval`.
2. Gate installations behind `automatic && maintenance enabled && inside window`.
3. Respect `reboot_policy` for the reboot step.
4. Append to history (newest first); expose via future API endpoints.

## Not implemented (honestly)

* No poll scheduling, no provider, no install, no reboot.
* `GET /api/v1/updates` returns `501 Not Implemented` with a reference to
  this document.

## Roadmap order

1. Store: persisted update state + history (SQLite or JSON file).
2. Provider: implement `check`/`status`/`history` for a real source.
3. Scheduler: poll + window gating in `aborad`.
4. API: `GET /api/v1/updates/status`, `.../available`, `.../history`.
5. Apply: `apply()` + reboot orchestration behind `RebootPolicy`.