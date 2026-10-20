# DeepDriveCleaner

Find and safely clean what development leaves on a Mac's disk: Python virtual environments, `node_modules`, build outputs, package and toolchain caches, Xcode data, local ML models and VM images. Everything it finds is grouped by risk. Nothing is removed until you have seen the plan and confirmed it.

```
$ ddc scan
DeepDriveCleaner · scanned ~ in 39.6s · 5.5M files · 544.8 GiB
  safe           1919 items   76.3 GiB   selected 76.3 GiB
  rebuildable     126 items   35.1 GiB   selected 24.4 GiB
  review          263 items   79.4 GiB   manual choice
  user-data        11 items  114.1 GiB   pick by hand
  Can free now: 100.6 GiB selected by default, frees ~88.6 GiB → run `ddc clean` to review and confirm
```

> **Built in the open with [ProjectStore](https://projectstore.dev).** Every decision, epic and story behind this code lives in a second public repository, [DeepDriveCleaner-projectstore](https://github.com/SmartAndPoint/DeepDriveCleaner-projectstore): the research, the ADRs, the acceptance criteria and the evidence each story was closed with. This repository is meant to be read as a worked example of that workflow, not only as a tool. See [Contributing](#contributing).

Status: early (0.1.0). macOS only (APFS), Apple Silicon builds. A terminal tool today; a menu bar app is next.

## Install

On a Mac with Apple Silicon:

```
brew install --cask smartandpoint/tap/deepdrivecleaner
```

This installs `deepdrivecleaner` and `ddc`, the same binary under a short name. The long name is what macOS shows in Full Disk Access, Activity Monitor and background items; `ddc` is for typing. The binary is signed with the SmartAndPoint Developer ID and notarized by Apple. If you use the background scan, run `ddc schedule uninstall` before `brew uninstall deepdrivecleaner`, or uninstall with `brew uninstall --zap`, which also removes the agent, the logs and the saved scan.

### From source

You need [rustup](https://rustup.rs) (the toolchain is pinned in `rust-toolchain.toml`).

```
git clone https://github.com/SmartAndPoint/DeepDriveCleaner.git
cd DeepDriveCleaner
make install
```

This installs `~/.cargo/bin/deepdrivecleaner` and a `ddc` symlink to it. A local build is not signed with the Developer ID, so macOS ties privacy grants such as Full Disk Access to that one build.

## Use

| Command | What it does |
|---|---|
| `ddc scan [PATH…]` | Read-only. Scans your home folder (or the given paths) and prints findings grouped by risk class and category, largest first. `--json` prints the report in a versioned schema ([`docs/scan.schema.json`](docs/scan.schema.json)). |
| `ddc clean --dry-run` | Scans again and prints the cleanup plan without changing anything. |
| `ddc clean` | Scans again, shows the plan, lets you toggle groups and items, asks for confirmation, then cleans only what you confirmed. |
| `ddc clean --items FILE` | Cleans exactly the items in a JSON list (`-` reads stdin), such as the ones a sheet in the Mac app confirmed; every item is checked again first. `--json` prints the result ([`docs/clean.schema.json`](docs/clean.schema.json)); `--dry-run` predicts each item without a rescan. |
| `ddc rules` | Lists the rules by group: what each type is and whether `settings.json` can switch it off. `--json` follows [`docs/rules.schema.json`](docs/rules.schema.json). |
| `ddc schedule install` | Installs a LaunchAgent that scans daily (or `--interval weekly`) and sends a notification when more than `--threshold` (default: the settings' threshold, else 10G) can be freed, or when a scanned disk is under 10 % free with something on it to clean. A threshold notification repeats after a fifth more or a week; a disk under 10 % free notifies at every scheduled scan until it is cleaned (`notify_low_disk: false` in `settings.json` turns that off). The background scan **never deletes anything**. It refuses while the Mac app runs the scheduled scan. `ddc schedule status` and `ddc schedule uninstall` do what they say. |

Exit codes: 0 done, 1 error, 2 refused, 75 another scan or clean is running. Ctrl-C during a clean stops it after the current item; a tool it runs gets the Ctrl-C too.

### Settings

`~/Library/Application Support/DeepDriveCleaner/settings.json` holds what the Mac app's Settings and the terminal share ([`docs/settings.schema.json`](docs/settings.schema.json)). Every field is optional except `schema_version`:

| Field | Meaning | Flag that wins over it |
|---|---|---|
| `roots` | Absolute folders to scan; empty means your home folder. A folder that does not exist, such as an unplugged disk, is skipped with a note. | `PATH…` |
| `excludes` | Absolute folders never touched, reported as user data. | `--exclude` |
| `min_age_days` | Idle days before a project's artifacts are selected (default 14). | `--min-age` |
| `disabled_rules` | Rule ids from `ddc rules` switched off: their findings are reported as ignored and never selected or counted. | none |
| `min_size` | Bytes: smaller findings are ignored. `clean --min-size` filters the plan on top of it, so the larger of the two applies. | none |
| `notify_threshold`, `notify_low_disk` | When the background scan notifies (default 10G, and low disk on). | `--threshold` |

`--exclude` replaces the excludes from the file. When that would leave a folder from the file unprotected, ddc names it first: `ddc clean` asks in a terminal and refuses without one; `ddc scan` and `ddc clean --dry-run` only warn, since they change nothing. A malformed file is an error that names it.

### Risk classes

| Class | What it is | Default |
|---|---|---|
| `safe` | Caches that refill themselves (`~/.npm/_cacache`, `~/.cache/uv`, Homebrew, pip, Gradle, …). | Selected. Removed with the tool's own command when it has one (`uv cache clean`, `go clean -modcache`, …). |
| `rebuildable` | Project artifacts an install or build restores: `.venv`, `node_modules`, `target/`, `.next`, CMake build folders, … | Selected only when the project has been idle for `--min-age` days (default 14) and has a lock file. |
| `review` | Expensive or uncertain to restore: environments without a lock file, global packages, toolchains, models, VM images. | Never selected. You choose each item; it goes to the Trash unless you pass `--permanent`. VM runtimes are cleaned only through their own prune commands. |
| `user-data` | Pictures, movies, music, Downloads, iPhone backups, anything protected by `--exclude` or a `.ddc-keep` file. Developer artifacts inside Downloads are still found as their own items. | Never selected, and never in `ddc clean`'s plan. Files and folders of Downloads, Pictures, Movies, Music and iPhone and iPad backups that you pick one by one (`ddc clean --items`, as the menu bar app sends them) go to the Trash only, never deleted. |

### Safety

- Right before touching anything, `ddc clean` checks every item again. It is still the same directory (device and inode), no symlink has appeared above it, it still matches its rule, it is not a project root, it is not protected, and no running process uses it. A file or folder you picked in one of your own folders is checked as that folder's entry instead: the same file or folder, still in that folder, not locked (an app's library, a `.ddc-keep`, a download still arriving, something changed in the last 10 minutes). Anything that fails a check is skipped with a reason.
- Deletion first renames the folder to a tombstone through a verified parent folder, then removes it. An interrupted run leaves `*.ddc-deleting-*`, which the next scan finds as `safe`.
- Every action is logged to `~/Library/Logs/DeepDriveCleaner/actions.jsonl`.
- It never runs as root, never deletes your home, `~/Documents`, `~/Projects` or any other protected folder, and never follows a symlink out of a finding.
- APFS frees space asynchronously, and local Time Machine snapshots keep deleted blocks until macOS thins them. `ddc clean` says so when snapshots exist.

### Full Disk Access

Without Full Disk Access some of `~/Library` cannot be read; the report lists those folders instead of guessing. The background scan skips the folders where macOS would show a permission prompt, because a prompt would block it. To scan everything, add the binary under System Settings → Privacy & Security → Full Disk Access: `$(brew --prefix)/Caskroom/deepdrivecleaner/<version>/deepdrivecleaner` (press ⇧⌘G in the file picker to type the path; `brew info --cask deepdrivecleaner` shows it). macOS keys the permission by the file's path as well as its signature, and Homebrew puts every version in its own directory, so after `brew upgrade` remove the old entry and add the new one; `brew reinstall` keeps the path, so the grant stays. A local `make install` build is not signed with the Developer ID and has to be granted again after every build.

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
make dist       # maintainers: signed, notarized release tarball (then scripts/publish-release.sh)
```

By submitting a contribution you agree that SmartAndPoint may use, modify and relicense it under any terms, including commercial ones, and you confirm that you have the right to grant this.

Links: [ProjectStore on GitHub](https://github.com/SmartAndPoint/ProjectStore) · [projectstore.dev](https://projectstore.dev)

## License

[PolyForm Noncommercial 1.0.0](LICENSE.md). You may read, use, change and share this code for any noncommercial purpose: personal use, research, education, nonprofits. You may not use it commercially, including selling an app built on it. Strictly speaking this makes the project source-available rather than "open source" in the OSI sense: the code is open so that you can see what a tool that deletes files on your disk actually does.
