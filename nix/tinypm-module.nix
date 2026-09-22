# TinyPM (`grab` and `tinypm`), built from the TinyPM repository, for a Framework system.
#
# Import this module to get TinyPM; leave it out to have none. (ANIX already bundles a copy for its own
# use; this puts `grab` and `tinypm` on every user's PATH.) `abora-framework-install` asks.
{ tinypm }:
{ lib, pkgs, ... }:

let
  tinypmPackage = pkgs.rustPlatform.buildRustPackage {
    pname = "tinypm";
    version = "0.8.1-alpha";
    src = tinypm;
    cargoLock.lockFile = tinypm + "/Cargo.lock";
    doCheck = false;
    meta = {
      description = "One friendly package workflow across Linux distributions";
      homepage = "https://github.com/AnimatedGTVR/TinyPM";
      mainProgram = "grab";
    };
  };
in
{
  # lowPrio: if ANIX is also imported it brings its own copy of `grab`/`tinypm`, and that one wins.
  environment.systemPackages = [ (lib.lowPrio tinypmPackage) ];
}
