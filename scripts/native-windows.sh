#!/bin/bash
# The Gate's Windows leg on a Windows machine reached over ssh, instead of a CI round
# (story "Build matrix: every target built, tested and packaged from one tag", DDC-007):
#
#   DDC_WINDOWS_HOST=<ssh host> scripts/native-windows.sh [nextest filter args...]
#
# The host's ssh lands in Git for Windows' bash. Everything stays in one folder of the
# host's home, ~/$DDC_WINDOWS_DIR (default ddc-native): src, target, cargo, rustup, home
# and tmp. Nothing is put on the host's PATH, and no other folder is written.
#
# Copies the working tree's tracked files to src and deletes the files there that git
# no longer tracks. Then runs what gate.yml's check job runs on Windows: clippy, nextest
# with profile ci, the doc tests and scripts/native-skips.sh windows. With filter
# arguments it runs only the matching tests, and no clippy, doc tests or skip compare.
#
# The host needs Visual Studio Build Tools with the C++ workload, and rustup with the
# toolchain of rust-toolchain.toml and cargo-nextest installed with CARGO_HOME and
# RUSTUP_HOME in that folder (rustup-init --no-modify-path).
set -euo pipefail
export LC_ALL=C

host="${DDC_WINDOWS_HOST:?set DDC_WINDOWS_HOST to the ssh host to run on}"
remote="${DDC_WINDOWS_DIR:-ddc-native}"
cd "$(git rev-parse --show-toplevel)"

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
mkdir -p "\$d/target" "\$d/home" "\$d/tmp"
w() { cygpath -w "\$1"; }
# TMP and TEMP sit beside the test HOME, not inside it, as on the CI runner.
export CARGO_HOME="\$(w "\$d/cargo")" RUSTUP_HOME="\$(w "\$d/rustup")" CARGO_TARGET_DIR="\$(w "\$d/target")"
export HOME="\$d/home" TMP="\$(w "\$d/tmp")" TEMP="\$(w "\$d/tmp")"
export PATH="\$d/cargo/bin:\$PATH" NEXTEST_PROFILE=ci CARGO_TERM_COLOR=never
cd "\$d/src"
if [ -z "$filter" ]; then
    cargo clippy --workspace --all-targets --locked -- -D warnings || exit 1
fi
cargo nextest run --workspace --locked $filter 2>&1 | tee "\$d/target/native-tests.log"; t=\$?
[ -n "$filter" ] && exit \$t
cargo test --workspace --doc --locked -q || exit 1
scripts/native-skips.sh windows "\$d/target/native-tests.log"; s=\$?
exit \$(( t || s ))
EOF
