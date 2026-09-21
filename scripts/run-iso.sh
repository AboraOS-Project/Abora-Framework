#!/usr/bin/env bash
# Boot build/abora-framework.iso under QEMU.
#
#   scripts/run-iso.sh                  window; serial console on this terminal
#   scripts/run-iso.sh --headless       no window, serial console on this terminal
#   scripts/run-iso.sh --capture FILE   no window, serial log to FILE, then exit
#   scripts/run-iso.sh --screenshot F   no window, save the screen to F (PPM)
#
# Knobs: ABORA_QEMU_MEM (default 512M), ABORA_UEFI=1 to boot through OVMF instead
# of BIOS, ABORA_QEMU_TIMEOUT / ABORA_QEMU_SETTLE seconds.

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ISO="$ROOT/build/abora-framework.iso"
[ -f "$ISO" ] || { echo "run-iso.sh: no ISO at build/abora-framework.iso; run 'make iso'" >&2; exit 1; }

QEMU=(qemu-system-x86_64 -M q35 -m "${ABORA_QEMU_MEM:-512M}" -cdrom "$ISO" -boot d -vga std -no-reboot)

if [ "${ABORA_UEFI:-0}" = 1 ]; then
    # Either one combined firmware image (OVMF.fd) or a CODE/VARS pair.
    COMBINED="${ABORA_OVMF:-$(ls /usr/share/qemu/OVMF.fd /usr/share/ovmf/OVMF.fd 2>/dev/null | head -1 || true)}"
    CODE="${ABORA_OVMF_CODE:-$(ls /usr/share/OVMF/OVMF_CODE*.fd /usr/share/edk2/x64/OVMF_CODE*.fd 2>/dev/null | head -1 || true)}"
    VARS="${ABORA_OVMF_VARS:-$(ls /usr/share/OVMF/OVMF_VARS*.fd /usr/share/edk2/x64/OVMF_VARS*.fd 2>/dev/null | head -1 || true)}"
    if [ -n "$COMBINED" ] && [ -f "$COMBINED" ]; then
        QEMU+=(-bios "$COMBINED")
    elif [ -f "$CODE" ] && [ -f "$VARS" ]; then
        cp "$VARS" "$ROOT/build/ovmf_vars.fd"
        QEMU+=(-drive "if=pflash,unit=0,format=raw,readonly=on,file=$CODE" -drive "if=pflash,unit=1,format=raw,file=$ROOT/build/ovmf_vars.fd")
    else
        echo "run-iso.sh: OVMF firmware not found (set ABORA_OVMF, or ABORA_OVMF_CODE and ABORA_OVMF_VARS)" >&2; exit 1
    fi
fi

case "${1:-window}" in
    # zoom-to-fit scales the 1280x800 guest screen to whatever size the window is, so nothing
    # (such as the logo on the right) is cut off in a small window.
    window)     exec "${QEMU[@]}" -display gtk,zoom-to-fit=on -serial stdio ;;
    --headless) exec "${QEMU[@]}" -display none -serial stdio ;;
    --capture)
        LOG="${2:?--capture needs a file}"
        : > "$LOG"
        timeout --foreground "${ABORA_QEMU_TIMEOUT:-15}" "${QEMU[@]}" -display none -serial "file:$LOG" </dev/null || true
        ;;
    --screenshot)
        OUT="${2:?--screenshot needs a file}"
        {
            sleep "${ABORA_QEMU_SETTLE:-8}"
            echo "screendump $OUT"
            sleep 2
            echo quit
        } | "${QEMU[@]}" -display none -serial none -monitor stdio >/dev/null
        ;;
    *) echo "run-iso.sh: unknown mode '$1'" >&2; exit 1 ;;
esac
