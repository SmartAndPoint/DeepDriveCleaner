#!/bin/bash
# Refuses unless origin holds the tag v<version> at HEAD (story "Build matrix: every
# target built, tested and packaged from one tag", DDC-007). `make dist` runs it before
# ci-green, so the most likely mistake under the tag-first order costs seconds, and
# scripts/build-dist.sh runs it again before building. Origin is asked, as
# scripts/publish-release.sh does: local tags are not fetched in this clone. An
# annotated tag is listed twice, the peeled commit second.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
CARGO="$HOME/.cargo/bin/cargo"
[ -x "$CARGO" ] || CARGO=cargo

fail() { printf '\033[0;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }

VERSION="$("$CARGO" pkgid -p ddc-cli | sed 's/.*[#@]//')"
TAG="v$VERSION"
REFS="$(git ls-remote origin "refs/tags/$TAG" "refs/tags/$TAG^{}")" || fail "git ls-remote origin failed"
TAG_SHA="$(printf '%s\n' "$REFS" | awk -v peeled="refs/tags/$TAG^{}" \
    '$2 == peeled { p = $1 } $2 != peeled && $1 != "" { t = $1 } END { print (p != "" ? p : t) }')"
[ -n "$TAG_SHA" ] || fail "origin has no tag $TAG: run 'make tag' first"
[ "$TAG_SHA" = "$(git rev-parse HEAD)" ] \
    || fail "tag $TAG on origin points at $TAG_SHA, not HEAD: 'git pull' if this clone is behind; a checkout of the tag cannot be published (publish needs HEAD = origin/main). Otherwise bump the version, or, if 'gh release view $TAG' says release not found, delete the tag on origin and run 'make tag'"
