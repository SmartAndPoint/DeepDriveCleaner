#!/bin/bash
# Builds the release CLI for one target and checks the binary before anything signs
# or packs it (story "Build matrix: every target built, tested and packaged from one
# tag", DDC-007). One build, three callers: scripts/build-dist.sh (then signs it), the
# Gate's release-build job and release.yml (then packs it).
#
#   scripts/build-cli.sh <target>
#
# Native only: the binary runs here (--version), so <target> is this machine's.
# The binary lands in ${CARGO_TARGET_DIR:-target}/<target>/release/.
set -euo pipefail

if [ "$#" != 1 ] || [ -z "$1" ]; then echo "usage: scripts/build-cli.sh <target>" >&2; exit 2; fi
TARGET="$1"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"                                # rustup reads rust-toolchain.toml from here
export PATH="$HOME/.cargo/bin:$PATH"     # Homebrew's Rust may shadow rustup
# Where rustup keeps no ~/.cargo/bin/cargo (GitHub's macOS image installs it with
# Homebrew), the cargo in PATH is rustup's: the Makefile's rule.
CARGO="$HOME/.cargo/bin/cargo"
[ -x "$CARGO" ] || CARGO=cargo

fail() { printf '\033[0;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }
step() { printf '\033[0;34m→ %s\033[0m\n' "$1"; }

EXE=""
WINDOWS=0
case "$TARGET" in *-windows-*) EXE=.exe; WINDOWS=1 ;; esac

VERSION="$("$CARGO" pkgid -p ddc-cli | sed 's/.*[#@]//')"

# The paths that must not ship: the home folder, the checkout and cargo's home. The
# remap keeps the home folder out of the binary; on macOS and Linux (the owner's Mac
# and the runners) the checkout and cargo's home lie under it. A Windows runner checks
# out on another drive and hands rustc native paths, so there the checkout is
# remapped too, in rustc's spelling.
FLAGS="--remap-path-prefix=$HOME=~"
NEEDLES=("$HOME" "$ROOT")
[ -z "${CARGO_HOME:-}" ] || NEEDLES+=("$CARGO_HOME")
if [ "$WINDOWS" = 1 ]; then
    FLAGS="--remap-path-prefix=$(cygpath -w "$HOME")=~ --remap-path-prefix=$(cygpath -w "$ROOT")=."
    for NEEDLE in "${NEEDLES[@]}"; do
        NEEDLES+=("$(cygpath -w "$NEEDLE")" "$(cygpath -m "$NEEDLE")")
    done
fi

step "Building $VERSION for $TARGET"
RUSTFLAGS="$FLAGS" "$CARGO" build --release --locked --target "$TARGET" -p ddc-cli
BIN="${CARGO_TARGET_DIR:-$ROOT/target}/$TARGET/release/deepdrivecleaner$EXE"
[ -f "$BIN" ] || fail "cargo built no $BIN"

# The architecture: lipo where there is one; elsewhere running --version on the
# target's own runner is the check.
case "$TARGET" in
    aarch64-apple-darwin) [ "$(lipo -archs "$BIN")" = "arm64" ] || fail "not an arm64-only binary: $(lipo -archs "$BIN")" ;;
    x86_64-apple-darwin) [ "$(lipo -archs "$BIN")" = "x86_64" ] || fail "not an x86_64-only binary: $(lipo -archs "$BIN")" ;;
esac
[ "$("$BIN" --version)" = "deepdrivecleaner $VERSION" ] || fail "the binary reports '$("$BIN" --version)'"

# grep on the binary itself: status 1 is "not there", anything else is a grep that
# could not read it, which must stop the build instead of passing the check.
for NEEDLE in "${NEEDLES[@]}"; do
    STATUS=0
    LC_ALL=C grep -aqF -- "$NEEDLE" "$BIN" || STATUS=$?
    [ "$STATUS" != 0 ] || fail "the binary still contains $NEEDLE"
    [ "$STATUS" = 1 ] || fail "grep cannot read $BIN"
done

# The cost counters of the complexity guard never ship (ddc-core feature op-stats).
# nm first and on its own, so a failing tool stops the build instead of passing the
# check. A Windows binary keeps its symbols in the PDB, out of nm's reach.
case "$TARGET" in
    *-apple-darwin | *-linux-*)
        SYMBOLS="$(nm "$BIN")"
        case "$TARGET" in
            *-apple-darwin) grep ' _main$' <<<"$SYMBOLS" >/dev/null || fail "nm sees no symbol table, so it cannot see the counters" ;;
            *) grep ' main$' <<<"$SYMBOLS" >/dev/null || fail "nm sees no symbol table, so it cannot see the counters" ;;
        esac
        if grep DDC_OP_STATS <<<"$SYMBOLS" >/dev/null; then fail "the binary carries the op-stats counters"; fi
        ;;
esac

printf '\033[0;32m✅ %s\033[0m\n' "$BIN"
