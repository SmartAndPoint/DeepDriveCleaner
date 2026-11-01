# ddc for coding agents

Guidance that teaches a coding agent to ask `ddc` about the disk before it guesses, and to leave
the cleaning to you. It is read-only: the agent runs `ddc disk agents --json`, `ddc scan <dir>
--json` and `ddc rules --json`, tells you what it found, and hands you the `ddc clean` command
(or the Mac app's Review window) to run yourself. Why an agent never cleans on its own: ADR
"Agent footprint: worktrees, harness data and bounded consent for agents", §5, in the
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

Claude Code finds the skill in its next session and uses it when you ask about disk space or a
command runs out of it. Updating is the same copy again.

`AGENTS.md`: paste [`AGENTS.snippet.md`](AGENTS.snippet.md) into the project's `AGENTS.md`
(or `~/.codex/AGENTS.md` for every Codex project), markers included. To update it, replace
everything from `<!-- ddc:begin -->` to `<!-- ddc:end -->` with the new snippet.

## Install by an agent

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

The guidance cites the JSON contracts in [`docs/`](../docs): `agents.schema.json` and
`scan.schema.json`. A test (`crates/ddc-cli/tests/structure.rs`) checks that every field the
skill names exists in them, that every `ddc` command it runs parses, and that its runnable
commands remove nothing.
