{
  description = "Abora Framework - shared server foundation for Abora Cloud and Abora Atlas (Abora OS / NixOS)";

  inputs = {
    # Pin to a release branch for reproducibility; override with
    # `--override-input nixpkgs github:NixOS/nixpkgs/nixos-XX.YY` when
    # targeting a specific Abora OS release.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
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
        default = self.nixosModules.aborad;
        aborad = import ./nix/abora-module.nix;
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