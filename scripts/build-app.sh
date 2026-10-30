#!/bin/bash
# Builds the menu bar app with the CLI embedded, and signs both with the Developer ID
# (ADR "Menu bar app: Swift shell over the ddc CLI", §1 and §8).
#
#   scripts/build-app.sh          Debug:   build/Debug/DeepDriveCleaner.app
#   scripts/build-app.sh --dist   Release: notarized and stapled, dist/DeepDriveCleaner-<version>.zip
#
# Sparkle.framework is signed here too, before the app (ADR "Updates through Sparkle,
# alongside the Homebrew cask", §8). A Debug build may take a Debug EdDSA public key
# from DDC_DEBUG_ED_PUBLIC_KEY, and allows http to 127.0.0.1 for scripts/debug-feed.sh;
# a release refuses both, the placeholder key and any feed but the production one.
#
# Debug and release are signed with the same Developer ID, so one Full Disk Access
# grant covers both (GrammarHelper's rule). Xcode builds unsigned and this script
# signs: Xcode's own signing would add get-task-allow, which notarization rejects.
# Never launch or grant a copy Xcode ran (Cmd-R): its ad-hoc signature replaces the
# requirement TCC stores, and the signed build then reads as not granted.
#
# `make dist` runs scripts/build-dist.sh first: it empties dist/ and writes
# dist/BUILD_COMMIT, and the zip is built only beside a tarball of the same commit.
# DDC_SIGN_IDENTITY and NOTARY_PROFILE come from .env, which is never committed.
set -euo pipefail

APP_ID=com.smartandpoint.deepdrivecleaner
CLI_ID=com.smartandpoint.deepdrivecleaner.cli
TEAM_ID=8MSDJTBW2D
TARGET=aarch64-apple-darwin
# Sparkle's own identifiers: its bundles carry them, its bare Autoupdate is given one.
SPARKLE_ID=org.sparkle-project.Sparkle
FEED_URL=https://github.com/SmartAndPoint/DeepDriveCleaner/releases/latest/download/appcast.xml
# What Info.plist holds until the owner pastes the public key of `generate_keys`.
ED_KEY_PLACEHOLDER=SPARKLE_PUBLIC_ED_KEY_PLACEHOLDER

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"                                # rustup reads rust-toolchain.toml from here
export PATH="$HOME/.cargo/bin:$PATH"     # Homebrew's Rust may shadow rustup
CARGO="$HOME/.cargo/bin/cargo"

fail() { printf '\033[0;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }
step() { printf '\033[0;34m→ %s\033[0m\n' "$1"; }

case "${1:-}" in
    "") DIST=0; CONFIG=Debug ;;
    --dist) DIST=1; CONFIG=Release ;;
    *) fail "usage: scripts/build-app.sh [--dist]" ;;
esac

if [ -f .env ]; then set -a; . ./.env; set +a; fi
[ -n "${DDC_SIGN_IDENTITY:-}" ] || fail "DDC_SIGN_IDENTITY is not set (.env): the app is signed with the Developer ID"
[[ "$DDC_SIGN_IDENTITY" =~ ^[0-9A-Fa-f]{40}$ ]] \
    || fail "DDC_SIGN_IDENTITY must be the certificate's SHA-1 (security find-identity -v -p codesigning)"

VERSION="$("$CARGO" pkgid -p ddc-cli | sed 's/.*[#@]//')"
APP_VERSION="$(sed -n 's/^[[:space:]]*MARKETING_VERSION = "\{0,1\}\([^";]*\)"\{0,1\};$/\1/p' app/DeepDriveCleaner.xcodeproj/project.pbxproj | sort -u)"
if [ "$DIST" = 1 ]; then
    [ -n "${NOTARY_PROFILE:-}" ] || fail "NOTARY_PROFILE is not set (.env)"
    # A zip built from uncommitted code could not be rebuilt from its tag.
    [ -z "$(git status --porcelain)" ] || fail "the tree is not clean (untracked files count): commit first"
    [ "$APP_VERSION" = "$VERSION" ] || fail "the app's MARKETING_VERSION ($APP_VERSION) is not the CLI's version ($VERSION)"
    # Both release assets come from one commit; scripts/publish-release.sh checks
    # BUILD_COMMIT against HEAD for both.
    [ "$(cat dist/BUILD_COMMIT 2>/dev/null || true)" = "$(git rev-parse HEAD)" ] \
        || fail "dist/BUILD_COMMIT is not HEAD: run scripts/build-dist.sh first (make dist runs both)"
    # Sparkle offers only a build whose CFBundleVersion grows.
    "$ROOT/scripts/check-build-number.sh"
fi

# Signs one piece of code and checks it carries the identifier and the team.
TIMESTAMP=--timestamp=none
[ "$DIST" = 1 ] && TIMESTAMP=--timestamp
sign_code() {
    local path="$1" identifier="$2"
    codesign --force "$TIMESTAMP" --options runtime --identifier "$identifier" --sign "$DDC_SIGN_IDENTITY" "$path"
    local info
    info="$(codesign -dv "$path" 2>&1)"
    grep -qxF "Identifier=$identifier" <<<"$info" || fail "$path is not signed as $identifier"
    grep -qxF "TeamIdentifier=$TEAM_ID" <<<"$info" || fail "$path is not signed by team $TEAM_ID"
}

# Signs one piece of Sparkle under Sparkle's identifier, never ours. A bundle keeps
# its own (no --identifier), so it must already be $2: a Sparkle that renamed one
# stops the build here. The bare Autoupdate's ad-hoc signature carries an identifier
# the linker made up, so it is given $2.
sign_sparkle() {
    local path="$1" identifier="$2" plist
    local args=(--force "$TIMESTAMP" --options runtime --sign "$DDC_SIGN_IDENTITY")
    if [ -d "$path" ]; then
        plist="$path/Contents/Info.plist"
        [ -f "$plist" ] || plist="$path/Resources/Info.plist"
        [ "$(plutil -extract CFBundleIdentifier raw -o - "$plist" 2>/dev/null)" = "$identifier" ] \
            || fail "$path is not the bundle $identifier: did the Sparkle version change?"
    else
        args+=(--identifier "$identifier")
    fi
    codesign "${args[@]}" "$path"
    local info
    info="$(codesign -dv "$path" 2>&1)"
    grep -qxF "Identifier=$identifier" <<<"$info" || fail "$path is not signed as $identifier"
    grep -qxF "TeamIdentifier=$TEAM_ID" <<<"$info" || fail "$path is not signed by team $TEAM_ID"
}

# Retries a command every 20 s (15 times by default): a notarization ticket can
# reach Apple's lookup service some time after notarytool reports Accepted.
retry() {
    local attempts="${NOTARY_CHECK_ATTEMPTS:-15}" attempt=1 output
    until output="$("$@" 2>&1)"; do
        [ "$attempt" -lt "$attempts" ] || { echo "$output" >&2; return 1; }
        step "Not yet: try $((attempt + 1))/$attempts in 20 s"
        attempt=$((attempt + 1))
        sleep 20
    done
}

step "Building the CLI $VERSION for $TARGET"
# The remap keeps the maintainer's home directory out of the public binary.
RUSTFLAGS="--remap-path-prefix=$HOME=~" "$CARGO" build --release --locked --target "$TARGET" -p ddc-cli
CLI="$ROOT/target/$TARGET/release/deepdrivecleaner"
[ "$(lipo -archs "$CLI")" = "arm64" ] || fail "not an arm64-only CLI: $(lipo -archs "$CLI")"
# The cost counters of the complexity guard never ship (ddc-core feature op-stats).
SYMBOLS="$(nm "$CLI")"
grep ' _main$' <<<"$SYMBOLS" >/dev/null || fail "nm sees no symbol table, so it cannot see the counters"
if grep DDC_OP_STATS <<<"$SYMBOLS" >/dev/null; then fail "the CLI carries the op-stats counters"; fi
[ "$("$CLI" --version)" = "deepdrivecleaner $VERSION" ] || fail "the CLI reports '$("$CLI" --version)'"

step "Building the app $APP_VERSION ($CONFIG)"
# Sparkle is resolved to the version Package.resolved pins, never to a newer one.
xcodebuild -project app/DeepDriveCleaner.xcodeproj -scheme DeepDriveCleaner -configuration "$CONFIG" \
    -disableAutomaticPackageResolution \
    -destination "platform=macOS,arch=arm64" ARCHS=arm64 \
    -derivedDataPath build/DerivedData CODE_SIGNING_ALLOWED=NO -quiet build
STAGE="$ROOT/build/$CONFIG"
APP="$STAGE/DeepDriveCleaner.app"
rm -rf "$APP"
mkdir -p "$STAGE"
ditto "build/DerivedData/Build/Products/$CONFIG/DeepDriveCleaner.app" "$APP"
# In Contents/Helpers, not Contents/MacOS: on a case-insensitive volume
# deepdrivecleaner and DeepDriveCleaner are the same name.
mkdir -p "$APP/Contents/Helpers"
cp "$CLI" "$APP/Contents/Helpers/deepdrivecleaner"
# The license and the disclaimer travel with the app: a cask install has no other copy.
cp LICENSE.md DISCLAIMER.md "$APP/Contents/Resources/"

PLIST="$APP/Contents/Info.plist"
SPARKLE="$APP/Contents/Frameworks/Sparkle.framework"
[ -d "$SPARKLE" ] || fail "Sparkle.framework is missing from the app"
[ "$(readlink "$SPARKLE/Versions/Current")" = B ] || fail "Sparkle.framework's current version is not B"
# Its downloader and installer services are for sandboxed apps only (ADR §8): the
# directory and the framework's link to it go.
rm -rf "$SPARKLE/Versions/B/XPCServices" "$SPARKLE/XPCServices"
if [ "$DIST" = 1 ]; then
    # A release checks its updates against the owner's key and the production feed, and
    # carries nothing of the Debug feed.
    ED_KEY="$(plutil -extract SUPublicEDKey raw -o - "$PLIST" 2>/dev/null || true)"
    [ "$ED_KEY" != "$ED_KEY_PLACEHOLDER" ] \
        || fail "SUPublicEDKey is still the placeholder: the owner runs generate_keys and pastes the public key into app/DeepDriveCleaner/Info.plist"
    [[ "$ED_KEY" =~ ^[A-Za-z0-9+/]{43}=$ ]] || fail "SUPublicEDKey is not an EdDSA public key: '$ED_KEY'"
    [ "$(plutil -extract SUFeedURL raw -o - "$PLIST" 2>/dev/null || true)" = "$FEED_URL" ] \
        || fail "SUFeedURL is not $FEED_URL"
    if plutil -extract NSAppTransportSecurity xml1 -o /dev/null "$PLIST" 2>/dev/null; then
        fail "a release carries no NSAppTransportSecurity exception"
    fi
    if strings "$APP/Contents/MacOS/DeepDriveCleaner" | grep -F 127.0.0.1 >/dev/null; then
        fail "the app binary names 127.0.0.1: the Debug feed is compiled in"
    fi
else
    # The Debug feed is http on 127.0.0.1 (scripts/debug-feed.sh). The Release
    # guard above looks for this string; a Debug binary without it means the
    # guard looks for the wrong thing.
    strings "$APP/Contents/MacOS/DeepDriveCleaner" | grep -F 127.0.0.1 >/dev/null \
        || fail "the Debug app binary does not name 127.0.0.1: the Release guard against the Debug feed would see nothing"
    plutil -replace NSAppTransportSecurity -json '{"NSAllowsLocalNetworking":true}' "$PLIST"
    if [ -n "${DDC_DEBUG_ED_PUBLIC_KEY:-}" ]; then
        [[ "$DDC_DEBUG_ED_PUBLIC_KEY" =~ ^[A-Za-z0-9+/]{43}=$ ]] \
            || fail "DDC_DEBUG_ED_PUBLIC_KEY is not an EdDSA public key"
        plutil -replace SUPublicEDKey -string "$DDC_DEBUG_ED_PUBLIC_KEY" "$PLIST"
    fi
fi
xattr -cr "$APP"

step "Signing with the Developer ID (team $TEAM_ID)"
# Inside out: Sparkle's helpers, then its framework, then the embedded CLI, the app
# last (no --deep, no entitlements).
sign_sparkle "$SPARKLE/Versions/B/Autoupdate" "$SPARKLE_ID.Autoupdate"
sign_sparkle "$SPARKLE/Versions/B/Updater.app" "$SPARKLE_ID.Updater"
sign_sparkle "$SPARKLE" "$SPARKLE_ID"
sign_code "$APP/Contents/Helpers/deepdrivecleaner" "$CLI_ID"
sign_code "$APP" "$APP_ID"
codesign --verify --deep --strict --verbose=2 "$APP"
# --verify --deep accepts nested code signed ad hoc or by another team: each Mach-O file
# has the team and the hardened runtime.
"$ROOT/scripts/check-signatures.sh" "$APP" "$TEAM_ID"
[ -f "$APP/Contents/Library/LaunchAgents/$APP_ID.scheduler.plist" ] || fail "the scheduler plist is missing"
[ -f "$APP/Contents/Library/LaunchAgents/$APP_ID.scheduler-open.plist" ] || fail "the scheduler's fallback plist is missing"
[ "$(plutil -extract CFBundleIconFile raw "$APP/Contents/Info.plist")" = "AppIcon" ] \
    && [ -f "$APP/Contents/Resources/AppIcon.icns" ] || fail "the app icon is missing"

if [ "$DIST" = 0 ]; then
    printf '\033[0;32m✅ %s\033[0m\n' "$APP"
    exit 0
fi

if strings "$APP/Contents/MacOS/DeepDriveCleaner" | grep -F "$HOME" >/dev/null; then fail "the app binary contains $HOME"; fi

ZIP="$ROOT/dist/DeepDriveCleaner-$VERSION.zip"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$ROOT/dist"
rm -f "$ZIP" "$ZIP.sha256"

step "Notarizing (waits for Apple)"
ditto -c -k --keepParent "$APP" "$WORK/notarize.zip"
xcrun notarytool submit "$WORK/notarize.zip" --keychain-profile "$NOTARY_PROFILE" \
    --wait --timeout 900s --output-format json > "$WORK/notary.json" || true
if ! grep -qE '"status" *: *"Accepted"' "$WORK/notary.json"; then
    cat "$WORK/notary.json" >&2
    ID="$(sed -n 's/.*"id" *: *"\([^"]*\)".*/\1/p' "$WORK/notary.json")"
    [ -n "$ID" ] && { xcrun notarytool log "$ID" --keychain-profile "$NOTARY_PROFILE" >&2 || true; }
    fail "notarization was not accepted"
fi

step "Stapling"
retry xcrun stapler staple "$APP" || fail "the ticket could not be stapled"
xcrun stapler validate "$APP" >/dev/null || fail "the stapled ticket does not validate"
codesign --verify --deep --strict "$APP" || fail "stapling broke the signature"
ASSESSMENT="$(spctl --assess --type execute --verbose=2 "$APP" 2>&1)" || { echo "$ASSESSMENT" >&2; fail "Gatekeeper rejects the app"; }
grep -q "source=Notarized Developer ID" <<<"$ASSESSMENT" || fail "Gatekeeper does not accept the app as Notarized Developer ID"

step "Packing $(basename "$ZIP")"
# Zipped after stapling, so the ticket travels with the app. The zip is Sparkle's
# enclosure too: --sequesterRsrc, as Sparkle advises, would keep a resource fork in
# __MACOSX/, which the app, stripped by `xattr -cr`, should never have.
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"
ZIP_LISTING="$(unzip -Z1 "$ZIP")" || fail "cannot list $(basename "$ZIP")"
if grep -q '^__MACOSX/' <<<"$ZIP_LISTING"; then fail "the zip holds __MACOSX/: the app carries extended attributes"; fi
shasum -a 256 "$ZIP" | awk '{print $1}' > "$ZIP.sha256"

printf '\033[0;32m✅ %s\033[0m\n' "$ZIP"
echo "   sha256 $(cat "$ZIP.sha256")"
