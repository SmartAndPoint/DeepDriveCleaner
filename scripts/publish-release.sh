#!/bin/bash
# Publishes the tarball that `make dist` built and points the Homebrew cask at it.
#
# Two repositories, in this order and no other (the same contract as GrammarHelper's
# publish script):
#   1. SmartAndPoint/DeepDriveCleaner — release v<version> with the tarball, its tag
#      created at the built commit
#   2. SmartAndPoint/homebrew-tap     — Casks/deepdrivecleaner.rb
# A cask that names an asset which does not exist yet breaks `brew install` for
# everyone who already ran `brew update`; a release nobody points at yet breaks
# nothing. So the asset goes first.
#
# The version is read from Cargo.toml and never passed in: a hand-typed version is
# how the cask and the binary drift apart.
set -euo pipefail

REPO="SmartAndPoint/DeepDriveCleaner"
TAP_REPO="SmartAndPoint/homebrew-tap"
CASK_PATH="Casks/deepdrivecleaner.rb"
TARGET=aarch64-apple-darwin
TEAM_ID=8MSDJTBW2D
GIT_NAME="Evgenii Konev"
GIT_EMAIL="ekonev@smartandpoint.com"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"
CARGO="$HOME/.cargo/bin/cargo"
TEMPLATE="$ROOT/packaging/homebrew/deepdrivecleaner.rb.tmpl"

fail() { printf '\033[0;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }
step() { printf '\033[0;34m→ %s\033[0m\n' "$1"; }

DRY_RUN=0
case "${1:-}" in
    "") ;;
    --dry-run) DRY_RUN=1 ;;
    *) fail "usage: scripts/publish-release.sh [--dry-run]" ;;
esac

# ── Preconditions ────────────────────────────────────────────────────────────
command -v gh >/dev/null || fail "gh CLI not found"
gh auth status >/dev/null 2>&1 || fail "gh is not authenticated (gh auth login)"
[ -f "$TEMPLATE" ] || fail "cask template missing: $TEMPLATE"

VERSION="$("$CARGO" pkgid -p ddc-cli | sed 's/.*[#@]//')"
TAG="v$VERSION"
ASSET="deepdrivecleaner-$VERSION-$TARGET.tar.gz"
[ -f "$ROOT/docs/releases/$VERSION.md" ] || fail "docs/releases/$VERSION.md missing: the release notes start there"

# The tag is created at the built commit, so it must be public: HEAD on origin/main.
[ -z "$(git status --porcelain)" ] || fail "the tree is not clean (untracked files count)"
[ "$(git branch --show-current)" = "main" ] || fail "not on main"
git fetch -q origin main
HEAD_SHA="$(git rev-parse HEAD)"
[ "$HEAD_SHA" = "$(git rev-parse origin/main)" ] || fail "HEAD is not origin/main — push first"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# The commit tag $TAG points at on origin, empty when there is no such tag. Origin is
# read directly: a local tag of the same name may be stale. An annotated tag is
# listed twice, the peeled commit second.
remote_tag_commit() {
    local refs
    refs="$(git ls-remote origin "refs/tags/$TAG" "refs/tags/$TAG^{}")" || fail "git ls-remote origin failed"
    printf '%s\n' "$refs" | awk -v peeled="refs/tags/$TAG^{}" \
        '$2 == peeled { p = $1 } $2 != peeled && $1 != "" { t = $1 } END { print (p != "" ? p : t) }'
}

# Republishing a version silently would leave users on a cask whose sha256 no
# longer matches what they download, so an existing release is never replaced. It is
# a half-finished publish (the asset went up, the cask did not): resume at the cask
# with the asset the release already holds. That asset is downloaded rather than
# taken from dist/, which a `make dist` after the release would have rebuilt, and a
# rebuild never reproduces the published digest (signature timestamp, tar mtimes).
# A failed read is not "no release": only gh's "release not found" is.
if IS_DRAFT="$(gh release view "$TAG" --repo "$REPO" --json isDraft --jq .isDraft 2>&1)"; then
    RELEASE_EXISTS=1
elif [ "$IS_DRAFT" = "release not found" ]; then
    RELEASE_EXISTS=0
else
    fail "could not read release $TAG: $IS_DRAFT"
fi
if [ "$RELEASE_EXISTS" = 1 ]; then
    ASSET_NAME="$(gh release view "$TAG" --repo "$REPO" \
        --json assets --jq ".assets[] | select(.name==\"$ASSET\") | .name")" || fail "could not read release $TAG"
    if [ -z "$ASSET_NAME" ] && [ "$IS_DRAFT" = true ]; then
        # gh deletes its own draft when an upload fails; this one was left by hand or by a failed cleanup.
        fail "release $TAG is a draft without $ASSET: inspect it with 'gh release view $TAG --repo $REPO', remove it with 'gh release delete $TAG --repo $REPO --cleanup-tag --yes', then rerun"
    fi
    [ -n "$ASSET_NAME" ] \
        || fail "release $TAG is published but holds no $ASSET: inspect it with 'gh release view $TAG --repo $REPO' — a cask may already point at it, so do not delete it"
    REMOTE_DIGEST="$(gh release view "$TAG" --repo "$REPO" \
        --json assets --jq ".assets[] | select(.name==\"$ASSET\") | .digest // empty")" || fail "could not read release $TAG"
    [ -n "$REMOTE_DIGEST" ] || fail "GitHub reports no digest for $ASSET in release $TAG yet — rerun later"
    step "Release $TAG exists — resuming at the cask with its asset"
    gh release download "$TAG" --repo "$REPO" --pattern "$ASSET" --dir "$WORK" \
        || fail "could not download $ASSET from release $TAG"
    TARBALL="$WORK/$ASSET"
    BUILD_SHA="$(remote_tag_commit)"
    [ -n "$BUILD_SHA" ] || fail "release $TAG exists but origin has no tag $TAG"
    # Resuming may run from a later commit, but only one that changes nothing in the
    # binary: a cask fix is a commit.
    git merge-base --is-ancestor "$BUILD_SHA" HEAD || fail "HEAD does not descend from $TAG ($BUILD_SHA)"
    CHANGED="$(git diff --no-renames --name-only "$BUILD_SHA" HEAD \
        | grep -v -E '^(packaging/homebrew/|scripts/publish-release\.sh$)' || true)"
    [ -z "$CHANGED" ] || fail "since $TAG, files outside the cask changed: $CHANGED — bump the version"
else
    # A new release is cut from HEAD, with the tarball `make dist` built from it.
    TARBALL="$ROOT/dist/$ASSET"
    [ -f "$TARBALL" ] || fail "$ASSET not found — run 'make dist' first"
    BUILD_SHA="$(cat "$ROOT/dist/BUILD_COMMIT" 2>/dev/null || true)"
    [ -n "$BUILD_SHA" ] || fail "dist/BUILD_COMMIT missing — run 'make dist' again"
    [ "$BUILD_SHA" = "$HEAD_SHA" ] || fail "dist/ was built from another commit — run 'make dist' again"
    # `gh release create` would attach the release to an existing tag wherever it points.
    STRAY_TAG="$(remote_tag_commit)"
    [ -z "$STRAY_TAG" ] || fail "tag $TAG already exists on origin without a release — delete it or bump the version"
fi

# Integrity before anything from the tarball runs: digest, then signature, then the binary.
SHA256="$(shasum -a 256 "$TARBALL" | awk '{print $1}')"
step "sha256 $SHA256"
if [ "$RELEASE_EXISTS" = 1 ]; then
    [ "$REMOTE_DIGEST" = "sha256:$SHA256" ] || fail "the downloaded $ASSET does not match the release's digest $REMOTE_DIGEST"
fi
step "Checking what the tarball holds"
mkdir "$WORK/unpacked"
tar -xzf "$TARBALL" -C "$WORK/unpacked"
BIN="$WORK/unpacked/deepdrivecleaner"
codesign --verify --strict "$BIN" || fail "the tarball's binary fails codesign --verify"
codesign -dv "$BIN" 2>&1 | grep "^TeamIdentifier=$TEAM_ID$" >/dev/null \
    || fail "the tarball's binary is not signed by team $TEAM_ID"
[ "$("$BIN" --version)" = "deepdrivecleaner $VERSION" ] || fail "the tarball's binary is not $VERSION"

# ── Release notes and cask ────────────────────────────────────────────────
NOTES="$WORK/release-notes.md"
{
    cat "$ROOT/docs/releases/$VERSION.md"
    echo
    echo "## Install (Apple Silicon)"
    echo
    echo '```'
    echo "brew install --cask smartandpoint/tap/deepdrivecleaner"
    echo '```'
    echo
    echo "## Build"
    echo
    echo "- Commit: \`$BUILD_SHA\`"
    echo "- Toolchain: \`$("$HOME/.cargo/bin/rustc" --version)\`"
    echo "- Command: \`RUSTFLAGS=\"--remap-path-prefix=\$HOME=~\" cargo build --release --locked --target $TARGET -p ddc-cli\`"
    echo "- Signed with the SmartAndPoint Developer ID (team \`$TEAM_ID\`) and notarized by Apple"
    echo "- SHA-256 of \`$ASSET\`: \`$SHA256\`"
} > "$NOTES"

mkdir -p "$WORK/Casks"   # under Casks/, `brew style` applies the cask cops
CASK="$WORK/Casks/deepdrivecleaner.rb"
sed -e "s/__VERSION__/$VERSION/g" -e "s/__SHA256__/$SHA256/g" "$TEMPLATE" > "$CASK"
if grep -qE '__[A-Z0-9]+__' "$CASK"; then fail "template placeholders left unsubstituted"; fi
ruby -c "$CASK" >/dev/null || fail "the rendered cask is not valid Ruby"
HOMEBREW_NO_AUTO_UPDATE=1 brew style "$CASK" || fail "brew style offenses in the rendered cask"

# Read before the dry-run exit, so the dry run shows what the tap write will do.
# On a 404 gh prints the error JSON to stdout: keep only a real blob sha.
EXISTING_SHA="$(gh api "repos/$TAP_REPO/contents/$CASK_PATH" --jq .sha 2>/dev/null)" || EXISTING_SHA=""
[[ "$EXISTING_SHA" =~ ^[0-9a-f]{40}$ ]] || EXISTING_SHA=""
if [ -z "$EXISTING_SHA" ]; then
    TAP_STATE="create"
elif [ "$EXISTING_SHA" = "$(git hash-object "$CASK")" ]; then
    TAP_STATE="current"
else
    TAP_STATE="update"
fi

printf '\033[0;32mPublishing %s\033[0m  (%s)\n' "$VERSION" "$ASSET"
if [ "$DRY_RUN" = 1 ]; then
    echo "── release ──"
    if [ "$RELEASE_EXISTS" = 1 ]; then echo "exists: resume at the cask"; else echo "would create $TAG at $BUILD_SHA"; fi
    echo "── tap $TAP_REPO ──"
    case "$TAP_STATE" in
        create)  echo "would create $CASK_PATH" ;;
        update)  echo "would update $CASK_PATH (blob $EXISTING_SHA)" ;;
        current) echo "$CASK_PATH already holds this cask" ;;
    esac
    echo "── release notes ──"; cat "$NOTES"
    echo "── rendered cask ──"; cat "$CASK"
    echo "   dry run — nothing was published"
    exit 0
fi

# ── 1. Release ───────────────────────────────────────────────────────────────
if [ "$RELEASE_EXISTS" = 0 ]; then
    step "Creating $TAG at $BUILD_SHA"
    gh release create "$TAG" "$TARBALL" --repo "$REPO" --target "$BUILD_SHA" \
        --title "DeepDriveCleaner $VERSION" --notes-file "$NOTES"
    REMOTE_DIGEST="$(gh release view "$TAG" --repo "$REPO" \
        --json assets --jq ".assets[] | select(.name==\"$ASSET\") | .digest")"
    [ "$REMOTE_DIGEST" = "sha256:$SHA256" ] \
        || fail "the uploaded asset's digest is '${REMOTE_DIGEST:-empty}' — the release exists; check it before rerunning"
    REMOTE_TAG_SHA="$(remote_tag_commit)"
    [ "$REMOTE_TAG_SHA" = "$BUILD_SHA" ] \
        || fail "tag $TAG on origin points at '${REMOTE_TAG_SHA:-nothing}', not at the built commit $BUILD_SHA — the release exists; check it before rerunning"
fi

# ── 2. Cask ───────────────────────────────────────────────────────────────
# Written through the GitHub contents API rather than clone-commit-push: a `git push`
# picks up whatever credential helper the machine has configured, and the local tap
# clone belongs to `brew update`. The API call uses gh's own token and carries the
# author and committer explicitly.
if [ "$TAP_STATE" = current ]; then
    printf '\033[0;32m✅ %s already holds this cask — nothing to publish\033[0m\n' "$TAP_REPO"
    exit 0
fi
step "Updating $TAP_REPO"
CONTENT_B64="$(base64 < "$CASK" | tr -d '\n')"
API_ARGS=(-X PUT "repos/$TAP_REPO/contents/$CASK_PATH"
    -f "message=deepdrivecleaner $VERSION"
    -f "content=$CONTENT_B64"
    -f "committer[name]=$GIT_NAME" -f "committer[email]=$GIT_EMAIL"
    -f "author[name]=$GIT_NAME" -f "author[email]=$GIT_EMAIL")
if [ -n "$EXISTING_SHA" ]; then
    API_ARGS+=(-f "sha=$EXISTING_SHA")   # update in place; absent on the first publish
fi
gh api "${API_ARGS[@]}" >/dev/null

printf '\033[0;32m✅ Published %s\033[0m\n' "$VERSION"
echo "   brew update && brew install --cask smartandpoint/tap/deepdrivecleaner"
