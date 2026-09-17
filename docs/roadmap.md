# Roadmap

Current state: **milestone 0** — foundation (workspace, config, logging,
sysinfo, API skeleton, read-only daemon, CLI, docs).

## Milestone 1 — daemon basics

- [x] Token authentication (`Authorization: Bearer`) with per-permission tokens.
- [x] Remove `allow_loopback_unauthenticated` bridge (tokens or honest preview fallback).
- [ ] Service registry: discover and report systemd units via `GET /api/v1/services`.
- [ ] Request IDs (`request_id` in error envelope).
- [ ] Config reload (SIGHUP) and config file watching.
- [ ] Structured panic/log-of-record wiring (already partially in place).
- [ ] CI pipeline: fmt + clippy + test on push; `ABORA_BUILD_COMMIT` stamped builds.

## Milestone 2 — updates

- [ ] Persistent update state + history store.
- [ ] A real `UpdateProvider` implementation (host packages first).
- [ ] Poll scheduler honouring `[maintenance]` windows and `reboot_policy`.
- [ ] `GET /api/v1/updates` (status / available / history) replacing the `501`.

## Milestone 3 — platform consumers

- [ ] `abora-cloud` / `abora-atlas` crates extending config with `[cloud.*]` / `[atlas.*]`.
- [ ] Authenticated remote management over a locked-down TLS listener (only after token auth; guards `[remote].enabled`).
- [ ] Installer packaging: systemd unit, dedicated `aborad` user, hardening (`NoNewPrivileges`, `ProtectSystem`, …).

## Later ideas

- Component health registry feeding `GET /api/v1/health`.
- Audit log for mutating operations (they do not exist yet; design first).
- Config layering/merge (`/etc` + drop-in files).
- i18n for human-readable messages (API strings stay English machine-readable).