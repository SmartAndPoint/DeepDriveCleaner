#!/bin/bash
# The Gate's Linux leg on a Linux machine reached over ssh, in about a minute instead of a
# CI round (story "Linux scanner and rule catalog", DDC-007):
#
#   DDC_LINUX_HOST=<ssh host> scripts/native-linux.sh [nextest filter args...]
#
# Copies the working tree's tracked files to ~/$DDC_LINUX_DIR/src on the host (default
# ddc-native) and deletes the files there that git no longer tracks; untracked files are
# not copied, so `git add` a new test first. Then, in the
# rust:<toolchain>-bookworm image, runs what gate.yml's check job runs: clippy, nextest
# with profile ci, the doc tests and scripts/native-skips.sh linux. With filter
# arguments it runs only the matching tests, and no clippy, doc tests or skip compare.
#
# The container runs as the host user's uid, never root: root reads folders the tests
# make unreadable, and changes their results. The host needs docker (the user in the
# docker group) and rustup with the toolchain of rust-toolchain.toml and cargo-nextest
# in ~/.rustup and ~/.cargo/bin, which are mounted into the container.
set -euo pipefail
export LC_ALL=C

host="${DDC_LINUX_HOST:?set DDC_LINUX_HOST to the ssh host to run on}"
remote="${DDC_LINUX_DIR:-ddc-native}"
cd "$(git rev-parse --show-toplevel)"
toolchain="$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)"

if [ -n "$(git ls-files --others --exclude-standard)" ]; then
    echo "native-linux: untracked files are not copied; git add them to test them:" >&2
    git ls-files --others --exclude-standard | sed 's/^/  /' >&2
fi

# Tracked files of the working tree, then the remote files git no longer tracks. Both
# sides sort bytewise: a locale difference makes comm delete files that are tracked.
list="$(mktemp)"
trap 'rm -f "$list"' EXIT
git ls-files | sort > "$list"
git ls-files -z | COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs --null -T - -cf - |
    ssh "$host" "mkdir -p $remote/src && tar -xf - -C $remote/src"
ssh "$host" "cd $remote/src && find . -type f | sed 's|^\./||' | LC_ALL=C sort" | comm -13 "$list" - |
    ssh "$host" "cd $remote/src && xargs -r -d '\n' rm -f --"

filter=""
for a in "$@"; do filter+=" $(printf '%q' "$a")"; done
ssh "$host" bash -s <<EOF
set -euo pipefail
mkdir -p ~/$remote/cargo-home ~/$remote/target ~/$remote/home
# The image's password database does not know the host's uid. The core reads the home
# folder from it, as a runner's does, so the container gets one that names this user
# with the mounted home.
printf 'root:x:0:0::/root:/bin/sh\n%s:x:%s:%s::/work/home:/bin/sh\n' \
    "\$(id -un)" "\$(id -u)" "\$(id -g)" > ~/$remote/passwd
printf 'root:x:0:\n%s:x:%s:\n' "\$(id -gn)" "\$(id -g)" > ~/$remote/group
docker run --rm --user \$(id -u):\$(id -g) \
    -v ~/$remote/src:/work/src -v ~/$remote/target:/work/target \
    -v ~/$remote/cargo-home:/work/cargo-home -v ~/$remote/home:/work/home \
    -v ~/.rustup:/work/rustup -v ~/.cargo/bin:/work/cargo-bin:ro \
    -v ~/$remote/passwd:/etc/passwd:ro -v ~/$remote/group:/etc/group:ro \
    -e CARGO_HOME=/work/cargo-home -e CARGO_TARGET_DIR=/work/target -e HOME=/work/home \
    -e PATH=/work/cargo-bin:/usr/local/bin:/usr/bin:/bin -e RUSTUP_HOME=/work/rustup \
    -e NEXTEST_PROFILE=ci -e CARGO_TERM_COLOR=never -w /work/src rust:$toolchain-bookworm bash -c '
set -o pipefail
if [ -z "$filter" ]; then
    cargo clippy --workspace --all-targets --locked -- -D warnings || exit 1
fi
cargo nextest run --workspace --locked --features ddc-core/op-stats $filter 2>&1 | tee /work/target/native-tests.log; t=\$?
[ -n "$filter" ] && exit \$t
cargo test --workspace --doc --locked -q || exit 1
scripts/native-skips.sh linux /work/target/native-tests.log; s=\$?
exit \$(( t || s ))'
EOF
