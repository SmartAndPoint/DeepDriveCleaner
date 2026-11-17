<!-- ddc:begin -->
## Disk space: ask ddc, and clean only through it

`ddc` (DeepDriveCleaner, also installed as `deepdrivecleaner`) knows what is on this disk and
how safe it is to remove. Ask it before guessing. The only removal that is yours is
`ddc agent clean`, inside your own project; the rest is the person's.

**When.** At the start of a session in a worktree and before installing dependencies into a new
worktree or a fresh checkout; a command fails with `ENOSPC` or "No space left on device", or an
install fails for lack of space; the person asks about disk space; at the end of a session, or
when asked to clean up after yourself; the person asks what agent sessions left behind.

**What to run** (read-only; with `--json` only the report goes to stdout):

```sh
ddc changes --json                  # what grew between the last two scans; before any scan
ddc agent footprint --json          # this project's checkouts and worktrees (macOS, Linux)
ddc agent status --json --no-sizes  # harnesses and live sessions (macOS, Linux)
ddc scan "$PWD" --json              # what can be freed under this project
ddc rules --json                    # what each rule id means
```

An older `ddc` that has no `ddc agent status` has it as `ddc disk agents`, with the same options,
and one before 0.37.0 has no `ddc agent footprint` or `ddc agent clean` (`ddc agent --help` lists
what it has). No `ddc` at all: propose
`brew install --cask smartandpoint/tap/deepdrivecleaner` on macOS, or the install script of the
DeepDriveCleaner repository (`packaging/install/install.sh`), and run it only after the person
says yes.

**Cleaning up after yourself:**

```sh
ddc agent clean --dry-run --json    # what it would remove, and what is the person's
ddc agent clean --json              # removes the safe items inside this project, if allowed
```

It removes only `safe` items (`__pycache__`, `.pytest_cache` and the like) in this project's
checkouts, and only while the person allows it in DeepDriveCleaner's settings, off until they
switch it on; that switch is theirs, never yours. In its report, `agents_clean_safe` says
whether it is on; each of `items[]` has an `outcome`, and a `skipped` one a `reason`
(`agent-opt-in-off`: not allowed yet; `not-consented`: the person's). `for_person` and the
`ddc clean --items -` command that `ddc agent clean --dry-run` prints without `--json` are the
person's: show them, never run them.

**How to read it.** `ddc changes` (docs/changes.schema.json) compares the person's last two
scans, so ask it before you scan; `status` is `not-compared`, with its `reason`, until two scans
are saved. `compared_roots` says what it compared, often the whole home: show only what the
person asked about, and leave removing what grew to them.
`ddc agent footprint` (docs/footprint.schema.json): `checkouts[]` with each one's `path`, `bytes`
and, for a worktree, its `bucket` (`now`, `after-sessions`, `held-back` with a `reason`);
`totals.reclaimable_now` keeps `safe` apart from `rebuildable`. A worktree is the person's, and
goes through `ddc`, never your own `git worktree remove`.
`ddc agent status` (docs/agents.schema.json): `harnesses[]`, and per
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
sessions are live. Clean what is yours with `ddc agent clean`; hand them the rest to run
themselves: `ddc clean <project> --dry-run`,
then `ddc clean <project>`, or the Mac app's Review window.

**Never** (ADR "Agent footprint: worktrees, harness data and bounded consent for agents", §5 and
§7: a list an agent writes carries no person's consent): run `ddc clean --yes`, or `ddc clean --items`
with a list you wrote, or `ddc disk free --actions` with a list you wrote (it removes through
simctl, brew and conda); delete a finding with `rm -rf` or any other way; remove a worktree with
`git worktree remove --force`; touch a live session's files, yours included; give
`ddc agent clean` an `--items` list you wrote; change DeepDriveCleaner's settings to let yourself
clean more.

<!-- License: MIT (Copyright 2026 SmartAndPoint; text in LICENSE-MIT.md of the DeepDriveCleaner repository) -->
<!-- ddc:end -->
