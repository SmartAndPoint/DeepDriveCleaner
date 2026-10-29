#!/bin/bash
# Checks every Mach-O file in an app bundle: signed by the expected team, with the
# hardened runtime (ADR "Updates through Sparkle, alongside the Homebrew cask", §8).
# `codesign --verify --deep --strict` accepts nested code signed ad hoc or by another
# team, so Sparkle's Autoupdate or Updater.app signed wrongly would pass it; this does not.
#
#   scripts/check-signatures.sh <app> <team id>
#
# scripts/build-app.sh runs it after `codesign --verify`. scripts/tests/test_signing_check.py
# runs it on a fixture signed ad hoc (make app-test) and with a fake codesign.
set -euo pipefail

fail() { printf '\033[0;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }

[ $# -eq 2 ] || fail "usage: scripts/check-signatures.sh <app> <team id>"
APP="$1"
TEAM="$2"
[ -d "$APP" ] || fail "no such bundle: $APP"
[[ "$TEAM" =~ ^[A-Z0-9]{10}$ ]] || fail "not a team identifier: '$TEAM'"

# Mach-O by content, not by name: a bundle's executables carry no extension. Links are
# skipped; the file they name is checked where it lives.
CHECKED=0
BAD=()
while IFS= read -r -d '' FILE; do
    case "$(file -b "$FILE")" in
        Mach-O*) ;;
        *) continue ;;
    esac
    CHECKED=$((CHECKED + 1))
    if ! INFO="$(codesign -dv --verbose=2 "$FILE" 2>&1)"; then
        BAD+=("$FILE: not signed")
        continue
    fi
    grep -qxF "TeamIdentifier=$TEAM" <<<"$INFO" || BAD+=("$FILE: not signed by team $TEAM ($(sed -n 's/^TeamIdentifier=//p' <<<"$INFO"))")
    # `flags=0x10000(runtime)`, or with other flags beside it: `flags=0x10002(adhoc,runtime)`.
    grep -qE '^CodeDirectory .*flags=0x[0-9a-f]+\(([a-z-]+,)*runtime(,[a-z-]+)*\)' <<<"$INFO" \
        || BAD+=("$FILE: no hardened runtime")
done < <(find "$APP" -type f -print0)

[ "$CHECKED" -gt 0 ] || fail "no Mach-O file in $APP"
if [ "${#BAD[@]}" -gt 0 ]; then
    printf '%s\n' "${BAD[@]}" >&2
    fail "${#BAD[@]} problem(s) in the signatures of $APP"
fi
echo "signatures: $CHECKED Mach-O files signed by team $TEAM with the hardened runtime"
