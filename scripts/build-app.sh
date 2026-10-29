#!/bin/bash
# Builds the menu bar app with the CLI embedded, and signs both with the Developer ID
# (ADR "Menu bar app: Swift shell over the ddc CLI", §1 and §8).
#
#   scripts/build-app.sh          Debug:   build/Debug/DeepDriveCleaner.app
#   scripts/build-app.sh --dist   Release: notarized and stapled, dist/DeepDriveCleaner-<version>.zip
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
APP_VERSION="$(sed -n 's/^[[:space:]]*MARKETING_VERSION = \(.*\);$/\1/p' app/DeepDriveCleaner.xcodeproj/project.pbxproj | sort -u)"
if [ "$DIST" = 1 ]; then
    [ -n "${NOTARY_PROFILE:-}" ] || fail "NOTARY_PROFILE is not set (.env)"
    # A zip built from uncommitted code could not be rebuilt from its tag.
    [ -z "$(git status --porcelain)" ] || fail "the tree is not clean (untracked files count): commit first"
    [ "$APP_VERSION" = "$VERSION" ] || fail "the app's MARKETING_VERSION ($APP_VERSION) is not the CLI's version ($VERSION)"
    # Both release assets come from one commit; scripts/publish-release.sh checks
    # BUILD_COMMIT against HEAD for both.
    [ "$(cat dist/BUILD_COMMIT 2>/dev/null || true)" = "$(git rev-parse HEAD)" ] \
        || fail "dist/BUILD_COMMIT is not HEAD: run scripts/build-dist.sh first (make dist runs both)"
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
xcodebuild -project app/DeepDriveCleaner.xcodeproj -scheme DeepDriveCleaner -configuration "$CONFIG" \
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
xattr -cr "$APP"

step "Signing with the Developer ID (team $TEAM_ID)"
# Inside out: the embedded CLI first, the app last (no --deep, no entitlements).
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
# Zipped after stapling, so the ticket travels with the app.
ditto -c -k --keepParent "$APP" "$ZIP"
shasum -a 256 "$ZIP" | awk '{print $1}' > "$ZIP.sha256"

printf '\033[0;32m✅ %s\033[0m\n' "$ZIP"
echo "   sha256 $(cat "$ZIP.sha256")"
