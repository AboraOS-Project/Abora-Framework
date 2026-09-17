#!/usr/bin/env bash
#
# check.sh — build, test, and (when installed) lint the entire workspace.
#
# Distro-packaged Rust often lacks `cargo fmt` / `cargo clippy`, and its
# rustdoc can fail to load libLLVM. We detect those situations, apply a
# harmless LD_LIBRARY_PATH workaround for rustdoc, and report skipped
# checks rather than failing the whole run.

set -euo pipefail

cd "$(dirname "$0")/.."

# --- rustdoc workaround for distro Rust -----------------------------------
ensure_rustdoc() {
    if rustdoc --version >/dev/null 2>&1; then
        return 0
    fi
    local host
    host="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')" || true
    local dir="/usr/lib/rustlib/${host}/lib"
    if [[ -n "${host}" && -d "${dir}" ]] && LD_LIBRARY_PATH="${dir}" rustdoc --version >/dev/null 2>&1; then
        echo "note: rustdoc needs LD_LIBRARY_PATH=${dir}; exporting it"
        export LD_LIBRARY_PATH="${dir}${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
        return 0
    fi
    echo "warn: rustdoc unavailable; tests that hit doctests may fail"
    return 1
}
ensure_rustdoc || true

has() { command -v "$1" >/dev/null 2>&1; }

# --- build ---------------------------------------------------------------
echo "==> cargo build --workspace"
cargo build --workspace

# --- test ----------------------------------------------------------------
echo "==> cargo test --workspace"
cargo test --workspace

# --- fmt (optional) ------------------------------------------------------
if has cargo-fmt; then
    echo "==> cargo fmt --all -- --check"
    cargo fmt --all -- --check
else
    echo "warn: cargo fmt not installed; skipping format check"
fi

# --- clippy (optional) ---------------------------------------------------
if has cargo-clippy; then
    echo "==> cargo clippy --workspace --all-targets -- -D warnings"
    cargo clippy --workspace --all-targets -- -D warnings
else
    echo "warn: cargo clippy not installed; skipping lint check"
fi

echo "All checks done."