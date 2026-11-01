#!/bin/bash
# A synthetic home full of coding-agent ghosts, for demos and screenshots (story "Launch
# demo assets", DDC-009). Nothing here comes from a real machine:
#
#   bench/demo/agent-ghosts.sh <dir>        build <dir>/home and <dir>/tmp, start the live session
#   bench/demo/agent-ghosts.sh <dir> stop   end the live session (its record then reads as gone)
#
# Then, in the same shell:
#
#   export HOME=<dir>/home CLAUDE_CODE_TMPDIR=<dir>/tmp
#   ddc disk agents
#
# It builds Claude Code, Codex, Cursor and opencode data: three Claude Code projects with
# five sessions, one of them live (a record naming a `sleep` this script starts), their
# checkpoints and their temporary folders under <dir>/tmp/claude-<uid>, two Codex
# worktrees with a build output each, and the caches of the other two. Files are written
# with real bytes, not sparse, because the scanner counts allocated blocks: about 600 MiB
# in all.
#
# It writes only inside <dir>, refuses a <dir> inside the real home, and rebuilds from
# scratch on every run: it removes <dir>/home and <dir>/tmp only when <dir> carries the
# marker it leaves there.
set -euo pipefail
export LC_ALL=C

die() { echo "agent-ghosts: $*" >&2; exit 1; }

[ $# -ge 1 ] || die "usage: $0 <dir> [stop]"
REAL_HOME=$(cd ~ && pwd -P)
inside_home() {
    case "$1/" in
        "$REAL_HOME"/* | "${HOME:-$REAL_HOME}"/*) return 0 ;;
    esac
    return 1
}
# Refused before anything is made: a refused <dir> leaves nothing behind. Decided on the
# path as given, made absolute, and on its nearest existing parent with symlinks resolved;
# a `..` would let the part not yet made climb back, so it is refused too.
case "/$1/" in
    */../*) die "$1: give <dir> without '..'" ;;
esac
case "$1" in
    /*) GIVEN=$1 ;;
    *) GIVEN="$PWD/$1" ;;
esac
PARENT=$GIVEN
while [ ! -d "$PARENT" ]; do PARENT=$(dirname "$PARENT"); done
PARENT=$(cd "$PARENT" && pwd -P)
if inside_home "$GIVEN" || inside_home "$PARENT"; then
    die "$GIVEN is inside the real home; pick a folder outside it"
fi
[ "${2:-}" = stop ] && [ ! -d "$GIVEN" ] && die "$GIVEN was not built by this script"
mkdir -p "$GIVEN"
DIR=$(cd "$GIVEN" && pwd -P)
inside_home "$DIR" && die "$DIR is inside the real home; pick a folder outside it"
MARK="$DIR/.agent-ghosts"
PIDFILE="$DIR/live.pid"

stop_live() {
    if [ -f "$PIDFILE" ]; then
        kill "$(cat "$PIDFILE")" 2>/dev/null || true
        rm -f "$PIDFILE"
    fi
}

if [ "${2:-}" = stop ]; then
    [ -f "$MARK" ] || die "$DIR was not built by this script"
    stop_live
    exit 0
fi

if [ -n "$(ls -A "$DIR")" ] && [ ! -f "$MARK" ]; then
    die "$DIR is not empty and was not built by this script"
fi
stop_live
rm -rf "$DIR/home" "$DIR/tmp"
touch "$MARK"

H="$DIR/home"
T="$DIR/tmp/claude-$(id -u)"

# blob <file> <MiB>: a file of real (allocated) bytes.
blob() {
    mkdir -p "$(dirname "$1")"
    dd if=/dev/zero of="$1" bs=1048576 count="$2" 2>/dev/null
}

# age <path> <days>: back-date a file or a whole tree.
age() {
    local stamp
    stamp=$(date -v-"$2"d +%Y%m%d%H%M 2>/dev/null || date -d "-$2 days" +%Y%m%d%H%M)
    find "$1" -exec touch -h -t "$stamp" {} +
}

# session <project slug> <id> <transcript MiB> <checkpoints MiB> <temp MiB>
session() {
    local slug=$1 id=$2
    blob "$H/.claude/projects/$slug/$id.jsonl" "$3"
    blob "$H/.claude/projects/$slug/$id/subagents/agent-1.jsonl" 1
    blob "$H/.claude/file-history/$id/0001" "$4"
    blob "$T/$slug/$id/scratchpad/build.log" "$(( $5 * 3 / 4 ))"
    blob "$T/$slug/$id/tasks/task-1.output" "$(( $5 / 4 ))"
    mkdir -p "$T/$slug/$id/images"
}

LIVE=7d3f9a2e-41c8-4b6e-9f0a-2c5e8b1d6a43
WEB=-Users-demo-code-webapp
API=-Users-demo-code-api
OLD=-Users-demo-code-old-prototype

session "$WEB" "$LIVE" 24 6 40
session "$WEB" 3a91c0de-5f27-4d18-b6e4-90c1f2a7d8b5 18 12 160
session "$API" 9e4b7c21-0d36-4a5f-8e19-b7c2d4f6a081 31 8 120
session "$API" c52e8f17-6a94-4b03-a1d7-3e8f0b9c2d64 9 4 20
session "$OLD" 0f6a2d93-b8e1-47c5-9d20-5a7e1c3b4f86 14 2 60
blob "$T/bash-edit-diff/edit-1.diff" 3
age "$H/.claude/projects/$WEB/3a91c0de-5f27-4d18-b6e4-90c1f2a7d8b5.jsonl" 2
age "$H/.claude/projects/$WEB/3a91c0de-5f27-4d18-b6e4-90c1f2a7d8b5" 2
age "$H/.claude/projects/$API" 2
age "$H/.claude/projects/$API/c52e8f17-6a94-4b03-a1d7-3e8f0b9c2d64.jsonl" 6
age "$H/.claude/projects/$OLD" 19
age "$H/.claude/file-history" 2
age "$T" 2

# Codex: two threads, a worktree each with its build output, a downloaded runtime.
blob "$H/.codex/sessions/2026/10/29/rollout-2026-10-29T18-30-00-03f4d63b-02b3-427b-ac15-6a259a4fa701.jsonl" 12
blob "$H/.codex/sessions/2026/10/12/rollout-2026-10-12T10-00-00-10f224b0-27e9-4a80-aa48-6b7d41023be4.jsonl" 8
blob "$H/.codex/worktrees/3f2a/webapp/node_modules/.package-lock.bin" 70
blob "$H/.codex/worktrees/9c1d/api/target/debug/api" 45
blob "$H/.cache/codex-runtimes/node/node" 30
mkdir -p "$H/.codex/thread-writer-locks"
age "$H/.codex" 3

# Cursor and opencode.
blob "$H/.cursor/extensions/demo.vsix" 12
blob "$H/.local/share/opencode/storage/session/demo.bin" 18
blob "$H/.cache/opencode/models/demo.bin" 25

# The live session: a record naming a process that runs now, written after it started.
sleep 86400 &
echo $! > "$PIDFILE"
disown
mkdir -p "$H/.claude/sessions"
printf '{"pid":%d,"sessionId":"%s","cwd":"/Users/demo/code/webapp","startedAt":%d}\n' \
    "$(cat "$PIDFILE")" "$LIVE" "$(( $(date +%s) * 1000 + 1000 ))" > "$H/.claude/sessions/$(cat "$PIDFILE").json"

echo "built $DIR (live session pid $(cat "$PIDFILE"); '$0 $DIR stop' ends it)"
echo "export HOME=$H CLAUDE_CODE_TMPDIR=$DIR/tmp"
