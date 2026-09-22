# ANIX, imported from the Abora OS flake, for a Framework system.
#
# ANIX is Abora OS's command line tool for everyday NixOS management (`anix status`,
# `anix switch`, `anix rollback`, ...). The Framework does not copy it: this module imports
# `nixosModules.anix` and the overlay that provides the `anix` package straight from the Abora OS
# flake, so there is one ANIX.
#
# Import this module to get ANIX; leave it out to have none. `abora-framework-install` asks.
{ abora-os }:
{ config, lib, pkgs, ... }:

let
  # The Abora OS ANIX module (stable and edge) sets `abora.desktop`, `abora.wallpaper` and
  # `abora.gaming.*` inside `mkIf` blocks, and the NixOS module system rejects a definition of an
  # option that is not declared even when the `mkIf` is false, so importing it on a system that is not
  # full Abora OS fails with "The option `abora' does not exist". The Framework is not full Abora OS,
  # so declare inert stand-ins: nothing reads them and they never do anything here.
  #
  # If you also import Abora OS's own `installed-base` module (the full distribution), do NOT use this
  # module; import `abora-os.nixosModules.anix` directly, since that module declares the real options.
  inert = lib.mkOption {
    type = lib.types.raw;
    default = null;
    internal = true;
    visible = false;
  };
in
{
  imports = [ abora-os.nixosModules.anix ];

  options.abora = {
    desktop = inert;
    wallpaper = inert;
    gaming = lib.genAttrs [
      "enable"
      "steam"
      "bigPictureShortcut"
      "bigPictureAutostart"
      "gamescopeSession"
      "controllerSupport"
      "mangohud"
      "gamemode"
      "vulkanTools"
      "launchers"
    ] (_: inert);
  };

  config = {
    nixpkgs.overlays = [ abora-os.overlays.default ];

    anix.enable = lib.mkDefault true;

    # ANIX's own defaults suit a desktop (Bluetooth, printing, Flatpak, PipeWire, unfree packages).
    # The Framework is a minimal base, so all of that starts OFF. Turn on what you need, either in
    # your configuration or with ANIX itself.
    anix.services.bluetooth = lib.mkDefault false;
    anix.services.printing = lib.mkDefault false;
    anix.services.flatpak = lib.mkDefault false;
    anix.services.audio = lib.mkDefault false;
    anix.allowUnfree = lib.mkDefault false;

    # The ANIX module configures the system but does not install the `anix` command itself.
    environment.systemPackages = [ pkgs.anix ];
  };
}
