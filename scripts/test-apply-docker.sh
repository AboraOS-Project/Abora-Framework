#!/usr/bin/env bash
# Test "apply updates" against REAL apt in a throwaway Docker container.
#
# Builds static aborad / abora / abora-apply, starts a clean ubuntu:24.04 container, builds a real
# two-version .deb in a local repo inside it, and checks that the daemon plus the root helper upgrade
# it correctly: only the requested package, the administrator's edited config file kept, no prompts,
# dpkg left clean, history and audit recorded. Nothing touches this machine's packages.
#
# Needs: docker and a Rust toolchain. Usage: scripts/test-apply-docker.sh   (or `make test-apply`)

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET=x86_64-unknown-linux-gnu
command -v docker >/dev/null || { echo "test-apply-docker: docker is required" >&2; exit 1; }

echo ">> building static binaries"
( cd "$ROOT" && RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static" \
    cargo build --release --locked --target "$TARGET" -p aborad -p abora -p abora-apply )
BIN="$ROOT/target/$TARGET/release"

echo ">> running the test in ubuntu:24.04"
docker run --rm \
    -v "$BIN:/bin-under-test:ro" \
    -v "$ROOT/config/abora.toml.default:/config.toml:ro" \
    -v "$ROOT/scripts/docker/apply-test.sh:/test.sh:ro" \
    ubuntu:24.04 bash /test.sh
