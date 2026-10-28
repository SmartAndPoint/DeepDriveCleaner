#!/bin/bash
# Refuses unless the Gate workflow (.github/workflows/gate.yml) passed on GitHub for
# HEAD: `make dist` builds a release only from a commit CI has already checked.
# Only push runs count. Waits while the run is still going, so a release right after a push does not fail.
set -euo pipefail

fail() { printf '\033[0;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }
step() { printf '\033[0;34m→ %s\033[0m\n' "$1"; }

command -v gh >/dev/null || fail "gh is not installed: it reads the CI result for HEAD"
sha="$(git rev-parse HEAD)"

latest_run() {
    gh run list --commit "$sha" --workflow gate.yml --event push --limit 1 \
        --json databaseId,status,conclusion,url \
        --jq '.[0] // empty | [.databaseId, .status, (.conclusion // ""), .url] | map(tostring) | join("|")'
}

row="$(latest_run)" || fail "gh could not list the Gate runs for HEAD (see its message above)"
[ -n "$row" ] || fail "no Gate run for $sha: push it and wait for CI"
IFS='|' read -r id status conclusion url <<<"$row"

if [ "$status" != completed ]; then
    step "the Gate run for ${sha:0:7} is $status: waiting for it ($url)"
    gh run watch "$id" --exit-status >/dev/null || true
    row="$(latest_run)" || fail "gh could not list the Gate runs for HEAD (see its message above)"
    IFS='|' read -r id status conclusion url <<<"$row"
fi

[ "$conclusion" = success ] || fail "the Gate run for ${sha:0:7} is ${conclusion:-$status}: $url"
step "CI is green for ${sha:0:7}"
