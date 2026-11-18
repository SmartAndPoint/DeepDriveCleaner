# ddc for coding agents

Guidance that teaches a coding agent to ask `ddc` about the disk before it guesses, and to leave
the cleaning to you. The agent runs `ddc changes --json`, `ddc agent footprint --json`,
`ddc agent status --json`, `ddc scan <dir> --json` and `ddc rules --json`, tells you what it
found, and hands you the `ddc clean` command (or the Mac app's Review window) to run yourself.
The one removal it runs is `ddc agent clean`: the `safe` items of its own project, only while
you allow it in DeepDriveCleaner's settings. Why an agent cleans no more than that: ADR "Agent
footprint: worktrees, harness data and bounded consent for agents", §5 and §7, in the
[vault](https://github.com/SmartAndPoint/DeepDriveCleaner-projectstore).

| File | For |
|---|---|
| [`claude-code/ddc/SKILL.md`](claude-code/ddc/SKILL.md) | Claude Code, as a skill |
| [`AGENTS.snippet.md`](AGENTS.snippet.md) | Codex, opencode and any harness reading `AGENTS.md` |

`ddc` itself must be installed: see [Install](../README.md#install).

Everything in this folder is under the [MIT license](../LICENSE-MIT.md), unlike the rest of the
repository (AGPL-3.0): copy it into any project.

## Install by hand

Claude Code, for every project (or into one project's `.claude/skills/` instead):

```sh
mkdir -p ~/.claude/skills
cp -R agents/claude-code/ddc ~/.claude/skills/
```

Claude Code finds the skill in its next session and uses it when you ask about disk space, when a
command runs out of it, at the start and end of a session in a worktree, and when you ask it to
clean up after itself. Updating is the same copy again.

`AGENTS.md`: paste [`AGENTS.snippet.md`](AGENTS.snippet.md) into the project's `AGENTS.md`
(or `~/.codex/AGENTS.md` for every Codex project), markers included. To update it, replace
everything from `<!-- ddc:begin -->` to `<!-- ddc:end -->` with the new snippet.

## Install with ddc

`ddc` 0.38 and later carry this guidance and install it themselves:

```sh
ddc agent skill install                          # the skill, into ~/.claude/skills/ddc
ddc agent skill install --project .              # into this project's .claude/skills instead
ddc agent skill install --agents-md AGENTS.md    # also the block in this AGENTS.md
ddc agent skill status                           # is what is installed this ddc's?
```

It lists the files it will write and asks first; it deletes nothing. It writes only into a skill
folder it installed itself and nobody edited since (`--replace` writes over one), and in
`AGENTS.md` it replaces only the block between the markers. It leaves a `.ddc-skill` file in the
skill folder, so it can tell its own copy from a hand edit. A coding agent runs it with
`--approved`, and only after you said yes in the chat.

## Install by an agent, without ddc

Ask your agent: "Install the ddc agent guidance from github.com/SmartAndPoint/DeepDriveCleaner,
folder `agents/`". It should:

1. Read this file and the two files above from a clone of this repository, or from
   `https://raw.githubusercontent.com/SmartAndPoint/DeepDriveCleaner/main/agents/`.
2. Claude Code: write `claude-code/ddc/SKILL.md` to `~/.claude/skills/ddc/SKILL.md`, or to
   `.claude/skills/ddc/SKILL.md` of the project you name.
3. `AGENTS.md`: when the file already has a `<!-- ddc:begin -->` … `<!-- ddc:end -->` block,
   replace that block and nothing else; otherwise append the snippet at the end. Never rewrite
   the rest of the file.
4. Show you the diff of what it wrote, and run `ddc --version` to check that `ddc` is there.

The guidance cites the JSON contracts in [`docs/`](../docs): `changes.schema.json`,
`footprint.schema.json`, `clean.schema.json`, `agents.schema.json` and `scan.schema.json`. A test (`crates/ddc-cli/tests/structure.rs`) checks that every field the
skill names exists in them, that every `ddc` command it runs parses, that its runnable
commands remove nothing but through `ddc agent clean`, and that `allowed-tools` lets it run
only the commands that read without asking you.
