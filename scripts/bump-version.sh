#!/bin/bash
# The one way to change the release version: writes the next version into every place
# that carries it, from the version on HEAD. Cargo.toml's [workspace.package] version is
# the source: make tag, make dist and release.yml read it, and they refuse an app whose
# MARKETING_VERSION differs or whose build number does not grow (check-build-number.sh).
#
#   scripts/bump-version.sh major|minor|patch|X.Y.Z
#
# Writes Cargo.toml, the workspace's crates in Cargo.lock and fuzz/Cargo.lock, and the
# app's MARKETING_VERSION and CURRENT_PROJECT_VERSION (build + 1) in Debug and Release.
# Commits nothing. Prints what merged since the last release tag: the release notes,
# docs/releases/X.Y.Z.md, cover all of it, whichever session wrote it. Run it on a branch
# fresh from origin/main (`make bump V=minor`).
set -euo pipefail

PBXPROJ=app/DeepDriveCleaner.xcodeproj/project.pbxproj

fail() { printf '\033[0;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }

[ $# = 1 ] || fail "usage: scripts/bump-version.sh major|minor|patch|X.Y.Z"
ROOT="$(git rev-parse --show-toplevel)" || fail "not in a git repository"
cd "$ROOT"

CURRENT="$(sed -n 's/^version = "\([0-9]*\.[0-9]*\.[0-9]*\)"$/\1/p' Cargo.toml | head -1)"
[ -n "$CURRENT" ] || fail "Cargo.toml has no version = \"X.Y.Z\""
IFS=. read -r MAJOR MINOR PATCH <<<"$CURRENT"
case "$1" in
    major) NEXT="$((MAJOR + 1)).0.0" ;;
    minor) NEXT="$MAJOR.$((MINOR + 1)).0" ;;
    patch) NEXT="$MAJOR.$MINOR.$((PATCH + 1))" ;;
    *)
        [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "not major, minor, patch or X.Y.Z: $1"
        NEXT="$1"
        [ "$(printf '%s\n%s\n' "$CURRENT" "$NEXT" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1)" = "$NEXT" ] \
            && [ "$NEXT" != "$CURRENT" ] || fail "$NEXT is not above $CURRENT"
        ;;
esac
git rev-parse -q --verify "refs/tags/v$NEXT" >/dev/null && fail "tag v$NEXT exists: pick the next version"

BUILD="$(sed -n 's/^[[:space:]]*CURRENT_PROJECT_VERSION = \([0-9]*\);$/\1/p' "$PBXPROJ" | sort -u)"
[[ "$BUILD" =~ ^[0-9]+$ ]] || fail "$PBXPROJ has no single CURRENT_PROJECT_VERSION"

python3 -I - "$CURRENT" "$NEXT" "$BUILD" "$PBXPROJ" <<'PY'
import re, sys
current, nxt, build, pbxproj = sys.argv[1:]

def write(path, text, old):
    if text == old:
        sys.exit(f"{path}: nothing to change")
    with open(path, "w") as f:
        f.write(text)

with open("Cargo.toml") as f:
    old = f.read()
write("Cargo.toml", re.sub(r'^version = "%s"$' % re.escape(current), f'version = "{nxt}"', old, count=1, flags=re.M), old)
for lock in ("Cargo.lock", "fuzz/Cargo.lock"):
    with open(lock) as f:
        old = f.read()
    write(lock, re.sub(r'(name = "ddc-[\w-]+"\nversion = )"%s"' % re.escape(current), rf'\1"{nxt}"', old), old)
with open(pbxproj) as f:
    old = f.read()
text = re.sub(r'(MARKETING_VERSION = )"?[^";]*"?;', rf'\g<1>{nxt};', old)
text = re.sub(r'(CURRENT_PROJECT_VERSION = )\d+;', rf'\g<1>{int(build) + 1};', text)
write(pbxproj, text, old)
PY

echo "version $CURRENT → $NEXT, build $BUILD → $((BUILD + 1))"
LAST="$(git -c versionsort.suffix=- tag -l 'v[0-9]*' --sort=-v:refname | head -1)"
if [ -n "$LAST" ]; then
    echo "merged since $LAST (docs/releases/$NEXT.md covers all of it):"
    git log --oneline --no-merges "$LAST..HEAD"
fi
