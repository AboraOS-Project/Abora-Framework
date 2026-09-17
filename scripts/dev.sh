#!/usr/bin/env bash
#
# dev.sh — build the daemon and run it against the shipped default config.
# Extra arguments are passed through to `aborad` (e.g. --config other.toml).

set -euo pipefail

cd "$(dirname "$0")/.."

cargo build -p aborad

exec cargo run -p aborad -- --config config/abora.toml.default "$@"