# DeepDriveCleaner

Find and safely clean what development leaves on a Mac's disk: Python virtual environments, `node_modules`, build outputs, package and toolchain caches, Xcode data, local ML models and VM images. Everything it finds is grouped by risk. Nothing is removed until you have seen the plan and confirmed it.

```
$ ddc scan
DeepDriveCleaner · scanned ~ in 39.6s · 5.5M files · 544.8 GiB
  safe           1919 items   76.3 GiB   selected 76.3 GiB
  rebuildable     126 items   35.1 GiB   selected 24.4 GiB
  review          263 items   79.4 GiB   manual choice
  user-data        11 items  114.1 GiB   report only
  Can free now: 100.6 GiB selected by default, frees ~88.6 GiB → run `ddc clean` to review and confirm
```

> **Built in the open with [ProjectStore](https://projectstore.dev).** Every decision, epic and story behind this code lives in a second public repository, [DeepDriveCleaner-projectstore](https://github.com/SmartAndPoint/DeepDriveCleaner-projectstore): the research, the ADRs, the acceptance criteria and the evidence each story was closed with. This repository is meant to be read as a worked example of that workflow, not only as a tool. See [Contributing](#contributing).

Status: early. macOS only (APFS). A terminal tool today; a Mac app may follow.

## Install

You need [rustup](https://rustup.rs) (the toolchain is pinned in `rust-toolchain.toml`).

```
git clone https://github.com/SmartAndPoint/DeepDriveCleaner.git
cd DeepDriveCleaner
make install
```

This installs `~/.cargo/bin/deepdrivecleaner` and a `ddc` symlink to it. The long name is what macOS shows in Full Disk Access, Activity Monitor and background items; `ddc` is for typing.

## Use

| Command | What it does |
|---|---|
| `ddc scan [PATH…]` | Read-only. Scans your home folder (or the given paths) and prints findings grouped by risk class and category, largest first. `--json` prints the report in a versioned schema ([`docs/scan.schema.json`](docs/scan.schema.json)). |
| `ddc clean --dry-run` | Scans again and prints the cleanup plan without changing anything. |
| `ddc clean` | Scans again, shows the plan, lets you toggle groups and items, asks for confirmation, then cleans only what you confirmed. |
| `ddc schedule install` | Installs a LaunchAgent that scans daily (or `--interval weekly`) and sends a notification when more than `--threshold` (default 10G) can be freed. The background scan **never deletes anything**. `ddc schedule status` and `ddc schedule uninstall` do what they say. |

### Risk classes

| Class | What it is | Default |
|---|---|---|
| `safe` | Caches that refill themselves (`~/.npm/_cacache`, `~/.cache/uv`, Homebrew, pip, Gradle, …). | Selected. Removed with the tool's own command when it has one (`uv cache clean`, `go clean -modcache`, …). |
| `rebuildable` | Project artifacts an install or build restores: `.venv`, `node_modules`, `target/`, `.next`, CMake build folders, … | Selected only when the project has been idle for `--min-age` days (default 14) and has a lock file. |
| `review` | Expensive or uncertain to restore: environments without a lock file, global packages, toolchains, models, VM images. | Never selected. You choose each item; it goes to the Trash unless you pass `--permanent`. VM runtimes are cleaned only through their own prune commands. |
| `user-data` | Pictures, movies, music, Downloads, iPhone backups, anything protected by `--exclude` or a `.ddc-keep` file. Developer artifacts inside Downloads are still found as their own items. | Reported, never touched. |

### Safety

- Right before touching anything, `ddc clean` checks every item again. It is still the same directory (device and inode), no symlink has appeared above it, it still matches its rule, it is not a project root, it is not protected, and no running process uses it. Anything that fails a check is skipped with a reason.
- Deletion first renames the folder to a tombstone through a verified parent folder, then removes it. An interrupted run leaves `*.ddc-deleting-*`, which the next scan finds as `safe`.
- Every action is logged to `~/Library/Logs/DeepDriveCleaner/actions.jsonl`.
- It never runs as root, never deletes your home, `~/Documents`, `~/Projects` or any other protected folder, and never follows a symlink out of a finding.
- APFS frees space asynchronously, and local Time Machine snapshots keep deleted blocks until macOS thins them. `ddc clean` says so when snapshots exist.

### Full Disk Access

Without Full Disk Access some of `~/Library` cannot be read; the report lists those folders instead of guessing. The background scan skips the folders where macOS would show a permission prompt, because a prompt would block it. To scan everything, add `~/.cargo/bin/deepdrivecleaner` under System Settings → Privacy & Security → Full Disk Access. The binary is not signed yet, so macOS ties the permission to one build: after `make install` you have to grant it again.

## How it is built

A Rust workspace:

- `crates/ddc-core`: the library. A parallel filesystem walker over `getattrlistbulk(2)` (the only module with `unsafe` is `sys`), the rule catalog, the report model, the cleanup plan and the executor. It never prints and never reads stdin, so a future GUI can use it as is.
- `crates/ddc-cli`: the `deepdrivecleaner` binary.

On the reference machine `ddc scan ~` takes 37 s for 5.5 million files and `ddc scan ~/Projects` takes 10 s, where `du -sk` needs 42 s for `~/Projects` alone. Why it is built this way is recorded in the ADRs: *Rust core: library + CLI with JSON output* and *Cleanup safety model: risk classes, plan and confirmation*.

## Contributing

Work here follows the ProjectStore loop. Before code there is an artifact (an ADR, a story); a fresh-context critic reviews it; a planner places the change; a reviewer checks the diff against the story's acceptance criteria before the story is closed. The code repository and the vault repository are cloned side by side.

1. **Clone the code.**

   ```
   git clone https://github.com/SmartAndPoint/DeepDriveCleaner.git
   ```

2. **Clone the vault next to it.**

   ```
   git clone https://github.com/SmartAndPoint/DeepDriveCleaner-projectstore.git
   ```

3. **Set up ProjectStore from the code folder and bind it to the vault clone.** You need Node.js 20 or later. Run this in a terminal outside any agent session:

   ```
   cd DeepDriveCleaner
   npx projectstore setup --vault ../DeepDriveCleaner-projectstore
   ```

   Setup installs the ProjectStore plugin for Claude Code (and Codex, if you use it) and binds this checkout to the vault. The binding is machine-local and stays out of git.

4. **Start Claude Code (or Codex) in `DeepDriveCleaner` and ask:**

   ```
   Where are we?
   ```

   The agent reads the vault and tells you which epic is in flight, which stories are open and what is left to verify. From there, pick a story or propose a new one.

`bench/` holds the scripts that measured rule coverage on the reference machine. The data they produced lists personal folder names, so it is not in the repository: `bench/make_baseline.py` builds a baseline for your own machine and `bench/check_baseline.py` checks `deepdrivecleaner du --rules ~` against it.

Development commands (they work around a Homebrew Rust that may shadow rustup in `PATH`):

```
make check      # fmt check, clippy -D warnings, all tests
make release    # optimized build
make install    # install deepdrivecleaner + ddc
```

By submitting a contribution you agree that SmartAndPoint may use, modify and relicense it under any terms, including commercial ones, and you confirm that you have the right to grant this.

Links: [ProjectStore on GitHub](https://github.com/SmartAndPoint/ProjectStore) · [projectstore.dev](https://projectstore.dev)

## License

[PolyForm Noncommercial 1.0.0](LICENSE.md). You may read, use, change and share this code for any noncommercial purpose: personal use, research, education, nonprofits. You may not use it commercially, including selling an app built on it. Strictly speaking this makes the project source-available rather than "open source" in the OSI sense: the code is open so that you can see what a tool that deletes files on your disk actually does.
