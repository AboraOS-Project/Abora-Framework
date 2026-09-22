{
  description = "Abora Framework - shared server foundation for Abora Cloud and Abora Atlas (Abora OS / NixOS)";

  inputs = {
    # Pin to a release branch for reproducibility; override with
    # `--override-input nixpkgs github:NixOS/nixpkgs/nixos-XX.YY` when
    # targeting a specific Abora OS release.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # ANIX comes from the Abora OS flake (one ANIX, not a copy). It is only fetched for systems that
    # import `nixosModules.anix`; a system that leaves ANIX out never evaluates it.
    abora-os = {
      url = "github:AnimatedGTVR/Abora-OS/stable";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # TinyPM (`grab` / `tinypm`), taken as plain source and built here. Same rule: only used by
    # systems that import `nixosModules.tinypm`.
    tinypm = {
      url = "github:AnimatedGTVR/TinyPM";
      flake = false;
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      abora-os,
      tinypm,
    }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);
      pkgsFor = system: nixpkgs.legacyPackages.${system};
      version = "0.1.0";

      # A derivation exposing only one of the framework's two binaries via a
      # `$out/bin/<name>` symlink, for `nix run` and `pkgs.<name>` ergonomics.
      binOnly = pkgs: name: framework:
        pkgs.runCommand "${name}-${version}" { } ''
          mkdir -p $out/bin
          ln -s ${framework}/bin/${name} $out/bin/${name}
          ${framework}/bin/${name} --version >/dev/null
        '';
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          framework = pkgs.callPackage ./nix/aborad-package.nix { };
        in
        rec {
          default = aborad;
          abora-framework = framework;
          aborad = binOnly pkgs "aborad" framework;
          abora = binOnly pkgs "abora" framework;
          # The installer program, also on the installer ISO.
          installer = pkgs.callPackage ./nix/installer.nix { };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            name = "abora-dev";

            packages = with pkgs; [
              rustPlatform.rust
              clippy
              rustfmt
              rust-analyzer
              jq
              curl
              nixfmt-rfc-style
            ];

            # The distro Rust toolchain on the development host lacks
            # fmt/clippy; the Nix shell does not.
            shellHook = ''
              echo "Abora Framework dev shell (rustfmt, clippy, rust-analyzer available)"
              echo "Checks: ./scripts/check.sh   |   Nix check: nix flake check"
            '';
          };
        }
      );

      checks = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          framework = pkgs.callPackage ./nix/aborad-package.nix { };
        in
        {
          # `cargo test` already runs during the package build; this verifies
          # both shipped binaries actually execute.
          default = pkgs.runCommand "abora-smoke" { nativeBuildInputs = [ framework ]; } ''
            set -euo pipefail
            aborad --version >/dev/null
            abora version
            touch $out
          '';
        }
      );

      nixosModules = {
        # The Framework base system: aborad + the abora command. Start here.
        default = self.nixosModules.framework;
        framework = import ./nix/framework-module.nix;
        # Just the daemon's service definition (no base system around it).
        aborad = import ./nix/abora-module.nix;
        # Optional parts: import them, or leave them out.
        anix = import ./nix/anix-module.nix { inherit abora-os; };
        tinypm = import ./nix/tinypm-module.nix { inherit tinypm; };
      };

      overlays = {
        default =
          final: _prev:
          let
            framework = final.callPackage ./nix/aborad-package.nix { };
          in
          {
            inherit framework;
            abora-framework = framework;
            aborad = binOnly final "aborad" framework;
            abora = binOnly final "abora" framework;
          };
      };

      formatter = forAllSystems (system: (pkgsFor system).nixfmt-rfc-style);
    };
}