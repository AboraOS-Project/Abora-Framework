#!/usr/bin/env bash
# List the "system files" a fork still has to customize.
#
# A system file carries one comment line like
#
#     # ABORA-SYSTEM-FILE 2: name your boot entry
#
# The number is the order to edit them in. When you have edited the file for
# your fork, delete that line; it then drops off this list.
#
# Usage: scripts/fork-check.sh [--strict]   (--strict exits 1 while any remain, for CI)

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Only files tracked by git, or all files if this is not a git checkout.
if git rev-parse --git-dir >/dev/null 2>&1; then FILES="$(git ls-files)"; else FILES="$(find . -type f -not -path './target/*' -not -path './build/*' | sed 's#^\./##')"; fi

# Match the marker only at the start of a comment (# or //), so docs and this script don't count.
PATTERN='^[[:space:]]*(#|//)[[:space:]]*ABORA-SYSTEM-FILE[[:space:]]+[0-9]+:'
LIST="$(printf '%s\n' "$FILES" | xargs -d '\n' grep -InE "$PATTERN" 2>/dev/null \
    | sed -E 's/^([^:]+):([0-9]+):[[:space:]]*(#|\/\/)[[:space:]]*ABORA-SYSTEM-FILE[[:space:]]+([0-9]+):[[:space:]]*(.*)$/\4\t\1:\2\t\5/' \
    | sort -n || true)"

if [ -z "$LIST" ]; then
    echo "fork-check: no system files left to customize."
    exit 0
fi
echo "System files still to customize (edit in this order, then delete the ABORA-SYSTEM-FILE line):"
echo
printf '%s\n' "$LIST" | awk -F'\t' '{ printf "  %s. %s\n     %s\n", $1, $2, $3 }'
[ "${1:-}" = "--strict" ] && exit 1
exit 0
