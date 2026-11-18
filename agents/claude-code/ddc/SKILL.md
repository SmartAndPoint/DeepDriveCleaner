---
name: ddc
description: Ask DeepDriveCleaner (ddc) what is on the disk before guessing, and clean up after yourself only through it. Use when the person asks about disk space or what fills the disk, when a command fails with ENOSPC or "No space left on device" or an install fails for lack of space, at the start of a session in a worktree, before installing dependencies into a new worktree or a fresh checkout, at the end of a session, when the person asks to clean up after yourself, and when they ask what coding-agent sessions (yours included) left behind. Read-only questions with JSON answers; the only removal is `ddc agent clean` inside your project, and the rest goes to the person.
license: MIT (Copyright 2026 SmartAndPoint; text in LICENSE-MIT.md of the DeepDriveCleaner repository)
allowed-tools:
  - Bash(ddc changes *)
  - Bash(ddc agent status *)
  - Bash(ddc agent footprint *)
  - Bash(ddc agent clean --dry-run *)
  - Bash(ddc rules *)
  - Bash(deepdrivecleaner changes *)
  - Bash(deepdrivecleaner agent status *)
  - Bash(deepdrivecleaner agent footprint *)
  - Bash(deepdrivecleaner agent clean --dry-run *)
  - Bash(deepdrivecleaner rules *)
metadata:
  version: "0.37.0"
---

# ddc: ask the disk before guessing

`ddc` (DeepDriveCleaner; the binary may also be called `deepdrivecleaner`) knows what lies on
this disk, who made it and how safe it is to remove. Ask it instead of guessing with `du`. You
report and propose; the person removes. The one exception is `ddc agent clean`: inside your own
project it removes the `safe` items, and only those, once the person has allowed it. With
`--json` the report goes to stdout and nothing else does, so the captured output parses as it
is.

## When to ask

- At the start of a session in a worktree, and before installing dependencies into a new
  worktree or a fresh checkout: one more copy of `node_modules` or a virtual environment may not
  fit. Ask `ddc agent footprint` first.
- A command fails with `ENOSPC` or "No space left on device", or an install fails for lack of
  space. Stop retrying and ask first.
- The person asks about disk space, a full disk, or what uses it.
- At the end of a session, or when the person asks you to clean up after yourself.
- The person asks what your sessions, or other agents' sessions, left behind.

## What to run

```sh
ddc --version                       # installed? if not, try: deepdrivecleaner --version
ddc changes --json                  # what grew between the person's last two scans; before any scan
ddc agent footprint --json          # this project: its checkouts, worktrees and what they hold
ddc agent status --json --no-sizes  # harnesses and live sessions, in a fraction of a second
ddc agent status --json             # the same, with folder sizes and each session's bytes
ddc scan "$PWD" --json              # what can be freed under this project
ddc rules --json                    # what each rule id means: its class and how it is removed
```

For your own project, `ddc agent footprint` answers in seconds, takes no lock and saves no
report; prefer it to `ddc scan "$PWD"`. Ask `ddc changes` before you scan: it compares the
person's last two scans, their own cleans apart, and a scan replaces the latest report. After
`ddc scan "$PWD"` it compares that project only. `ddc scan` without a path walks the whole home
folder and can take a minute: run it only when the person asks, never to get a comparison.
`ddc agent status` and `ddc agent footprint` run on macOS and Linux: when a scan's
`capabilities` lacks `coding-agents`, skip `ddc agent status`. An older `ddc` that has no
`ddc agent status` (its `ddc --help` lists no agent command) has it as `ddc disk agents`, with
the same options and the same report. A `ddc` before 0.37.0 has no `ddc agent footprint` or
`ddc agent clean` (`ddc agent --help` lists what it has): stay with the read-only commands it
has.

If `ddc` is not installed, say so and propose the install for the person; run it only after
they say yes:

```text
brew install --cask smartandpoint/tap/deepdrivecleaner     # macOS, with the app
curl -fsSL https://raw.githubusercontent.com/SmartAndPoint/DeepDriveCleaner/main/packaging/install/install.sh | sh
```

The second line installs the command line alone, into `~/.local/bin`.

## Cleaning up after yourself

```sh
ddc agent clean --dry-run --json    # what it would remove now, and what is the person's
ddc agent clean --json              # removes the safe items inside this project, if allowed
```

`ddc agent clean` works on the project of the current folder: its main checkout and the
worktrees `ddc` knows as this project's. It removes only `safe` items there (`__pycache__`,
`.pytest_cache` and the like), and only while the person allows it in DeepDriveCleaner's
settings, which is off until they switch it on. It never removes a `rebuildable` or `review`
item, a worktree, a tool's command, or anything outside the project, such as `~/.cargo` or
another project. Run the dry run first, then the real one when you clean up after yourself: at
the end of a session, when the person asks, or when a lack of space stops your work. When it is off, tell the person
what that means; switching it on is their choice, made in the app or at their own terminal,
never yours.

## Reading `ddc changes --json` (docs/changes.schema.json)

- `status` is `compared` or `not-compared`. With `not-compared`, `reason` says why; `no-report`
  or `no-previous` is the normal answer at first, before two scans are saved. Say so and go on.
- `compared_roots` says what was compared. After two scans of the home folder that is the whole
  home, the builds in coding agents' worktrees included. Show the person only what they asked
  about.
- Showing what grew is fine. Removing it is not yours, beyond the `safe` items of your own
  project through `ddc agent clean`: the rest goes to the person like any other finding.

## Reading `ddc agent footprint --json` (docs/footprint.schema.json)

- `main` is the main checkout; `checkouts[]` lists it and each worktree, with its `path`,
  `kind` (`main`, `linked`, `orphan`) and `bytes`.
- A worktree's `bucket`: `now` (removable now), `after-sessions` (a live session holds it, or it
  was used too recently) or `held-back`, with `reason` (`dirty`, `own-commits`,
  `ignored-content`, `yours`, …). A worktree is never yours to remove: it is the person's, with
`ddc clean` or the app.
- `totals.reclaimable_now` has `safe` apart from `rebuildable`: only the `safe` part is ever
  yours to clean, with `ddc agent clean`.
- `clean_roots` are the folders the person's own `ddc clean` would review.

## Reading `ddc agent clean --json` (docs/clean.schema.json)

- `agents_clean_safe` says whether the person allowed it; `settings_error` says why the
  settings could not be read, and then nothing was considered.
- `dry_run` is true for the dry run. Each of `items[]` has `path`, `class` and its `outcome`:
  `deleted`, `not-run` in a dry run, `failed` with an `error`, or `skipped` with a `reason`. `agent-opt-in-off`: a `safe`
  item the person has not allowed you to clean. `not-consented`: an item that is the person's
  in any case.
- `for_person` lists, by `path`, what the person may clean; `left_out` names checkouts outside
  the bound, and why. The JSON carries no command. For the person's command, run the dry run
  without `--json`:

  ```sh
  ddc agent clean --dry-run           # ends with the person's command, to show them as it is
  ```

  Its text ends with a `ddc clean --items -` command that carries the list in a heredoc. Show
  it to the person as text, as it is, and never run it yourself. With more than 50 items, or a
  path a terminal cannot show, it prints the paths instead: point the person to the app's
  Review window then.

## Reading `ddc agent status --json` (docs/agents.schema.json)

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
3. Clean what is yours with `ddc agent clean` as above. Hand the rest over, for the person to
   run in their own terminal. Show it, do not run it:

   ```text
   ddc clean <project> --dry-run    # the plan; nothing changes
   ddc clean <project>              # they review the plan and confirm each step
   ```

   Or the Mac app: its Review window shows the same findings for them to pick from.
4. On `ENOSPC`, report and wait for the person. Do not free space outside `ddc agent clean` to
   make your own command pass.

## Installing or updating this guidance

`ddc agent skill status --json` says whether the skill here and an `AGENTS.md` block are this
`ddc`'s. To install or update them, ask the person first; once they said yes, run:

```text
ddc agent skill install --approved                         # for Claude Code and the shared folder
ddc agent skill install --approved --agents-md AGENTS.md   # also the block in an AGENTS.md
```

It writes the skill into `~/.claude/skills/ddc` (Claude Code) and `~/.agents/skills/ddc` (Codex,
opencode and others), writes only `ddc`'s own text, deletes nothing, and refuses a copy someone
edited.

## What never to do

ADR "Agent footprint: worktrees, harness data and bounded consent for agents", §5 and §7: a
list an agent writes carries no person's consent, and nobody looks at it. So never:

- run `ddc clean --yes`, or `ddc clean --items` with a list you wrote yourself, even when asked
  to "clean it": give the person the command instead;
- give `ddc agent clean` an `--items` list you wrote: it only narrows what ddc chose, and the
  choice stays ddc's;
- delete a finding yourself with `rm -rf`, `find -delete` or a move to the Trash, not even a
  `safe` one;
- run `ddc disk free --actions` with a list you wrote: it removes through simctl, brew and
  conda, and an agent's list is the same trap as `--items`;
- remove a worktree with `git worktree remove --force`, or delete its folder: a worktree goes
  through `ddc`, for the person;
- touch a live session's files: its transcript, its temporary folder or its worktree, yours
  included;
- change DeepDriveCleaner's settings, or edit its files, to let yourself clean more.
