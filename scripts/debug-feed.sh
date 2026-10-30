#!/bin/bash
# Renders a local Sparkle feed for a Debug build (ADR "Updates through Sparkle,
# alongside the Homebrew cask", §1): build/debug-feed/appcast.xml and a copy of the zip
# beside it, with http URLs on 127.0.0.1. A Debug build never reads the production
# feed; it reads this one.
#
#   scripts/debug-feed.sh <zip> [--port <8000>] [--ed-key-file <private key file>]
#
# The owner's test of Update and Relaunch:
#   1. Build the update: raise CURRENT_PROJECT_VERSION, `make app`, and zip it:
#        ditto -c -k --sequesterRsrc --keepParent build/Debug/DeepDriveCleaner.app <zip>
#      Put CURRENT_PROJECT_VERSION back and `make app` again: that is the copy to update.
#   2. scripts/debug-feed.sh <zip>
#   3. Serve it:       cd build/debug-feed && python3 -m http.server 8000 --bind 127.0.0.1
#   4. Point the app:  defaults write com.smartandpoint.deepdrivecleaner DebugFeedPort 8000
#      (8000 when unset), then open the older copy and press Check Now in Settings.
#
# Signing: sign_update reads the private key from the login keychain unless
# --ed-key-file names a Debug key (`generate_keys --account <name>`, exported with
# `-x <file>`). The Debug app must carry that key's public half: build it with
# DDC_DEBUG_ED_PUBLIC_KEY=<public key> make app. sign_update is $SPARKLE_SIGN_UPDATE,
# else the one in the build's resolved Sparkle package.
set -euo pipefail

TEAM_ID=8MSDJTBW2D

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# The tests point it elsewhere.
OUT_DIR="${DDC_DEBUG_FEED_DIR:-$ROOT/build/debug-feed}"

fail() { printf '\033[0;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }

[ $# -ge 1 ] || fail "usage: scripts/debug-feed.sh <zip> [--port <8000>] [--ed-key-file <file>]"
ZIP="$1"
shift
PORT=8000
KEY_ARGS=()
while [ $# -gt 0 ]; do
    [ $# -ge 2 ] || fail "$1 needs a value"
    case "$1" in
        --port) PORT="$2" ;;
        --ed-key-file) KEY_ARGS=(--ed-key-file "$2") ;;
        *) fail "unknown argument: $1" ;;
    esac
    shift 2
done
[ -f "$ZIP" ] || fail "no such zip: $ZIP"
[[ "$PORT" =~ ^[1-9][0-9]{0,4}$ ]] || fail "--port must be a port number: '$PORT'"

export SPARKLE_SIGN_UPDATE="${SPARKLE_SIGN_UPDATE:-$ROOT/build/DerivedData/SourcePackages/artifacts/sparkle/Sparkle/bin/sign_update}"
[ -x "$SPARKLE_SIGN_UPDATE" ] || fail "sign_update not found at $SPARKLE_SIGN_UPDATE: run 'make app' first, which resolves Sparkle"

# The versions come from the app in the zip, never from the project: the zip is
# what Sparkle installs.
PLIST="$(unzip -p "$ZIP" DeepDriveCleaner.app/Contents/Info.plist)" || fail "the zip holds no DeepDriveCleaner.app/Contents/Info.plist"
value() { plutil -extract "$1" raw -o - - <<<"$PLIST"; }
VERSION="$(value CFBundleVersion)" || fail "the app in the zip has no CFBundleVersion"
SHORT_VERSION="$(value CFBundleShortVersionString)" || fail "the app in the zip has no CFBundleShortVersionString"
MIN_MACOS="$(value LSMinimumSystemVersion)" || fail "the app in the zip has no LSMinimumSystemVersion"

mkdir -p "$OUT_DIR"
NAME="$(basename "$ZIP")"
cp "$ZIP" "$OUT_DIR/$NAME"
BASE="http://127.0.0.1:$PORT"
"$ROOT/scripts/render-appcast.sh" --local ${KEY_ARGS[@]+"${KEY_ARGS[@]}"} \
    --zip "$OUT_DIR/$NAME" --version "$VERSION" --short-version "$SHORT_VERSION" --min-macos "$MIN_MACOS" \
    --url "$BASE/$NAME" --notes-url "$BASE/" --team "$TEAM_ID" --out "$OUT_DIR/appcast.xml"

printf '\033[0;32m✅ %s\033[0m\n' "$OUT_DIR/appcast.xml"
echo "   version $SHORT_VERSION ($VERSION) at $BASE/appcast.xml"
echo "   serve it:  cd build/debug-feed && python3 -m http.server $PORT --bind 127.0.0.1"
