# Builds the whole Abora Framework Cargo workspace from one derivation so
# both binaries (`aborad`, `abora`) share a compile of every dependency.
#
# `cargo test` runs as part of the build via buildRustPackage's default
# checkPhase, so `nix build .#aborad` also runs the test suite.
{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "abora-framework";
  version = "0.1.0";

  src = lib.cleanSource ../.;

  cargoLock.lockFile = ../Cargo.lock;

  # Build the whole virtual workspace; buildRustPackage then installs every
  # binary target (<root>/target/*/abin/ -> $out/bin).
  cargoBuildFlags = [ "--workspace" ];

  meta = {
    description = "Abora Framework - shared server foundation for Abora Cloud and Abora Atlas";
    homepage = "https://github.com/AboraOS-Project/Abora-Framework";
    license = lib.licenses.asl20;
    mainProgram = "aborad";
    platforms = lib.platforms.linux;
  };
}