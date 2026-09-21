#!/usr/bin/env bash
# Check the installer without root and without touching the system:
#   1. the systemd unit passes `systemd-analyze verify` (skipped if systemd-analyze is missing)
#   2. installer/install.sh stages a complete install into a temp directory
#   3. re-running it never overwrites an edited config
#   4. --uninstall keeps config and state, --purge removes them
#
# Needs release binaries: run `cargo build --release` first (or pass BIN_DIR=...).

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${BIN_DIR:-$ROOT/target/release}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
fail() { echo "check-installer: FAIL: $*" >&2; exit 1; }

# 1. unit file
if command -v systemd-analyze >/dev/null; then
    # verify checks that ExecStart exists, so point it at the real binary for the check
    sed "s#^ExecStart=/usr/sbin/aborad#ExecStart=$BIN_DIR/aborad#" "$ROOT/installer/systemd/aborad.service" > "$TMP/aborad.service"
    # The aborad user does not exist on the build machine; that is the only complaint we accept.
    out="$(systemd-analyze verify "$TMP/aborad.service" 2>&1 || true)"
    bad="$(printf '%s\n' "$out" | grep -v -E "Failed to resolve (user|group)|Unknown (user|group)|does not exist" || true)"
    [ -z "$bad" ] || fail "systemd-analyze verify: $bad"
    echo "ok   unit passes systemd-analyze verify"
else
    echo "skip systemd-analyze not installed"
fi
for key in NoNewPrivileges=true ProtectSystem=strict PrivateTmp=true User=aborad CapabilityBoundingSet= MemoryDenyWriteExecute=true IPAddressDeny=any; do
    grep -q "^$key" "$ROOT/installer/systemd/aborad.service" || fail "unit is missing hardening line $key"
done
echo "ok   unit keeps its hardening lines"

# 2. staged install
STAGE="$TMP/root"
"$ROOT/installer/install.sh" --root "$STAGE" --bin-dir "$BIN_DIR" >/dev/null
for f in usr/sbin/aborad usr/bin/abora etc/abora/abora.toml etc/systemd/system/aborad.service; do
    [ -e "$STAGE/$f" ] || fail "missing $f after install"
done
[ -x "$STAGE/usr/sbin/aborad" ] || fail "aborad is not executable"
mode="$(stat -c %a "$STAGE/etc/abora/abora.toml")"
[ "$mode" = 640 ] || fail "abora.toml mode is $mode, expected 640"
echo "ok   staged install is complete"

# 3. idempotent, never clobbers edits
echo "# my edit" >> "$STAGE/etc/abora/abora.toml"
"$ROOT/installer/install.sh" --root "$STAGE" --bin-dir "$BIN_DIR" >/dev/null
grep -q "# my edit" "$STAGE/etc/abora/abora.toml" || fail "re-install overwrote the edited config"
[ -e "$STAGE/etc/abora/abora.toml.default" ] || fail "re-install did not refresh abora.toml.default"
echo "ok   re-install keeps an edited config"

# 4. uninstall vs purge
mkdir -p "$STAGE/var/lib/abora" && echo state > "$STAGE/var/lib/abora/updates.json"
"$ROOT/installer/install.sh" --root "$STAGE" --uninstall >/dev/null
[ ! -e "$STAGE/usr/sbin/aborad" ] && [ ! -e "$STAGE/etc/systemd/system/aborad.service" ] || fail "uninstall left binaries or the unit"
[ -e "$STAGE/etc/abora/abora.toml" ] && [ -e "$STAGE/var/lib/abora/updates.json" ] || fail "uninstall removed config or state"
"$ROOT/installer/install.sh" --root "$STAGE" --uninstall --purge >/dev/null
[ ! -e "$STAGE/etc/abora" ] && [ ! -e "$STAGE/var/lib/abora" ] || fail "purge left config or state"
echo "ok   uninstall keeps data, --purge removes it"

echo "check-installer: all good"
