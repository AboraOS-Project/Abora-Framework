# Installer

Packaging for a systemd host (tested on Debian/Ubuntu).

## Install

```sh
cargo build --release -p aborad -p abora
sudo installer/install.sh            # create user, install, enable and start
journalctl -u aborad -f
abora status
```

`install.sh` creates the `aborad` system user (no login, no home), installs `aborad` to
`/usr/sbin` and `abora` to `/usr/bin`, installs the config to `/etc/abora/abora.toml`,
installs the unit, then enables and starts the service. Running it again is safe, and **it
never overwrites `/etc/abora/abora.toml`**: if you have edited it, the new default is written
next to it as `abora.toml.default`.

| Command | Effect |
|---------|--------|
| `sudo installer/install.sh --no-start` | install and enable, but do not start |
| `sudo installer/install.sh --uninstall` | stop and remove binaries and the unit; **keeps** `/etc/abora` and `/var/lib/abora` |
| `sudo installer/install.sh --uninstall --purge` | also remove config, state and the `aborad` user |
| `installer/install.sh --root DIR` | stage into `DIR` (no user, no ownership changes, systemd untouched), for packaging and tests |

## The systemd unit

[`systemd/aborad.service`](systemd/aborad.service) runs `aborad` as the unprivileged `aborad`
user with:

* no capabilities and `NoNewPrivileges`;
* read-only `/` (`ProtectSystem=strict`), no `/home`, private `/tmp` and `/dev`, `/proc` limited
  to its own processes;
* the only writable places are `/var/lib/abora` (update history, tokens; mode `0750`) and
  `/run/abora`; `/etc/abora` is read-only; files it creates are private (`UMask=0077`);
* no kernel, clock or hostname changes; no namespaces; no writable+executable memory;
  system calls limited to `@system-service` minus `@privileged` and `@resources`;
* network limited to loopback (`IPAddressDeny=any`, `IPAddressAllow=localhost`) and the
  address families it needs. **Loosen `IPAddress*` here when a provider must reach the network.**
* logs to journald, restarts on failure.

`systemd-analyze security` scores it **1.1 OK**.

## The update helper

Applying updates needs root, and `aborad` is deliberately not root. So a second, tiny program does it:

* `abora-apply.path` watches `/var/lib/abora/apply-request.json` (which `aborad` writes).
* `abora-apply.service` (root, one-shot) runs `abora-apply`. It validates the request itself,
  refuses outside a maintenance window, runs one fixed `apt-get` command and writes its result to
  `/var/lib/abora-apply/result.json` (root-owned; `aborad` can only read it).
* It is sandboxed **much less** than `aborad`, because installing packages needs wide write access.
  Its safety comes from what it will do: one kind of request, validated, policy re-checked.

See [docs/apply-design.md](../docs/apply-design.md).

## How it was checked

* `scripts/check-installer.sh` (also run in CI): the unit passes `systemd-analyze verify`, a
  staged install is complete, re-install keeps an edited config, `--uninstall` keeps data and
  `--purge` removes it. It needs no root and does not touch the system.
* The daemon was run for real under this unit's sandbox options as a transient user service:
  health, the apt update check, the systemd service listing and state writing all worked.
* The apply path was run end to end with a **fake `apt-get`** (real `aborad`, real systemd path unit, real helper):
  request accepted, status `installing`, helper ran with the exact expected arguments, history entry
  recorded, audit line logged, request file removed. No real package was installed.
* `make test-apply` upgrades a real `.deb` through the real daemon and helper in a throwaway Docker container
  (see `docs/apply-design.md`), including refusing forged requests and the opt-in automatic apply.
* **`make test-systemd`** (also in CI) is the real deployment test, in a throwaway container running systemd:
  the real installer as root (creates the `aborad` user, `/etc/abora` is `root:aborad 0750`), `aborad` under its
  real hardened unit as the unprivileged user (its apt check works inside the sandbox), `abora updates apply`,
  the **systemd path unit starting the root helper**, a real `.deb` upgraded, the result root-owned, history and
  audit recorded, the `aborad` user unable to write `/var/lib/abora-apply`, `/etc/abora` or `/usr/sbin`, an
  edited config kept on re-install, and the real `--uninstall` / `--purge`. The container has `CAP_SYS_ADMIN` but is
  not `--privileged`.
* **Not yet tested:** a reboot (the helper's `systemctl reboot` path), other distributions, and real hardware (`useradd`, `StateDirectory` ownership, `ProtectHome`)
  on a clean machine. Try it in a VM or container before relying on it.

## Why no .deb/.rpm yet

Packaging (deb/rpm/arch) belongs to the broader Abora v5 packaging effort, which spans more
than the framework daemon. `install.sh --root` stages exactly the file layout a package needs,
so that work can reuse it.
