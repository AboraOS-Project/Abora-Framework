# The Abora Framework base system, as a NixOS module.
#
# Importing this module and setting `aboraFramework.enable = true` gives a minimal NixOS system
# that runs `aborad` (see ./abora-module.nix) and has the `abora` command line client. ANIX and
# TinyPM are NOT part of it: they are separate modules (`nixosModules.anix`, `nixosModules.tinypm`)
# that you import, or leave out. The installer (`abora-framework-install`) asks which you want.
#
# This is the file to start from when you build your own OS on the Framework.
{ config, lib, pkgs, ... }:

let
  cfg = config.aboraFramework;
  frameworkPackage = pkgs.callPackage ./aborad-package.nix { };
in
{
  imports = [ ./abora-module.nix ];

  # Deliberately NOT under `abora.*`: ANIX (from Abora OS) uses the presence of an `abora` option
  # namespace to decide it is running inside full Abora OS, and would then set options that do not
  # exist here.
  options.aboraFramework = {
    enable = lib.mkEnableOption "the Abora Framework base system (aborad and the abora command)";

    # ABORA-SYSTEM-FILE 4: name your distribution here (shown in the login banner and /etc/os-release)
    distroName = lib.mkOption {
      type = lib.types.str;
      default = "Abora Framework";
      description = "Name of the operating system built on the Framework. Forks change this.";
    };
  };

  config = lib.mkIf cfg.enable {
    system.nixos.distroName = lib.mkDefault cfg.distroName;

    # The daemon is on by default; nothing else about it is decided here.
    services.aborad.enable = lib.mkDefault true;

    environment.systemPackages = [ frameworkPackage ];

    # Flakes are how the Framework, ANIX and updates work, so a Framework system always has them.
    nix.settings.experimental-features = [ "nix-command" "flakes" ];
  };
}
