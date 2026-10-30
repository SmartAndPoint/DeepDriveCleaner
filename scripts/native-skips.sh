#!/bin/bash
# Compares the tests a native Linux or Windows job skipped with the committed list
# (story "Build matrix: every target built, tested and packaged from one tag", DDC-007,
# stage 2; ADR "Platform seam: one platform module per OS with declared capabilities",
# §9, amended):
#
#   scripts/native-skips.sh <os> <test log>
#
# Every skip prints "skipped <test>: <reason>", and nextest's `ci` profile shows the
# output of passing tests, so the log holds one such line per skip. Their test names,
# sorted and unique, must equal .config/native-skips/<os>.txt (blank lines and #
# comments aside): a new skip, or a listed test that no longer skips, fails with the
# difference. The list is keyed by OS, not by target triple, so every Linux target
# shares one. Writes the counts to $GITHUB_STEP_SUMMARY when it is set.
set -euo pipefail
export LC_ALL=C

fail() { printf 'native-skips: %s\n' "$1" >&2; exit 1; }

[ "$#" -eq 2 ] || { echo "usage: scripts/native-skips.sh <os> <test log>" >&2; exit 2; }
os="$1"
log="$2"
list="$(cd "$(dirname "$0")/.." && pwd)/.config/native-skips/$os.txt"
[ -f "$list" ] || fail "no list for $os: .config/native-skips/$os.txt"
[ -f "$log" ] || fail "no test log at $log"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Colour codes off, then the name of each skip line; nextest indents a test's output.
sed -E $'s/\x1b\\[[0-9;]*m//g' "$log" \
    | sed -nE 's/^[[:space:]]*skipped ([A-Za-z0-9_]+): .*/\1/p' \
    | sort -u >"$tmp/skipped"
{ grep -vE '^[[:space:]]*(#|$)' "$list" || true; } | sed -E 's/[[:space:]]+$//' | sort -u >"$tmp/listed"

skipped="$(wc -l <"$tmp/skipped" | tr -d ' ')"
listed="$(wc -l <"$tmp/listed" | tr -d ' ')"
summary="$(sed -E $'s/\x1b\\[[0-9;]*m//g' "$log" | grep -E '^[[:space:]]*Summary .* tests? run:' | tail -n 1 \
    | sed -E 's/^[[:space:]]*Summary \[[^]]*\][[:space:]]*//' || true)"

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    {
        echo "### Native tests: $os"
        echo
        echo "- nextest: ${summary:-no summary line in the log}"
        echo "- $skipped skip with a printed reason; .config/native-skips/$os.txt lists $listed"
    } >>"$GITHUB_STEP_SUMMARY"
fi

if ! diff -u --label ".config/native-skips/$os.txt" --label "skipped in this run" "$tmp/listed" "$tmp/skipped"; then
    fail "the skips differ from .config/native-skips/$os.txt (+ a new skip, - a listed test that ran): fix the test, or list the skip in the same change"
fi
echo "native-skips: $skipped tests skip on $os, as listed (${summary:-no summary line})"
