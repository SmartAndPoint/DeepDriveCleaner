#!/bin/bash
# The Gate's Linux leg on a Linux machine reached over ssh, in about a minute instead of a
# CI round (story "Linux scanner and rule catalog", DDC-007):
#
#   DDC_LINUX_HOST=<ssh host> scripts/native-linux.sh [nextest filter args...]
#   DDC_LINUX_HOST=<ssh host> scripts/native-linux.sh --setup
#   DDC_LINUX_HOST=<ssh host> scripts/native-linux.sh --make <target> [VAR=value...]
#   DDC_LINUX_HOST=<windows host> DDC_LINUX_WSL=<distribution> scripts/native-linux.sh ...
#
# With DDC_LINUX_WSL the host is a Windows machine whose ssh lands in Git bash, and every
# command runs in that WSL distribution through wsl.exe, from its home: the same folder,
# docker and tests as on a Linux machine.
#
# Everything stays in one folder of the host's home, ~/$DDC_LINUX_DIR (default
# ddc-native): src, target, cargo, rustup, cargo-home and home. The host's own ~/.rustup
# and ~/.cargo are never used or changed.
#
# Copies the working tree's tracked files to src and deletes the files there that git no longer tracks; untracked files are
# not copied, so `git add` a new test first. Then, in the
# rust:<toolchain>-bookworm image, runs what gate.yml's check job runs: clippy, nextest
# with profile ci, the doc tests and scripts/native-skips.sh linux. With filter
# arguments it runs only the matching tests, and no clippy, doc tests or skip compare.
# --make runs that make target in the same container instead (`load`, `load-baseline`,
# `scaling`: the benchmarks), then copies the host's target/load back to the local
# target/native-linux/load. What it measures describes the host: it stays out of both
# repositories. DDC_LINUX_LIMITS=<local file> judges a load run against that file (a
# load-limits.tsv this host recorded with `--make load-baseline`) instead of the
# repository's, which are a hosted runner's: it replaces the host's copy of
# bench/load-limits-linux.tsv, which the next run copies back.
#
# The container runs as the host user's uid, never root: root reads folders the tests
# make unreadable, and changes their results. The host needs docker (the user in the
# docker group) and curl. --setup installs, inside the folder, rustup (--no-modify-path),
# the toolchain of rust-toolchain.toml and the cargo-nextest of .config/tools.mk, for the
# host's architecture; run it again after either pin changes. The container mounts them.
# It builds without incremental data, as CI does, and says when target passes 40 GiB:
# what to remove there is the host owner's call.
set -euo pipefail
export LC_ALL=C

host="${DDC_LINUX_HOST:?set DDC_LINUX_HOST to the ssh host to run on}"
remote="${DDC_LINUX_DIR:-ddc-native}"
cd "$(git rev-parse --show-toplevel)"
toolchain="$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)"

# Runs `$1` on the host, or with no argument the script on stdin.
on_host() {
    local wsl
    if [ -z "${DDC_LINUX_WSL:-}" ]; then
        if [ "$#" -eq 0 ]; then ssh "$host" bash -s; else ssh "$host" "$1"; fi
        return
    fi
    # Git bash would turn ~ and /paths into Windows paths: no conversion, ~ quoted.
    wsl="MSYS_NO_PATHCONV=1 wsl.exe -d $(printf %q "$DDC_LINUX_WSL") --cd '~' -e bash"
    if [ "$#" -eq 0 ]; then ssh "$host" "$wsl -s"; else ssh "$host" "$wsl -c $(printf %q "$1")"; fi
}

if [ "${1:-}" = --setup ]; then
    nextest="$(sed -n 's/^NEXTEST_VERSION := //p' .config/tools.mk)"
    on_host <<EOF
set -euo pipefail
d=~/$remote
mkdir -p "\$d/cargo/bin" "\$d/rustup"
export CARGO_HOME="\$d/cargo" RUSTUP_HOME="\$d/rustup"
if [ ! -x "\$d/cargo/bin/rustup" ]; then
    curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path --default-toolchain none --profile minimal
fi
"\$d/cargo/bin/rustup" toolchain install $toolchain --profile minimal -c clippy -c rustfmt
case "\$(uname -m)" in aarch64) build=linux-arm ;; *) build=linux ;; esac
if ! "\$d/cargo/bin/cargo-nextest" nextest --version 2>/dev/null | grep -qx "cargo-nextest $nextest.*"; then
    curl -fsSL "https://get.nexte.st/$nextest/\$build" | tar zxf - -C "\$d/cargo/bin"
fi
"\$d/cargo/bin/cargo-nextest" nextest --version | sed -n 1p
EOF
    exit
fi

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
    on_host "mkdir -p $remote/src && tar -xf - -C $remote/src"
on_host "cd $remote/src && find . -type f | sed 's|^\./||' | LC_ALL=C sort" | comm -13 "$list" - |
    on_host "cd $remote/src && xargs -r -d '\n' rm -f --"

if [ -n "${DDC_LINUX_LIMITS:-}" ]; then
    on_host "cat > $remote/src/bench/load-limits-linux.tsv" <"$DDC_LINUX_LIMITS"
fi

filter=""
make_args=""
if [ "${1:-}" = --make ]; then
    shift
    [ "$#" -gt 0 ] || { echo "native-linux: --make needs a target" >&2; exit 2; }
    for a in "$@"; do make_args+=" $(printf '%q' "$a")"; done
else
    for a in "$@"; do filter+=" $(printf '%q' "$a")"; done
fi
if on_host <<EOF
set -euo pipefail
test -x ~/$remote/cargo/bin/cargo-nextest || { echo "native-linux: run --setup first" >&2; exit 1; }
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
    -v ~/$remote/rustup:/work/rustup -v ~/$remote/cargo/bin:/work/cargo-bin:ro \
    -v ~/$remote/passwd:/etc/passwd:ro -v ~/$remote/group:/etc/group:ro \
    -e CARGO_HOME=/work/cargo-home -e CARGO_TARGET_DIR=/work/target -e HOME=/work/home \
    -e PATH=/work/cargo-bin:/usr/local/bin:/usr/bin:/bin -e RUSTUP_HOME=/work/rustup \
    -e NEXTEST_PROFILE=ci -e CARGO_TERM_COLOR=never -e CARGO_INCREMENTAL=0 -w /work/src rust:$toolchain-bookworm bash -c '
set -o pipefail
if [ -n "$make_args" ]; then
    # The Makefile reads target/ beside the sources: the build is in the mounted one.
    ln -sfn /work/target target
    make $make_args
    exit
fi
if [ -z "$filter" ]; then
    cargo clippy --workspace --all-targets --locked -- -D warnings || exit 1
fi
cargo nextest run --workspace --locked --features ddc-core/op-stats $filter 2>&1 | tee /work/target/native-tests.log; t=\$?
[ -n "$filter" ] && exit \$t
cargo test --workspace --doc --locked -q || exit 1
scripts/native-skips.sh linux /work/target/native-tests.log; s=\$?
exit \$(( t || s ))' || status=\$?
size=\$(du -s --block-size=1G ~/$remote/target | cut -f1)
if [ "\$size" -gt 40 ]; then
    echo "native-linux: ~/$remote/target holds \$size GiB; ask the host's owner before removing anything" >&2
fi
exit \${status:-0}
EOF
then status=0; else status=$?; fi
if [ -n "$make_args" ]; then
    mkdir -p target/native-linux
    on_host "test -d $remote/target/load && tar -cf - -C $remote/target load" | tar -xf - -C target/native-linux &&
        echo "native-linux: the measurements are in target/native-linux/load" >&2
fi
exit "$status"
