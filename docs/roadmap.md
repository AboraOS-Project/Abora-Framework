# Roadmap

Current state: **milestone 0** — foundation (workspace, config, logging,
sysinfo, API skeleton, read-only daemon, CLI, docs).

## Milestone 1 — daemon basics

- [x] Token authentication (`Authorization: Bearer`) with per-permission tokens.
- [x] Remove `allow_loopback_unauthenticated` bridge (tokens or honest preview fallback).
- [x] Service registry: discover and report systemd units via `GET /api/v1/services`.
- [x] Filters/pagination for the service registry and per-service detail (`systemctl show`).
- [x] Request IDs (`request_id` in error envelope + `X-Request-ID` correlation).
- [x] Config reload (SIGHUP) and config file watching.
- [x] Structured panic logging: `abora_log::install_panic_hook`, wired into `aborad` (panics are JSON error records with thread, location and, with `RUST_BACKTRACE`, a backtrace).
- [x] Live ISO (`make iso` / `make run`): Limine + Linux + `aborad`, with the Abora logo and boot logs on screen.
- [x] CI pipeline (`.github/workflows/ci.yml`): fmt + clippy + test, live-ISO boot test; `ABORA_BUILD_COMMIT` stamped builds. Not yet run on GitHub.

## Milestone 2 — updates

- [x] Persistent update state + history store (`abora_update::UpdateStore`, used by `aborad` via `[updates] state_file`).
- [x] A real `UpdateProvider` implementation: `HostPackageProvider` (apt, read-only check; `apply` still refused).
- [x] Poll scheduler (`scheduler.rs` + `abora_config::schedule`): checks every `check_interval`; reports what `[maintenance]` windows, `automatic` and `reboot_policy` permit. Nothing is installed yet.
- [x] `GET /api/v1/updates` (status / available / reboot / history) replacing the `501`; checks run at startup and every `check_interval`.
- [x] **Applying updates**, manual first: root helper `abora-apply`, token-only `POST /api/v1/updates/apply` (with `dry_run`), reboot only when policy allows. Verified end to end with a fake `apt-get`; not yet run against real packages in a VM. Automatic apply is not implemented. See [apply-design.md](apply-design.md).

## Milestone 3 — platform consumers

- [ ] `abora-cloud` / `abora-atlas` crates extending config with `[cloud.*]` / `[atlas.*]`.
- [ ] Authenticated remote management over a locked-down TLS listener (only after token auth; guards `[remote].enabled`).
- [x] Installer packaging: hardened systemd unit (exposure 1.1 OK), `installer/install.sh` (dedicated `aborad` user, never overwrites your config, `--uninstall`/`--purge`), `scripts/check-installer.sh`. Real root install not yet run on a clean machine; no .deb/.rpm yet.

## Later ideas

- Component health registry feeding `GET /api/v1/health`.
- Audit log for mutating operations (they do not exist yet; design first).
- Config layering/merge (`/etc` + drop-in files).
- i18n for human-readable messages (API strings stay English machine-readable).