#!/usr/bin/env bash
# Build the Abora Framework live ISO: build/abora-framework.iso
#
# Boots (BIOS and UEFI) through Limine into a small RAM-only Linux system that
# runs aborad and shows the Abora logo with live boot logs on the framebuffer.
#
# Inputs (fetched or built on first use, cached under build/):
#   * Limine bootloader   git clone of the pinned binary branch
#   * a Linux kernel      $KERNEL, or the Ubuntu generic kernel via `apt-get download`
#   * busybox             a statically linked one ($BUSYBOX or the one on PATH)
#   * aborad, abora, abora-boot   built here, statically linked against glibc
#
# Usage: scripts/make-iso.sh        (or `make iso`)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD="$ROOT/build"
LIMINE_DIR="$BUILD/limine"
ROOTFS="$BUILD/initramfs"
ISO_ROOT="$BUILD/iso_root"
ISO="$BUILD/abora-framework.iso"
LIMINE_BRANCH="v9.x-binary"
TARGET="x86_64-unknown-linux-gnu"
FONT="${FONT:-/usr/share/consolefonts/Lat15-Terminus16.psf.gz}"

say() { printf '>> %s\n' "$*"; }
die() { printf 'make-iso.sh: %s\n' "$*" >&2; exit 1; }

for tool in cargo cpio gzip python3 git make; do
    command -v "$tool" >/dev/null || die "missing required tool '$tool'"
done
if command -v xorriso >/dev/null; then MKISO=xorriso
elif command -v genisoimage >/dev/null; then MKISO=genisoimage
elif command -v mkisofs >/dev/null; then MKISO=mkisofs
else die "need xorriso, genisoimage or mkisofs to write the ISO"; fi

BUSYBOX="${BUSYBOX:-$(command -v busybox || true)}"
[ -n "$BUSYBOX" ] || die "busybox not found (install busybox-static, or set BUSYBOX=/path)"
file "$BUSYBOX" 2>/dev/null | grep -q "statically linked" || die "$BUSYBOX is not statically linked (install busybox-static)"

mkdir -p "$BUILD"

# --- kernel ------------------------------------------------------------------
KERNEL_CACHE="$BUILD/kernel/vmlinuz"
if [ -n "${KERNEL:-}" ]; then
    [ -r "$KERNEL" ] || die "KERNEL=$KERNEL is not readable (distro kernels in /boot are often root-only)"
    KERNEL_FILE="$KERNEL"
elif [ -r "$KERNEL_CACHE" ]; then
    KERNEL_FILE="$KERNEL_CACHE"
else
    command -v apt-get >/dev/null && command -v dpkg >/dev/null \
        || die "no kernel: set KERNEL=/path/to/a/readable/vmlinuz (apt is not available to fetch one)"
    say "fetching the Ubuntu generic kernel package (kernel only, ~15 MB)"
    PKG="$(apt-cache depends linux-image-virtual | awk '/Depends: linux-image-/{print $2; exit}')"
    [ -n "$PKG" ] || die "could not resolve a kernel package name"
    TMP="$(mktemp -d)"
    ( cd "$TMP" && apt-get download "$PKG" >/dev/null && dpkg -x ./*.deb x )
    mkdir -p "$BUILD/kernel"
    cp "$TMP"/x/boot/vmlinuz-* "$KERNEL_CACHE"
    chmod 644 "$KERNEL_CACHE"
    rm -rf "$TMP"
    KERNEL_FILE="$KERNEL_CACHE"
fi

# --- bootloader --------------------------------------------------------------
if [ ! -d "$LIMINE_DIR" ]; then
    say "fetching limine ($LIMINE_BRANCH)"
    git clone --depth 1 --branch "$LIMINE_BRANCH" https://github.com/limine-bootloader/limine.git "$LIMINE_DIR"
fi
[ -x "$LIMINE_DIR/limine" ] || { say "building limine host utility"; make -C "$LIMINE_DIR" >/dev/null; }

# --- Abora binaries ----------------------------------------------------------
# Stamp the commit into aborad's version endpoint (CI sets it already).
export ABORA_BUILD_COMMIT="${ABORA_BUILD_COMMIT:-$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || true)}"
say "building aborad, abora and abora-boot (static, release)"
( cd "$ROOT" && RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static" \
    cargo build --release --locked --target "$TARGET" -p aborad -p abora -p abora-boot )
BIN="$ROOT/target/$TARGET/release"

# --- initramfs ---------------------------------------------------------------
say "assembling the initramfs"
rm -rf "$ROOTFS"
mkdir -p "$ROOTFS"/{bin,sbin,usr/bin,usr/sbin,usr/share/abora,etc/abora,proc,sys,dev,run,tmp,root}
cp "$BUSYBOX" "$ROOTFS/bin/busybox"
"$ROOTFS/bin/busybox" --list-full 2>/dev/null | while read -r applet; do
    [ -e "$ROOTFS/$applet" ] || { mkdir -p "$ROOTFS/$(dirname "$applet")"; ln -s /bin/busybox "$ROOTFS/$applet"; }
done
install -m755 "$BIN/aborad" "$BIN/abora" "$BIN/abora-boot" "$ROOTFS/usr/sbin/"
ln -s /usr/sbin/abora "$ROOTFS/usr/bin/abora"
ln -s /usr/sbin/abora-boot "$ROOTFS/usr/bin/abora-boot"
ln -s /usr/sbin/aborad "$ROOTFS/usr/bin/aborad"
install -m644 "$ROOT/config/abora.toml.default" "$ROOTFS/etc/abora/abora.toml"
install -m755 "$ROOT/iso/init" "$ROOTFS/init"
python3 "$ROOT/scripts/logo-to-raw.py" "${LOGO:-$ROOT/assets/abora-logo.png}" "$ROOTFS/usr/share/abora/logo.rgba"
case "$FONT" in
    *.gz) gzip -dc "$FONT" > "$ROOTFS/usr/share/abora/font.psf" ;;
    *)    cp "$FONT" "$ROOTFS/usr/share/abora/font.psf" ;;
esac
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
COMMIT="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
echo "v$VERSION ($COMMIT)" > "$ROOTFS/etc/abora-version"
# System files a fork has not customized yet (see docs/forking.md). The boot screen warns about them.
"$ROOT/scripts/fork-check.sh" --plain > "$ROOTFS/etc/abora-fork-todo"
( cd "$ROOTFS" && find . | sort | cpio -o -H newc --owner=0:0 --quiet | gzip -9 ) > "$BUILD/initramfs.cpio.gz"

# --- ISO ---------------------------------------------------------------------
say "laying out the ISO"
rm -rf "$ISO_ROOT"
mkdir -p "$ISO_ROOT/boot/limine" "$ISO_ROOT/EFI/BOOT"
cp "$KERNEL_FILE" "$ISO_ROOT/boot/vmlinuz"
cp "$BUILD/initramfs.cpio.gz" "$ISO_ROOT/boot/initramfs.cpio.gz"
cp "$ROOT/iso/limine.conf" "$ISO_ROOT/boot/limine/limine.conf"
cp "$LIMINE_DIR/limine-bios.sys" "$LIMINE_DIR/limine-bios-cd.bin" "$LIMINE_DIR/limine-uefi-cd.bin" "$ISO_ROOT/boot/limine/"
cp "$LIMINE_DIR/BOOTX64.EFI" "$LIMINE_DIR/BOOTIA32.EFI" "$ISO_ROOT/EFI/BOOT/"

say "writing $ISO ($MKISO)"
rm -f "$ISO"
if [ "$MKISO" = xorriso ]; then
    xorriso -as mkisofs -quiet -R -r -J -V ABORA_LIVE \
        -b boot/limine/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
        --efi-boot boot/limine/limine-uefi-cd.bin -efi-boot-part --efi-boot-image \
        --protective-msdos-label "$ISO_ROOT" -o "$ISO"
else
    "$MKISO" -quiet -R -r -J -V ABORA_LIVE \
        -b boot/limine/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
        -eltorito-alt-boot -e boot/limine/limine-uefi-cd.bin -no-emul-boot \
        -o "$ISO" "$ISO_ROOT"
fi
"$LIMINE_DIR/limine" bios-install "$ISO" >/dev/null
say "done: $ISO ($(du -h "$ISO" | cut -f1))"
