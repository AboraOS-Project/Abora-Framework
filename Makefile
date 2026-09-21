# Abora Framework: build entry points.
#
# `make iso` builds a bootable live ISO; `make run` boots it in QEMU and shows
# the Abora logo with the boot logs. The scripts in scripts/ do the work.

.PHONY: fork-check all iso run run-headless boot-test screenshot build test clean distclean help

all: iso

## fork-check    List the system files a fork still has to customize (see docs/forking.md)
fork-check:
	@scripts/fork-check.sh

## iso           Build the bootable live ISO at build/abora-framework.iso
iso:
	@scripts/make-iso.sh

## run           Build the ISO and boot it in a QEMU window (serial console here)
run: iso
	@scripts/run-iso.sh

## run-headless  Build the ISO and boot it with no window, serial console here
run-headless: iso
	@scripts/run-iso.sh --headless

## boot-test     Boot headless and check the boot log shows aborad answering
boot-test: iso
	@scripts/run-iso.sh --capture build/serial.log
	@grep -aq "aborad answers on" build/serial.log && echo "boot-test: OK, aborad answered inside the ISO" \
	  || { echo "boot-test: FAILED, see build/serial.log" >&2; tail -30 build/serial.log >&2; exit 1; }

## screenshot    Boot headless and save the screen to build/screen.ppm
screenshot: iso
	@scripts/run-iso.sh --screenshot build/screen.ppm
	@echo "wrote build/screen.ppm"

## build         Build the workspace (host, release)
build:
	@cargo build --release

## test          Run the workspace tests
test:
	@cargo test

## clean         Remove the ISO and its staging folders (keeps fetched Limine/kernel)
clean:
	@rm -rf build/iso_root build/initramfs build/initramfs.cpio.gz build/abora-framework.iso build/serial.log build/screen.ppm build/ovmf_vars.fd

## distclean     Remove everything under build/ and cargo output
distclean:
	@rm -rf build target

## help          List these targets
help:
	@grep -E '^## ' Makefile | sed 's/^## /  make /'
