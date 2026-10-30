#!/bin/bash
# Pushes the annotated release tag v<version> at HEAD of origin/main (`make tag`, after
# a green Gate run for HEAD; story "Build matrix: every target built, tested and
# packaged from one tag", DDC-007). The tag starts release.yml, which builds the
# other targets; `make dist` and scripts/publish-release.sh then require the tag on
# origin at the commit they build and publish. The owner runs it: the tag is the
# release's start, and nothing else creates it.
#
# The version is read from Cargo.toml and never passed in, as in publish-release.sh.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"
CARGO="$HOME/.cargo/bin/cargo"
[ -x "$CARGO" ] || CARGO=cargo

fail() { printf '\033[0;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }
step() { printf '\033[0;34m→ %s\033[0m\n' "$1"; }

[ "$#" = 0 ] || fail "usage: scripts/tag-release.sh (the version comes from Cargo.toml)"

# A tag on uncommitted or unpushed code names a commit nobody else can build.
[ -z "$(git status --porcelain)" ] || fail "the tree is not clean (untracked files count): commit first"
[ "$(git branch --show-current)" = "main" ] || fail "not on main: a release is tagged on main"
git fetch -q origin main
HEAD_SHA="$(git rev-parse HEAD)"
[ "$HEAD_SHA" = "$(git rev-parse origin/main)" ] || fail "HEAD is not origin/main: push first, or pull"
# Here, not only in the Makefile, so running this script directly cannot skip it.
"$ROOT/scripts/ci-green.sh"

VERSION="$("$CARGO" pkgid -p ddc-cli | sed 's/.*[#@]//')"
TAG="v$VERSION"
# The app ships in the same release: scripts/build-dist.sh refuses this too, but only
# after the tag would already be out.
APP_VERSION="$(sed -n 's/^[[:space:]]*MARKETING_VERSION = \(.*\);$/\1/p' app/DeepDriveCleaner.xcodeproj/project.pbxproj | sort -u)"
[ "$APP_VERSION" = "$VERSION" ] || fail "the app's MARKETING_VERSION ($APP_VERSION) is not the CLI's version ($VERSION): bump both"
[ -f "docs/releases/$VERSION.md" ] || fail "docs/releases/$VERSION.md missing: write the release notes first"

# Never moved: a tag that exists names a release, or a failed one whose recovery is
# deleting it by hand on origin.
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    fail "tag $TAG exists here: remove it with 'git tag -d $TAG' if origin has none, or bump the version"
fi
REMOTE="$(git ls-remote origin "refs/tags/$TAG")" || fail "git ls-remote origin failed"
[ -z "$REMOTE" ] || fail "tag $TAG exists on origin: bump the version, or, if release.yml failed and there is no release, delete it with 'git push origin :refs/tags/$TAG'"

step "Tagging $TAG at ${HEAD_SHA:0:7}"
git tag -a "$TAG" -m "DeepDriveCleaner $VERSION" "$HEAD_SHA"
# A tag left here by a failed push would block the rerun.
git push origin "refs/tags/$TAG" || { git tag -d "$TAG" >/dev/null; fail "pushing $TAG failed: the local tag was removed, rerun"; }

printf '\033[0;32m✅ pushed %s\033[0m\n' "$TAG"
echo "   next: wait for release.yml on $TAG, then make dist and scripts/publish-release.sh --dry-run"
