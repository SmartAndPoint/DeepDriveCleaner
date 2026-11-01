#!/bin/bash
# Which tiers of the gate a change needs (story "Gate by what changed: load at night, one
# gate per release, app-only pushes skip Rust", DDC-005). A change whose every path is on
# .config/rust-free-paths skips the Rust tiers; any other path, and every doubt, runs them.
#
#   scripts/gate-scope.sh paths       newline-separated repo paths on stdin
#   scripts/gate-scope.sh staged      the index, for .githooks/pre-commit
#   scripts/gate-scope.sh staged-cases   the index again: whether pre-commit runs `make cases`
#   scripts/gate-scope.sh mounts <sha>   whether the Gate's linux-mounts job runs its tests
#   scripts/gate-scope.sh push        git's pre-push lines on stdin, for .githooks/pre-push
#   scripts/gate-scope.sh ci <sha>    the Gate's scope job: <sha> against the newest ancestor
#                                     whose push run passed gate, coverage, release-build and
#                                     the two native check jobs
#
# Prints one word and exits 0: `rust`, `skip`, or for `push` `swift` (the Rust tiers are
# skipped, DDCKit's package changed); `staged-cases` and `mounts` print `yes` or `no`. A usage error prints `rust` and exits 2. Callers
# take anything but `skip` or `swift` as `rust`, an empty answer included. Every diff is
# --no-renames, so a file moved out of crates/ shows where it came from.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LIST="$ROOT/.config/rust-free-paths"
# The Gate jobs a base run must have passed: the ones the scope job skips that judge.
BASE_JOBS='["gate","coverage","release-build","check-x86_64-unknown-linux-gnu","check-x86_64-pc-windows-msvc"]'
MAX_ANCESTORS=20

answer() { printf '%s\n' "$1"; exit 0; }
rust() { printf 'gate-scope: %s: the full gate\n' "$1" >&2; answer rust; }
usage() { printf 'usage: scripts/gate-scope.sh paths | staged | staged-cases | mounts <sha> | push | ci <sha>\n' >&2; printf 'rust\n'; exit 2; }

ON=()
OFF=()
while IFS= read -r entry || [ -n "$entry" ]; do
    case "$entry" in
        '' | '#'*) ;;
        '!'*) OFF+=("${entry#!}") ;;
        *) ON+=("$entry") ;;
    esac
done <"$LIST"

# 0 when `path` matches an entry of the list (a folder ends in /, anything else is a file).
matches() {
    local path="$1" entry
    shift
    for entry in "$@"; do
        case "$entry" in
            */) [[ "$path" == "$entry"* ]] && return 0 ;;
            *) [ "$path" = "$entry" ] && return 0 ;;
        esac
    done
    return 1
}

# What decides the scope is never on the list, whatever the list says: an edit to it runs
# the gate that checks it.
NEVER=(.config/rust-free-paths scripts/ .github/ .githooks/ Makefile)

listed() { ! matches "$1" "${NEVER[@]}" && matches "$1" "${ON[@]}" && ! matches "$1" "${OFF[@]+"${OFF[@]}"}"; }

# Newline-separated paths on stdin: `rust` when one is off the list, otherwise `swift` when
# one is in app/Packages/ and `skip` when none is. No path at all is `rust`: an empty
# change set proves nothing. git quotes a name with a tab, a newline or a quote; the quote
# is on no list, so such a name answers `rust`.
classify() {
    local path any=no swift=no
    while IFS= read -r path; do
        [ -n "$path" ] || continue
        any=yes
        listed "$path" || { echo rust; return; }
        case "$path" in app/Packages/*) swift=yes ;; esac
    done
    [ "$any" = yes ] || { echo rust; return; }
    [ "$swift" = yes ] && echo swift || echo skip
}

# `git diff --name-only` of two trees, never through a pipe: a failed git must not leave a
# partial list behind.
changed() {
    git -c core.quotePath=false diff --no-renames --name-only "$1" "$2"
}

staged_scope() {
    local names
    names="$(git -c core.quotePath=false diff --cached --no-renames --name-only)" || rust "git could not list the staged paths"
    [ -n "$names" ] || rust "nothing staged"
    case "$(classify <<<"$names")" in
        skip | swift) answer skip ;;
        *) answer rust ;;
    esac
}

# Whether the commit runs the CLI cases (story "CLI invocation cases: a table of ddc calls
# and the suggested commands they print", DDC-005): `yes` when a staged path is under
# crates/ or is Cargo.toml or Cargo.lock, and on every doubt (git failing, nothing staged,
# a name git had to quote); `no` otherwise. A word of its own, so `staged` keeps its one.
staged_cases_scope() {
    local names path
    names="$(git -c core.quotePath=false diff --cached --no-renames --name-only)" || answer yes
    [ -n "$names" ] || answer yes
    while IFS= read -r path; do
        case "$path" in
            crates/* | Cargo.toml | Cargo.lock | '"'*) answer yes ;;
        esac
    done <<<"$names"
    answer no
}

# Whether the linux-mounts job runs its tests on <sha> (story "Linux CI mounts: a
# filesystem matrix job with root-only steps", DDC-010): `yes` when a path changed since
# the merge base with origin/main is one the mounted filesystems judge (the Linux
# platform module, the walk, the clean, any test, the mounts script, the skip lists, this
# workflow, the manifests), and on every doubt (git failing, no merge base, nothing to
# diff, as on main itself, a name git had to quote); `no` otherwise.
MOUNTS=(crates/ddc-core/src/platform/linux/ crates/ddc-core/src/scan/ crates/ddc-core/src/exec/
    crates/ddc-core/tests/ crates/ddc-cli/tests/ crates/ddc-loadgen/tests/ crates/ddc-core/Cargo.toml
    Cargo.toml Cargo.lock rust-toolchain.toml scripts/ci-mounts.sh scripts/native-skips.sh
    .config/native-skips/ .config/nextest.toml .github/workflows/gate.yml .github/workflows/nightly.yml)
mounts_scope() {
    local head="$1" base names path
    git cat-file -e "$head^{commit}" 2>/dev/null || answer yes
    base="$(git merge-base origin/main "$head^{commit}" 2>/dev/null)" || answer yes
    names="$(changed "$base" "$head^{commit}")" || answer yes
    [ -n "$names" ] || answer yes
    while IFS= read -r path; do
        case "$path" in '"'*) answer yes ;; esac
        matches "$path" "${MOUNTS[@]}" && answer yes
    done <<<"$names"
    answer no
}

# A deletion sends nothing, and a tag adds no branch: gate.yml ignores tags, and a release
# checks its commit again (ci-green --full, require-tag.sh). A push of only those skips;
# a branch line beside them is judged as usual.
push_scope() {
    local local_ref local_sha remote_ref remote_sha base names all="" lines=no branches=no
    while read -r local_ref local_sha remote_ref remote_sha; do
        [ -n "$local_ref" ] || continue
        lines=yes
        case "$local_sha" in *[!0]*) ;; *) continue ;; esac
        case "$remote_ref" in refs/tags/*) continue ;; esac
        branches=yes
        case "$remote_sha" in
            *[!0]*)
                git cat-file -e "$remote_sha^{commit}" 2>/dev/null || rust "$remote_ref: the remote's ${remote_sha:0:7} is not in this clone"
                base="$remote_sha"
                ;;
            *) base="$(git merge-base origin/main "$local_sha^{commit}" 2>/dev/null)" || rust "$local_ref: no merge base with origin/main" ;;
        esac
        names="$(changed "$base" "$local_sha^{commit}")" || rust "git could not diff $local_ref"
        [ -n "$names" ] || continue
        all+="$names"$'\n'
    done
    if [ -z "$all" ]; then
        [ "$lines" = yes ] && [ "$branches" = no ] && answer skip
        rust "nothing pushed to judge"
    fi
    answer "$(classify <<<"$all")"
}

ci_scope() {
    local head="$1" runs rows id sha jobs passed names checked=0
    [ "${GITHUB_EVENT_NAME:-}" = push ] || rust "a ${GITHUB_EVENT_NAME:-local} run"
    [ -n "${GITHUB_REPOSITORY:-}" ] || rust "GITHUB_REPOSITORY is not set"
    command -v gh >/dev/null || rust "gh is not installed"
    command -v jq >/dev/null || rust "jq is not installed"
    git cat-file -e "$head^{commit}" 2>/dev/null || rust "${head:0:7} is not in this clone"
    runs="$(gh api "repos/$GITHUB_REPOSITORY/actions/workflows/gate.yml/runs?event=push&status=success&per_page=50")" ||
        rust "gh could not list the Gate runs"
    rows="$(jq -r '.workflow_runs | sort_by(.created_at) | reverse | .[] | "\(.id) \(.head_sha)"' <<<"$runs")" ||
        rust "the Gate runs are not the JSON expected"
    while read -r id sha; do
        [ -n "$id" ] || continue
        git cat-file -e "$sha^{commit}" 2>/dev/null || continue
        git merge-base --is-ancestor "$sha" "$head" 2>/dev/null || continue
        checked=$((checked + 1))
        [ "$checked" -le "$MAX_ANCESTORS" ] || break
        jobs="$(gh api "repos/$GITHUB_REPOSITORY/actions/runs/$id/jobs?filter=latest&per_page=100")" ||
            rust "gh could not read the jobs of run $id"
        passed="$(jq --argjson want "$BASE_JOBS" \
            '[.jobs[] | select(.name as $n | $want | index($n)) | select(.conclusion == "success")] | length' <<<"$jobs")" ||
            rust "the jobs of run $id are not the JSON expected"
        [ "$passed" = "$(jq length <<<"$BASE_JOBS")" ] || continue
        [ "$sha" != "$(git rev-parse "$head^{commit}")" ] || answer skip
        names="$(changed "$sha" "$head")" || rust "git could not diff ${sha:0:7}..${head:0:7}"
        printf 'gate-scope: base %s (run %s)\n' "${sha:0:7}" "$id" >&2
        case "$(classify <<<"$names")" in
            skip | swift) answer skip ;;
            *) rust "a path off .config/rust-free-paths changed since ${sha:0:7}" ;;
        esac
    done <<<"$rows"
    rust "no Gate run passed gate, coverage, release-build and the native checks on an ancestor of ${head:0:7}"
}

case "${1:-}" in
    paths) [ "$#" -eq 1 ] || usage; answer "$(classify)" ;;
    staged) [ "$#" -eq 1 ] || usage; staged_scope ;;
    staged-cases) [ "$#" -eq 1 ] || usage; staged_cases_scope ;;
    mounts) [ "$#" -eq 2 ] || usage; mounts_scope "$2" ;;
    push) [ "$#" -eq 1 ] || usage; push_scope ;;
    ci) [ "$#" -eq 2 ] || usage; ci_scope "$2" ;;
    *) usage ;;
esac
