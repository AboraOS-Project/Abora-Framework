# The `abora-framework-install` command, as a Nix package. writeShellApplication adds the shebang and
# `set -euo pipefail`, puts the runtime tools on PATH, and runs shellcheck over the script at build time.
{
  lib,
  writeShellApplication,
  coreutils,
  gawk,
  gnugrep,
  gnused,
  util-linux,
  parted,
  dosfstools,
  e2fsprogs,
  mkpasswd,
  nixos-install-tools,
  nix,
  git,
  systemd,
}:

writeShellApplication {
  name = "abora-framework-install";
  runtimeInputs = [
    coreutils
    gawk
    gnugrep
    gnused
    util-linux
    parted
    dosfstools
    e2fsprogs
    mkpasswd
    nixos-install-tools
    nix
    git
    systemd
  ];
  text = builtins.readFile ../installer/nixos/abora-framework-install.sh;

  meta = {
    description = "Install a NixOS system built on Abora Framework";
    license = lib.licenses.asl20;
    mainProgram = "abora-framework-install";
    platforms = lib.platforms.linux;
  };
}
