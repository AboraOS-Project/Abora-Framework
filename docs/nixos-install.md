# Installing a NixOS system built on Abora Framework

Abora Framework is meant to be the **minimal base you fork**: a small NixOS system (`aborad` and the
`abora` command) that you build your own OS on top of, the way Abora OS itself does. `abora-framework-install`
partitions a disk, writes a small flake, and installs it. Two optional pieces can be imported, or not:

* **ANIX**, Abora OS's system management CLI (`anix status`, `anix switch`, `anix rollback`, ...),
  imported straight from the [Abora OS flake](https://github.com/AnimatedGTVR/Abora-OS) — there is one
  ANIX, not a copy.
* **TinyPM** (`grab`, `tinypm`), built from the [TinyPM repository](https://github.com/AnimatedGTVR/TinyPM).

Neither is required. A "base" install has just `aborad` and `abora`.

## Running the installer

Boot a NixOS installer medium with `abora-framework-install` on its `PATH` (see
[Building the tools into an ISO](#building-the-tools-into-an-iso) below — the Framework does not yet
publish its own installer ISO), then:

```sh
sudo abora-framework-install
```

It asks for a disk, hostname, administrator account and password, time zone, and whether to import
ANIX and TinyPM, shows the plan, and asks you to type the disk path back to confirm before erasing
anything. Non-interactively:

```sh
sudo abora-framework-install \
  --disk /dev/sda --hostname myhost --user alice --password-hash "$(mkpasswd -m sha-512)" \
  --timezone Europe/Berlin --anix --no-tinypm --yes
```

`--generate-only DIR` writes `flake.nix` and `configuration.nix` into `DIR` without touching a disk or
needing root — a safe way to see exactly what an install would contain, or to hand-tune it before
running `nixos-install` yourself. See `abora-framework-install --help` for every option.

## What you get

```nix
{
  inputs.abora-framework.url = "github:AboraOS-Project/Abora-Framework";
  # ...
  modules = [
    abora-framework.nixosModules.framework   # always: aborad + abora
    abora-framework.nixosModules.anix        # only if you chose ANIX
    abora-framework.nixosModules.tinypm      # only if you chose TinyPM
  ];
}
```

`flake.nix` and `configuration.nix` land in `/etc/etc/nixos` (well, `/etc/nixos`) and are yours from
then on: edit them and `sudo nixos-rebuild switch --flake /etc/nixos#<hostname>` as with any NixOS
system. `aboraFramework.enable` (in `configuration.nix`) turns the base on; `services.aborad.*` (see
[docs/nix.md](nix.md)) configures the daemon itself.

## The three modules

| Module | Gives you | Source |
|--------|-----------|--------|
| `nixosModules.framework` (default) | `aboraFramework.enable`; turns on `services.aborad` and installs `abora` | this repo |
| `nixosModules.anix` | `anix.*` options and the `anix` command, with desktop-oriented defaults (Bluetooth, printing, audio, unfree packages) turned **off** to match a minimal base | imported from `abora-os.nixosModules.anix` (Abora OS's `stable` branch) |
| `nixosModules.tinypm` | `grab` and `tinypm` on `PATH` | built from the TinyPM repository |

Import only `nixosModules.aborad` instead of `framework` if you want the daemon's systemd service on
its own, with no opinion about the rest of the system (see [docs/nix.md](nix.md)).

### Why the ANIX module looks the way it does

Abora OS's own `anix.nix` sets a few options (`abora.desktop`, `abora.wallpaper`, `abora.gaming.*`)
that only exist on the full Abora OS distribution (`nixosModules.installed-base`). Importing ANIX
alone, on a system that is not full Abora OS, would fail to evaluate ("The option `abora.desktop` does
not exist"). The Framework's `nix/anix-module.nix` declares harmless placeholders for exactly those
options so the import is safe standalone; they do nothing here. If you *also* import Abora OS's
`installed-base` module, use `abora-os.nixosModules.anix` directly instead of the Framework's wrapper.

## Building the tools into an ISO

`nix build github:AboraOS-Project/Abora-Framework#installer` builds `abora-framework-install` on its
own; put it on the `PATH` of any NixOS installer image (for example a plain `nixos-installer` ISO with
one extra `environment.systemPackages` entry) to get an Abora-Framework-aware installer. A dedicated
live ISO (the way Abora OS ships `abora-live-*`) is not built yet.

## How this was tested

`make test-nix` (`scripts/test-nix-docker.sh`, also run in CI) builds `abora-framework-install`
(shellcheck runs as part of the build), evaluates the flake, checks that the installer refuses hostile
input (hostnames, user names, time zones, password hashes and flake references that could break out of
the generated Nix files) before writing anything, and generates and evaluates all four combinations of
ANIX and TinyPM as real NixOS systems, plus both boot modes. It does not partition a disk or run
`nixos-install` — that needs a VM, and is not yet tested.
