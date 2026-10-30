#!/bin/bash
# Renders the release's appcast.xml from packaging/sparkle/appcast.xml.tmpl (ADR
# "Updates through Sparkle, alongside the Homebrew cask", §6). scripts/publish-release.sh
# calls it before the release exists; scripts/tests/test_appcast.py runs it with a fake
# sign_update. It reads nothing from the network.
#
#   scripts/render-appcast.sh --zip <zip> --version <CFBundleVersion> --short-version <x.y.z>
#       --min-macos <26.0> --url <enclosure URL> --notes-url <release page> --team <team id>
#       --out <appcast.xml> [--previous <the previous release's appcast.xml>] [--team-change]
#       [--local] [--ed-key-file <private key file>]
#
# sign_update is $SPARKLE_SIGN_UPDATE: it reads the private EdDSA key from the login
# keychain, so a real release runs unsandboxed, like notarytool. --ed-key-file hands it
# a key file instead (a Debug key). --local, for scripts/debug-feed.sh only, takes the
# URLs as http on 127.0.0.1, the Debug build's feed.
#
# Refuses a CFBundleVersion not greater than the previous appcast's (Sparkle compares
# CFBundleVersion, so it would never offer it), and a team other than the previous
# appcast's ddc:team unless --team-change, which renders an information-only item with
# no enclosure: a link to the release page, never an install.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMPLATE="$ROOT/packaging/sparkle/appcast.xml.tmpl"

fail() { printf '\033[0;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }

ZIP="" VERSION="" SHORT_VERSION="" MIN_MACOS="" URL="" NOTES_URL="" TEAM="" OUT="" PREVIOUS="" TEAM_CHANGE=0
LOCAL=0 ED_KEY_FILE=""
while [ $# -gt 0 ]; do
    case "$1" in
        --team-change) TEAM_CHANGE=1; shift; continue ;;
        --local) LOCAL=1; shift; continue ;;
        --zip | --version | --short-version | --min-macos | --url | --notes-url | --team | --out | --previous | --ed-key-file)
            [ $# -ge 2 ] || fail "$1 needs a value" ;;
        *) fail "unknown argument: $1" ;;
    esac
    case "$1" in
        --zip) ZIP="$2" ;;
        --version) VERSION="$2" ;;
        --short-version) SHORT_VERSION="$2" ;;
        --min-macos) MIN_MACOS="$2" ;;
        --url) URL="$2" ;;
        --notes-url) NOTES_URL="$2" ;;
        --team) TEAM="$2" ;;
        --out) OUT="$2" ;;
        --previous) PREVIOUS="$2" ;;
        --ed-key-file) ED_KEY_FILE="$2" ;;
    esac
    shift 2
done

# Every value is checked against its form before it reaches sed: none can hold the
# `|` delimiter, a `&` or a character XML would need escaped.
[ -f "$ZIP" ] || fail "--zip: no such file: $ZIP"
[[ "$VERSION" =~ ^[1-9][0-9]*$ ]] || fail "--version must be the app's CFBundleVersion, a whole number: '$VERSION'"
[[ "$SHORT_VERSION" =~ ^[0-9]+(\.[0-9]+){1,2}$ ]] || fail "--short-version must be x.y or x.y.z: '$SHORT_VERSION'"
# Sparkle's minimumSystemVersion in three parts: 26.0 is 26.0.0.
case "$MIN_MACOS" in
    *.*.*) ;;
    *.*) MIN_MACOS="$MIN_MACOS.0" ;;
    *) MIN_MACOS="$MIN_MACOS.0.0" ;;
esac
[[ "$MIN_MACOS" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "--min-macos must be a macOS version such as 26.0: '$MIN_MACOS'"
URL_FORM='^https://[A-Za-z0-9./_~:%+-]+$'
[ "$LOCAL" = 0 ] || URL_FORM='^http://127\.0\.0\.1:[0-9]+/[A-Za-z0-9./_~%+-]*$'
[[ "$URL" =~ $URL_FORM ]] || fail "--url must be an https URL (with --local, http on 127.0.0.1): '$URL'"
[[ "$NOTES_URL" =~ $URL_FORM ]] || fail "--notes-url must be an https URL (with --local, http on 127.0.0.1): '$NOTES_URL'"
[ -z "$ED_KEY_FILE" ] || [ -f "$ED_KEY_FILE" ] || fail "--ed-key-file: no such file: $ED_KEY_FILE"
[[ "$TEAM" =~ ^[A-Z0-9]{10}$ ]] || fail "--team must be a team identifier: '$TEAM'"
[ -n "$OUT" ] || fail "--out is required"
[ -f "$TEMPLATE" ] || fail "appcast template missing: $TEMPLATE"
command -v xmllint >/dev/null || fail "xmllint not found"

# The previous release's item: its CFBundleVersion and its team. The prefixes are not
# bound in an xpath here, so elements are matched by local name.
if [ -n "$PREVIOUS" ]; then
    [ -f "$PREVIOUS" ] || fail "--previous: no such file: $PREVIOUS"
    xmllint --noout "$PREVIOUS" 2>/dev/null || fail "the previous appcast is not well-formed XML: $PREVIOUS"
    item="//*[local-name()='item']"
    PREVIOUS_VERSION="$(xmllint --xpath "string($item/*[local-name()='version'])" "$PREVIOUS")"
    PREVIOUS_TEAM="$(xmllint --xpath "string($item/*[local-name()='team'])" "$PREVIOUS")"
    [[ "$PREVIOUS_VERSION" =~ ^[0-9]+$ ]] || fail "the previous appcast has no sparkle:version: $PREVIOUS"
    [ "$VERSION" -gt "$PREVIOUS_VERSION" ] \
        || fail "CFBundleVersion $VERSION is not greater than the previous release's $PREVIOUS_VERSION: Sparkle would never offer it (raise CURRENT_PROJECT_VERSION)"
    if [ -n "$PREVIOUS_TEAM" ] && [ "$PREVIOUS_TEAM" != "$TEAM" ]; then
        [ "$TEAM_CHANGE" = 1 ] \
            || fail "the app is signed by team $TEAM, the previous release by $PREVIOUS_TEAM: a team change is never installed through Sparkle (--team-change publishes it as information only)"
    elif [ "$TEAM_CHANGE" = 1 ]; then
        fail "--team-change, but the previous release is signed by the same team ($TEAM)"
    fi
elif [ "$TEAM_CHANGE" = 1 ]; then
    fail "--team-change needs --previous: there is no team to change from"
fi

# sign_update prints `sparkle:edSignature="<base64>" length="<bytes>"`.
SIGN_UPDATE="${SPARKLE_SIGN_UPDATE:-}"
[ -n "$SIGN_UPDATE" ] || fail "SPARKLE_SIGN_UPDATE is not set: the path of Sparkle's sign_update"
[ -x "$SIGN_UPDATE" ] || fail "sign_update is not executable: $SIGN_UPDATE"
SIGN_ARGS=()
[ -z "$ED_KEY_FILE" ] || SIGN_ARGS=(--ed-key-file "$ED_KEY_FILE")
SIGNED="$("$SIGN_UPDATE" ${SIGN_ARGS[@]+"${SIGN_ARGS[@]}"} "$ZIP")" || fail "sign_update failed on $ZIP"
SIGNATURE="$(sed -n 's/.*sparkle:edSignature="\([^"]*\)".*/\1/p' <<<"$SIGNED")"
LENGTH="$(sed -n 's/.*length="\([^"]*\)".*/\1/p' <<<"$SIGNED")"
[[ "$SIGNATURE" =~ ^[A-Za-z0-9+/]+=*$ ]] || fail "sign_update printed no edSignature: $SIGNED"
[ "$LENGTH" = "$(stat -f %z "$ZIP")" ] || fail "sign_update's length ($LENGTH) is not the zip's size ($(stat -f %z "$ZIP"))"

if [ "$TEAM_CHANGE" = 1 ]; then
    INFORMATIONAL=(-e 's|__INFORMATIONAL__|<sparkle:informationalUpdate/>|' -e '/<enclosure /d')
else
    INFORMATIONAL=(-e '/__INFORMATIONAL__/d')
fi
PUB_DATE="$(LC_ALL=C date -u '+%a, %d %b %Y %H:%M:%S +0000')"

# Written beside the target and moved over it, so a failed render leaves no appcast.
RENDERED="$(mktemp "$OUT.XXXXXX")"
trap 'rm -f "$RENDERED"' EXIT
# `|` as the delimiter: base64 and URLs hold `/`.
sed -e "s|__VERSION__|$VERSION|g" \
    -e "s|__SHORT_VERSION__|$SHORT_VERSION|g" \
    -e "s|__MIN_MACOS__|$MIN_MACOS|g" \
    -e "s|__NOTES_URL__|$NOTES_URL|g" \
    -e "s|__URL__|$URL|g" \
    -e "s|__LENGTH__|$LENGTH|g" \
    -e "s|__SIGNATURE__|$SIGNATURE|g" \
    -e "s|__TEAM__|$TEAM|g" \
    -e "s|__PUB_DATE__|$PUB_DATE|g" \
    "${INFORMATIONAL[@]}" \
    "$TEMPLATE" > "$RENDERED"
if grep -qE '__[A-Z0-9_]+__' "$RENDERED"; then fail "appcast template placeholders left unsubstituted"; fi
xmllint --noout "$RENDERED" || fail "the rendered appcast is not well-formed XML"
mv "$RENDERED" "$OUT"
trap - EXIT
