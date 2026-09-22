#!/bin/sh
# Checks the Nix side of the Framework: the packages build, the installer behaves, and the systems the
# installer generates (with and without ANIX / TinyPM) are valid NixOS configurations.
#
# Runs either inside a nixos/nix container with the repository at /src (scripts/test-nix-docker.sh, for
# a machine with no Nix), or directly in CI where Nix is installed on the runner. Either way it only
# needs `nix` (flakes enabled) and `git` on PATH, and the repository as its current directory.
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
git config --global --add safe.directory "$PWD" 2>/dev/null || true
fail() { echo "FAIL: $*" >&2; exit 1; }
step() { echo; echo "== $*"; }

step "the packages build (aborad and the tests, the installer with shellcheck)"
nix build .#aborad .#installer --no-link
INSTALLER="$(nix build .#installer --no-link --print-out-paths)/bin/abora-framework-install"
[ -x "$INSTALLER" ] || fail "no installer binary"
echo "ok"

step "the flake evaluates"
nix flake check --no-build 2>&1 | tail -3
echo "ok"

step "installer: help, and refusal of anything that could break out of the generated Nix files"
"$INSTALLER" --help | grep -q -- "--generate-only" || fail "--help does not mention --generate-only"
T=$(mktemp -d)
for bad in 'a"b' 'Up' '-x' 'a b' 'a;b' '$(id)' '' 'a.b'; do
  if "$INSTALLER" --generate-only "$T/bad" --hostname "$bad" --no-anix --no-tinypm >/dev/null 2>&1; then fail "accepted hostname '$bad'"; fi
done
for bad in 'root' 'A' 'a b' 'a"b' '1abc' '$x'; do
  if "$INSTALLER" --generate-only "$T/bad" --user "$bad" --no-anix --no-tinypm >/dev/null 2>&1; then fail "accepted user name '$bad'"; fi
done
for bad in '../x' 'a"b' 'x y' 'a;b'; do
  if "$INSTALLER" --generate-only "$T/bad" --timezone "$bad" --no-anix --no-tinypm >/dev/null 2>&1; then fail "accepted time zone '$bad'"; fi
done
if "$INSTALLER" --generate-only "$T/bad" --password-hash 'plaintext' --no-anix --no-tinypm >/dev/null 2>&1; then fail "accepted a non-hash as a password hash"; fi
if "$INSTALLER" --generate-only "$T/bad" --framework-flake 'x"; evil' --no-anix --no-tinypm >/dev/null 2>&1; then fail "accepted a hostile flake reference"; fi
[ ! -e "$T/bad" ] || fail "a refused run still wrote files"
echo "ok: hostile hostnames, user names, time zones, hashes and flake references are refused"

step "installer: a real run without root, disk or a terminal refuses instead of guessing"
if "$INSTALLER" --disk /dev/null >/dev/null 2>&1 </dev/null; then fail "ran without root"; fi
echo "ok"

HASH='$6$saltsalt$abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789./abcdefghijklmnopqrstuvwxyz'
for combo in "no no" "yes no" "no yes" "yes yes"; do
  set -- $combo; anix=$1; tiny=$2
  name="anix-$anix-tinypm-$tiny"
  step "generate a system: ANIX $anix, TinyPM $tiny, and evaluate it as a real NixOS system"
  D="$T/$name"
  flags=""
  [ "$anix" = yes ] && flags="$flags --anix" || flags="$flags --no-anix"
  [ "$tiny" = yes ] && flags="$flags --tinypm" || flags="$flags --no-tinypm"
  # shellcheck disable=SC2086
  "$INSTALLER" --generate-only "$D" --hostname testbox --user alice --password-hash "$HASH" --timezone Europe/Berlin $flags >/dev/null
  # the choices show up exactly where they should
  if [ "$anix" = yes ]; then grep -q "nixosModules.anix" "$D/flake.nix" || fail "ANIX chosen but not imported"; else ! grep -q "nixosModules.anix" "$D/flake.nix" || fail "ANIX not chosen but imported"; fi
  if [ "$tiny" = yes ]; then grep -q "nixosModules.tinypm" "$D/flake.nix" || fail "TinyPM chosen but not imported"; else ! grep -q "nixosModules.tinypm" "$D/flake.nix" || fail "TinyPM not chosen but imported"; fi
  grep -q 'nixosModules.framework' "$D/flake.nix" || fail "the Framework base is always imported"
  grep -q 'hostName = "testbox"' "$D/configuration.nix" || fail "hostname missing"
  # and it is a valid system (the Framework input points at THIS checkout instead of GitHub)
  drv=$(nix eval --raw "path:$D#nixosConfigurations.testbox.config.system.build.toplevel.drvPath" \
        --override-input abora-framework "path:$PWD")
  case "$drv" in /nix/store/*.drv) echo "ok: evaluates to $drv" ;; *) fail "did not evaluate: $drv" ;; esac
  # the configuration itself: what the options resolved to
  nix eval --raw "path:$D#nixosConfigurations.testbox.config.services.aborad.enable" --apply 'x: if x then "aborad on" else "aborad OFF"' \
        --override-input abora-framework "path:$PWD" | grep -q "aborad on" || fail "aborad is not enabled in the generated system"
  has_anix=$(nix eval "path:$D#nixosConfigurations.testbox.config.anix.enable" --override-input abora-framework "path:$PWD" 2>/dev/null || echo absent)
  if [ "$anix" = yes ]; then [ "$has_anix" = true ] || fail "ANIX chosen but anix.enable is '$has_anix'"; else [ "$has_anix" = absent ] || fail "ANIX not chosen but the option exists ($has_anix)"; fi
done

step "BIOS boot mode generates GRUB, UEFI generates systemd-boot"
"$INSTALLER" --generate-only "$T/bios" --boot-mode bios --disk /dev/vda --no-anix --no-tinypm >/dev/null
grep -q 'grub.device = "/dev/vda"' "$T/bios/configuration.nix" || fail "BIOS config lacks grub"
"$INSTALLER" --generate-only "$T/uefi" --boot-mode uefi --no-anix --no-tinypm >/dev/null
grep -q 'systemd-boot.enable = true' "$T/uefi/configuration.nix" || fail "UEFI config lacks systemd-boot"
echo "ok"

echo; echo "ALL GOOD"
