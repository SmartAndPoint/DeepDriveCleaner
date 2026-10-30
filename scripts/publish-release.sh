#!/bin/bash
# Publishes the release of tag v<version>: the two assets `make dist` built, the other
# targets' archives release.yml built on the tag, SHA256SUMS over them, and the signed
# appcast; then points the Homebrew cask at the app.
#
# Two repositories, in this order and no other (the same contract as GrammarHelper's
# publish script):
#   1. SmartAndPoint/DeepDriveCleaner — release v<version> with the app zip (the
#      cask's and Sparkle's), the macOS CLI tarball and the CI archives (for a manual
#      install), SHA256SUMS, and appcast.xml (Sparkle's feed), on the tag `make tag`
#      pushed at the built commit
#   2. SmartAndPoint/homebrew-tap     — Casks/deepdrivecleaner.rb
# A cask that names an asset which does not exist yet breaks `brew install` for
# everyone who already ran `brew update`; a release nobody points at yet breaks
# nothing. So the assets go first. The release becomes "latest" with its feed in
# place, so releases/latest/download/appcast.xml never returns 404; after the tap is
# written, the live feed is read back (ADR "Updates through Sparkle, alongside the
# Homebrew cask", §6).
#
# A pre-release (a version with a suffix, such as 0.8.0-rc.1) is published as one and
# stops there: the cask never points at it, and it carries no appcast, so Sparkle never
# offers it.
#
# The version is read from Cargo.toml and never passed in: a hand-typed version is
# how the cask and the binary drift apart.
#
# The appcast is signed with Sparkle's sign_update, which reads the owner's EdDSA key
# from the login keychain: run this unsandboxed, like `make dist`. --team-change
# publishes a release signed by another team as information only: a link, never an
# install.
set -euo pipefail

REPO="SmartAndPoint/DeepDriveCleaner"
TAP_REPO="SmartAndPoint/homebrew-tap"
CASK_PATH="Casks/deepdrivecleaner.rb"
TARGET=aarch64-apple-darwin
TEAM_ID=8MSDJTBW2D
APP_ID=com.smartandpoint.deepdrivecleaner
CLI_ID=com.smartandpoint.deepdrivecleaner.cli
MIN_MACOS=26.0   # the cask's `depends_on macos: :tahoe`
GIT_NAME="Evgenii Konev"
GIT_EMAIL="ekonev@smartandpoint.com"
FEED_URL="https://github.com/$REPO/releases/latest/download/appcast.xml"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"
CARGO="$HOME/.cargo/bin/cargo"
TEMPLATE="$ROOT/packaging/homebrew/deepdrivecleaner.rb.tmpl"
RELEASE_WORKFLOW="$ROOT/.github/workflows/release.yml"

fail() { printf '\033[0;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }
step() { printf '\033[0;34m→ %s\033[0m\n' "$1"; }

DRY_RUN=0
TEAM_CHANGE=()
for ARG in "$@"; do
    case "$ARG" in
        --dry-run) DRY_RUN=1 ;;
        --team-change) TEAM_CHANGE=(--team-change) ;;
        *) fail "usage: scripts/publish-release.sh [--dry-run] [--team-change]" ;;
    esac
done

# ── Preconditions ────────────────────────────────────────────────────────────
command -v gh >/dev/null || fail "gh CLI not found"
command -v jq >/dev/null || fail "jq not found: it reads release.yml's targets"
gh auth status >/dev/null 2>&1 || fail "gh is not authenticated (gh auth login)"
[ -f "$TEMPLATE" ] || fail "cask template missing: $TEMPLATE"
# Sparkle's tools from the package the build resolved (scripts/build-app.sh's DerivedData).
SPARKLE_BIN="$ROOT/build/DerivedData/SourcePackages/artifacts/sparkle/Sparkle/bin"
for TOOL in sign_update generate_keys; do
    [ -x "$SPARKLE_BIN/$TOOL" ] || fail "Sparkle's $TOOL is missing from $SPARKLE_BIN: resolve the package with 'xcodebuild -resolvePackageDependencies -project app/DeepDriveCleaner.xcodeproj -scheme DeepDriveCleaner -derivedDataPath build/DerivedData', or run 'make dist'"
done
command -v xmllint >/dev/null || fail "xmllint not found"

VERSION="$("$CARGO" pkgid -p ddc-cli | sed 's/.*[#@]//')"
TAG="v$VERSION"
TARBALL_NAME="deepdrivecleaner-$VERSION-$TARGET.tar.gz"
ZIP_NAME="DeepDriveCleaner-$VERSION.zip"
PRERELEASE=0
case "$VERSION" in *-*) PRERELEASE=1 ;; esac
[ -f "$ROOT/docs/releases/$VERSION.md" ] || fail "docs/releases/$VERSION.md missing: the release notes start there"

# The archives release.yml builds: one per entry of its RELEASE_TARGETS line. A line
# that cannot be read is an error, never an empty list: that would publish a release
# without the targets it promises.
[ "$(grep -c "^  RELEASE_TARGETS: '" "$RELEASE_WORKFLOW" || true)" = 1 ] \
    || fail "release.yml holds no single RELEASE_TARGETS line"
TARGETS_JSON="$(sed -n "s/^  RELEASE_TARGETS: '\(.*\)'\$/\1/p" "$RELEASE_WORKFLOW")"
jq -e 'type == "array" and all(.[]; (.target | type) == "string" and (.runner | type) == "string" and (.archive == "tar.gz" or .archive == "zip"))' \
    <<<"$TARGETS_JSON" >/dev/null 2>&1 || fail "release.yml's RELEASE_TARGETS is not a list of {target, runner, archive}: $TARGETS_JSON"
CI_NAMES=()
while read -r CI_TARGET CI_ARCHIVE; do
    [ -n "$CI_TARGET" ] || continue
    CI_NAMES+=("deepdrivecleaner-$VERSION-$CI_TARGET.$CI_ARCHIVE")
done <<<"$(jq -r '.[] | "\(.target) \(.archive)"' <<<"$TARGETS_JSON")"

# What SHA256SUMS covers, and what the release holds: those plus SHA256SUMS. A later
# asset that SHA256SUMS does not cover (an appcast) joins UPLOADS only.
SUMMED=("$ZIP_NAME" "$TARBALL_NAME" ${CI_NAMES[@]+"${CI_NAMES[@]}"})
UPLOADS=("${SUMMED[@]}" SHA256SUMS)

# The tag is at the built commit, so it must be public: HEAD on origin/main.
[ -z "$(git status --porcelain)" ] || fail "the tree is not clean (untracked files count)"
[ "$(git branch --show-current)" = "main" ] || fail "not on main"
git fetch -q origin main
HEAD_SHA="$(git rev-parse HEAD)"
[ "$HEAD_SHA" = "$(git rev-parse origin/main)" ] || fail "HEAD is not origin/main — push first"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
ASSETS="$WORK/assets"
mkdir "$ASSETS"

# The commit tag $TAG points at on origin, empty when there is no such tag. Origin is
# read directly: a local tag of the same name may be stale. An annotated tag is
# listed twice, the peeled commit second.
remote_tag_commit() {
    local refs
    refs="$(git ls-remote origin "refs/tags/$TAG" "refs/tags/$TAG^{}")" || fail "git ls-remote origin failed"
    printf '%s\n' "$refs" | awk -v peeled="refs/tags/$TAG^{}" \
        '$2 == peeled { p = $1 } $2 != peeled && $1 != "" { t = $1 } END { print (p != "" ? p : t) }'
}

# The digest GitHub reports for asset $1 of release $TAG, empty when it has none.
remote_digest() {
    gh release view "$TAG" --repo "$REPO" --json assets --jq ".assets[] | select(.name==\"$1\") | .digest // empty"
}

# The sha256 of asset $1 in $ASSETS.
sha_of() {
    shasum -a 256 "$ASSETS/$1" | awk '{print $1}'
}

# Republishing a version silently would leave users on a cask whose sha256 no
# longer matches what they download, so an existing release is never replaced. It is
# a half-finished publish (the assets went up, the cask did not): resume at the cask
# with the assets the release already holds. They are downloaded rather than
# taken from dist/, which a `make dist` after the release would have rebuilt, and a
# rebuild never reproduces the published digest (signature timestamp, tar mtimes).
# A failed read is not "no release": only gh's "release not found" is. Resume needs
# SHA256SUMS, so it serves releases published since the build matrix (after 0.7.0).
APPCAST_PRESENT=0
if IS_DRAFT="$(gh release view "$TAG" --repo "$REPO" --json isDraft --jq .isDraft 2>&1)"; then
    RELEASE_EXISTS=1
elif [ "$IS_DRAFT" = "release not found" ]; then
    RELEASE_EXISTS=0
else
    fail "could not read release $TAG: $IS_DRAFT"
fi
if [ "$RELEASE_EXISTS" = 1 ]; then
    # A draft is never resumed, even a complete one: gh leaves one when it is stopped
    # after the uploads and before it publishes, and a cask pointing at a draft's
    # assets gives everyone a 404.
    [ "$IS_DRAFT" = true ] \
        && fail "release $TAG is a draft: inspect it with 'gh release view $TAG --repo $REPO', remove it with 'gh release delete $TAG --repo $REPO --yes' (keep the tag), then rerun"
    NAMES="$(gh release view "$TAG" --repo "$REPO" --json assets --jq '.assets[].name')" \
        || fail "could not read release $TAG"
    # A release cut before the appcast was an asset gets it on resume.
    grep -qxF appcast.xml <<<"$NAMES" && APPCAST_PRESENT=1
    PATTERNS=()
    for NAME in "${UPLOADS[@]}"; do
        PATTERNS+=(--pattern "$NAME")
        grep -qxF "$NAME" <<<"$NAMES" && continue
        fail "release $TAG is published but holds no $NAME: inspect it with 'gh release view $TAG --repo $REPO' — a cask may already point at it, so do not delete it"
    done
    step "Release $TAG exists — resuming at the cask with its assets"
    gh release download "$TAG" --repo "$REPO" "${PATTERNS[@]}" --dir "$ASSETS" \
        || fail "could not download the assets of release $TAG"
    BUILD_SHA="$(remote_tag_commit)"
    [ -n "$BUILD_SHA" ] || fail "release $TAG exists but origin has no tag $TAG"
    # Resuming may run from a later commit, but only one that changes nothing in the
    # assets: a cask fix is a commit.
    git merge-base --is-ancestor "$BUILD_SHA" HEAD || fail "HEAD does not descend from $TAG ($BUILD_SHA)"
    CHANGED="$(git diff --no-renames --name-only "$BUILD_SHA" HEAD \
        | grep -v -E '^(packaging/homebrew/|packaging/sparkle/|scripts/publish-release\.sh$|scripts/render-appcast\.sh$|scripts/tests/)' || true)"
    [ -z "$CHANGED" ] || fail "since $TAG, files outside the cask changed: $CHANGED — bump the version"
else
    # A new release is cut from HEAD, on the tag `make tag` pushed there, with the
    # assets `make dist` built from it (it builds the zip only beside a tarball of the
    # same commit) and the archives release.yml built on the tag.
    for FILE in "$ROOT/dist/$ZIP_NAME" "$ROOT/dist/$TARBALL_NAME"; do
        [ -f "$FILE" ] || fail "$(basename "$FILE") not found — run 'make dist' first"
    done
    BUILD_SHA="$(cat "$ROOT/dist/BUILD_COMMIT" 2>/dev/null || true)"
    [ -n "$BUILD_SHA" ] || fail "dist/BUILD_COMMIT missing — run 'make dist' again"
    [ "$BUILD_SHA" = "$HEAD_SHA" ] || fail "dist/ was built from another commit — run 'make dist' again"
    # `gh release create` attaches the release to the existing tag wherever it points.
    TAG_SHA="$(remote_tag_commit)"
    [ -n "$TAG_SHA" ] || fail "origin has no tag $TAG: run 'make tag', wait for release.yml, then 'make dist' again"
    [ "$TAG_SHA" = "$HEAD_SHA" ] \
        || fail "tag $TAG on origin points at $TAG_SHA, not HEAD: it has no release yet, so delete it on origin ('git push origin :refs/tags/$TAG') and run 'make tag', or bump the version"
    cp "$ROOT/dist/$ZIP_NAME" "$ROOT/dist/$TARBALL_NAME" "$ASSETS/"
    if [ "${#CI_NAMES[@]}" -gt 0 ]; then
        # Every archive comes from the green release.yml run on this tag; a red or
        # missing one is fixed on main and tagged again (the tag has no release yet).
        "$ROOT/scripts/ci-green.sh" release.yml
        RUN_ID="$(gh run list --repo "$REPO" --commit "$HEAD_SHA" --workflow release.yml --event push --limit 1 \
            --json databaseId,conclusion --jq '.[0] // empty | select(.conclusion == "success") | .databaseId')" \
            || fail "gh could not list the release.yml runs for HEAD"
        [ -n "$RUN_ID" ] || fail "no green release.yml run for $TAG"
        for NAME in "${CI_NAMES[@]}"; do
            step "Downloading $NAME from release.yml run $RUN_ID"
            gh run download "$RUN_ID" --repo "$REPO" --name "$NAME" --dir "$WORK/ci/$NAME" \
                || fail "release.yml run $RUN_ID has no artifact $NAME: an enabled target was not built"
            FOUND="$(cd "$WORK/ci/$NAME" && find . -type f | sed 's|^\./||')"
            [ "$FOUND" = "$NAME" ] || fail "artifact $NAME should hold $NAME alone, not: $FOUND"
            mv "$WORK/ci/$NAME/$NAME" "$ASSETS/"
        done
    fi
    # One line per archive, `<sha256>  <name>` as shasum and sha256sum write and
    # read it, ordered by name.
    (cd "$ASSETS" && shasum -a 256 "${SUMMED[@]}") | LC_ALL=C sort -k 2 > "$WORK/SHA256SUMS"
    mv "$WORK/SHA256SUMS" "$ASSETS/SHA256SUMS"
fi
ZIP="$ASSETS/$ZIP_NAME"
TARBALL="$ASSETS/$TARBALL_NAME"

# Integrity before anything from the assets runs: digest, then contents, then
# signatures, then the binaries. Nothing here opens the app.
for NAME in "${SUMMED[@]}"; do
    step "sha256 $(sha_of "$NAME")  $NAME"
done
ZIP_SHA="$(sha_of "$ZIP_NAME")"   # the cask's
if [ "$RELEASE_EXISTS" = 1 ]; then
    for NAME in "${UPLOADS[@]}"; do
        DIGEST="$(remote_digest "$NAME")" || fail "could not read release $TAG"
        [ -n "$DIGEST" ] || fail "GitHub reports no digest for $NAME of release $TAG yet — rerun later"
        [ "$DIGEST" = "sha256:$(sha_of "$NAME")" ] || fail "the downloaded $NAME does not match the release's digest $DIGEST"
    done
fi

step "Checking SHA256SUMS"
SUMS="$ASSETS/SHA256SUMS"
if grep -qvE '^[0-9a-f]{64}  [^ /]+$' "$SUMS"; then fail "SHA256SUMS holds a line that is not '<sha256>  <name>'"; fi
[ -z "$(tail -c 1 "$SUMS")" ] || fail "SHA256SUMS does not end with a newline"
LISTED="$(awk '{print $2}' "$SUMS" | tr '\n' ' ')"
EXPECTED="$(printf '%s\n' "${SUMMED[@]}" | LC_ALL=C sort | tr '\n' ' ')"
[ "$LISTED" = "$EXPECTED" ] || fail "SHA256SUMS lists $LISTED, not $EXPECTED in that order"
(cd "$ASSETS" && shasum -a 256 -c --quiet SHA256SUMS) || fail "an asset does not match SHA256SUMS"

# A signed command-line tool $1 ($2 in messages) of this version, signed as $CLI_ID.
check_cli() {
    local bin="$1" what="$2" info
    codesign --verify --strict "$bin" || fail "$what fails codesign --verify"
    info="$(codesign -dv "$bin" 2>&1)" || fail "codesign cannot read the signature of $what"
    grep -qxF "Identifier=$CLI_ID" <<<"$info" || fail "$what is not signed as $CLI_ID"
    grep -qxF "TeamIdentifier=$TEAM_ID" <<<"$info" || fail "$what is not signed by team $TEAM_ID"
    [ "$("$bin" --version)" = "deepdrivecleaner $VERSION" ] || fail "$what is not $VERSION"
}

step "Checking what the tarball holds"
LISTING="$(tar -tzf "$TARBALL" | LC_ALL=C sort | tr '\n' ' ')" || fail "cannot list $TARBALL_NAME"
[ "$LISTING" = "DISCLAIMER.md LICENSE.md README.md deepdrivecleaner " ] || fail "unexpected tarball contents: $LISTING"
mkdir "$WORK/unpacked"
tar -xzf "$TARBALL" -C "$WORK/unpacked"
check_cli "$WORK/unpacked/deepdrivecleaner" "the tarball's binary"

# The CI archives cannot run here (another OS): their contents, and that release.yml
# built them from this commit (the provenance attestation it signed).
for NAME in ${CI_NAMES[@]+"${CI_NAMES[@]}"}; do
    step "Checking $NAME"
    case "$NAME" in
        *.tar.gz) BIN_NAME=deepdrivecleaner; LISTING="$(tar -tzf "$ASSETS/$NAME" | LC_ALL=C sort | tr '\n' ' ')" ;;
        *.zip) BIN_NAME=deepdrivecleaner.exe; LISTING="$(unzip -Z1 "$ASSETS/$NAME" | LC_ALL=C sort | tr '\n' ' ')" ;;
        *) fail "$NAME is neither a tarball nor a zip" ;;
    esac
    [ "$LISTING" = "DISCLAIMER.md LICENSE.md README.md $BIN_NAME " ] || fail "unexpected $NAME contents: $LISTING"
    gh attestation verify "$ASSETS/$NAME" --repo "$REPO" \
        --signer-workflow "$REPO/.github/workflows/release.yml" --source-digest "$BUILD_SHA" >/dev/null \
        || fail "$NAME has no provenance attestation from release.yml at $BUILD_SHA"
done

step "Checking what the zip holds"
TOP="$(unzip -Z1 "$ZIP" | sed 's|/.*||' | LC_ALL=C sort -u | tr '\n' ' ')" || fail "cannot list $ZIP_NAME"
[ "$TOP" = "DeepDriveCleaner.app " ] || fail "the zip should hold DeepDriveCleaner.app alone, not: $TOP"
mkdir "$WORK/app"
ditto -x -k "$ZIP" "$WORK/app"
APP="$WORK/app/DeepDriveCleaner.app"
codesign --verify --deep --strict "$APP" || fail "the app fails codesign --verify --deep"
APP_INFO="$(codesign -dv "$APP" 2>&1)" || fail "codesign cannot read the app's signature"
grep -qxF "Identifier=$APP_ID" <<<"$APP_INFO" || fail "the app is not signed as $APP_ID"
grep -qxF "TeamIdentifier=$TEAM_ID" <<<"$APP_INFO" || fail "the app is not signed by team $TEAM_ID"
# The appcast names the team the zip's app is signed by, read from it, not assumed.
APP_TEAM="$(sed -n 's/^TeamIdentifier=//p' <<<"$APP_INFO")"
xcrun stapler validate -q "$APP" || fail "the app has no valid stapled notarization ticket"
plist() { plutil -extract "$1" raw -o - "$APP/Contents/Info.plist"; }
[ "$(plist CFBundleShortVersionString)" = "$VERSION" ] || fail "the app is $(plist CFBundleShortVersionString), not $VERSION"
[ "$(plist CFBundleIdentifier)" = "$APP_ID" ] || fail "the app's bundle id is $(plist CFBundleIdentifier), not $APP_ID"
[ "$(plist LSMinimumSystemVersion)" = "$MIN_MACOS" ] \
    || fail "the app needs macOS $(plist LSMinimumSystemVersion), the cask says $MIN_MACOS"
# Sparkle checks every update against this key: it must be the one sign_update signs with.
ED_KEY="$(plist SUPublicEDKey 2>/dev/null || true)"
[[ "$ED_KEY" =~ ^[A-Za-z0-9+/]{43}=$ ]] || fail "the app's SUPublicEDKey is not an EdDSA public key: '$ED_KEY'"
[ "$(plist SUFeedURL)" = "$FEED_URL" ] || fail "the app's SUFeedURL is $(plist SUFeedURL), not $FEED_URL"
for DOC in LICENSE.md DISCLAIMER.md; do
    cmp -s "$APP/Contents/Resources/$DOC" "$ROOT/$DOC" || fail "the app's $DOC is missing or differs from the repository's"
done
check_cli "$APP/Contents/Helpers/deepdrivecleaner" "the app's command-line tool"

# ── Appcast ──────────────────────────────────────────────────────────────────
# Rendered before the release exists, so the release carries its feed from the start.
# A resumed release that already holds one keeps it: its copy is what Sparkle reads.
# A pre-release has none: Sparkle never offers it, and its version is not x.y.z.
APPCAST="$ASSETS/appcast.xml"   # the asset's name is the file's
xpath_item() { xmllint --xpath "string(//*[local-name()='item']/$1)" "$2"; }
if [ "$PRERELEASE" = 1 ]; then
    step "A pre-release: no appcast"
elif [ "$RELEASE_EXISTS" = 1 ] && [ "$APPCAST_PRESENT" = 1 ]; then
    step "Release $TAG holds its appcast — reading it"
    gh release download "$TAG" --repo "$REPO" --pattern appcast.xml --dir "$ASSETS" \
        || fail "could not download the appcast of release $TAG"
    xmllint --noout "$APPCAST" || fail "the appcast of release $TAG is not well-formed XML"
else
    step "Rendering the appcast"
    PUBLIC_KEY="$("$SPARKLE_BIN/generate_keys" -p)" || fail "generate_keys -p failed: is the EdDSA key in the login keychain?"
    [ "$PUBLIC_KEY" = "$ED_KEY" ] \
        || fail "the keychain's EdDSA public key ($PUBLIC_KEY) is not the app's SUPublicEDKey ($ED_KEY): Sparkle would reject the update"
    # The previous release: the newest published one other than this. 0.7.0 and
    # earlier hold no appcast, and there are no guards to apply against them.
    PREVIOUS_ARGS=()
    PREVIOUS_TAG="$(gh release list --repo "$REPO" --exclude-drafts --exclude-pre-releases --limit 20 \
        --json tagName --jq '.[].tagName' | { grep -vxF "$TAG" || true; } | head -n 1)" \
        || fail "could not list the releases of $REPO"
    if [ -n "$PREVIOUS_TAG" ]; then
        PREVIOUS_NAMES="$(gh release view "$PREVIOUS_TAG" --repo "$REPO" --json assets --jq '.assets[].name')" \
            || fail "could not read release $PREVIOUS_TAG"
        if grep -qxF appcast.xml <<<"$PREVIOUS_NAMES"; then
            mkdir "$WORK/previous"
            gh release download "$PREVIOUS_TAG" --repo "$REPO" --pattern appcast.xml --dir "$WORK/previous" \
                || fail "could not download the appcast of release $PREVIOUS_TAG"
            PREVIOUS_ARGS=(--previous "$WORK/previous/appcast.xml")
            step "Previous appcast: $PREVIOUS_TAG"
        else
            step "$PREVIOUS_TAG has no appcast: no version or team guard to apply"
        fi
    fi
    SPARKLE_SIGN_UPDATE="$SPARKLE_BIN/sign_update" "$ROOT/scripts/render-appcast.sh" \
        --zip "$ZIP" --version "$(plist CFBundleVersion)" --short-version "$VERSION" --min-macos "$MIN_MACOS" \
        --url "https://github.com/$REPO/releases/download/$TAG/$ZIP_NAME" \
        --notes-url "https://github.com/$REPO/releases/tag/$TAG" \
        --team "$APP_TEAM" --out "$APPCAST" \
        ${PREVIOUS_ARGS[@]+"${PREVIOUS_ARGS[@]}"} ${TEAM_CHANGE[@]+"${TEAM_CHANGE[@]}"}
fi
if [ "$PRERELEASE" = 0 ]; then
    APPCAST_VERSION="$(xpath_item "*[local-name()='version']" "$APPCAST")"
    APPCAST_SIGNATURE="$(xpath_item "*[local-name()='enclosure']/@*[local-name()='edSignature']" "$APPCAST")"
    [ "$APPCAST_VERSION" = "$(plist CFBundleVersion)" ] \
        || fail "the appcast offers build $APPCAST_VERSION, the zip holds $(plist CFBundleVersion)"
    [ ${#TEAM_CHANGE[@]} -gt 0 ] || [ -n "$APPCAST_SIGNATURE" ] || fail "the appcast's enclosure has no edSignature"
fi
# What a new release holds: UPLOADS, and the appcast unless it is a pre-release.
RELEASED=("${UPLOADS[@]}")
[ "$PRERELEASE" = 1 ] || RELEASED+=(appcast.xml)

# A release missing its appcast (resumed, or already in the tap) gets it, and its digest is checked.
upload_appcast() {
    [ "$RELEASE_EXISTS" = 1 ] && [ "$APPCAST_PRESENT" = 0 ] || return 0
    step "Uploading the missing appcast.xml to $TAG"
    gh release upload "$TAG" "$APPCAST" --repo "$REPO" || fail "could not upload appcast.xml to release $TAG"
    local digest
    digest="$(remote_digest appcast.xml)" || digest=""
    [ "$digest" = "sha256:$(shasum -a 256 "$APPCAST" | awk '{print $1}')" ] \
        || fail "the uploaded appcast.xml's digest is '${digest:-empty}' — check release $TAG before rerunning"
}

# What Sparkle reads now: the live feed must be this appcast, signature and all.
# GitHub's "latest" redirect and its CDN can lag the release by a little: a
# mismatch is read again, 6 tries 10 s apart, before it fails.
check_live_feed() {
    step "Reading the live feed $FEED_URL"
    local try version="" signature=""
    for try in 1 2 3 4 5 6; do
        [ "$try" = 1 ] || { echo "   the live feed is not this release yet; reading it again in 10 s ($try/6)"; sleep 10; }
        curl -fsSL --max-time 60 --retry 3 -o "$WORK/live.xml" "$FEED_URL" || continue
        xmllint --noout "$WORK/live.xml" 2>/dev/null || continue
        version="$(xpath_item "*[local-name()='version']" "$WORK/live.xml")"
        signature="$(xpath_item "*[local-name()='enclosure']/@*[local-name()='edSignature']" "$WORK/live.xml")"
        if [ "$version" = "$APPCAST_VERSION" ] && [ "$signature" = "$APPCAST_SIGNATURE" ]; then
            printf '\033[0;32m✅ the live feed offers %s (build %s)\033[0m\n' "$VERSION" "$APPCAST_VERSION"
            return 0
        fi
    done
    fail "the live feed $FEED_URL offers build '${version:-unreadable}', not $APPCAST_VERSION, or another signature: is $TAG the latest release? Check 'gh release view $TAG --repo $REPO' and $FEED_URL"
}

# ── Release notes and cask ────────────────────────────────────────────────
NOTES="$WORK/release-notes.md"
{
    cat "$ROOT/docs/releases/$VERSION.md"
    echo
    if [ "$PRERELEASE" = 1 ]; then
        echo "## Pre-release"
        echo
        echo "A pre-release: the Homebrew cask and the app's update feed do not point at it. \`$ZIP_NAME\` is the app; \`$TARBALL_NAME\` is the command-line tool alone, for a manual install."
    else
        echo "## Install (Apple Silicon, macOS 26)"
        echo
        echo '```'
        echo "brew install --cask smartandpoint/tap/deepdrivecleaner"
        echo '```'
        echo
        echo "\`$ZIP_NAME\` is the app the cask installs; the app updates itself from \`appcast.xml\`. \`$TARBALL_NAME\` is the command-line tool alone, for a manual install."
    fi
    echo
    echo "## Build"
    echo
    echo "- Commit: \`$BUILD_SHA\`"
    echo "- Toolchain: \`$("$HOME/.cargo/bin/rustc" --version)\`"
    echo "- Command-line tool: \`RUSTFLAGS=\"--remap-path-prefix=\$HOME=~\" cargo build --release --locked --target $TARGET -p ddc-cli\`"
    if [ "${#CI_NAMES[@]}" -gt 0 ]; then
        echo "- The other command-line archives: built by \`.github/workflows/release.yml\` on the tag, each on its own platform's runner, with a provenance attestation: \`gh attestation verify <archive> --repo $REPO\`"
    fi
    echo "- App: \`scripts/build-app.sh --dist\` (Release, the command-line tool in \`Contents/Helpers\`)"
    echo "- Signed with the SmartAndPoint Developer ID (team \`$TEAM_ID\`): the app as \`$APP_ID\`, the command-line tool as \`$CLI_ID\`. Notarized by Apple; the app's ticket is stapled"
    for NAME in "${SUMMED[@]}"; do
        echo "- SHA-256 of \`$NAME\`: \`$(sha_of "$NAME")\`"
    done
    echo "- Verify a download with \`SHA256SUMS\`: \`shasum -a 256 -c --ignore-missing SHA256SUMS\` (macOS) or \`sha256sum -c --ignore-missing SHA256SUMS\` (Linux)"
} > "$NOTES"

if [ "$PRERELEASE" = 0 ]; then
    mkdir -p "$WORK/Casks"   # under Casks/, `brew style` applies the cask cops
    CASK="$WORK/Casks/deepdrivecleaner.rb"
    sed -e "s/__VERSION__/$VERSION/g" -e "s/__SHA256__/$ZIP_SHA/g" "$TEMPLATE" > "$CASK"
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
else
    TAP_STATE="prerelease"
fi

printf '\033[0;32mPublishing %s\033[0m  (%s)\n' "$VERSION" "${RELEASED[*]}"
if [ "$DRY_RUN" = 1 ]; then
    echo "── release ──"
    if [ "$RELEASE_EXISTS" = 1 ] && [ "$PRERELEASE" = 1 ]; then
        echo "exists: a pre-release, nothing to do"
    elif [ "$RELEASE_EXISTS" = 1 ]; then
        echo "exists: resume at the cask"
        [ "$APPCAST_PRESENT" = 1 ] || echo "would upload the missing appcast.xml"
    else
        [ "$PRERELEASE" = 0 ] || echo "a pre-release"
        echo "would create the release of tag $TAG (at $BUILD_SHA) with:"
        for NAME in "${RELEASED[@]}"; do echo "  $NAME"; done
    fi
    echo "── tap $TAP_REPO ──"
    case "$TAP_STATE" in
        create)  echo "would create $CASK_PATH" ;;
        update)  echo "would update $CASK_PATH (blob $EXISTING_SHA)" ;;
        current) echo "$CASK_PATH already holds this cask" ;;
        prerelease) echo "a pre-release: the cask is not touched" ;;
    esac
    echo "── SHA256SUMS ──"; cat "$SUMS"
    echo "── release notes ──"; cat "$NOTES"
    [ "$PRERELEASE" = 1 ] || { echo "── rendered cask ──"; cat "$CASK"; echo "── appcast.xml ──"; cat "$APPCAST"; }
    echo "   dry run — nothing was published"
    exit 0
fi

# ── 1. Release ───────────────────────────────────────────────────────────────
if [ "$RELEASE_EXISTS" = 0 ]; then
    step "Creating the release of $TAG at $BUILD_SHA"
    UPLOAD_PATHS=()
    for NAME in "${RELEASED[@]}"; do UPLOAD_PATHS+=("$ASSETS/$NAME"); done
    RELEASE_FLAGS=()
    [ "$PRERELEASE" = 0 ] || RELEASE_FLAGS+=(--prerelease)
    # --verify-tag: never a tag of gh's own, only the one `make tag` pushed.
    gh release create "$TAG" "${UPLOAD_PATHS[@]}" --repo "$REPO" --verify-tag \
        --title "DeepDriveCleaner $VERSION" --notes-file "$NOTES" ${RELEASE_FLAGS[@]+"${RELEASE_FLAGS[@]}"}
    for NAME in "${RELEASED[@]}"; do
        DIGEST="$(remote_digest "$NAME")" || DIGEST=""
        [ "$DIGEST" = "sha256:$(sha_of "$NAME")" ] \
            || fail "the uploaded $NAME's digest is '${DIGEST:-empty}' — the release exists; check it before rerunning"
    done
    REMOTE_TAG_SHA="$(remote_tag_commit)"
    [ "$REMOTE_TAG_SHA" = "$BUILD_SHA" ] \
        || fail "tag $TAG on origin points at '${REMOTE_TAG_SHA:-nothing}', not at the built commit $BUILD_SHA — the release exists; check it before rerunning"
fi

if [ "$PRERELEASE" = 1 ]; then
    if [ "$RELEASE_EXISTS" = 1 ]; then
        printf '\033[0;32m✅ Pre-release %s was already published — nothing to do\033[0m\n' "$VERSION"
    else
        printf '\033[0;32m✅ Published pre-release %s — the cask is not touched\033[0m\n' "$VERSION"
    fi
    exit 0
fi

# ── 2. Cask ───────────────────────────────────────────────────────────────
# Written through the GitHub contents API rather than clone-commit-push: a `git push`
# picks up whatever credential helper the machine has configured, and the local tap
# clone belongs to `brew update`. The API call uses gh's own token and carries the
# author and committer explicitly.
upload_appcast
if [ "$TAP_STATE" = current ]; then
    printf '\033[0;32m✅ %s already holds this cask\033[0m\n' "$TAP_REPO"
    check_live_feed
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
check_live_feed

printf '\033[0;32m✅ Published %s\033[0m\n' "$VERSION"
echo "   brew update && brew install --cask smartandpoint/tap/deepdrivecleaner"
