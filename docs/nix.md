# Nix / NixOS integration

Abora OS is a NixOS distribution, so the Abora Framework ships a
[flake](../flake.nix) as a first-class integration point: derivations for
both binaries, a development shell with the full Rust toolchain, and a
NixOS module that runs `aborad` as a hardened systemd service.

> **flake.lock** — this repository does not commit `flake.lock` because the
> development host has no Nix toolchain. Run `nix flake lock` once, or let
> the first `nix develop` / `nix build` generate it, and consider committing
> it afterwards for reproducibility.

## Prerequisites

* Nix with flakes enabled (`experimental-features = nix-command flakes`),
  or a NixOS machine.

## Build the binaries

```sh
nix build .#aborad          # daemon
nix build .#abora           # CLI
nix build .#default         # same as .#aborad
nix build .#checks.default  # smoke: both binaries execute
nix flake check             # full check
```

`nix build .#aborad` runs the whole `cargo test` suite as part of the build
(`buildRustPackage`'s default `checkPhase`).

## Development shell

The distro Rust toolchain (`cargo`/`rustc` from apt) does not ship
`rustfmt` or `clippy`; the flake's shell does.

```sh
nix develop
./scripts/check.sh   # fmt + clippy + test (all present in the shell)
```

Packages: `rustPlatform.rust`, `clippy`, `rustfmt`, `rust-analyzer`, `jq`,
`curl`, `nixfmt-rfc-style` (`nix fmt` formats the Nix files).

## NixOS module

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    abora.url = "github:AboraOS-Project/Abora-Framework";
  };

  outputs = { self, nixpkgs, abora, ... }: {
    nixosConfigurations.myServer = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        abora.nixosModules.aborad
        {
          services.aborad = {
            enable = true;
            # Optional overrides; defaults are used otherwise.
            # settings = { system = { description = "foo"; }; };
          };
        }
      ];
    };
  };
}
```

What the module does:

* writes `/etc/abora/abora.toml` from `services.aborad.settings` (missing
  sections fall back to compiled-in defaults, see
  [configuration.md](configuration.md)),
* creates an unprivileged `aborad` user and a hardened systemd unit
  (`ProtectSystem=strict`, `NoNewPrivileges`, ... — same posture as
  [installer/systemd/aborad.service](../installer/systemd/aborad.service)),
* provisions bearer-token authentication (see below).

### Tokens

Two mutually exclusive options:

```nix
services.aborad = {
  enable = true;

  # (a) Declare tokens directly. Generate the hash with
  #     `nix develop -c cargo run -p abora -- auth generate-token --name=... --permission=read_all`
  authTokens = [
    { name = "operator"; permissions = [ "read_all" ];
      secretHash = "sha256:<hex-from-generate-token>"; }
    { name = "health-watcher"; permissions = [ "read:health" ];
      secretHash = "sha256:<hex-from-generate-token>"; }
  ];

  # (b) Point at a secret-manager file (sops-nix, agenix): same format as
  #     config/auth.toml.example.
  # authTokenFile = config.sops.secrets.abora-auth.path;
};
```

Option (a) writes `/var/lib/abora/auth.toml` (owner `aborad`, mode `0600`,
hash-only) via `ExecStartPre`. Only hashes ever touch disk — the plaintext
secret is printed exactly once by `generate-token` and must be distributed
out of band. Option (b) hands the file's location to `[security].token_file`
so the secret never enters the Nix store or this repository at all.

Without either option the daemon runs in preview mode: loopback clients are
trusted, remote clients are refused (see [security.md](security.md)).

### Firewall

Do not open a firewall port. `aborad` refuses non-loopback binds in this
milestone; `services.aborad.openFirewall = true` is rejected by assertion.

## Overlay

For `pkgs.aborad` / `pkgs.abora` anywhere in the evaluation (e.g. an Abora
OS image build):

```nix
{ nixpkgs.overlays = [ abora.overlays.default ]; }
```

## Pin nixpkgs

The flake pins `nixos-unstable`. For release-targeted builds (an Abora OS
release) override the input:

```sh
nix build .#aborad \
  --override-input nixpkgs github:NixOS/nixpkgs/nixos-25.11
```

Release branches of Abora OS will pin the matching nixpkgs here.