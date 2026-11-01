<!-- ddc:begin -->
## Disk space: ask ddc, never clean

`ddc` (DeepDriveCleaner, also installed as `deepdrivecleaner`) knows what is on this disk and
how safe it is to remove. Ask it before guessing; never clean on your own.

**When.** The person asks about disk space; a command fails with `ENOSPC` or "No space left on
device"; before installing dependencies into a new worktree or a fresh checkout; the person asks
what agent sessions left behind.

**What to run** (read-only; with `--json` only the report goes to stdout):

```sh
ddc disk agents --json --no-sizes   # harnesses and live sessions (macOS)
ddc scan "$PWD" --json              # what can be freed under this project
ddc rules --json                    # what each rule id means
```

**How to read it.** `ddc disk agents` (docs/agents.schema.json): `harnesses[]`, and per
session in `sessions.list[]` its `cwd` and `state`: `live` is hands off, `stale` has ended,
`unknown` counts as live.
Each session's bytes (Claude Code only, and absent under `--no-sizes`, so drop that flag for
them): `sessions.footprints[]`, one row per session with its `state` (`live`, `not-live`, or
`unknown`, which counts as live) and its `transcript`, `checkpoints`, `temp` and `total`. The
`bytes` of `sessions.temp_root` minus the rows' `temp` is what no session owns; a lower bound
when its `unreadable` is above 0.
`ddc scan` (docs/scan.schema.json): `volumes[]` with `free_bytes`; `findings[]` with `path`,
`bytes`, `rule`, `class`, `selected`, `not_selected`, `project.root` and `origin.kind`
(`worktree`, `session-data`, `runtime-cache`). Classes: `safe` caches refill themselves;
`rebuildable` project artifacts come back with an install or a build; `review` is the person's
call, one by one; `user-data` is theirs and never planned. A `lock` of `live-session` marks a
live session's files.

**What to do.** Tell the person what is there, by class, with sizes and paths, and which
sessions are live. Hand them the cleanup to run themselves: `ddc clean <project> --dry-run`,
then `ddc clean <project>`, or the Mac app's Review window.

**Never** (ADR "Agent footprint: worktrees, harness data and bounded consent for agents", §5: a
list an agent writes carries no person's consent): run `ddc clean --yes`, or `ddc clean --items`
with a list you wrote, or `ddc disk free --actions` with a list you wrote (it removes through
simctl, brew and conda); delete a finding with `rm -rf` or any other way; remove a worktree with
`git worktree remove --force`; touch a live session's files, yours included.
<!-- ddc:end -->
