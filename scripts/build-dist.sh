#!/bin/bash
# Builds the CLI tarball that scripts/publish-release.sh attaches to a release:
# release build -> Developer ID signature -> Apple notarization -> tarball.
#
# Signed with the SmartAndPoint Developer ID, as GrammarHelper is. The designated
# requirement of such a signature is the identifier plus the team, so macOS keeps a
# Full Disk Access grant when a newer signed build replaces this one; an ad-hoc
# signature is matched by its exact hash and loses the grant on every build.
# DDC_SIGN_IDENTITY and NOTARY_PROFILE come from .env, which is never committed.
set -euo pipefail

TARGET=aarch64-apple-darwin
# The app's bundle id is com.smartandpoint.deepdrivecleaner; the CLI, in the app and
# here, is .cli (ADR "Menu bar app: Swift shell over the ddc CLI", §4).
IDENTIFIER=com.smartandpoint.deepdrivecleaner.cli
TEAM_ID=8MSDJTBW2D

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"                                # rustup reads rust-toolchain.toml from here
export PATH="$HOME/.cargo/bin:$PATH"     # Homebrew's Rust may shadow rustup
CARGO="$HOME/.cargo/bin/cargo"
DIST="$ROOT/dist"

fail() { printf '\033[0;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }
step() { printf '\033[0;34m→ %s\033[0m\n' "$1"; }

# First, so a failed run leaves nothing scripts/publish-release.sh could pick up.
[ "$DIST" = "$ROOT/dist" ] || fail "unexpected dist directory: $DIST"
rm -rf "$DIST"

# shellcheck source=/dev/null
if [ -f .env ]; then set -a; . ./.env; set +a; fi
[ -n "${DDC_SIGN_IDENTITY:-}" ] || fail "DDC_SIGN_IDENTITY is not set (.env): a release is signed with the Developer ID"
[[ "$DDC_SIGN_IDENTITY" =~ ^[0-9A-Fa-f]{40}$ ]] \
    || fail "DDC_SIGN_IDENTITY must be the certificate's SHA-1 (security find-identity -v -p codesigning)"
[ -n "${NOTARY_PROFILE:-}" ] || fail "NOTARY_PROFILE is not set (.env)"
# A tarball built from uncommitted code could not be rebuilt from its tag.
[ -z "$(git status --porcelain)" ] || fail "the tree is not clean (untracked files count): commit first"

VERSION="$("$CARGO" pkgid -p ddc-cli | sed 's/.*[#@]//')"
# scripts/build-app.sh --dist refuses this too, but only after this script has
# spent a notarization round: refuse before it.
APP_VERSION="$(sed -n 's/^[[:space:]]*MARKETING_VERSION = "\{0,1\}\([^";]*\)"\{0,1\};$/\1/p' app/DeepDriveCleaner.xcodeproj/project.pbxproj | sort -u)"
[ "$APP_VERSION" = "$VERSION" ] || fail "the app's MARKETING_VERSION ($APP_VERSION) is not the CLI's version ($VERSION)"
# So is a build number that does not grow past the last release's: Sparkle compares it.
"$ROOT/scripts/check-build-number.sh"
ASSET="deepdrivecleaner-$VERSION-$TARGET.tar.gz"

# The tag comes first (make tag), and release.yml builds the other targets from it:
# this tarball is built from the commit it names, or one release would mix two.
"$ROOT/scripts/require-tag.sh"

# Build and the unsigned checks (architecture, version, no home folder in the binary,
# no op-stats counters), shared with CI.
"$ROOT/scripts/build-cli.sh" "$TARGET"
BIN="${CARGO_TARGET_DIR:-$ROOT/target}/$TARGET/release/deepdrivecleaner"

STAGE="$DIST/stage"
mkdir -p "$STAGE"
cp "$BIN" "$STAGE/deepdrivecleaner"
xattr -c "$STAGE/deepdrivecleaner"

step "Signing with $IDENTIFIER (team $TEAM_ID)"
codesign --force --timestamp --options runtime --identifier "$IDENTIFIER" \
    --sign "$DDC_SIGN_IDENTITY" "$STAGE/deepdrivecleaner"
codesign --verify --strict --verbose=2 "$STAGE/deepdrivecleaner"
SIGNATURE="$(codesign -dv "$STAGE/deepdrivecleaner" 2>&1)" || fail "codesign cannot read the signature"
grep -qxF "Identifier=$IDENTIFIER" <<<"$SIGNATURE" || fail "not signed as $IDENTIFIER"
grep -qxF "TeamIdentifier=$TEAM_ID" <<<"$SIGNATURE" || fail "not signed by team $TEAM_ID"

step "Notarizing (waits for Apple)"
ditto -c -k "$STAGE/deepdrivecleaner" "$DIST/notarize.zip"
xcrun notarytool submit "$DIST/notarize.zip" --keychain-profile "$NOTARY_PROFILE" \
    --wait --timeout 900s --output-format json > "$DIST/notary.json" || true
if ! grep -qE '"status" *: *"Accepted"' "$DIST/notary.json"; then
    cat "$DIST/notary.json" >&2
    ID="$(sed -n 's/.*"id" *: *"\([^"]*\)".*/\1/p' "$DIST/notary.json")"
    [ -n "$ID" ] && { xcrun notarytool log "$ID" --keychain-profile "$NOTARY_PROFILE" >&2 || true; }
    fail "notarization was not accepted"
fi
rm -f "$DIST/notarize.zip"
# A bare binary cannot be stapled: Gatekeeper looks the ticket up online. `spctl
# --type execute` rejects every command-line tool ("does not seem to be an app");
# this is the check `brew audit` applies to cask binaries. The ticket can reach the
# lookup service some time after notarytool reports Accepted, so it is retried
# (every 20 s, five minutes by default) rather than failing a whole notarization round.
ATTEMPTS="${NOTARY_CHECK_ATTEMPTS:-15}"
ATTEMPT=1
until NOTARY_CHECK="$(codesign --verify -R="notarized" --check-notarization "$STAGE/deepdrivecleaner" 2>&1)"; do
    [ "$ATTEMPT" -lt "$ATTEMPTS" ] || { echo "$NOTARY_CHECK" >&2; fail "Apple does not report the binary as notarized"; }
    step "The ticket is not visible yet; check $((ATTEMPT + 1))/$ATTEMPTS in 20 s"
    ATTEMPT=$((ATTEMPT + 1))
    sleep 20
done

step "Packing $ASSET"
cp LICENSE.md DISCLAIMER.md README.md "$STAGE/"
# No extended attributes, no AppleDouble files, no local user in the archive.
COPYFILE_DISABLE=1 tar --no-xattrs --no-mac-metadata --uid 0 --gid 0 --uname root --gname wheel \
    -czf "$DIST/$ASSET" -C "$STAGE" deepdrivecleaner LICENSE.md DISCLAIMER.md README.md
LISTING="$(tar -tzf "$DIST/$ASSET" | LC_ALL=C sort | tr '\n' ' ')"
[ "$LISTING" = "DISCLAIMER.md LICENSE.md README.md deepdrivecleaner " ] || fail "unexpected tarball contents: $LISTING"
shasum -a 256 "$DIST/$ASSET" | awk '{print $1}' > "$DIST/$ASSET.sha256"
git rev-parse HEAD > "$DIST/BUILD_COMMIT"
rm -rf "$STAGE"

printf '\033[0;32m✅ %s\033[0m\n' "$DIST/$ASSET"
echo "   sha256 $(cat "$DIST/$ASSET.sha256")"
echo "   next: scripts/build-app.sh --dist (make dist runs it), then scripts/publish-release.sh --dry-run"
