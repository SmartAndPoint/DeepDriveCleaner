#!/bin/bash
# Refuses unless a workflow passed on GitHub for HEAD: by default the Gate
# (.github/workflows/gate.yml), as `make dist` and `make tag` build and tag only a
# commit CI has already checked; release.yml's tag run for scripts/publish-release.sh.
#
#   scripts/ci-green.sh [workflow file]
#
# Only push runs count. Waits while the run is still going, so a release right after a push does not fail.
set -euo pipefail

fail() { printf '\033[0;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }
step() { printf '\033[0;34m→ %s\033[0m\n' "$1"; }

[ "$#" -le 1 ] || fail "usage: scripts/ci-green.sh [workflow file]"
workflow="${1:-gate.yml}"
command -v gh >/dev/null || fail "gh is not installed: it reads the CI result for HEAD"
sha="$(git rev-parse HEAD)"

latest_run() {
    gh run list --commit "$sha" --workflow "$workflow" --event push --limit 1 \
        --json databaseId,status,conclusion,url \
        --jq '.[0] // empty | [.databaseId, .status, (.conclusion // ""), .url] | map(tostring) | join("|")'
}

row="$(latest_run)" || fail "gh could not list the $workflow runs for HEAD (see its message above)"
[ -n "$row" ] || fail "no $workflow push run for $sha: push it (a tag, for release.yml) and wait for CI"
IFS='|' read -r id status conclusion url <<<"$row"

if [ "$status" != completed ]; then
    step "the $workflow run for ${sha:0:7} is $status: waiting for it ($url)"
    gh run watch "$id" --exit-status >/dev/null || true
    row="$(latest_run)" || fail "gh could not list the $workflow runs for HEAD (see its message above)"
    IFS='|' read -r id status conclusion url <<<"$row"
fi

[ "$conclusion" = success ] || fail "the $workflow run for ${sha:0:7} is ${conclusion:-$status}: $url"
step "CI is green for ${sha:0:7} ($workflow)"
