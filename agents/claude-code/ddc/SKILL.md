---
name: ddc
description: Ask DeepDriveCleaner (ddc) what is on the disk before guessing. Use when the person asks about disk space or what fills the disk, when a command fails with ENOSPC or "No space left on device", when the person asks what coding-agent sessions (yours included) left behind, and before installing dependencies into a new worktree or a fresh checkout. Read-only questions with JSON answers; the person does the cleaning.
---

# ddc: ask the disk before guessing

`ddc` (DeepDriveCleaner; the binary may also be called `deepdrivecleaner`) knows what lies on
this disk, who made it and how safe it is to remove. Ask it instead of guessing with `du`, and
never clean on your own: you report and propose, the person removes. Every command below is
read-only. With `--json` the report goes to stdout and nothing else does, so the captured
output parses as it is.

## When to ask

- The person asks about disk space, a full disk, or what uses it.
- A command fails with `ENOSPC` or "No space left on device". Stop retrying and ask first.
- Before installing dependencies into a new worktree or a fresh checkout: one more copy of
  `node_modules` or a virtual environment may not fit.
- The person asks what your sessions, or other agents' sessions, left behind.

## What to run

```sh
ddc --version                       # installed? if not, try: deepdrivecleaner --version
ddc disk agents --json --no-sizes   # harnesses and live sessions, in a fraction of a second
ddc disk agents --json              # the same, with folder sizes and each session's bytes
ddc scan "$PWD" --json              # what can be freed under this project
ddc rules --json                    # what each rule id means: its class and how it is removed
```

Scan the project first. `ddc scan` without a path walks the whole home folder and can take a
minute: run it only when the person asks. `ddc disk agents` is macOS's for now: when a scan's
`capabilities` lacks `coding-agents`, skip it.

## Reading `ddc disk agents --json` (docs/agents.schema.json)

- `harnesses[]`: one per harness, by `id` (`claude-code`, `codex`, `cursor`, `opencode`), with
  its `folders[]` and their `bytes`, and the processes of it that are `running`.
- `sessions.list[]` (Claude Code and Codex): each session's `cwd`, `name` and `state`:
  - `live`: a running process owns it. Hands off.
  - `stale`: a record left behind with no live process; that session has ended.
  - `unknown`: the record does not say. Treat it as live.
- When `sessions.records` is `unknown`, no session is called stale: treat every one as live.
- `sessions.transcripts`: how many transcripts the harness keeps, and their `bytes`.
- `sessions.footprints[]`: Claude Code only, and only with sizes: absent under `--no-sizes`,
  so leave that flag out when you want each session's bytes. One row per session, by `id` and
  `project`, largest first. Its `state`:
  - `live`: in use now; `live_by` says what shows it. Hands off.
  - `not-live`: that session has ended.
  - `unknown`: `unknown_cause` says why it cannot tell. Treat it as live.

  Its bytes: `transcript`, `checkpoints` and `temp` (its temporary folder, at `temp_path`),
  summed in `total`; `modified` is its last change.
- `sessions.temp_root`: Claude Code's temporary folder outside the home, at `path`, under the
  same two conditions. Its `bytes` minus the rows' `temp` is what no session accounts for.
  When `unreadable` is above 0, `bytes` is a lower bound. When the folder is `skipped` or
  `refused` (not a folder the user owns), `bytes` is absent and no row counts `temp`.

## Reading `ddc scan <dir> --json` (docs/scan.schema.json)

- `volumes[]`: the `free_bytes` and `total_bytes` of the disk the scan lies on.
- `totals.classes[]`: per `class`, its `bytes` and `selected_bytes`;
  `totals.selected_reclaimable_bytes` is what a default clean would free.
- `findings[]`: each has `path`, `bytes`, `rule` (look it up in `ddc rules --json`), `class`,
  `selected` and, when not selected, `not_selected` with its reason (`active-project`,
  `review`, …), the `project.root` and `project.idle_days` it belongs to, and `hint`, how to
  get it back.
- `origin` marks what a coding agent's harness made: `origin.kind` is `worktree`,
  `session-data` or `runtime-cache`.
- Live versus dead: a session folder's `contents.entries[]` carry a `lock` whose reason is
  `live-session` while that session is live. ddc checks again for use right before any clean.

The risk classes, in `class`:

- `safe`: caches that refill themselves. Selected by default.
- `rebuildable`: project artifacts an install or a build restores (`node_modules`, `.venv`,
  `target/`). Selected only when the project is idle (14 days by default) and has a lock file.
- `review`: expensive or uncertain to restore: toolchains, models, environments without a lock
  file. Never selected; the person decides each one.
- `user-data`: the person's own files. Never planned.

## What to do with the answer

1. Tell the person what is there, by class, with sizes and paths: what is `safe`, what is
   `rebuildable` and why it is or is not selected, and what needs their judgment (`review`).
2. Name the live sessions, yours included, and say their files stay as they are.
3. Hand the cleanup over, for the person to run in their own terminal. Show it, do not run it:

   ```text
   ddc clean <project> --dry-run    # the plan; nothing changes
   ddc clean <project>              # they review the plan and confirm each step
   ```

   Or the Mac app: its Review window shows the same findings for them to pick from.
4. On `ENOSPC`, report and wait for the person. Do not free space yourself to make your own
   command pass.

## What never to do

ADR "Agent footprint: worktrees, harness data and bounded consent for agents", §5: a list an
agent writes carries no person's consent, and nobody looks at it. So never:

- run `ddc clean --yes`, or `ddc clean --items` with a list you wrote yourself, even when asked
  to "clean it": give the person the command instead;
- delete a finding yourself with `rm -rf`, `find -delete` or a move to the Trash, not even a
  `safe` one;
- run `ddc disk free --actions` with a list you wrote: it removes through simctl, brew and
  conda, and an agent's list is the same trap as `--items`;
- remove a worktree with `git worktree remove --force`, or delete its folder;
- touch a live session's files: its transcript, its temporary folder or its worktree, yours
  included.

A bounded cleanup for agents is planned (the same ADR, §5); until it ships, cleaning is the
person's.
