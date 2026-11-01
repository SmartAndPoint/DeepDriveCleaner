#!/bin/bash
# The Gate's Windows leg on a Windows machine reached over ssh, instead of a CI round
# (story "Build matrix: every target built, tested and packaged from one tag", DDC-007):
#
#   DDC_WINDOWS_HOST=<ssh host> scripts/native-windows.sh [nextest filter args...]
#   DDC_WINDOWS_HOST=<ssh host> scripts/native-windows.sh --setup
#
# The host's ssh lands in Git for Windows' bash. Everything stays in one folder of the
# host's home, ~/$DDC_WINDOWS_DIR (default ddc-native, the folder
# scripts/windows-host-setup.ps1 made and excluded from Defender): src, target, cargo,
# rustup, home and tmp. Nothing goes on the host's PATH or anywhere else.
#
# Copies the working tree's tracked files to src and deletes the files there that git
# no longer tracks; untracked files are not copied, so `git add` a new test first. Then
# runs what gate.yml's check job runs on Windows: clippy, nextest with profile ci, the
# doc tests and scripts/native-skips.sh windows, for the job's target. With filter
# arguments it runs only the matching tests, and no clippy, doc tests or skip compare:
# end with a whole run before a push.
#
# The tests' home is the folder's: HOME, USERPROFILE, HOMEDRIVE and HOMEPATH, APPDATA and
# LOCALAPPDATA all point inside it, so a test that writes to its home never reaches the
# host's real one. TMP and TEMP sit beside that home, not inside it, as on the CI runner.
#
# The host needs what scripts/windows-host-setup.ps1 installs, run once by its owner as
# administrator. --setup then installs, inside the folder, rustup (--no-modify-path),
# the toolchain of rust-toolchain.toml and the cargo-nextest of .config/tools.mk; run it
# again after either pin changes. rustup's release adds an "Apps" entry under the user's
# registry key only when it edits PATH, its development branch always: --setup removes
# it when it points into the folder, so nothing is left outside.
set -euo pipefail
export LC_ALL=C

host="${DDC_WINDOWS_HOST:?set DDC_WINDOWS_HOST to the ssh host to run on}"
remote="${DDC_WINDOWS_DIR:-ddc-native}"
target=x86_64-pc-windows-msvc
cd "$(git rev-parse --show-toplevel)"

if [ "${1:-}" = --setup ]; then
    toolchain="$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)"
    nextest="$(sed -n 's/^NEXTEST_VERSION := //p' .config/tools.mk)"
    ssh "$host" bash -s <<EOF
set -euo pipefail
d=~/$remote
mkdir -p "\$d/cargo" "\$d/rustup" "\$d/tmp"
export CARGO_HOME="\$(cygpath -w "\$d/cargo")" RUSTUP_HOME="\$(cygpath -w "\$d/rustup")"
export TMP="\$(cygpath -w "\$d/tmp")" TEMP="\$(cygpath -w "\$d/tmp")"
if [ ! -x "\$d/cargo/bin/rustup.exe" ]; then
    curl -fsSL -o "\$d/tmp/rustup-init.exe" https://static.rust-lang.org/rustup/dist/$target/rustup-init.exe
    "\$d/tmp/rustup-init.exe" -y --no-modify-path --default-toolchain none --profile minimal
    rm -f "\$d/tmp/rustup-init.exe"
fi
# rustup-init's Apps entry, when it points into this folder.
key='HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Rustup'
if MSYS_NO_PATHCONV=1 reg query "\$key" /v UninstallString 2>/dev/null | grep -qF "\$CARGO_HOME"; then
    MSYS_NO_PATHCONV=1 reg delete "\$key" /f >/dev/null
fi
"\$d/cargo/bin/rustup" toolchain install $toolchain --profile minimal -c clippy -c rustfmt
if ! "\$d/cargo/bin/cargo-nextest" nextest --version 2>/dev/null | grep -qx "cargo-nextest $nextest.*"; then
    curl -fsSL https://get.nexte.st/$nextest/windows-tar | tar zxf - -C "\$d/cargo/bin"
fi
"\$d/cargo/bin/cargo-nextest" nextest --version | sed -n 1p
EOF
    exit
fi

if [ -n "$(git ls-files --others --exclude-standard)" ]; then
    echo "native-windows: untracked files are not copied; git add them to test them:" >&2
    git ls-files --others --exclude-standard | sed 's/^/  /' >&2
fi

# Tracked files of the working tree, then the remote files git no longer tracks. Both
# sides sort bytewise: a locale difference makes comm delete files that are tracked.
list="$(mktemp)"
trap 'rm -f "$list"' EXIT
git ls-files | sort > "$list"
git ls-files -z | COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs --null -T - -cf - |
    ssh "$host" "mkdir -p ~/$remote/src && tar -xf - -C ~/$remote/src"
ssh "$host" "cd ~/$remote/src && find . -type f | sed 's|^\./||' | LC_ALL=C sort" | comm -13 "$list" - |
    ssh "$host" "cd ~/$remote/src && xargs -r -d '\n' rm -f --"

filter=""
for a in "$@"; do filter+=" $(printf '%q' "$a")"; done
ssh "$host" bash -s <<EOF
set -uo pipefail
d=~/$remote
h="\$d/home"
mkdir -p "\$d/target" "\$h/AppData/Roaming" "\$h/AppData/Local" "\$d/tmp"
w() { cygpath -w "\$1"; }
export CARGO_HOME="\$(w "\$d/cargo")" RUSTUP_HOME="\$(w "\$d/rustup")" CARGO_TARGET_DIR="\$(w "\$d/target")"
export HOME="\$h" USERPROFILE="\$(w "\$h")" APPDATA="\$(w "\$h/AppData/Roaming")" LOCALAPPDATA="\$(w "\$h/AppData/Local")"
export HOMEDRIVE="\${USERPROFILE%%\\\\*}" HOMEPATH="\${USERPROFILE#*:}"
export TMP="\$(w "\$d/tmp")" TEMP="\$(w "\$d/tmp")"
export PATH="\$d/cargo/bin:\$PATH" NEXTEST_PROFILE=ci CARGO_TERM_COLOR=never
cd "\$d/src"
if [ -z "$filter" ]; then
    cargo clippy --workspace --all-targets --locked --target $target -- -D warnings || exit 1
fi
cargo nextest run --workspace --locked --features ddc-core/op-stats --target $target $filter 2>&1 | tee "\$d/target/native-tests.log"; t=\$?
[ -n "$filter" ] && exit \$t
cargo test --workspace --doc --locked --target $target -q || exit 1
scripts/native-skips.sh windows "\$d/target/native-tests.log"; s=\$?
exit \$(( t || s ))
EOF
