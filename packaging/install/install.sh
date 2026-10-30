#!/bin/sh
# Installs the DeepDriveCleaner command-line tool, `deepdrivecleaner`, and `ddc`, a
# symlink to it, into a folder of the user's (story "One-line install on macOS and
# Linux: ddc.sh/install.sh and cargo", DDC-007):
#
#   curl -fsSL https://raw.githubusercontent.com/SmartAndPoint/DeepDriveCleaner/main/packaging/install/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/SmartAndPoint/DeepDriveCleaner/main/packaging/install/install.sh | sh -s -- --prefix "$HOME/bin"
#
# (https://ddc.sh/install.sh will serve the same script.) The whole script is one
# function, main, called on its last line: a download cut short runs nothing.
#
# Options:
#   --version <v>   install release v<v> instead of the latest (a pre-release needs it)
#   --prefix <dir>  the folder to install into (default: ~/.local/bin)
#   --force         install even when another copy is found, or reinstall a current one
#   --uninstall     remove the two files this script installs from --prefix
#   --modify-path   add the folder to PATH in the shell's profile (one line); without
#                   it, the script only prints the line to add
#   --help          print this help
#
# Exit codes:
#   0  installed, already current, or uninstalled
#   1  anything else, such as a --prefix that cannot be written
#   2  unsupported platform or not released for it yet, or a bad option
#   3  verification failed: checksum, archive contents, signature or --version; also a
#      release without SHA256SUMS (releases from FIRST_VERIFIABLE on carry one)
#   4  an existing install was found (it is named); --force installs anyway. With
#      --uninstall: the background scan of `ddc schedule` still runs this binary, or
#      --prefix holds a `ddc` this script did not create
#   5  download failed, or there is no such release
#
# It never asks anything, never runs sudo, and writes nothing outside --prefix and a
# temporary folder, except the one profile line of --modify-path. A run that does not
# finish installs nothing: the new binary is checked first, then renamed over the old
# one in one step, so a running `ddc` and a `ddc schedule` LaunchAgent keep working.
#
# Verification: the archive's SHA-256 against the release's SHA256SUMS, the archive's
# listing, then on macOS the code signature, pinned to the SmartAndPoint Developer ID
# team, and finally `--version` of the new binary. Checksums prove integrity, not
# origin: on macOS the team pin covers a replaced archive plus a replaced SHA256SUMS.
# On Linux there is no signature in the archive, so a compromised release account
# could replace both: an accepted risk. Who wants more checks the provenance by hand:
# gh attestation verify <archive> --repo SmartAndPoint/DeepDriveCleaner
#
# DDC_INSTALL_BASE_URL replaces https://github.com/<REPO>/releases, for tests: an
# https URL, or http://127.0.0.1:<port> only. DDC_INSTALL_ALLOW_ADHOC=1 with that
# loopback URL is the test mode: an ad-hoc signature with the right identifier passes,
# and DDC_INSTALL_APP_PATH may name another app bundle than /Applications'.
set -eu

# The repository whose releases hold the archives.
REPO=SmartAndPoint/DeepDriveCleaner
# The CLI's code-signing identifier (scripts/build-dist.sh IDENTIFIER, publish-release.sh CLI_ID).
CLI_ID=com.smartandpoint.deepdrivecleaner.cli
# The Developer ID team that signs it (scripts/publish-release.sh TEAM_ID). A team id is
# in every binary it signs: not a secret.
TEAM_ID=8MSDJTBW2D
# The oldest macOS the CLI was run on (scripts/build-cli.sh MACOSX_DEPLOYMENT_TARGET).
MIN_MACOS=26.0
# The first release that carries SHA256SUMS: older ones have nothing to verify against.
FIRST_VERIFIABLE=0.8.0
# The LaunchAgent of `ddc schedule install` (crates/ddc-cli/src/launchd.rs LABEL).
LABEL=com.smartandpoint.deepdrivecleaner
# The app the Homebrew cask installs, or a copy dragged into ~/Applications; it
# carries the same CLI.
APP_PATH=/Applications/DeepDriveCleaner.app

say() { printf '%s\n' "$*"; }
note() { printf 'install.sh: %s\n' "$*" >&2; }
die() {
    status=$1
    shift
    printf 'install.sh: error: %s\n' "$*" >&2
    exit "$status"
}

usage() {
    cat <<'EOF'
Installs deepdrivecleaner and ddc (a symlink to it) from a DeepDriveCleaner release.

  curl -fsSL https://raw.githubusercontent.com/SmartAndPoint/DeepDriveCleaner/main/packaging/install/install.sh | sh -s -- [options]

  --version <v>   install release v<v> instead of the latest (a pre-release needs it)
  --prefix <dir>  the folder to install into (default: ~/.local/bin)
  --force         install even when another copy is found, or reinstall a current one
  --uninstall     remove the two files this script installs from --prefix
  --modify-path   add the folder to PATH in the shell's profile (one line)

Exit codes: 0 done, 1 other error, 2 unsupported platform or bad option,
3 verification failed, 4 existing install found, 5 download failed.
EOF
}

main() {
    # ── Options ──────────────────────────────────────────────────────────────────
    VERSION=""
    PREFIX=""
    FORCE=0
    UNINSTALL=0
    MODIFY_PATH=0
    while [ "$#" -gt 0 ]; do
        case $1 in
            --version)
                [ "$#" -ge 2 ] || die 2 "--version needs a version, such as 0.8.0"
                VERSION=$2
                shift 2
                ;;
            --version=*) VERSION=${1#--version=}; shift ;;
            --prefix)
                [ "$#" -ge 2 ] || die 2 "--prefix needs a folder"
                PREFIX=$2
                shift 2
                ;;
            --prefix=*) PREFIX=${1#--prefix=}; shift ;;
            --force) FORCE=1; shift ;;
            --uninstall) UNINSTALL=1; shift ;;
            --modify-path) MODIFY_PATH=1; shift ;;
            -h | --help) usage; exit 0 ;;
            *) die 2 "unknown option $1 (see --help)" ;;
        esac
    done

    VERSION=${VERSION#v}
    if [ -n "$VERSION" ]; then
        case $VERSION in
            *[!0-9A-Za-z.-]* | .* | -*) die 2 "--version $VERSION is not a version such as 0.8.0" ;;
            [0-9]*.[0-9]*.[0-9]*) ;;
            *) die 2 "--version $VERSION is not a version such as 0.8.0" ;;
        esac
    fi
    [ "$UNINSTALL" = 0 ] || [ "$MODIFY_PATH" = 0 ] || die 2 "--uninstall and --modify-path do not go together"

    : "${HOME:?HOME is not set}"
    [ -n "$PREFIX" ] || PREFIX=$HOME/.local/bin
    case $PREFIX in
        /*) ;;
        *) PREFIX=$(pwd -P)/$PREFIX ;;
    esac
    while [ "${#PREFIX}" -gt 1 ] && [ "${PREFIX%/}" != "$PREFIX" ]; do PREFIX=${PREFIX%/}; done

    # A path with every existing folder resolved (pwd -P), the missing rest appended. A
    # relative path is taken from the current folder; an empty one fails.
    canon_path() {
        cp_dir=$1
        cp_rest=""
        case $cp_dir in
            '') return 1 ;;
            /*) ;;
            *) cp_dir=$(pwd -P)/$cp_dir || return 1 ;;
        esac
        # Absolute from here on, so every step below drops one component and ends at /.
        while [ ! -d "$cp_dir" ]; do
            cp_rest=/${cp_dir##*/}$cp_rest
            cp_dir=${cp_dir%/*}
            [ -n "$cp_dir" ] || cp_dir=/
        done
        cp_dir=$(cd "$cp_dir" && pwd -P) || return 1
        [ "$cp_dir" != / ] || cp_dir=""
        printf '%s%s\n' "$cp_dir" "$cp_rest"
    }

    PREFIX=$(canon_path "$PREFIX") || die 1 "cannot resolve --prefix $PREFIX"
    [ -n "$PREFIX" ] || die 2 "--prefix / is not a folder for one tool"
    DEST=$PREFIX/deepdrivecleaner
    LINK=$PREFIX/ddc
    OS=$(uname -s)

    # `ddc` in --prefix is this script's: a symlink to deepdrivecleaner beside it.
    own_link() {
        [ -L "$LINK" ] || return 1
        ol_target=$(readlink "$LINK") || return 1
        case $ol_target in
            "$DEST" | deepdrivecleaner | ./deepdrivecleaner) return 0 ;;
        esac
        return 1
    }

    # ── Uninstall ────────────────────────────────────────────────────────────────
    if [ "$UNINSTALL" = 1 ]; then
        if [ "$OS" = Darwin ]; then
            plist=$HOME/Library/LaunchAgents/$LABEL.plist
            if [ -f "$plist" ]; then
                program=$(/usr/bin/plutil -extract ProgramArguments.0 raw -o - "$plist" 2>/dev/null) \
                    || die 4 "$plist cannot be read: check it with 'ddc schedule status' first"
                program_dir=$(canon_path "$(dirname "$program")") || program_dir=""
                if [ "$program_dir" = "$PREFIX" ]; then
                    die 4 "the background scan ($plist) runs $program: run 'ddc schedule uninstall' first, then this again"
                fi
            fi
        fi
        if { [ -e "$LINK" ] || [ -L "$LINK" ]; } && ! own_link; then
            die 4 "$LINK is not a link to $DEST, so this script did not create it: nothing removed"
        fi
        if [ -e "$DEST" ] && { [ -L "$DEST" ] || [ ! -f "$DEST" ]; }; then
            die 4 "$DEST is not a file this script installed: nothing removed"
        fi
        removed=0
        if [ -L "$LINK" ]; then rm -f "$LINK"; say "removed $LINK"; removed=1; fi
        if [ -f "$DEST" ]; then rm -f "$DEST"; say "removed $DEST"; removed=1; fi
        [ "$removed" = 1 ] || say "nothing to remove in $PREFIX"
        say "A PATH line added with --modify-path stays in your shell profile; remove it by hand."
        exit 0
    fi

    # ── Platform ─────────────────────────────────────────────────────────────────
    # a >= b for dotted numbers: 26.1 >= 26.0, 26 >= 26.0.
    version_ge() {
        awk -v a="$1" -v b="$2" 'BEGIN {
            n = split(a, x, "."); m = split(b, y, "."); if (m > n) n = m
            for (i = 1; i <= n; i++) { p = x[i] + 0; q = y[i] + 0; if (p > q) exit 0; if (p < q) exit 1 }
            exit 0 }'
    }

    ARCH=$(uname -m)
    case $OS in
        Darwin)
            if [ "$ARCH" = x86_64 ]; then
                # A Rosetta shell on Apple Silicon reports x86_64: install the native build.
                if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
                    ARCH=arm64
                else
                    die 2 "Intel Macs are not supported: DeepDriveCleaner is built for Apple Silicon only"
                fi
            fi
            [ "$ARCH" = arm64 ] || die 2 "unsupported Mac architecture $ARCH"
            MACOS=$(sw_vers -productVersion 2>/dev/null) || die 2 "cannot read the macOS version (sw_vers)"
            version_ge "$MACOS" "$MIN_MACOS" || die 2 "macOS $MACOS is not supported: the command-line tool needs macOS $MIN_MACOS or later"
            TARGET=aarch64-apple-darwin
            ;;
        Linux)
            case $ARCH in
                x86_64 | amd64) TARGET=x86_64-unknown-linux-musl ;;
                aarch64 | arm64) TARGET=aarch64-unknown-linux-musl ;;
                *) die 2 "unsupported Linux architecture $ARCH: x86_64 and aarch64 only" ;;
            esac
            ;;
        *) die 2 "unsupported system $OS: macOS and Linux only" ;;
    esac

    # ── Download host and test mode ──────────────────────────────────────────────
    BASE=https://github.com/$REPO/releases
    SECURE=1
    TEST_MODE=0
    loopback() {
        case $1 in *@*) return 1 ;; http://127.0.0.1:*) ;; *) return 1 ;; esac
        lb_rest=${1#http://127.0.0.1:}
        lb_port=${lb_rest%%/*}
        case $lb_port in '' | *[!0-9]*) return 1 ;; esac
        return 0
    }
    if [ -n "${DDC_INSTALL_BASE_URL:-}" ]; then
        BASE=${DDC_INSTALL_BASE_URL%/}
        case $BASE in
            https://*) ;;
            *) loopback "$BASE" || die 2 "DDC_INSTALL_BASE_URL must be an https URL or http://127.0.0.1:<port>" ;;
        esac
        case $BASE in http://*) SECURE=0 ;; esac
    fi
    if [ "${DDC_INSTALL_ALLOW_ADHOC:-}" = 1 ]; then
        [ "$SECURE" = 0 ] || die 2 "DDC_INSTALL_ALLOW_ADHOC is for tests: it needs DDC_INSTALL_BASE_URL=http://127.0.0.1:<port>"
        TEST_MODE=1
        note "WARNING: test mode: ad-hoc signatures accepted, downloads from $BASE"
        APP_PATH=${DDC_INSTALL_APP_PATH:-$APP_PATH}
    elif [ -n "${DDC_INSTALL_APP_PATH:-}" ]; then
        die 2 "DDC_INSTALL_APP_PATH is for tests: it needs DDC_INSTALL_ALLOW_ADHOC=1"
    fi

    # ── Tools ────────────────────────────────────────────────────────────────────
    if command -v curl >/dev/null 2>&1; then
        DL=curl
    elif command -v wget >/dev/null 2>&1 && wget --version 2>/dev/null | grep -q 'GNU Wget'; then
        DL=wget
    else
        die 2 "no downloader: install curl or GNU wget (BusyBox wget may not verify certificates)"
    fi
    if command -v sha256sum >/dev/null 2>&1; then
        sha256() { sha256sum "$1" | awk '{ print $1 }'; }
    elif command -v shasum >/dev/null 2>&1; then
        sha256() { shasum -a 256 "$1" | awk '{ print $1 }'; }
    else
        die 3 "no sha256sum or shasum: nothing to verify the download with"
    fi

    # fetch <url> <file>: the whole file, or a failure.
    fetch() {
        if [ "$DL" = curl ]; then
            if [ "$SECURE" = 1 ]; then
                curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSL --retry 3 -o "$2" "$1"
            else
                curl -fsSL --retry 3 -o "$2" "$1"
            fi
        elif [ "$SECURE" = 1 ]; then
            wget --https-only -q -O "$2" "$1"
        else
            wget -q -O "$2" "$1"
        fi
    }

    # exists <url>: the first byte of it can be downloaded.
    exists() {
        if [ "$DL" = curl ]; then
            if [ "$SECURE" = 1 ]; then
                curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSL -r 0-0 -o /dev/null "$1" 2>/dev/null
            else
                curl -fsSL -r 0-0 -o /dev/null "$1" 2>/dev/null
            fi
        elif [ "$SECURE" = 1 ]; then
            wget --https-only -q --spider "$1" 2>/dev/null
        else
            wget -q --spider "$1" 2>/dev/null
        fi
    }

    # The latest release, from where <base>/latest redirects (…/tag/v<version>): no API
    # token, no rate limit. It skips pre-releases.
    latest() {
        if [ "$DL" = curl ]; then
            if [ "$SECURE" = 1 ]; then
                curl --proto '=https' --tlsv1.2 -fsS -o /dev/null -w '%{redirect_url}' "$BASE/latest" 2>/dev/null || true
            else
                curl -fsS -o /dev/null -w '%{redirect_url}' "$BASE/latest" 2>/dev/null || true
            fi
        else
            # -S prints the response's headers; the first Location is the redirect.
            if [ "$SECURE" = 1 ]; then
                { wget --https-only -S --max-redirect=0 -O /dev/null "$BASE/latest" 2>&1 || true; }
            else
                { wget -S --max-redirect=0 -O /dev/null "$BASE/latest" 2>&1 || true; }
            fi | tr -d '\r' | sed -n 's/^ *Location: *\([^ ]*\).*/\1/p' | head -n 1
        fi
    }

    # ── Version ──────────────────────────────────────────────────────────────────
    if [ -z "$VERSION" ]; then
        location=$(latest)
        VERSION=${location##*/}
        VERSION=${VERSION#v}
        case $VERSION in
            *[!0-9A-Za-z.-]* | '') die 5 "cannot find the latest release at $BASE/latest: try again, or pass --version" ;;
            [0-9]*.[0-9]*.[0-9]*) ;;
            *) die 5 "cannot find the latest release at $BASE/latest: try again, or pass --version" ;;
        esac
    fi
    ARCHIVE=deepdrivecleaner-$VERSION-$TARGET.tar.gz

    # ── PATH ─────────────────────────────────────────────────────────────────────
    # Says how to put --prefix on PATH, or, with --modify-path, adds one line to the
    # shell's profile, once.
    path_hint() {
        on_path=0
        while IFS= read -r dir; do
            # A relative entry (bin, node_modules/.bin) depends on the folder a shell is
            # in: never the install's.
            case $dir in /*) ;; *) continue ;; esac
            if [ "$(canon_path "$dir" 2>/dev/null || true)" = "$PREFIX" ]; then on_path=1; break; fi
        done <<EOF
$(printf '%s\n' "$PATH" | tr ':' '\n')
EOF
        if [ "$on_path" = 0 ]; then
            case ${SHELL:-} in
                */zsh) PROFILE=${ZDOTDIR:-$HOME}/.zshrc ;;
                */bash) if [ "$OS" = Darwin ]; then PROFILE=$HOME/.bash_profile; else PROFILE=$HOME/.bashrc; fi ;;
                *) PROFILE=$HOME/.profile ;;
            esac
            LINE="export PATH=\"$PREFIX:\$PATH\""
            if [ "$MODIFY_PATH" = 1 ]; then
                case $PREFIX in
                    *[\"\$\`\\]*) die 1 "installed, but $PREFIX holds a character that cannot go into $PROFILE: add it to PATH by hand" ;;
                esac
                MARKED="$LINE # added by the DeepDriveCleaner installer"
                if [ -f "$PROFILE" ] && grep -qxF "$MARKED" "$PROFILE"; then
                    say "$PROFILE already adds $PREFIX to PATH; open a new shell, or run:"
                else
                    if [ -s "$PROFILE" ] && [ -n "$(tail -c 1 "$PROFILE")" ]; then printf '\n' >>"$PROFILE" || die 1 "installed, but cannot write $PROFILE: add the line below by hand"; fi
                    printf '%s\n' "$MARKED" >>"$PROFILE" || die 1 "installed, but cannot write $PROFILE: add the line below by hand"
                    say "Added $PREFIX to PATH in $PROFILE; open a new shell, or run:"
                fi
            else
                say "$PREFIX is not on your PATH. Add this line to $PROFILE (or run the script with --modify-path):"
            fi
            say "  $LINE"
        fi
    }

    # ── Another copy ─────────────────────────────────────────────────────────────
    # How to update or remove what is at $1.
    describe() {
        d_target=""
        [ ! -L "$1" ] || d_target=$(readlink "$1" 2>/dev/null || true)
        case "$1 $d_target" in
            *.app/Contents/* | *Caskroom* | */homebrew/* | */linuxbrew/*)
                say "the Homebrew cask (update: brew upgrade --cask deepdrivecleaner; remove: brew uninstall --cask deepdrivecleaner)" ;;
            */.cargo/bin/*)
                say "cargo install or make install (update: cargo install --git https://github.com/$REPO --locked ddc-cli; remove: cargo uninstall ddc-cli and rm $(dirname "$1")/ddc)" ;;
            *)
                say "another copy (if this script installed it, run it again with --prefix $(dirname "$1"); otherwise remove it)" ;;
        esac
    }

    if [ "$FORCE" = 0 ]; then
        other=""
        while IFS= read -r dir; do
            # Absolute entries only, as in path_hint.
            case $dir in /*) ;; *) continue ;; esac
            for name in deepdrivecleaner ddc; do
                if [ -e "$dir/$name" ] || [ -L "$dir/$name" ]; then
                    dir_canon=$(canon_path "$dir") || continue
                    [ "$dir_canon" = "$PREFIX" ] && continue
                    other=$dir/$name
                    break 2
                fi
            done
        done <<EOF
$(printf '%s\n' "$PATH" | tr ':' '\n')
EOF
        if [ -n "$other" ]; then
            die 4 "$other is already installed, by $(describe "$other"). Two copies mean two ddc on PATH and two background schedulers; --force installs anyway"
        fi
        if [ "$OS" = Darwin ]; then
            for app in "$APP_PATH" "$HOME/Applications/DeepDriveCleaner.app"; do
                [ -e "$app" ] || continue
                die 4 "$app is installed and carries the same tool (update: brew upgrade --cask deepdrivecleaner, or the app's own updater; remove the app to use this install). --force installs anyway"
            done
        fi
        if { [ -e "$LINK" ] || [ -L "$LINK" ]; } && ! own_link; then
            die 4 "$LINK exists and is not a link to $DEST: this script will not replace it. --force replaces it"
        fi
    fi

    OLD=""
    if [ -f "$DEST" ] && [ -x "$DEST" ]; then
        OLD=$("$DEST" --version </dev/null 2>/dev/null | sed -n 's/^deepdrivecleaner //p' | head -n 1)
    fi
    if [ "$FORCE" = 0 ] && [ "$OLD" = "$VERSION" ] && own_link; then
        say "deepdrivecleaner $VERSION is already current in $PREFIX"
        path_hint
        exit 0
    fi

    # rename(2) onto a folder, or a link to one, would move the binary into it.
    if [ -d "$DEST" ]; then die 1 "$DEST is a folder: remove it first"; fi

    # ── Download and verify ──────────────────────────────────────────────────────
    TMP=""
    STAGED=""
    STAGED_LINK=""
    cleanup() {
        [ -z "$STAGED" ] || rm -f "$STAGED"
        [ -z "$STAGED_LINK" ] || rm -f "$STAGED_LINK"
        [ -z "$TMP" ] || rm -rf "$TMP"
    }
    trap cleanup EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
    TMP=$(mktemp -d "${TMPDIR:-/tmp}/ddc-install.XXXXXX") || die 1 "cannot create a temporary folder"

    say "Installing deepdrivecleaner $VERSION for $TARGET into $PREFIX"
    if ! fetch "$BASE/download/v$VERSION/SHA256SUMS" "$TMP/SHA256SUMS" 2>/dev/null; then
        if exists "$BASE/download/v$VERSION/deepdrivecleaner-$VERSION-aarch64-apple-darwin.tar.gz"; then
            [ "$OS" = Darwin ] || die 2 "DeepDriveCleaner is not released for Linux yet (v$VERSION has no Linux archive)"
            die 3 "v$VERSION has no SHA256SUMS to verify against: releases from $FIRST_VERIFIABLE on carry one. Pick a later --version, or download it by hand (README, Install)"
        fi
        die 5 "no release v$VERSION at $BASE (or the download failed): check the version and the network"
    fi
    EXPECTED=$(awk -v n="$ARCHIVE" '$2 == n { print $1 }' "$TMP/SHA256SUMS")
    if [ -z "$EXPECTED" ]; then
        [ "$OS" = Darwin ] || die 2 "DeepDriveCleaner is not released for Linux yet (v$VERSION has no $TARGET archive)"
        die 3 "SHA256SUMS of v$VERSION lists no $ARCHIVE"
    fi
    case $EXPECTED in
        *[!0-9a-f]*) die 3 "SHA256SUMS of v$VERSION holds a malformed line for $ARCHIVE" ;;
    esac
    [ "${#EXPECTED}" = 64 ] || die 3 "SHA256SUMS of v$VERSION holds a malformed line for $ARCHIVE"

    fetch "$BASE/download/v$VERSION/$ARCHIVE" "$TMP/$ARCHIVE" 2>/dev/null || die 5 "cannot download $ARCHIVE from $BASE"
    ACTUAL=$(sha256 "$TMP/$ARCHIVE")
    [ "$ACTUAL" = "$EXPECTED" ] || die 3 "$ARCHIVE does not match SHA256SUMS (got $ACTUAL, expected $EXPECTED): nothing installed"

    # Flat: the binary and the three documents, nothing else; only the binary is unpacked.
    LISTING=$(tar -tzf "$TMP/$ARCHIVE" 2>/dev/null | LC_ALL=C sort | tr '\n' ' ') || die 3 "$ARCHIVE cannot be read"
    [ "$LISTING" = "DISCLAIMER.md LICENSE.md README.md deepdrivecleaner " ] || die 3 "$ARCHIVE holds $LISTING, not the four expected files"
    mkdir "$TMP/x"
    tar -xzf "$TMP/$ARCHIVE" -C "$TMP/x" deepdrivecleaner 2>/dev/null || die 3 "cannot unpack deepdrivecleaner from $ARCHIVE"
    NEW=$TMP/x/deepdrivecleaner
    { [ -f "$NEW" ] && [ ! -L "$NEW" ]; } || die 3 "deepdrivecleaner in $ARCHIVE is not a regular file"

    if [ "$OS" = Darwin ]; then
        # /usr/bin/codesign ships with macOS; without it the check is refused, not skipped.
        [ -x /usr/bin/codesign ] || die 3 "/usr/bin/codesign is missing: the signature cannot be checked"
        if [ "$TEST_MODE" = 1 ]; then
            REQUIREMENT="identifier \"$CLI_ID\""
        else
            # Apple's anchor, a Developer ID Application certificate, this team, this identifier.
            REQUIREMENT="anchor apple generic and identifier \"$CLI_ID\" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"$TEAM_ID\""
        fi
        /usr/bin/codesign --verify --strict -R="$REQUIREMENT" "$NEW" 2>/dev/null \
            || die 3 "deepdrivecleaner in $ARCHIVE is not signed as $CLI_ID by the SmartAndPoint Developer ID (team $TEAM_ID): nothing installed"
    fi
    chmod 755 "$NEW"
    REPORTED=$("$NEW" --version </dev/null 2>/dev/null) || REPORTED=""
    [ "$REPORTED" = "deepdrivecleaner $VERSION" ] || die 3 "the downloaded binary reports '$REPORTED', not 'deepdrivecleaner $VERSION'"

    # ── Install ──────────────────────────────────────────────────────────────────
    # Only now is --prefix touched. Same folder, so the rename replaces the file in one
    # step: the path stays, and a process running the old binary keeps its copy.
    mkdir -p "$PREFIX" || die 1 "cannot create $PREFIX"
    STAGED=$PREFIX/.deepdrivecleaner.new.$$
    cp "$NEW" "$STAGED" || die 1 "cannot write to $PREFIX"
    chmod 755 "$STAGED"
    mv -f "$STAGED" "$DEST" || die 1 "cannot replace $DEST"
    STAGED=""
    if ! own_link || [ "$(readlink "$LINK")" != "$DEST" ]; then
        # Only a foreign ddc (--force) is not replaced in one step; a folder never is.
        if [ -d "$LINK" ] && [ ! -L "$LINK" ]; then die 1 "$LINK is a folder: remove it first"; fi
        own_link || rm -f "$LINK"
        STAGED_LINK=$PREFIX/.ddc.new.$$
        ln -s "$DEST" "$STAGED_LINK" || die 1 "cannot create the ddc link in $PREFIX"
        mv -f "$STAGED_LINK" "$LINK" || die 1 "cannot replace $LINK"
        STAGED_LINK=""
    fi

    if [ -n "$OLD" ] && [ "$OLD" != "$VERSION" ]; then
        say "deepdrivecleaner $OLD -> $VERSION"
    else
        say "deepdrivecleaner $VERSION"
    fi
    say "  $DEST"
    say "  $LINK -> $DEST"
    if [ "$OS" = Darwin ]; then
        say "Full Disk Access granted to $DEST stays across upgrades; moving the binary loses it."
    fi
    path_hint
}

main "$@"
