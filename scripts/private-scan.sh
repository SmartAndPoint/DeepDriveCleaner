#!/bin/bash
# Refuses a commit that would put a private value into this public repository:
#
#   scripts/private-scan.sh staged          the lines a commit adds, and its file names
#   scripts/private-scan.sh message <file>  a commit message
#
# The values come from two local files that are never committed, beside the main
# checkout (a worktree finds them through git's common folder):
#   .env                     every VAR=value whose value has at least 8 characters,
#                            matched exactly;
#   .local/private-strings   one string per line (# starts a comment), matched as a
#                            whole word, ignoring case: the owner's machines, their
#                            addresses and user names.
# DDC_PRIVATE_ENV and DDC_PRIVATE_LIST name other files (the vault's hooks use them).
#
# A match names the variable or the list's line number, never the value, so the value
# reaches neither the terminal nor a log. A missing file checks nothing.
set -euo pipefail
export LC_ALL=C

usage() { echo "usage: scripts/private-scan.sh staged | message <file>" >&2; exit 2; }
mode="${1:-}"
case "$mode" in
    staged) [ "$#" -eq 1 ] || usage ;;
    message) [ "$#" -eq 2 ] || usage; message="$2" ;;
    *) usage ;;
esac

common="$(git rev-parse --path-format=absolute --git-common-dir)"
root="$(dirname "$common")"
env_file="${DDC_PRIVATE_ENV:-$root/.env}"
list_file="${DDC_PRIVATE_LIST:-$root/.local/private-strings}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
n=0

# One file per pattern, so a value never appears on a command line.
if [ -f "$env_file" ]; then
    while IFS= read -r line || [ -n "$line" ]; do
        line="${line#export }"
        case "$line" in [A-Za-z_]*=*) ;; *) continue ;; esac
        name="${line%%=*}"
        value="${line#*=}"
        case "$value" in \"*\") value="${value#\"}"; value="${value%\"}" ;; \'*\') value="${value#\'}"; value="${value%\'}" ;; esac
        [ "${#value}" -ge 8 ] || continue
        n=$((n + 1))
        printf '%s\n' "$value" > "$tmp/p$n"
        printf 'the value of %s in .env\n' "$name" > "$tmp/l$n"
        : > "$tmp/exact$n"
    done < "$env_file"
fi
if [ -f "$list_file" ]; then
    i=0
    while IFS= read -r line || [ -n "$line" ]; do
        i=$((i + 1))
        case "$line" in ''|'#'*) continue ;; esac
        n=$((n + 1))
        printf '%s\n' "$line" > "$tmp/p$n"
        printf 'line %d of .local/private-strings\n' "$i" > "$tmp/l$n"
    done < "$list_file"
fi
[ "$n" -gt 0 ] || exit 0

found=0
# check <label for the text> <file holding the text>
check() {
    local k
    for k in $(seq 1 "$n"); do
        if [ -e "$tmp/exact$k" ]; then
            grep -qF -f "$tmp/p$k" -- "$2" || continue
        else
            grep -qiwF -f "$tmp/p$k" -- "$2" || continue
        fi
        echo "private-scan: $1 holds $(cat "$tmp/l$k")" >&2
        found=1
    done
}

if [ "$mode" = message ]; then
    check "the commit message" "$message"
else
    git diff --cached --name-only --diff-filter=ACMR -z |
        while IFS= read -r -d '' path; do printf '%s\n' "$path"; done > "$tmp/names"
    check "a staged file name" "$tmp/names"
    while IFS= read -r path; do
        git diff --cached -U0 --no-color --no-ext-diff -- "$path" |
            sed -n '/^+++ /d; s/^+//p' > "$tmp/added"
        check "$path" "$tmp/added"
    done < "$tmp/names"
fi

if [ "$found" -eq 1 ]; then
    echo "private-scan: take the value out; never commit with --no-verify. The values stay in .env and .local/private-strings." >&2
    exit 1
fi
