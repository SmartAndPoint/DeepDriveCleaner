//! Git, asked under one contract (ADR "Agent footprint: worktrees, harness
//! data and bounded consent for agents", §2, "The git probe contract", and
//! its amendment of 2026-11-01). An ordinary `git status` is not a read: it
//! runs the helper `core.fsmonitor` names and refreshes the index, whose
//! mtime is a worktree's activity. So every probe runs with
//! `--no-optional-locks`, the `-c` overrides of [`PROBE_CONFIG`], the
//! variables of [`PROBE_ENV`], and without any `GIT_*` variable ddc
//! inherited: run from a git hook, ddc must not act on the hook's
//! repository. A probe that fails in any way is [`Probe::Unknown`] and is
//! never retried with fewer guards.
//!
//! Which git runs is chosen here, never by a search of `PATH` alone: on
//! macOS `/usr/bin/git` is the `xcrun` shim that offers to install the
//! developer tools, so it is never run (decision 10 of story "Worktree
//! findings and provenance").

use std::path::{Path, PathBuf};
use std::time::Duration;

pub use crate::exec::PROBE_TIMEOUT;
use crate::exec::{CmdOutput, CommandRunner};
use crate::platform::Canonical as _;
use crate::platform::folders::{
    DEVELOPER_DIR_TOOL, GIT_PROGRAM as PROGRAM, GIT_SHIM_DIRS as SHIM_DIRS, GIT_SYSTEM_DIRS as SYSTEM_DIRS,
};

/// The `-c` overrides every probe carries. The command line outranks every
/// configuration file, so no repository or user setting can start a helper:
/// no file-system monitor, no untracked cache written, no hook, no
/// submodule summary, no pager, no password prompt.
pub const PROBE_CONFIG: [&str; 6] = [
    "core.fsmonitor=false",
    "core.untrackedCache=false",
    "core.hooksPath=/dev/null",
    "status.submoduleSummary=false",
    "core.pager=cat",
    "core.askPass=",
];

/// The variables every probe sets: no optional lock (the index is never
/// refreshed), no prompt on the terminal.
pub const PROBE_ENV: [(&str, &str); 2] = [("GIT_OPTIONAL_LOCKS", "0"), ("GIT_TERMINAL_PROMPT", "0")];

/// Every variable ddc inherited whose name starts with this is removed
/// before a probe starts: a git hook exports `GIT_DIR`, `GIT_INDEX_FILE`
/// and the like, which would point the probe at the hook's repository.
pub const CLEARED: &str = "GIT_";

/// More surviving commits than this are not passed on one command line:
/// the probe is unknown instead.
pub const MAX_SURVIVORS: usize = 10_000;

/// A probe's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe<T> {
    Known(T),
    /// It did not start, exited non-zero, timed out, or printed what the
    /// parser does not expect; why, in words.
    Unknown(String),
}

impl<T> Probe<T> {
    /// `Ok` for a known answer, the reason otherwise: for `?`.
    pub fn known(self) -> Result<T, String> {
        match self {
            Probe::Known(t) => Ok(t),
            Probe::Unknown(why) => Err(why),
        }
    }
}

impl<T> From<Result<T, String>> for Probe<T> {
    fn from(r: Result<T, String>) -> Self {
        match r {
            Ok(t) => Probe::Known(t),
            Err(why) => Probe::Unknown(why),
        }
    }
}

/// The git to run: the first `git` in `tool_dirs` (then the system's
/// folder, off macOS) outside the shim folders, else the developer
/// directory's (`xcode-select -p`, macOS only), else `None`: no probe runs.
pub fn locate(commands: &dyn CommandRunner, tool_dirs: &[PathBuf], timeout: Duration) -> Option<PathBuf> {
    let found = tool_dirs
        .iter()
        .map(PathBuf::as_path)
        .chain(SYSTEM_DIRS.iter().map(Path::new))
        .filter(|dir| !shim_dir(dir))
        .find_map(|dir| commands.at(&dir.join(PROGRAM)).filter(|exe| !shim(exe)));
    found.or_else(|| developer_git(commands, timeout))
}

/// The developer directory's git, run directly: the Command Line Tools' or
/// Xcode's (macOS only).
fn developer_git(commands: &dyn CommandRunner, timeout: Duration) -> Option<PathBuf> {
    let select = commands.at(Path::new(DEVELOPER_DIR_TOOL?))?;
    let out = commands.run_cleared(&select, &["-p".to_owned()], CLEARED, &[], timeout).ok().filter(|o| o.success)?;
    let dir = PathBuf::from(out.stdout.trim());
    if !dir.is_absolute() {
        return None;
    }
    commands.at(&dir.join("usr/bin").join(PROGRAM)).filter(|exe| !shim(exe))
}

fn shim_dir(dir: &Path) -> bool {
    SHIM_DIRS.iter().any(|s| dir == Path::new(s))
}

/// `exe`, or what it links to, lies in a shim folder.
fn shim(exe: &Path) -> bool {
    let real = exe.canonical().unwrap_or_else(|_| exe.to_path_buf());
    [exe, real.as_path()].iter().any(|p| p.parent().is_some_and(shim_dir))
}

/// What `status --porcelain=v1 -z --untracked-files=all --ignored=matching` says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// A tracked change or an untracked file.
    pub dirty: bool,
    /// The ignored paths, relative to the worktree, as git lists them (a
    /// folder ends with `/`); only read while the worktree is not dirty.
    pub ignored: Vec<String>,
}

/// Where `HEAD` points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// The commit, in hex.
    pub commit: String,
    /// The branch's full name (`refs/heads/x`); `None` when detached.
    pub branch: Option<String>,
}

/// One git, run under the contract.
pub struct Git<'a> {
    commands: &'a dyn CommandRunner,
    exe: PathBuf,
    timeout: Duration,
}

impl<'a> Git<'a> {
    pub fn new(commands: &'a dyn CommandRunner, exe: PathBuf, timeout: Duration) -> Self {
        Self { commands, exe, timeout }
    }

    pub fn exe(&self) -> &Path {
        &self.exe
    }

    /// The whole argument list of `git <args>` run in `dir`, under the contract.
    pub fn args(dir: &Path, args: &[&str]) -> Result<Vec<String>, String> {
        let dir = dir.to_str().ok_or_else(|| format!("{} is not UTF-8", dir.display()))?;
        let mut all = vec!["--no-optional-locks".to_owned()];
        for entry in PROBE_CONFIG {
            all.push("-c".to_owned());
            all.push(entry.to_owned());
        }
        all.push("-C".to_owned());
        all.push(dir.to_owned());
        all.extend(args.iter().map(|a| (*a).to_owned()));
        Ok(all)
    }

    /// Runs `git <args>` in `dir`; its output whatever its exit code.
    fn output(&self, dir: &Path, args: &[&str]) -> Result<CmdOutput, String> {
        let all = Self::args(dir, args)?;
        self.commands
            .run_cleared(&self.exe, &all, CLEARED, &PROBE_ENV, self.timeout)
            .map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))
    }

    /// Runs `git <args>` in `dir`; its stdout when it exits 0.
    pub fn run(&self, dir: &Path, args: &[&str]) -> Probe<String> {
        self.output(dir, args).and_then(|out| succeeded(args, out)).into()
    }

    /// Tracked changes, untracked files and ignored paths of the worktree
    /// at `dir`. Paths are NUL-separated (`-z`), never quoted.
    pub fn status(&self, dir: &Path) -> Probe<Status> {
        self.run(dir, &["status", "--porcelain=v1", "-z", "--untracked-files=all", "--ignored=matching"])
            .known()
            .and_then(|out| parse_status(&out))
            .into()
    }

    /// The commit `HEAD` names in `dir`, and its branch unless detached.
    pub fn head(&self, dir: &Path) -> Probe<Head> {
        self.run(dir, &["rev-parse", "HEAD", "--symbolic-full-name", "HEAD"])
            .known()
            .and_then(|out| parse_head(&out))
            .into()
    }

    /// The paths `.gitmodules` in `dir` lists; none without the file.
    pub fn submodule_paths(&self, dir: &Path) -> Probe<Vec<String>> {
        if std::fs::symlink_metadata(dir.join(".gitmodules")).is_err() {
            return Probe::Known(Vec::new());
        }
        let args = ["config", "--file", ".gitmodules", "--null", "--get-regexp", r"^submodule\..*\.path$"];
        let out = match self.output(dir, &args) {
            Ok(out) => out,
            Err(why) => return Probe::Unknown(why),
        };
        match out.code {
            // No key matches.
            Some(1) if out.stdout.is_empty() => Probe::Known(Vec::new()),
            Some(0) => parse_config_values(&out.stdout).into(),
            _ => Probe::Unknown(failed(&args, &out)),
        }
    }

    /// The commits that survive the removal of the worktree at `candidate`,
    /// asked in `from` (its main checkout, or any worktree of it): every
    /// branch but `branch`, every remote-tracking branch and tag, and the
    /// `HEAD` of every other worktree. Sorted, each once.
    pub fn survivors(&self, from: &Path, candidate: &Path, branch: Option<&str>) -> Probe<Vec<String>> {
        let refs = self.run(
            from,
            &["for-each-ref", "--format=%(objectname) %(refname)", "refs/heads", "refs/remotes", "refs/tags"],
        );
        let refs = match refs.known().and_then(|out| parse_refs(&out, branch)) {
            Ok(refs) => refs,
            Err(why) => return Probe::Unknown(why),
        };
        let heads =
            self.run(from, &["worktree", "list", "--porcelain"]).known().and_then(|out| other_heads(&out, candidate));
        let mut all = match heads {
            Ok(heads) => refs.into_iter().chain(heads).collect::<Vec<_>>(),
            Err(why) => return Probe::Unknown(why),
        };
        all.sort_unstable();
        all.dedup();
        Probe::Known(all)
    }

    /// How many commits `head` reaches that none of `survivors` does:
    /// `rev-list --count <head> --not <survivors…>`, never `--all` (it holds
    /// `HEAD`) nor `--branches` (it holds the candidate's own branch).
    pub fn own_commits(&self, from: &Path, head: &str, survivors: &[String]) -> Probe<u64> {
        if !is_object(head) || !survivors.iter().all(|s| is_object(s)) {
            return Probe::Unknown("not a commit".to_owned());
        }
        if survivors.len() > MAX_SURVIVORS {
            return Probe::Unknown(format!("{} references to compare, more than {MAX_SURVIVORS}", survivors.len()));
        }
        let mut args = vec!["rev-list", "--count", head, "--not"];
        args.extend(survivors.iter().map(String::as_str));
        self.run(from, &args)
            .known()
            .and_then(|out| out.trim().parse::<u64>().map_err(|_| format!("rev-list printed {:?}", short(&out))))
            .into()
    }
}

fn succeeded(args: &[&str], out: CmdOutput) -> Result<String, String> {
    if out.success { Ok(out.stdout) } else { Err(failed(args, &out)) }
}

fn failed(args: &[&str], out: &CmdOutput) -> String {
    let code = out.code.map_or_else(|| "a signal".to_owned(), |c| c.to_string());
    let said = out.stderr.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("");
    format!("git {} exited with {code}{}{}", args.first().unwrap_or(&""), if said.is_empty() { "" } else { ": " }, said)
}

/// The first 80 characters, for a message.
fn short(text: &str) -> String {
    text.chars().take(80).collect()
}

/// A full object name: 40 (SHA-1) or 64 (SHA-256) hex digits.
pub fn is_object(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn parse_status(out: &str) -> Result<Status, String> {
    let mut status = Status::default();
    if out.is_empty() {
        return Ok(status);
    }
    let garbage = || format!("status printed {:?}", short(out));
    let body = out.strip_suffix('\0').ok_or_else(garbage)?;
    let mut entries = body.split('\0');
    while let Some(entry) = entries.next() {
        let b = entry.as_bytes();
        let code = |c: u8| b" MTADRCU?!".contains(&c);
        if b.len() < 4 || !code(b[0]) || !code(b[1]) || b[2] != b' ' {
            return Err(garbage());
        }
        let (xy, path) = (&b[..2], &entry[3..]);
        if xy == b"!!" {
            status.ignored.push(path.to_owned());
        } else {
            status.dirty = true;
            // A rename or a copy names its source next.
            if xy.iter().any(|c| matches!(c, b'R' | b'C')) {
                entries.next();
            }
        }
    }
    Ok(status)
}

fn parse_head(out: &str) -> Result<Head, String> {
    let mut lines = out.lines().map(str::trim);
    let (Some(commit), Some(name), None) = (lines.next(), lines.next(), lines.next()) else {
        return Err(format!("rev-parse printed {:?}", short(out)));
    };
    if !is_object(commit) || !(name == "HEAD" || name.starts_with("refs/")) {
        return Err(format!("rev-parse printed {:?}", short(out)));
    }
    Ok(Head { commit: commit.to_owned(), branch: name.starts_with("refs/").then(|| name.to_owned()) })
}

/// `key\nvalue\0`, each entry.
fn parse_config_values(out: &str) -> Result<Vec<String>, String> {
    let body = out.strip_suffix('\0').ok_or_else(|| format!("config printed {:?}", short(out)))?;
    body.split('\0')
        .map(|entry| {
            entry.split_once('\n').map(|(_, value)| value.to_owned()).ok_or_else(|| format!("config printed {entry:?}"))
        })
        .collect()
}

/// `<object> <refname>` lines, every one but `skip`'s object.
fn parse_refs(out: &str, skip: Option<&str>) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    for line in out.lines().filter(|l| !l.is_empty()) {
        let (object, name) = line.split_once(' ').ok_or_else(|| format!("for-each-ref printed {:?}", short(line)))?;
        if !is_object(object) || !name.starts_with("refs/") {
            return Err(format!("for-each-ref printed {:?}", short(line)));
        }
        if Some(name) != skip {
            found.push(object.to_owned());
        }
    }
    Ok(found)
}

/// The `HEAD` of every worktree `worktree list --porcelain` lists but
/// `candidate`, which is recognized by file identity. A quoted path cannot
/// be compared: unknown.
fn other_heads(out: &str, candidate: &Path) -> Result<Vec<String>, String> {
    let garbage = |line: &str| format!("worktree list printed {:?}", short(line));
    let mut heads = Vec::new();
    for block in out.split("\n\n").filter(|b| !b.trim().is_empty()) {
        let mut lines = block.lines();
        let first = lines.next().unwrap_or("");
        let path = first.strip_prefix("worktree ").ok_or_else(|| garbage(first))?;
        if path.starts_with('"') {
            return Err(garbage(first));
        }
        let head = lines.find_map(|l| l.strip_prefix("HEAD "));
        match head {
            Some(head) if !is_object(head) => return Err(garbage(head)),
            Some(head) if !same(Path::new(path), candidate) => heads.push(head.to_owned()),
            // The candidate itself, or a bare repository (no `HEAD` line).
            _ => {}
        }
    }
    Ok(heads)
}

/// Both name one existing folder, by `(dev, ino)`.
fn same(a: &Path, b: &Path) -> bool {
    let id = |p: &Path| crate::platform::meta::stat(p).ok().map(|s| s.id());
    id(a).is_some_and(|x| Some(x) == id(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    /// One test per entry of the contract: each override is on every
    /// probe's command line, before the subcommand.
    #[test]
    fn the_fsmonitor_is_off() {
        assert_override("core.fsmonitor=false");
    }

    #[test]
    fn the_untracked_cache_is_off() {
        assert_override("core.untrackedCache=false");
    }

    #[test]
    fn no_hook_runs() {
        assert_override("core.hooksPath=/dev/null");
    }

    #[test]
    fn no_submodule_summary_runs() {
        assert_override("status.submoduleSummary=false");
    }

    #[test]
    fn no_pager_runs() {
        assert_override("core.pager=cat");
    }

    #[test]
    fn no_password_is_asked() {
        assert_override("core.askPass=");
    }

    #[test]
    fn the_contract_has_exactly_these_overrides() {
        assert_eq!(PROBE_CONFIG.len(), 6);
        assert_eq!(PROBE_ENV, [("GIT_OPTIONAL_LOCKS", "0"), ("GIT_TERMINAL_PROMPT", "0")]);
        assert_eq!(CLEARED, "GIT_");
    }

    fn assert_override(entry: &str) {
        assert!(PROBE_CONFIG.contains(&entry), "{entry}");
        let args = Git::args(Path::new("/r"), &["status"]).unwrap();
        let at = args.iter().position(|a| a == entry).unwrap_or_else(|| panic!("{entry}: {args:?}"));
        assert_eq!(args[at - 1], "-c");
        assert_eq!(args[0], "--no-optional-locks");
        assert!(at < args.iter().position(|a| a == "status").unwrap());
    }

    #[test]
    fn status_reads_dirt_and_ignored_paths() {
        assert_eq!(parse_status(""), Ok(Status::default()));
        let s = parse_status("!! .venv/\0!! .env\0").unwrap();
        assert_eq!((s.dirty, s.ignored), (false, vec![".venv/".to_owned(), ".env".to_owned()]));
        assert!(parse_status("?? new\0").unwrap().dirty);
        let renamed = parse_status("R  to\0from\0!! x\0").unwrap();
        assert_eq!((renamed.dirty, renamed.ignored), (true, vec!["x".to_owned()]));
        assert!(parse_status(" M a").is_err(), "no closing NUL");
        assert!(parse_status("garbage\0").is_err());
        assert!(parse_status("é! x\0").is_err());
    }

    #[test]
    fn head_is_a_commit_and_a_branch_or_detached() {
        let on = parse_head(&format!("{A}\nrefs/heads/x\n")).unwrap();
        assert_eq!((on.commit.as_str(), on.branch.as_deref()), (A, Some("refs/heads/x")));
        assert_eq!(parse_head(&format!("{A}\nHEAD\n")).unwrap().branch, None);
        assert!(parse_head("HEAD\nHEAD\n").is_err());
        assert!(parse_head(&format!("{A}\n")).is_err());
        assert!(parse_head(&format!("{A}\nmain\n")).is_err());
    }

    #[test]
    fn refs_leave_out_the_candidates_branch() {
        let out = format!("{A} refs/heads/main\n{B} refs/heads/wt\n{A} refs/tags/v1\n");
        assert_eq!(parse_refs(&out, Some("refs/heads/wt")), Ok(vec![A.to_owned(), A.to_owned()]));
        assert_eq!(parse_refs(&out, None).unwrap().len(), 3);
        assert!(parse_refs("nonsense", None).is_err());
        assert!(parse_refs(&format!("{A} main"), None).is_err());
    }

    #[test]
    fn config_values_are_nul_separated() {
        assert_eq!(
            parse_config_values("submodule.a.path\nlibs/a\0submodule.b.path\nb\0"),
            Ok(vec!["libs/a".to_owned(), "b".to_owned()])
        );
        assert!(parse_config_values("x").is_err());
        assert!(parse_config_values("novalue\0").is_err());
    }

    /// Skipped where the scanner is not ported (Windows): the candidate is
    /// told by file identity.
    #[test]
    fn worktree_heads_leave_out_the_candidate() {
        if crate::platform::skipped("worktree_heads_leave_out_the_candidate") {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let (main, wt) = (d.path().join("main"), d.path().join("wt"));
        std::fs::create_dir_all(&main).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        let out = format!(
            "worktree {}\nHEAD {A}\nbranch refs/heads/main\n\nworktree {}\nHEAD {B}\ndetached\n\nworktree /gone\nHEAD {B}\nprunable gitdir file points to non-existent location\n\nworktree /bare\nbare\n\n",
            main.display(),
            wt.display()
        );
        assert_eq!(other_heads(&out, &wt), Ok(vec![A.to_owned(), B.to_owned()]));
        assert!(other_heads("worktree \"quoted\\npath\"\nHEAD x\n\n", &wt).is_err());
        assert!(other_heads("garbage\n\n", &wt).is_err());
        assert!(other_heads(&format!("worktree {}\nHEAD nothex\n\n", main.display()), &wt).is_err());
    }

    #[test]
    fn objects_are_full_hex_names() {
        assert!(is_object(A));
        assert!(is_object(&"a".repeat(64)));
        assert!(!is_object("abc"));
        assert!(!is_object(&"g".repeat(40)));
    }

    #[test]
    fn the_xcrun_shim_is_never_a_candidate() {
        if SHIM_DIRS.is_empty() {
            return;
        }
        assert!(shim(Path::new("/usr/bin/git")));
        assert!(!shim(Path::new("/opt/homebrew/bin/git")));
        assert!(shim_dir(Path::new("/usr/bin/")));
    }
}
