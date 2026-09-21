#!/usr/bin/env bash
# Test the REAL deployment: installer, systemd units and the update helper, in a throwaway container
# that runs systemd as PID 1.
#
# It builds static binaries, boots an ubuntu:24.04 container with systemd, runs installer/install.sh
# as root (useradd, unit files), starts aborad under its real hardened unit as the unprivileged aborad
# user, asks for an update, lets the systemd path unit start the root helper, upgrades a real .deb with
# real apt, checks the sandbox, and runs the real uninstaller. Nothing touches this machine.
#
# The container gets CAP_SYS_ADMIN (systemd and the unit sandboxing need it) but is NOT --privileged.
# Needs: docker and a Rust toolchain. Usage: scripts/test-systemd-docker.sh   (or `make test-systemd`)

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET=x86_64-unknown-linux-gnu
IMAGE=abora-systemd-test
NAME="abora-systemd-test-$$"
command -v docker >/dev/null || { echo "test-systemd-docker: docker is required" >&2; exit 1; }
cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo ">> building static binaries"
( cd "$ROOT" && RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static" \
    cargo build --release --locked --target "$TARGET" -p aborad -p abora -p abora-apply )
BIN="$ROOT/target/$TARGET/release"

echo ">> building the test image (ubuntu:24.04 + systemd)"
docker build -q -t "$IMAGE" - >/dev/null <<'DOCKERFILE'
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update -qq && apt-get install -y -qq systemd systemd-sysv dbus dpkg-dev curl ca-certificates python3 adduser passwd >/dev/null && rm -rf /var/lib/apt/lists/*
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
DOCKERFILE

echo ">> booting systemd in a container"
docker run -d --name "$NAME" --cap-add SYS_ADMIN \
    --security-opt seccomp=unconfined --security-opt apparmor=unconfined --security-opt writable-cgroups=true \
    --tmpfs /run --tmpfs /run/lock --tmpfs /tmp "$IMAGE" >/dev/null
for _ in $(seq 1 30); do
    state="$(docker exec "$NAME" systemctl is-system-running 2>/dev/null || true)"
    case "$state" in running|degraded) break ;; esac
    sleep 1
done
[ "$state" = running ] || [ "$state" = degraded ] || { echo "test-systemd-docker: systemd did not come up ($state)" >&2; docker logs "$NAME" | tail -20 >&2; exit 1; }

echo ">> copying the build and the installer in"
docker exec "$NAME" mkdir -p /bin-under-test /repo
docker cp "$BIN/aborad" "$NAME:/bin-under-test/"
docker cp "$BIN/abora" "$NAME:/bin-under-test/"
docker cp "$BIN/abora-apply" "$NAME:/bin-under-test/"
docker cp "$ROOT/installer" "$NAME:/repo/"
docker cp "$ROOT/config" "$NAME:/repo/"
docker cp "$ROOT/scripts/docker/systemd-test.sh" "$NAME:/systemd-test.sh"

echo ">> running the test"
docker exec "$NAME" bash /systemd-test.sh
