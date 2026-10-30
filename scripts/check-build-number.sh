#!/bin/bash
# Refuses a release whose app build number (CURRENT_PROJECT_VERSION, the app's
# CFBundleVersion) is not greater than the one at the last release tag (ADR "Updates
# through Sparkle, alongside the Homebrew cask", §6). Sparkle compares CFBundleVersion,
# not the short version, so a build number that does not grow is never offered.
#
#   scripts/check-build-number.sh [--no-fetch]
#
# Reads the repository it runs in. The release tags are fetched from origin first, so a
# tag made on another machine counts; --no-fetch is for scripts/tests/test_build_number.py.
# scripts/build-dist.sh and scripts/build-app.sh --dist run it before notarizing.
set -euo pipefail

PBXPROJ=app/DeepDriveCleaner.xcodeproj/project.pbxproj

fail() { printf '\033[0;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }

FETCH=1
case "${1:-}" in
    "") ;;
    --no-fetch) FETCH=0 ;;
    *) fail "usage: scripts/check-build-number.sh [--no-fetch]" ;;
esac

ROOT="$(git rev-parse --show-toplevel)" || fail "not in a git repository"
cd "$ROOT"

# The project's one CURRENT_PROJECT_VERSION, from standard input; fails unless Debug
# and Release agree on a whole number.
build_number() {
    local numbers
    numbers="$(sed -n 's/^[[:space:]]*CURRENT_PROJECT_VERSION = \(.*\);$/\1/p' | sort -u)"
    [[ "$numbers" =~ ^[0-9]+$ ]] || return 1
    echo "$numbers"
}

if [ "$FETCH" = 1 ]; then
    git fetch -q --tags origin || fail "git fetch --tags origin failed: the last release tag is unknown"
fi
CURRENT="$(build_number < "$PBXPROJ")" || fail "$PBXPROJ has no single CURRENT_PROJECT_VERSION"

# The highest version tag, wherever it points: a release is never cut below it.
TAGS="$(git tag -l 'v[0-9]*' --sort=-v:refname)" || fail "git tag failed"
TAG="${TAGS%%$'\n'*}"
if [ -z "$TAG" ]; then
    echo "build number: $CURRENT (no release tag yet)"
    exit 0
fi
TAGGED="$(git show "$TAG:$PBXPROJ" 2>/dev/null)" || fail "$TAG has no $PBXPROJ"
PREVIOUS="$(build_number <<<"$TAGGED")" || fail "$PBXPROJ at $TAG has no single CURRENT_PROJECT_VERSION"
[ "$CURRENT" -gt "$PREVIOUS" ] \
    || fail "CURRENT_PROJECT_VERSION is $CURRENT, not greater than $PREVIOUS at $TAG: Sparkle would never offer this build. Raise CURRENT_PROJECT_VERSION in $PBXPROJ to $((PREVIOUS + 1)), in Debug and Release"
echo "build number: $CURRENT, after $PREVIOUS at $TAG"
