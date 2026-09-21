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

`aborad` opens it at `[updates] state_file` and `GET /api/v1/updates` reads from it.

## Host packages (`HostPackageProvider`)

The first real provider, and deliberately **read-only**: it never installs anything (`apply`
keeps the trait's refusal).

| Call | How |
|------|-----|
| `check` | `apt-get -s -o Debug::NoLocking=1 upgrade`, a simulation that needs no root and changes nothing. Each `Inst` line becomes an `AvailableUpdate` (component = package, summary = `pkg: old -> new (source)`). |
| `status` | From the last `check`: `up_to_date`, `update_available` (`pkg version` strings) or `error`. Before any check it is a conflict ("no update check has run yet"), never a false "up to date". |
| `reboot_required` | `/var/run/reboot-required` (and `.pkgs` for the reason); remembered in the store with when it was first seen, cleared when the marker goes. |
| `history` | From the `UpdateStore`. |

Notes: it reads the package lists apt already has (refreshing them needs root and is left to
the system's own timers). Debian versions are not semver, so `AvailableUpdate.version` is a
best-effort `a.b.c` from the leading digits and the exact string is in the summary. apt does
not report sizes in a simulation, so `size_bytes` is `0` (unknown). Host packages have no
channels: the `channel` argument is echoed back. Programs are run without a shell, with fixed
arguments, `LC_ALL=C` and a 120 s timeout; package names are validated. Checked against this
development machine, it found the same 209 upgrades as `apt-get -s` itself.

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

`[updates]` (channel, automatic, check_interval, reboot_policy, state_file) and
`[maintenance]` (enabled, windows) live in the shared config (see
[docs/configuration.md](configuration.md)).

## The scheduler

`aborad` runs a background task (`services/aborad/src/scheduler.rs`) that wakes every
30 seconds, re-reads the configuration (so a reload applies within seconds) and:

1. **Checks** for updates: immediately at startup, then every `check_interval`. If a check
   fails it is retried after at most 15 minutes instead of waiting out the whole interval.
   Checks are read-only and are **not** limited to maintenance windows.
2. **Evaluates the policy** (`abora_config::schedule`, pure and unit tested) for the server's
   local time and publishes it as `schedule` in `GET /api/v1/updates`. Changes are logged.
3. **Applies automatically, only if you opted in** (`[updates] automatic = true`, default `false`):
   right after a check, if updates are available, the window is open, nothing is already pending
   and the last automatic attempt was at least one `check_interval` ago, it makes the same request
   `abora updates apply` would (caller `automatic (scheduler)`, audit line logged) and the same root
   helper does the work, re-checking everything itself. A failed apply is retried at most once per
   interval, never in a tight loop.

The policy:

| Question | Answer |
|----------|--------|
| Automatic apply permitted? | only if `[updates] automatic` (**off by default**) **and** `[maintenance] enabled` **and** the local time is inside a window. No windows means never. |
| Requested apply permitted? | `[maintenance] enabled` and inside a window; `automatic` is not needed. |
| Reboot permitted? | `reboot_policy = "always"`: yes. `"never"`: no. `"ask"` (default): only inside a window. |
| Local time unknown? | nothing is permitted (except `always` for reboots, which does not use the clock). |

Windows are half-open (`start <= now < end`), per weekday, and never cross midnight. The local
time comes from `date` (it honours `TZ`), because the standard library cannot ask.

## Applying updates

`POST /api/v1/updates/apply` (token with `manage:updates`) asks the separate root helper `abora-apply`
to upgrade the packages the last check listed. `aborad` stays unprivileged: it only writes a request
file, and a systemd path unit starts the helper. The helper re-checks the request, the maintenance
window and what apt still lists, runs one fixed `apt-get install --only-upgrade --no-remove` command
for the intersection, writes the result, and reboots only if the reboot policy allows it. The full
design, its safety rules and how to test it are in [docs/apply-design.md](apply-design.md).

### From the command line

```sh
export ABORA_DAEMON_TOKEN=<secret>     # a token with read:updates, and manage:updates to apply

abora updates                    # status, policy, reboot, what is available, recent history
abora updates --json             # the raw API body
abora updates apply --dry-run    # show the plan, request nothing
abora updates apply              # show the plan, ask "[y/N]", then request it
abora updates apply --yes        # no question (required when not run in a terminal)
```

`apply` always shows the plan first. Without `--yes` it asks for confirmation, and it refuses
outright when there is no terminal to ask on, so a script can never apply by accident. It exits
non-zero if the daemon refuses (nothing to apply, outside a maintenance window, already running).

## Not implemented (honestly)

* Automatic apply is opt-in and deliberately simple: no per-package rules, no security-only mode, no
  "only on the first Sunday" logic beyond the window.
* Only the apt provider exists.

## Roadmap order

1. Store: persisted update state + history (JSON file). **Done.**
2. Provider: read-only `check`/`status`/`history` for apt. **Done.**
3. Scheduler: poll + policy evaluation in `aborad`. **Done.**
4. API: `GET /api/v1/updates`. **Done.**
5. Apply: root helper + `POST /api/v1/updates/apply`, reboot behind `RebootPolicy`, opt-in automatic apply. **Done.**