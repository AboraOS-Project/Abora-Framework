#!/usr/bin/env bash
# Test the Nix side of Abora Framework in a throwaway nixos/nix container: the packages build, the flake
# evaluates, the installer validates its input, and the systems it generates (with and without ANIX and
# TinyPM) are valid NixOS configurations. Nothing touches this machine; the Nix store lives in a Docker
# volume (abora-nix-store) so repeat runs are fast.
#
# Needs docker. Usage: scripts/test-nix-docker.sh   (or `make test-nix`)

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v docker >/dev/null || { echo "test-nix-docker: docker is required" >&2; exit 1; }
docker volume create abora-nix-store >/dev/null

# Flakes only see files git knows about, so make sure new files are at least staged.
git -C "$ROOT" add -N . >/dev/null 2>&1 || true

docker run --rm \
    -v abora-nix-store:/nix \
    -v "$ROOT:/src" -w /src \
    -e NIX_CONFIG="experimental-features = nix-command flakes
sandbox = false
accept-flake-config = true" \
    nixos/nix sh scripts/docker/nix-test.sh
