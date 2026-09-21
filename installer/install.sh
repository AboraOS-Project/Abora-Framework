#!/usr/bin/env bash
# Install (or remove) the Abora Framework daemon and CLI on a systemd host.
#
#   sudo installer/install.sh                 install from ./target/release, enable and start
#   sudo installer/install.sh --no-start      install and enable, but do not start yet
#   sudo installer/install.sh --uninstall     stop and remove binaries and the unit;
#                                             KEEPS /etc/abora and /var/lib/abora
#   sudo installer/install.sh --uninstall --purge   also remove config, state and the user
#
# Options:
#   --bin-dir DIR   where aborad and abora are (default: <repo>/target/release)
#   --root DIR      stage into DIR instead of / (packaging and tests: no user is created,
#                   no ownership is changed, systemd is not touched)
#
# What it does, in order: creates the `aborad` system user (no login, no home), installs
# aborad to /usr/sbin and abora to /usr/bin, installs the default config to
# /etc/abora/abora.toml ONLY if that file does not exist yet (your edits are never
# overwritten), installs the hardened unit, reloads systemd and enables the service.
# Running it again is safe.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
BIN_DIR="$REPO/target/release"
ROOT=""
ACTION=install
START=1
PURGE=0

while [ $# -gt 0 ]; do
    case "$1" in
        --uninstall) ACTION=uninstall ;;
        --purge) PURGE=1 ;;
        --no-start) START=0 ;;
        --bin-dir) BIN_DIR="${2:?--bin-dir needs a directory}"; shift ;;
        --root) ROOT="${2:?--root needs a directory}"; shift ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) echo "install.sh: unknown option '$1' (see --help)" >&2; exit 2 ;;
    esac
    shift
done

say() { printf '>> %s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }

STAGING=0
[ -n "$ROOT" ] && STAGING=1
if [ "$STAGING" = 0 ] && [ "$(id -u)" != 0 ]; then
    die "run as root (sudo), or use --root DIR to stage into a directory"
fi

SBIN="$ROOT/usr/sbin"
BIN="$ROOT/usr/bin"
ETC="$ROOT/etc/abora"
UNIT_DIR="$ROOT/etc/systemd/system"
UNIT="$UNIT_DIR/aborad.service"

systemd_present() { [ "$STAGING" = 0 ] && command -v systemctl >/dev/null && [ -d /run/systemd/system ]; }

if [ "$ACTION" = uninstall ]; then
    if systemd_present; then
        systemctl disable --now aborad 2>/dev/null || true
    fi
    rm -f "$SBIN/aborad" "$BIN/abora" "$UNIT"
    systemd_present && systemctl daemon-reload
    if [ "$PURGE" = 1 ]; then
        rm -rf "$ETC" "$ROOT/var/lib/abora"
        if [ "$STAGING" = 0 ] && getent passwd aborad >/dev/null; then userdel aborad 2>/dev/null || true; fi
        say "removed aborad, its unit, configuration, state and user"
    else
        say "removed aborad and its unit; kept $ETC and /var/lib/abora (use --purge to remove them)"
    fi
    exit 0
fi

[ -x "$BIN_DIR/aborad" ] && [ -x "$BIN_DIR/abora" ] \
    || die "aborad and abora not found in $BIN_DIR; run 'cargo build --release' or pass --bin-dir"

if [ "$STAGING" = 0 ]; then
    if ! getent passwd aborad >/dev/null; then
        say "creating system user aborad"
        useradd --system --user-group --no-create-home --home-dir /var/lib/abora --shell /usr/sbin/nologin aborad
    fi
    OWN=(-o root -g aborad)
else
    OWN=()
fi

say "installing binaries"
install -Dm755 "$BIN_DIR/aborad" "$SBIN/aborad"
install -Dm755 "$BIN_DIR/abora" "$BIN/abora"

say "installing configuration"
install -d -m 0750 "${OWN[@]}" "$ETC"
if [ -e "$ETC/abora.toml" ]; then
    say "keeping your existing $ETC/abora.toml (default is at $ETC/abora.toml.default)"
    install -m 0640 "${OWN[@]}" "$REPO/config/abora.toml.default" "$ETC/abora.toml.default"
else
    install -m 0640 "${OWN[@]}" "$REPO/config/abora.toml.default" "$ETC/abora.toml"
fi

say "installing the systemd unit"
install -Dm644 "$HERE/systemd/aborad.service" "$UNIT"

if systemd_present; then
    systemctl daemon-reload
    systemctl enable aborad
    if [ "$START" = 1 ]; then
        systemctl restart aborad
        sleep 1
        systemctl --no-pager --lines=0 status aborad || true
    fi
    say "done. Try: abora status   (logs: journalctl -u aborad -f)"
else
    say "done (systemd was not touched)"
fi
