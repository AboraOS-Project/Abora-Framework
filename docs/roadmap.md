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
- [x] Poll scheduler (`scheduler.rs` + `abora_config::schedule`): checks every `check_interval`; reports what `[maintenance]` windows, `automatic` and `reboot_policy` permit.
- [x] `GET /api/v1/updates` (status / available / reboot / history) replacing the `501`; checks run at startup and every `check_interval`.
- [x] **Applying updates**, manual first: root helper `abora-apply`, token-only `POST /api/v1/updates/apply` (with `dry_run`), reboot only when policy allows. Verified against real apt in throwaway Docker containers (`make test-apply`, and `make test-systemd` with systemd, the real installer and the path unit; both also in CI). Not yet tested: a reboot. See [apply-design.md](apply-design.md).
- [x] Automatic apply, **opt-in** (`[updates] automatic = true`, off by default; inside a window, at most once per `check_interval`). Verified on real apt in Docker, both off and on. The default of `automatic` changed from `true` to `false` so that nothing installs unattended on an existing config.

## Milestone 3.5 — a fork-ready NixOS base

- [x] `nixosModules.framework` (the minimal Abora Framework base) plus optional `nixosModules.anix`
  (imported from Abora OS, not copied) and `nixosModules.tinypm`.
- [x] `abora-framework-install`: an interactive/scriptable NixOS installer that asks whether to
  include ANIX and/or TinyPM, with `--generate-only` for a no-root dry run. Verified with
  `make test-nix` (input validation, all 4 ANIX/TinyPM combinations evaluate as real NixOS systems,
  both boot modes). **Not yet tested:** an actual `nixos-install` run in a VM.
- [x] A Nix-based `UpdateProvider` for Framework-based NixOS systems. Decision: call `nix` and
  `nixos-rebuild` directly (a Framework server is not necessarily an Abora OS desktop with ANIX or
  `abora update`). `check` refreshes the system flake's branch inputs read-only; `abora-apply`
  runs `nix flake update <inputs>` + `nixos-rebuild switch` and restores `flake.lock` if the
  rebuild fails. `[updates] provider = "auto"` picks it on NixOS. Checked against real Nix;
  **not yet tested:** a real apply on a NixOS machine.
- [ ] A dedicated installer ISO (`abora-framework-install` is a package today; Abora OS ships
  `abora-live-*` ISOs the same tools could follow).

## Milestone 3 — platform consumers

- [ ] `abora-cloud` / `abora-atlas` crates extending config with `[cloud.*]` / `[atlas.*]`.
- [x] Authenticated remote management over a locked-down TLS listener: `[remote] enabled` adds a TLS-only listener on `tls_listen_addr` where every request needs a token (no preview mode); refuses to start without cert, key and token file. `abora tls generate` makes a server certificate, `ABORA_CA_FILE` lets the CLI trust it. Verified with `make test-remote` (real binaries, curl and the CLI).
- [x] Installer packaging: hardened systemd unit (exposure 1.1 OK), `installer/install.sh` (dedicated `aborad` user, never overwrites your config, `--uninstall`/`--purge`), `scripts/check-installer.sh`. Real root install/uninstall verified in a systemd container (`make test-systemd`); no .deb/.rpm yet.

## Later ideas

- Component health registry feeding `GET /api/v1/health` — started: `AppState::components_health` reports `updates`; other subsystems still to register.
- Audit log for mutating operations (they do not exist yet; design first).
- Config layering/merge (`/etc` + drop-in files).
- i18n for human-readable messages (API strings stay English machine-readable).