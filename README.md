# DeepDriveCleaner

Find and safely clean what development leaves on a Mac's disk: Python virtual environments, `node_modules`, build outputs, package and toolchain caches, Xcode data, local ML models and VM images. Everything it finds is grouped by risk. Nothing is removed until you have seen the plan and confirmed it.

It is a menu bar app with the command-line tool inside: the same engine, a window or a terminal, whichever you prefer.

> [!WARNING]
> **DeepDriveCleaner deletes files.** It checks every item before it touches it, but it can remove almost anything it is told to, by you or by an AI agent acting for you. You use it at your own risk: nobody who made it is responsible for what it removes. Read the [Disclaimer](DISCLAIMER.md) before you clean.

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

Status: 0.3.0. macOS 26 on Apple Silicon. The app now schedules scans itself and notifies with Review and Remind Me Tomorrow (Settings → Schedule).

## Install

On a Mac with Apple Silicon and macOS 26:

```
brew install --cask smartandpoint/tap/deepdrivecleaner
```

This installs `/Applications/DeepDriveCleaner.app` and links two commands into Homebrew's `bin`: `deepdrivecleaner` and `ddc`, the same binary under a short name. The binary lives inside the app, in `DeepDriveCleaner.app/Contents/Helpers/`. The long name is what macOS shows in Full Disk Access, Activity Monitor and background items; `ddc` is for typing. The app and the binary are signed with the SmartAndPoint Developer ID and notarized by Apple.

Open the app once from Applications and grant it Full Disk Access when it asks (see [Full Disk Access](#full-disk-access)).

The app updates itself. It checks for a new version once a day (see [Network](#network)); when there is one, the popover and Settings → General show "Version X is available" with its release notes and **Update and Relaunch**. Nothing installs until you press it. If a scan or a clean is running, the update waits for it to finish, then the app quits, is replaced and opens again. A copy dragged to Applications by hand updates the same way.

`brew upgrade` works too, so a Homebrew install that only uses the command line stays current. The cask says `auto_updates true`: brew upgrades the app when the installed copy is older than the cask, and leaves it alone once the app has updated itself (brew's own list may then still show the older version). If you set `HOMEBREW_NO_UPGRADE_AUTO_UPDATES_CASKS`, brew skips the app; update it with `brew upgrade --greedy deepdrivecleaner`. An upgrade through brew quits the app; it opens again at your next login if Open at login is on, or when you open it. Do not run it while the app is cleaning.

To go back to an older version: download its zip from the [releases](https://github.com/SmartAndPoint/DeepDriveCleaner/releases) and replace the app in Applications with it, or install an older cask with `brew reinstall`. Then switch off Check for updates daily in Settings → General, or the app offers the newer version again.

To remove: `brew uninstall --zap --cask deepdrivecleaner` removes the app, the commands, the background agent, the saved scan, the settings, the logs and the update cache. Without `--zap` they stay, and the app's entry in Login Items can stay until macOS drops it. If you use the background scan and uninstall without `--zap`, run `ddc schedule uninstall` first.

If you installed the command line with the script below, remove it before you install the cask (`curl -fsSL https://raw.githubusercontent.com/SmartAndPoint/DeepDriveCleaner/main/packaging/install/install.sh | sh -s -- --uninstall`). The cask cannot check for it, and two `ddc`s would land on your `PATH`.

### On an older macOS, or the command line only

The cask needs macOS 26. On an older macOS, `brew` keeps 0.1.0 (the command line only).

On macOS 26 or later on Apple Silicon, the command-line tool alone, without the app and without Homebrew:

```
curl -fsSL https://raw.githubusercontent.com/SmartAndPoint/DeepDriveCleaner/main/packaging/install/install.sh | sh
```

The script installs `deepdrivecleaner` into `~/.local/bin`, with `ddc` as a symlink to it. It downloads the latest release's `deepdrivecleaner-<version>-aarch64-apple-darwin.tar.gz` and checks it before it installs anything: against the release's `SHA256SUMS`, then that the binary is signed by the SmartAndPoint Developer ID, then that it runs and reports that version. It never uses `sudo` and never asks anything. If the folder is not on your `PATH`, it prints the line to add. The script is `packaging/install/install.sh` in this repository, and each release's tag holds its own copy; `https://ddc.sh/install.sh` will serve the same script. Options go after `sh -s --`:

```
curl -fsSL https://raw.githubusercontent.com/SmartAndPoint/DeepDriveCleaner/main/packaging/install/install.sh | sh -s -- --prefix "$HOME/bin"
```

| Option | What it does |
|---|---|
| `--version <v>` | Install release `v<v>` instead of the latest. A pre-release is installed only this way. |
| `--prefix <dir>` | Install into `<dir>` instead of `~/.local/bin`. |
| `--force` | Install even when another copy is found, or reinstall the current version. |
| `--uninstall` | Remove the two files from the folder. If `ddc schedule install` runs that binary, it stops and asks you to run `ddc schedule uninstall` first. |
| `--modify-path` | Add the folder to `PATH` in your shell's profile (one line, added once). |

- **To update,** run it again. It replaces the binary at the same path in one step, so a Full Disk Access grant and a `ddc schedule` agent keep working. Moving the binary loses the grant.
- **Another copy:** if `deepdrivecleaner` or `ddc` already resolves on your `PATH` to another folder (the cask, `cargo install`, `make install`), or `DeepDriveCleaner.app` is in `/Applications` or `~/Applications`, it installs nothing, names that copy and how to update or remove it, and exits 4. Two copies would mean two `ddc`s and two schedulers.
- **Releases:** it installs releases from 0.8.0 on; earlier ones carry no `SHA256SUMS` to check against. It refuses an Intel Mac and a macOS older than 26.
- **Linux:** the script runs on x86_64 and aarch64 Linux and says "not released for Linux yet" until a release carries the Linux archives. On Linux it checks the checksum only: a checksum proves the download is intact, not who published it. To check that a Linux archive was built by this repository's release workflow, use the manual route below with `gh attestation verify`.

**By hand.** Download the archive and `SHA256SUMS` from the [release](https://github.com/SmartAndPoint/DeepDriveCleaner/releases), then:

```
shasum -a 256 -c --ignore-missing SHA256SUMS       # Linux: sha256sum -c --ignore-missing SHA256SUMS
tar -xzf deepdrivecleaner-<version>-aarch64-apple-darwin.tar.gz deepdrivecleaner
mkdir -p ~/.local/bin && mv deepdrivecleaner ~/.local/bin/
ln -s ~/.local/bin/deepdrivecleaner ~/.local/bin/ddc
```

A file a browser downloads is quarantined. The bare binary is notarized but cannot carry a stapled ticket, so its first run asks Apple's servers online; `curl` sets no quarantine. On Linux, `gh attestation verify <archive> --repo SmartAndPoint/DeepDriveCleaner` checks the archive's provenance.

### With cargo

From source, the current `main`:

```
cargo install --git https://github.com/SmartAndPoint/DeepDriveCleaner --locked ddc-cli
ln -s ~/.cargo/bin/deepdrivecleaner ~/.cargo/bin/ddc
```

With [cargo-binstall](https://github.com/cargo-bins/cargo-binstall), the release archive of `main`'s version instead of a build:

```
cargo binstall --git https://github.com/SmartAndPoint/DeepDriveCleaner ddc-cli
ln -s ~/.cargo/bin/deepdrivecleaner ~/.cargo/bin/ddc
```

binstall reads the version from `main`. Between a version bump and its release there is no archive yet, and it compiles instead; maintainers bump, tag and release in one sitting to keep that window short.

### From source

You need [rustup](https://rustup.rs) (the toolchain is pinned in `rust-toolchain.toml`).

```
git clone https://github.com/SmartAndPoint/DeepDriveCleaner.git
cd DeepDriveCleaner
make install
```

This installs `~/.cargo/bin/deepdrivecleaner` and a `ddc` symlink to it. A local build is not signed with the Developer ID, so macOS ties privacy grants such as Full Disk Access to that one build. Building the app needs Xcode and a Developer ID (`make app`).

## The menu bar app

- **The popover** (click the icon) shows the last scan: what can be freed by risk class, the biggest finds, when the last scan ran, and Scan Now. The icon shows a turning arc while a scan or a clean runs, shows a small warning when something needs you (Full Disk Access, a failed scan), and goes grey while scans are paused (Pause Scans, in the icon's right-click menu).
- **The Review window** lists every finding by group (a sidebar ordered by size, per risk class) or as one list you can filter and sort. You tick what to clean; safe and idle rebuildable items start ticked, review items never do. Clean N items… at the bottom cleans every tick; Clean N from <group>… in a group's Select all row cleans only that group's. Right-click a folder to show it in Finder or protect it with a `.ddc-keep` file.
- **Whole disk** (Where Does the Rest Go? in the popover, or the last row of the Review sidebar) shows the startup disk as one bar: the home folder, with what the last scan can free in its class colours, then apps, system data, Homebrew, conda, simulator runtimes and what could not be counted, one row each. Scan Whole Disk runs `ddc disk` beside any scan or clean, never on its own, and Stop keeps what it measured so far. Under the rows, **Coding agents** shows what Claude Code, Codex, Cursor and opencode keep and their live sessions, from `ddc disk agents`; it shows project folders and session names. Some rows offer their owning tool's cleanup: Delete Runtime… on a downloaded simulator runtime no simulator runs on, Clean Up Homebrew…, and for a conda install in /opt, Clean Package Cache… and Remove Environment… on each of its environments, listed under the Conda row (none on a folder your user cannot write). Each opens a sheet with the tool's own dry run and the exact command, and runs only after you confirm (`ddc disk free`); nothing else in this view removes anything.
- **The confirmation sheet** lists every item with its action, checks each one with a dry run first, and names what would be skipped and why. Review items go to the Trash unless you choose Delete permanently for one. After the clean it shows what was deleted, moved to the Trash or skipped, grouped by cause.
- **Your own files.** Downloads, Pictures, Movies, Music and iPhone and iPad backups list what they hold, largest first. Nothing in them is ever selected for you; you pick files and folders one by one, and they go to the Trash, never deleted. App libraries (Photos and others), partial downloads, unfinished backups and anything changed in the last 10 minutes are locked. Cloud folders (iCloud Drive and other File Provider folders) are not scanned.
- **Settings** has General (Open at login, the icon, the daily update check, when it last ran, and Check Now), Folders, Schedule, Automation, Types and Permissions.
- **Updates.** When a new version is out, a line above the popover's footer says so, with its release notes and Update and Relaunch. See [Install](#install) and [Network](#network).
- **In the Dock while a window is open.** While the Review window, Settings or About is open, the app is in the Dock and in Cmd-Tab, with its own menu bar (⌘W, ⌘Q, copy and paste in the search field). When the last window closes, it lives in the menu bar only again and the app you were using comes back to the front.

The app runs the same command-line tool for every scan and clean, so everything below applies to it too.

## The command line

Every command is `ddc <command>`, or `deepdrivecleaner <command>`. `ddc --help` and `ddc <command> --help` print the options; `ddc --version` prints the version.

When it starts in a terminal, `ddc` first prints a two-line warning to stderr: it deletes files, can remove almost anything it is told to, and is used at your own risk, with a link to the [Disclaimer](DISCLAIMER.md). Every command shows it except `--version`. It appears only when stderr is a terminal: the app, the background agent, pipes and redirects never see it, so `--json` output and scripts are unchanged. `NO_COLOR` turns off its color.

### Quick start

```
ddc scan                 # what is there, read-only
ddc clean --dry-run      # the plan, without changing anything
ddc clean                # review the plan, then clean what you confirm
```

### `ddc scan`: find what can be freed

Read-only: it never changes a file. It walks your home folder (or the paths you give), groups findings by risk class and type, and prints the largest per group. It saves the report as the last scan, which the app shows.

```
ddc scan [PATH…] [--json] [--all] [--exclude PATH]… [--skip PATH]… [--min-age DAYS] [--data-dir DIR] [--threads N]
         [--progress json]
```

| Option | Meaning |
|---|---|
| `PATH…` | Folders to scan. Default: the `roots` in `settings.json`, else your home folder. |
| `--json` | Print the full report as JSON ([`docs/scan.schema.json`](docs/scan.schema.json)) instead of the summary. |
| `--all` | List every finding, not only the largest ones per group. |
| `--exclude PATH` | Never touch this folder: it is reported as protected user data. Repeatable. Replaces the `excludes` of `settings.json` (see [Settings](#settings)). |
| `--skip PATH` | Never read this folder: it is not scanned and never cleaned, and the report lists it as not scanned. Repeatable. Adds to the `skips` of `settings.json`. |
| `--min-age DAYS` | A project untouched for at least this many days has its rebuildable artifacts selected. Default: `settings.json`, else 14. |
| `--data-dir DIR` | Where the last scan, the settings and the lock live (default `~/Library/Application Support/DeepDriveCleaner`). Logs then go to `DIR/logs`. The `DDC_DATA_DIR` environment variable does the same. |
| `--threads N` | Worker threads. Default: one per core. |
| `--progress json` | Write progress to stderr as events, one JSON line each ([`docs/progress.schema.json`](docs/progress.schema.json)): the files, folders and bytes read so far, at most every 100 ms, the last line with the report's totals. Other stderr lines start with `ddc:`. Without it, a terminal shows a live line instead. |

```
ddc scan                         # your home folder
ddc scan ~/Projects ~/Work --all # two folders, every finding
ddc scan --exclude ~/Projects/keep-this
ddc scan --json > report.json    # the full report for scripts
```

Without Full Disk Access some of `~/Library` cannot be read; the report lists those folders instead of guessing.

### `ddc clean`: clean what you confirm

`ddc clean` scans again (it never trusts an old report), shows a cleanup plan grouped by class and type, and lets you change it before anything happens:

- type group numbers (`2 5`) to toggle groups, `l N` to list group N, `t N.M` to toggle item M of group N, Enter to go on, `q` to quit;
- each `review` item is asked about one by one (`[y/N]`): the Trash by default, deleted permanently with `--permanent`;
- a last question shows the count, the size and what it frees; only `y` cleans.

Right before touching an item it checks it again (see [Safety](#safety)) and skips it with a reason if anything changed. Every action is written to the journal.

```
ddc clean [PATH…] [--dry-run] [--yes] [--class CLASS]… [--category CATEGORY]… [--min-size SIZE]
          [--min-age DAYS] [--permanent] [--exclude PATH]… [--skip PATH]… [--data-dir DIR] [--progress json]
```

| Option | Meaning |
|---|---|
| `PATH…` | Folders to scan, as for `scan`. |
| `--dry-run` | Show the plan and exit; nothing is changed. |
| `--yes` | Clean the default-selected items without asking, for scripts. Only the classes you name with `--class` (default `safe`); never `review`, never a tool's prune command for VMs. |
| `--class CLASS` | Only these classes: `safe`, `rebuildable`, `review`. Repeatable. |
| `--category CATEGORY` | Only these categories: `project-artifact`, `package-cache`, `xcode`, `vm-container`, `toolchain`, `ml-model`, `ide`, `app-cache`, `generic-cache`. Repeatable. |
| `--min-size SIZE` | Skip findings smaller than this: `512K`, `100M`, `2G`, `1T`, or bytes. Applies on top of `min_size` in `settings.json`. |
| `--min-age DAYS` | As for `scan`. |
| `--permanent` | Delete `review` items instead of moving them to the Trash. |
| `--exclude PATH` | Never touch this folder. Repeatable. |
| `--skip PATH` | Never read this folder. Repeatable. Adds to the `skips` of `settings.json`. |
| `--data-dir DIR` | As for `scan`. |
| `--progress json` | Write progress to stderr as events, one JSON line each ([`docs/progress.schema.json`](docs/progress.schema.json)): the phase (`preparing`, then `cleaning` right before the first item is touched), the re-check and keep-marker walks, item `i` of `n` with the bytes processed, and one line per item with its outcome. Without it, a terminal shows a live line: what is checked while preparing, then the item, the bytes processed of the total and the path or the tool that runs. |

```
ddc clean --dry-run                          # what would happen
ddc clean                                    # interactive
ddc clean --yes                              # safe caches only, no questions
ddc clean --yes --class safe --class rebuildable --min-size 100M
ddc clean ~/Projects --class rebuildable     # only project artifacts under ~/Projects
ddc clean --category ml-model --permanent    # review ML models one by one, delete for good
```

Ctrl-C during a clean stops it after the current item; a tool it runs (such as `uv cache clean`) gets the Ctrl-C too.

#### `ddc clean --items`: clean an exact list

This is how the app cleans, and how a script can clean exactly what a person confirmed elsewhere. The list is the selection and the consent: no class filter, no default selection, no questions, no terminal needed. It checks every item again; anything that changed is skipped with a reason. It reads only the listed items and what lies above and beside them, unless the list holds a toolchain version (nvm, pyenv, rustup), whose pins can sit in any project: then it rescans the whole home. Afterwards it updates `last-scan.json` with what it did, so no scan is needed (`last_scan_saved` in the JSON report).

```
ddc clean --items FILE [--json] [--dry-run] [PATH…] [--exclude PATH]… [--skip PATH]… [--data-dir DIR] [--progress json]
```

| Option | Meaning |
|---|---|
| `--items FILE` | The list, as JSON; `-` reads it from stdin. It cannot be combined with `--yes`, `--permanent`, `--class`, `--category` or `--min-size`. |
| `--json` | Print the result as JSON ([`docs/clean.schema.json`](docs/clean.schema.json)). With `--dry-run`, one JSON line per item, then a summary. |
| `--dry-run` | Predict each item (would delete, would move to the Trash, would run its tool, would skip and why) without a rescan and without changing anything. |

Each item names a finding of a scan report exactly: its path, `dev` and `ino`, rule, class and action.

```json
{
  "schema_version": 1,
  "items": [
    { "path": "/Users/me/p1/node_modules", "dev": 16777232, "ino": 4211, "rule": "node.node_modules",
      "class": "rebuildable", "action": "delete" },
    { "path": "/Users/me/Downloads/old.iso", "dev": 16777232, "ino": 9012, "rule": "user.downloads",
      "class": "user-data", "action": "trash" }
  ]
}
```

- `"permanent": true` is allowed only on a `review` item listed as `delete`; anywhere else the whole list is refused (exit 2) before anything runs.
- A `user-data` item is a file or folder directly inside Downloads, Pictures, Movies, Music or an iPhone or iPad backup folder, as `ddc scan --json` lists it under `contents`. It goes to the Trash only. It is skipped if it is locked now, is no longer an entry of its folder, or changed since the scan.
- If the list holds a folder and an item inside it, the folder goes first; the item inside is skipped once the folder is in the Trash.

```
ddc scan --json | jq '…' > list.json     # pick items from a report
ddc clean --items list.json --dry-run    # what would happen to each
ddc clean --items list.json --json       # clean them, report as JSON
```

### `ddc rules`: what it looks for

```
ddc rules [--json] [--platform macos|linux|windows]
```

Lists every rule by group: what each type of finding is, its class, how it is removed, and under it the folders in your home it looks at (`~/…/*` claims each child, `~/…/…/name` a folder of that name anywhere below). `settings.json` can switch a rule off (`disabled_rules`). `--json` follows [`docs/rules.schema.json`](docs/rules.schema.json). The rules are those of the platform ddc runs on; `--platform` prints another platform's, for review.

```
ddc rules                                         # every rule, by group
ddc rules --json | jq -r '.rules[] | .id'         # rule ids for disabled_rules
ddc rules --platform linux                        # the rules ddc would use on Linux
```

### `ddc disk`: what uses the whole disk

```
ddc disk [--json] [--progress json] [--threads N] [--data-dir DIR]
```

Shows what uses the startup disk, outside your home folder too: Applications, the shared Library, macOS's downloaded data, Homebrew, Conda, `/usr/local`, simulator runtimes, temporary folders, the sleep image and the other users. Each row says what it is and what frees it. It only measures: nothing in it is a finding, and it removes nothing.

- The home folder row comes from the last `ddc scan` when that scan covered the whole home folder, on this disk, within a day; otherwise `ddc disk` measures it, without opening cloud folders or your skips.
- **Not counted** is what the Data volume uses minus everything counted: the Spotlight index, folders only root can read, APFS metadata. When more was counted than the volume holds, it says why.
- Temporary folders are read up to a budget of entries; past it the row reads "at least".
- Simulator runtimes are listed only when Xcode has finished its first launch, so the report never asks for a password.
- `--json` follows [`docs/disk.schema.json`](docs/disk.schema.json); the report is also saved as `last-disk.json` in the data directory. `--progress json` writes walk events with `"step": "disk"`.

```
ddc disk                                           # the table
ddc disk --json | jq '.areas[] | {id, bytes}'      # area sizes
```

`ddc disk agents` shows what coding-agent harnesses keep under your home folder: Claude Code, Codex, Cursor and opencode, with each one's folders and sizes, what of it runs, its live sessions and how many other transcripts it keeps. A Claude Code session is live while the process that wrote its record runs; a Codex thread is open while a process holds its lock file. It reads only the session records' named fields, the names of lock files and transcripts, the process list and sizes: never a transcript's content or a harness database. Its output shows project folders and session names. `--no-sizes` lists the sessions without measuring the folders, in a fraction of a second; `--json` follows [`docs/agents.schema.json`](docs/agents.schema.json). It saves nothing and removes nothing.

```
ddc disk agents [--json] [--no-sizes] [--threads N] [--data-dir DIR]
ddc disk agents --no-sizes                         # live sessions only
```

`ddc disk brew-packages` lists the Homebrew formulae you installed, largest first. Each size is measured by ddc from the formula's kegs in the Cellar, every installed version on its own line; brew's own list has no sizes. Under each formula it names the dependencies only it needs and their size, and says why its removal would not be offered: another formula or a cask needs it, it is pinned, or a `brew services` job is registered for it. It asks `/opt/homebrew/bin/brew` for its installed formulae (`brew info`, which can take many seconds) and its service jobs (`brew services list`), with `HOMEBREW_NO_AUTO_UPDATE=1`. `--json` follows [`docs/brew-packages.schema.json`](docs/brew-packages.schema.json). It saves nothing and removes nothing.

```
ddc disk brew-packages [--json] [--data-dir DIR]
```

`ddc disk free` frees three areas with their owning tool, and only what you confirm. It deletes a simulator runtime with `simctl runtime delete <identifier>`, runs `brew cleanup` with `HOMEBREW_NO_AUTOREMOVE=1`, which removes old versions in the Cellar and old downloads, uninstalls a Homebrew formula the app's list of packages confirmed with `brew uninstall`, formulae only, and runs an `/opt` conda install's own `bin/conda`: `conda env remove -p <path>` for one environment, and `conda clean` of the unused packages, the downloads, the index cache and the logs, with `CONDA_PKGS_DIRS` set to the install's `pkgs` alone. At a terminal it lists the actions available now with each tool's own dry run, then asks for each one, default No. Without a terminal it needs `--actions`, the exact list the app's sheet confirmed ([`docs/disk-free.schema.json`](docs/disk-free.schema.json)); no option skips the question.

- A runtime is offered only when it lies under `/System/Library/AssetsV2/`, macOS says it can be deleted, and no simulator on it is booted. Xcode downloads its SDK's runtime again at the next build for it.
- Right before each action it checks again where the runtime or Homebrew lives, and refuses with a reason when that changed. After the action it measures the folders again: freed, removed but not freed, or still deleting when CoreSimulator has not finished yet.
- A conda environment is offered only when it lies in an `/opt` install's `envs` folder, conda lists that folder, it is not the base, your user can write it, and no running process uses it. The package cache is offered only when conda lists it, no install links packages by symlink, and conda's package folders can be limited to it. A folder your user cannot write is refused before conda runs: conda would need an administrator.
- A formula is uninstalled only from the list the app confirmed, never offered at the terminal: by its full name, every installed version of it, and then, when you confirm them too, the dependencies nothing needs any more. Right before brew runs, ddc asks `brew info` and `brew services list` again and refuses a formula that is gone, pinned, has other versions than you saw, is needed by another formula or a cask, or has a `brew services` job. It never uninstalls a cask, never zaps, and never ignores dependencies. brew runs with `HOMEBREW_NO_AUTOREMOVE=1` and `HOMEBREW_NO_AUTO_UPDATE=1`, and only when the check left at least a minute of the timeout.
- When a tool fails for lack of permission, the report gives the same command to paste into Terminal. `ddc` never asks for a password.
- Every action is logged to `actions.jsonl`. `last-disk.json` marks the area as measured before the action, and `brew cleanup` measures the Homebrew cache finding of the last scan again.
- `--dry-run` runs only each tool's own dry run: `simctl runtime delete <identifier> --dry-run`, `brew cleanup -n` and `conda clean`'s own dry run; an environment's dry run measures the bytes only it holds, and a formula's is the check before `brew uninstall`, with each version measured.

```
ddc disk free [--actions FILE|-] [--dry-run] [--json] [--data-dir DIR]
ddc disk free                                      # asks for each action
ddc disk free --dry-run                            # what each tool would remove
```

### `ddc keep`: remove a folder's protection

A `.ddc-keep` file keeps its folder, and everything in it, out of every plan: the scan lists it under "Protected by you". `ddc keep remove <folder>` removes that file from one folder and nothing else; the folder and what lies in it stay. The next scan lists the folder in its own group, where it can be cleaned with that group's checks, and may be selected for cleanup by default. To keep it again, put the file back (the app's Protect This Folder).

- Give the folder that holds the file as an absolute path, as the scan report names it. At a terminal, `ddc keep remove` asks first, default No. Without a terminal it needs `--folder-dev` and `--folder-ino`, the folder's device and inode as the scan report recorded them; the app passes them from its Remove Protection… confirmation.
- It takes the lock a clean takes, so it waits for no scan or clean: it exits 75 while one runs.
- It refuses a folder outside the folders ddc scans (`roots` in the settings, or your home folder), a folder that is no longer the one the report saw (another device or inode, or now a symlink), a folder that is not there, and a `.ddc-keep` that is a symlink or a folder rather than a file: remove that one in Finder. A folder with no `.ddc-keep` any more is reported as such.
- Every decision is logged to `actions.jsonl`, refusals included.
- An exclusion or a skip is removed in the settings (`excludes`, `skips`), not here.

```
ddc keep remove FOLDER [--folder-dev DEV --folder-ino INO] [--json] [--data-dir DIR]
ddc keep remove ~/Projects/app/node_modules         # asks first
```

`--json` prints the report ([`docs/keep.schema.json`](docs/keep.schema.json)): the folder, its device and inode, the rule the file kept when the last scan says it, and the result: `removed`, `absent`, `refused` with a `reason` (`outside-scan-roots`, `changed-since-scan`, `no-folder`, `not-a-file`) or `failed` with an `error`.

### `ddc schedule`: scan in the background

```
ddc schedule install [--interval daily|weekly] [--threshold SIZE]
ddc schedule status
ddc schedule uninstall
```

- `install` installs (or updates) a LaunchAgent that scans daily, or weekly with `--interval weekly`. It notifies when at least `--threshold` can be freed (default: the settings' threshold, else 10G), or when a scanned disk is under 10 % free with something on it to clean. A threshold notification repeats after a fifth more or a week; a disk under 10 % free notifies at every scheduled scan until it is cleaned (`notify_low_disk: false` in `settings.json` turns that off). The background scan **never deletes anything**, and it skips the folders where macOS would show a permission prompt, because a prompt would block it. Once the Mac app runs the scheduled scan itself (0.3.0), `install` refuses: one scheduler at a time.
- `status` shows whether the agent is installed, loaded and runs this binary.
- `uninstall` removes the agent.
- The Mac app's own schedule (Settings → Schedule) replaces this agent: the app offers Replace It while the agent exists, and `ddc schedule install` refuses while the app schedules.

```
ddc schedule install --interval weekly --threshold 20G
ddc schedule status
```

`ddc notify` is internal, for the Mac app's notifications (0.3.0), and hidden from `--help`.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Done. `clean --items --json`, `disk free --json` and `keep remove --json` exit 0 even when an item or action failed or was refused: the report says which. |
| 1 | An error; in text mode, `clean` also exits 1 when an item failed, `disk free` when an action failed, and `keep remove` when the file was not removed for a reason other than being gone already. |
| 2 | Refused before anything was touched: run as root, a malformed list, a forbidden option, or a confirmation that cannot be asked without a terminal. |
| 69 | Unavailable: ddc does not support this platform yet, where only `--version`, `--help` and `rules` run, or the command needs something this platform lacks. Nothing was scanned or changed. |
| 75 | Busy: a clean is running, or a clean was asked for while another scan or clean runs. Try again when it ends. |

With `--json` (and `scan --notify json`), a command refused with 69 prints an error report on stdout instead of its usual output ([`docs/error.schema.json`](docs/error.schema.json)): a `code`, `platform-not-supported` or `capability-absent`, the platform and, for the latter, the capability that is missing. The scan, clean and disk reports name their `platform` and its `capabilities`.

The install script (under [Install](#on-an-older-macos-or-the-command-line-only)) has its own:

| Code | Meaning |
|---|---|
| 0 | Installed, already current, or uninstalled. |
| 1 | Any other error, such as a folder it cannot write. |
| 2 | Unsupported platform, not released for it yet, or a bad option. |
| 3 | Verification failed (checksum, archive contents, signature or `--version`), or the release carries no `SHA256SUMS`. Nothing was installed. |
| 4 | Another install was found, and named; `--force` installs anyway. With `--uninstall`: the background agent still runs that binary, or the folder holds a `ddc` the script did not create. |
| 5 | Download failed, or there is no such release. |

### Where things live

| Path | What |
|---|---|
| `~/Library/Application Support/DeepDriveCleaner/last-scan.json` | The last scan's report, which the app shows. `ddc clean --items` edits it after cleaning instead of scanning again; the next scan replaces it. |
| `~/Library/Application Support/DeepDriveCleaner/last-disk.json` | The last `ddc disk` report; the next one replaces it. |
| `~/Library/Application Support/DeepDriveCleaner/settings.json` | Settings shared by the app and the terminal (below). |
| `~/Library/Application Support/DeepDriveCleaner/notify-state.json` | When the background scan last notified, so it does not repeat itself. |
| `~/Library/Application Support/DeepDriveCleaner/lock` | Scans share it; a clean takes it alone. |
| `~/Library/Logs/DeepDriveCleaner/actions.jsonl` | The journal: every deletion, move to the Trash, tool run and `keep remove` decision, one JSON line each. |
| `~/Library/Logs/DeepDriveCleaner/background.log` | What the scheduled scans found; `background.out.log` and `background.err.log` hold their raw output. |
| `~/Library/Logs/DeepDriveCleaner/app.log` | What the menu bar app did: each run of the command-line tool and its outcome. |
| `~/Library/LaunchAgents/com.smartandpoint.deepdrivecleaner.plist` | The background agent of `ddc schedule install`. |

`--data-dir DIR` or `DDC_DATA_DIR=DIR` moves the data files to `DIR` and the command-line tool's logs to `DIR/logs`, which is how to try `ddc` without touching your real ones.

### Settings

`~/Library/Application Support/DeepDriveCleaner/settings.json` holds what the Mac app's Settings and the terminal share ([`docs/settings.schema.json`](docs/settings.schema.json)). Every field is optional except `schema_version`:

| Field | Meaning | Flag that wins over it |
|---|---|---|
| `roots` | Absolute folders to scan; empty means your home folder. A folder that does not exist, such as an unplugged disk, is skipped with a note. | `PATH…` |
| `excludes` | Absolute folders never touched, reported as user data. | `--exclude` |
| `skips` | Absolute folders never read: not scanned, never cleaned, listed as not scanned. A finding that holds one is kept whole. | `--skip` adds to it |
| `min_age_days` | Idle days before a project's artifacts are selected (default 14). | `--min-age` |
| `disabled_rules` | Rule ids from `ddc rules` switched off: their findings are reported as ignored and never selected or counted. | none |
| `min_size` | Bytes: smaller findings are ignored. `clean --min-size` filters the plan on top of it, so the larger of the two applies. | none |
| `notify_threshold`, `notify_low_disk` | When the background scan notifies (default 10G, and low disk on). | `--threshold` |

`--skip` only adds to the skips from the file, and `--exclude` never drops them. `--exclude` replaces the excludes from the file. When that would leave a folder from the file unprotected, ddc names it first: `ddc clean` asks in a terminal and refuses without one; `ddc scan` and `ddc clean --dry-run` only warn, since they change nothing. A malformed file is an error that names it.

## Risk classes

| Class | What it is | Default |
|---|---|---|
| `safe` | Caches that refill themselves (`~/.npm/_cacache`, `~/.cache/uv`, Homebrew, pip, Gradle, …). | Selected. Removed with the tool's own command when it has one (`uv cache clean`, `go clean -modcache`, …). |
| `rebuildable` | Project artifacts an install or build restores: `.venv`, `node_modules`, `target/`, `.next`, CMake build folders, … | Selected only when the project has been idle for `--min-age` days (default 14) and has a lock file. |
| `review` | Expensive or uncertain to restore: environments without a lock file, global packages, toolchains, models, VM images. | Never selected. You choose each item; it goes to the Trash unless you pass `--permanent`. VM runtimes are cleaned only through their own prune commands. |
| `user-data` | Pictures, movies, music, Downloads, iPhone backups, anything protected by `--exclude` or a `.ddc-keep` file. Developer artifacts inside Downloads are still found as their own items. | Never selected, and never in `ddc clean`'s plan. Files and folders of Downloads, Pictures, Movies, Music and iPhone and iPad backups that you pick one by one (`ddc clean --items`, as the menu bar app sends them) go to the Trash only, never deleted. |

## Safety

- Right before touching anything, `ddc clean` checks every item again. It is still the same directory (device and inode), no symlink has appeared above it, it still matches its rule, it is not a project root, it is not protected, and no running process uses it: runs from it, works in it, or has a file open in it. A process that holds a file only for a moment does not block it, and before a tool's own clean command (such as `uv cache clean`) the check runs again, so the tool does not wait on a lock that a running process already holds. A move to the Trash ignores open files, since a file open in the Trash keeps working, and Docker's and Podman's prune commands decide for themselves what is in use. A file or folder you picked in one of your own folders is checked as that folder's entry instead: the same file or folder, still in that folder, not locked (an app's library, a `.ddc-keep`, a download still arriving, something changed in the last 10 minutes). Anything that fails a check is skipped with a reason.
- Deletion first renames the folder to a tombstone through a verified parent folder, then removes it. An interrupted run leaves `*.ddc-deleting-*`, which the next scan finds as `safe`.
- Every action is logged to `~/Library/Logs/DeepDriveCleaner/actions.jsonl`.
- It never runs as root, never deletes your home, `~/Documents`, `~/Projects` or any other protected folder, and never follows a symlink out of a finding.
- To keep a folder out of every plan for good, put an empty `.ddc-keep` file in it (the app's Protect This Folder does that), or add it to `excludes`. `ddc keep remove` removes that file again, and only that file.
- APFS frees space asynchronously, and local Time Machine snapshots keep deleted blocks until macOS thins them. `ddc clean` says so when snapshots exist.

These checks lower the risk; they do not remove it. See the [Disclaimer](#disclaimer).

## Full Disk Access

Without Full Disk Access some of `~/Library` cannot be read; the report lists those folders instead of guessing. Grant it to the app: System Settings → Privacy & Security → Full Disk Access → DeepDriveCleaner. The grant is tied to the app's identity (its Developer ID signature and bundle id) rather than to one copy of it, so it should survive an update, by Update and Relaunch or by `brew upgrade`: both replace the app with a copy of the same identity. If the app reports no Full Disk Access after an update, switch it off and on again in that list.

- Run from Terminal, `ddc` is covered by Terminal's own grant, not the app's.
- The app's own scheduled scans (Settings → Schedule) use the app's grant; they replace the agent of `ddc schedule install`, which refuses while the app schedules.
- The background agent of `ddc schedule install` runs the binary inside the app. To let it scan everything, add `/Applications/DeepDriveCleaner.app/Contents/Helpers/deepdrivecleaner` to Full Disk Access (press ⇧⌘G in the file picker to type the path). Since 0.2.0 that path stays the same across upgrades. If you upgraded from 0.1.0, remove the old `Caskroom/deepdrivecleaner/0.1.0` entry.
- A local `make install` build is not signed with the Developer ID and has to be granted again after every build.
- The install script keeps the binary at one path, `~/.local/bin/deepdrivecleaner` by default, and it is signed with the Developer ID: a grant to it survives updates by the script, and moving it loses the grant.

## Network

The app's only network requests of its own are for updates, made through [Sparkle](https://sparkle-project.org):

- **What.** One HTTPS request for `appcast.xml`, the list of releases, from github.com (GitHub redirects it to its download host). The first launch of a version with the updater checks at once, then at most once a day. Check Now in Settings → General makes one request, also with the daily check off. Update and Relaunch downloads the release's zip from the same hosts.
- **What GitHub sees.** Your IP address, the time, the `User-Agent` `DeepDriveCleaner` with no version, and your system language through the `Accept-Language` header. The app sends no headers or parameters of its own, and Sparkle's system profiling is off.
- **What the maintainers see.** GitHub's download counter for `appcast.xml`: a total count, nothing per user or per Mac.
- **To turn it off,** switch off Check for updates daily in Settings → General. Updates then come only from Check Now, or from `brew upgrade` for a Homebrew install.

## How it is built

A Rust workspace and a Swift app:

- `crates/ddc-core`: the library. A parallel filesystem walker over `getattrlistbulk(2)` (the only module with `unsafe` is `sys`), the rule catalog, the report model, the cleanup plan and the executor. It never prints and never reads stdin.
- `crates/ddc-cli`: the `deepdrivecleaner` binary.
- `app/`: the menu bar app (AppKit and SwiftUI) and `DDCKit`, its logic, tested with `swift test`. The app never deletes anything itself: it runs the CLI for every scan and clean, so the re-checks, the journal and the refusals stay in one place.

On the reference machine `ddc scan ~` takes 37 s for 5.5 million files and `ddc scan ~/Projects` takes 10 s, where `du -sk` needs 42 s for `~/Projects` alone. Why it is built this way is recorded in the ADRs: *Rust core: library + CLI with JSON output*, *Cleanup safety model: risk classes, plan and confirmation* and *Menu bar app: Swift shell over the ddc CLI*.

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

Once per clone, install the pinned check tools and the git hooks. The hooks run the gate before every commit and push:

```
make tools      # cargo-nextest, cargo-deny, cargo-machete, cargo-llvm-cov, the MSRV toolchain
make hooks      # git runs .githooks/: lint before a commit, gate-fast before a push
```

A commit or push that changes only paths on `.config/rust-free-paths` (the app's own sources, the website, the release notes) skips the Rust tiers in the hooks and in CI (`scripts/gate-scope.sh`): a green hook on such a push says nothing about Rust. `make tag` and `make dist` need the Rust jobs to have passed on the release commit itself. The M-size load trees run nightly.

CI also builds and tests the workspace natively on Linux and Windows (the required checks `check-x86_64-unknown-linux-gnu` and `check-x86_64-pc-windows-msvc`). A test of something not ported to that OS yet skips with a printed reason, and the job fails unless those skips match `.config/native-skips/<os>.txt`: a new skip is a reviewed line in the same change. `DDC_LINUX_HOST=<ssh host> scripts/native-linux.sh` runs the same Linux leg on a Linux machine with docker in minutes, instead of a CI round, and `DDC_WINDOWS_HOST=<ssh host> scripts/native-windows.sh` the Windows leg on a Windows machine prepared by `scripts/windows-host-setup.ps1` and `native-windows.sh --setup`. Both copy only tracked files.

`main` takes no direct push. Work on a branch started from `origin/main`, push it, open a pull request and let it merge itself once the Gate passes:

```
git fetch && git switch -c my-change origin/main
git push -u origin my-change   # the pre-push hook gates it
gh pr create --fill
gh pr merge --auto --rebase    # merges once scope, gate, coverage, app, release-build and the Linux and Windows checks pass
```

A rebase merge gives the commits new SHAs, so the next branch starts from a fresh `origin/main` again. A pull request from a fork gets no Gate run: a maintainer pushes its commits to a branch here to run it.

```
make lint       # fmt check, clippy -D warnings
make gate-fast  # lint, the unit tests (nextest, no load trees), doc tests
make gate       # lint, every test, cargo deny, unused dependencies, docs, the MSRV build, a release build (CI on main)
make check      # the same as make gate-fast
make coverage   # line coverage, written to target/lcov.info
make release    # optimized build
make install    # install deepdrivecleaner + ddc
make app-test   # the app's logic (DDCKit) under swift test, then the signing check
make ddckit-test # DDCKit's swift test alone
make app        # the app, Debug, signed with your Developer ID
make tag        # maintainers: after the bump's PR merged, main is pulled and its CI run is green, push the tag v<version>; release.yml builds the other targets from it
make dist       # maintainers: then, on the tagged commit whose own CI run is green, the signed, notarized CLI tarball and app zip (then scripts/publish-release.sh)
```

By submitting a contribution you agree that SmartAndPoint may use, modify and relicense it under any terms, including commercial ones, and you confirm that you have the right to grant this.

Links: [ProjectStore on GitHub](https://github.com/SmartAndPoint/ProjectStore) · [projectstore.dev](https://projectstore.dev)

## Disclaimer

DeepDriveCleaner deletes files and folders, and it can remove almost anything it is told to: by you, by a script, or by an AI agent acting for you. You use it entirely at your own risk. To the fullest extent the law allows, nobody who made it is responsible for what it removes: not SmartAndPoint, not the maintainer, not any developer or contributor. The full text is in [DISCLAIMER.md](DISCLAIMER.md); it adds to the license's "No Liability" section and never narrows it.

## License

[PolyForm Noncommercial 1.0.0](LICENSE.md). You may read, use, change and share this code for any noncommercial purpose: personal use, research, education, nonprofits. You may not use it commercially, including selling an app built on it. Strictly speaking this makes the project source-available rather than "open source" in the OSI sense: the code is open so that you can see what a tool that deletes files on your disk actually does.
