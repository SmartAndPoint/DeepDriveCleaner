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
#
# Groups (story "Linux CI mounts: a filesystem matrix job with root-only steps", DDC-010):
# a comment line "# [environment] <why>" starts the group of tests that need the CI
# mounts, and any other "# [<name>]" line ends it. Such a test skips with a reason that
# starts "no <kind> mount" or "no <kind> fixture" (tests/common/mounts.rs), and only a
# test of that group may skip so.
#
#   scripts/native-skips.sh --filter <os>          the nextest filter of the environment group
#   scripts/native-skips.sh --mounts <os> <log>    the linux-mounts job's run of that filter:
#                                                  every test of the group passed, none skipped
set -euo pipefail
export LC_ALL=C

fail() { printf 'native-skips: %s\n' "$1" >&2; exit 1; }

usage() {
    echo "usage: scripts/native-skips.sh <os> <test log> | --filter <os> | --mounts <os> <test log>" >&2
    exit 2
}
mode=check
case "${1:-}" in
    --filter) [ "$#" -eq 2 ] || usage; mode=filter; shift ;;
    --mounts) [ "$#" -eq 3 ] || usage; mode=mounts; shift ;;
    *) [ "$#" -eq 2 ] || usage ;;
esac
os="$1"
log="${2:-}"
list="$(cd "$(dirname "$0")/.." && pwd)/.config/native-skips/$os.txt"
[ -f "$list" ] || fail "no list for $os: .config/native-skips/$os.txt"
[ "$mode" = filter ] || [ -f "$log" ] || fail "no test log at $log"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# The listed names, and those of the environment group.
awk '
    /^[[:space:]]*#[[:space:]]*\[environment\]/ { env = 1; next }
    /^[[:space:]]*#[[:space:]]*\[/ { env = 0; next }
    /^[[:space:]]*(#|$)/ { next }
    { sub(/[[:space:]]+$/, ""); print > (env ? envfile : "/dev/null"); print }
' envfile="$tmp/env.unsorted" "$list" | sort -u >"$tmp/listed"
touch "$tmp/env.unsorted"
sort -u "$tmp/env.unsorted" >"$tmp/env"

if [ "$mode" = filter ]; then
    if [ -s "$tmp/env" ]; then
        sed 's/.*/test(=&)/' "$tmp/env" | paste -sd '|' - | sed 's/|/ | /g'
    else
        echo 'none()'
    fi
    exit 0
fi

# Colour codes off; nextest indents a test's output.
sed -E $'s/\x1b\\[[0-9;]*m//g' "$log" >"$tmp/log"
sed -nE 's/^[[:space:]]*skipped ([A-Za-z0-9_]+): .*/\1/p' "$tmp/log" | sort -u >"$tmp/skipped"
sed -nE 's/^[[:space:]]*skipped ([A-Za-z0-9_]+): no .+ (mount|fixture)( .*)?$/\1/p' "$tmp/log" | sort -u >"$tmp/env-skipped"

if [ "$mode" = mounts ]; then
    [ -s "$tmp/env" ] || fail "the environment group of .config/native-skips/$os.txt is empty"
    if [ -s "$tmp/skipped" ]; then
        fail "skipped in the mounts job, where every mount is made: $(paste -sd ' ' "$tmp/skipped")"
    fi
    sed -nE 's/^[[:space:]]*PASS .*[ :]([A-Za-z0-9_]+)[[:space:]]*$/\1/p' "$tmp/log" | sort -u >"$tmp/passed"
    missing="$(comm -23 "$tmp/env" "$tmp/passed" | paste -sd ' ' -)"
    [ -z "$missing" ] || fail "the environment group's tests did not pass in the mounts job: $missing"
    echo "native-skips: the $(wc -l <"$tmp/env" | tr -d ' ') tests of the environment group passed on the mounts"
    exit 0
fi

outside="$(comm -23 "$tmp/env-skipped" "$tmp/env" | paste -sd ' ' -)"
[ -z "$outside" ] || fail "a mount skip outside the environment group of .config/native-skips/$os.txt: $outside"
other="$(comm -23 "$tmp/env" "$tmp/env-skipped" | comm -12 - "$tmp/skipped" | paste -sd ' ' -)"
[ -z "$other" ] || fail "a test of the environment group skipped for another reason than a missing mount: $other"

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
