#!/bin/bash
# Refuses unless a workflow passed on GitHub for HEAD: by default the Gate
# (.github/workflows/gate.yml), as `make dist` and `make tag` build and tag only a
# commit CI has already checked; release.yml's tag run for scripts/publish-release.sh.
#
#   scripts/ci-green.sh [--full] [workflow file]
#
# Only push runs count. Waits while the run is still going, so a release right after a merge does not fail.
# A green Gate run may have skipped its Rust jobs: the commit changed only Rust-free paths
# since a fully green ancestor (scripts/gate-scope.sh). --full, for `make tag` and `make
# dist`, also requires the gate, coverage, release-build and the two native check jobs of
# HEAD's own run to have passed, so a release never rests on that rule.
set -euo pipefail

fail() { printf '\033[0;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }
step() { printf '\033[0;34m→ %s\033[0m\n' "$1"; }

full=no
if [ "${1:-}" = --full ]; then
    full=yes
    shift
fi
[ "$#" -le 1 ] || fail "usage: scripts/ci-green.sh [--full] [workflow file]"
workflow="${1:-gate.yml}"
command -v gh >/dev/null || fail "gh is not installed: it reads the CI result for HEAD"
sha="$(git rev-parse HEAD)"

latest_run() {
    gh run list --commit "$sha" --workflow "$workflow" --event push --limit 1 \
        --json databaseId,status,conclusion,url \
        --jq '.[0] // empty | [.databaseId, .status, (.conclusion // ""), .url] | map(tostring) | join("|")'
}

row="$(latest_run)" || fail "gh could not list the $workflow runs for HEAD (see its message above)"
[ -n "$row" ] || fail "no $workflow push run for ${sha:0:7} yet: after a PR merges, main's run starts within a minute (a tag's, for release.yml); wait and rerun"
IFS='|' read -r id status conclusion url <<<"$row"

if [ "$status" != completed ]; then
    step "the $workflow run for ${sha:0:7} is $status: waiting for it ($url)"
    gh run watch "$id" --exit-status >/dev/null || true
    row="$(latest_run)" || fail "gh could not list the $workflow runs for HEAD (see its message above)"
    IFS='|' read -r id status conclusion url <<<"$row"
fi

[ "$conclusion" = success ] || fail "the $workflow run for ${sha:0:7} is ${conclusion:-$status}: $url"
if [ "$full" = yes ]; then
    # Any green run of HEAD that ran them: the push run, or one started by hand
    # (workflow_dispatch always runs the Rust jobs).
    full_run=""
    for run in $(gh run list --commit "$sha" --workflow "$workflow" --status success --limit 20 \
        --json databaseId,event --jq '.[] | select(.event == "push" or .event == "workflow_dispatch") | .databaseId'); do
        passed="$(gh api "repos/{owner}/{repo}/actions/runs/$run/jobs?filter=latest&per_page=100" \
            --jq '[.jobs[] | select(.name == "gate" or .name == "coverage" or .name == "release-build"
                    or .name == "check-x86_64-unknown-linux-gnu" or .name == "check-x86_64-pc-windows-msvc")
                | select(.conclusion == "success")] | length')" || fail "gh could not read the jobs of run $run"
        if [ "$passed" = 5 ]; then
            full_run="$run"
            break
        fi
    done
    [ -n "$full_run" ] || fail "no $workflow run for ${sha:0:7} ran its Rust jobs (only Rust-free paths changed since a green ancestor, $url): a release needs them on its own commit. While HEAD is still main's tip, run 'gh workflow run $workflow --ref main', wait for it to pass, and rerun"
fi
step "CI is green for ${sha:0:7} ($workflow)"
