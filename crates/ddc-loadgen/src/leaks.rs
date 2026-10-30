//! `ddc-loadgen leaks`: `scan --json` of every profile's S tree under
//! `leaks -quiet -atExit` (story "Memory and CPU under load: peak memory,
//! leaks and CPU time"). `-atExit` sets `MallocStackLogging=lite`, so a
//! leak report carries its stacks. A profile passes on exit 0, a parsed
//! `0 leaks for 0 total leaked bytes` line and a `last-scan.json` in the
//! data dir (the scan finished its walk); an exit above 1 or no line (a
//! child a signal killed) is a failure, not a leak report. The report
//! shares stdout with `--json`, so only the summary line is read.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use ddc_core::settings::SETTINGS;
use ddc_core::store::LAST_SCAN;

use crate::isolate::{self, Target};
use crate::measure::measure;
use crate::profiles::Profile;
use crate::{Generated, generate, target};

/// `/usr/bin/leaks` itself holds `com.apple.system-task-ports.read`, the
/// entitlement to read another process's memory: no `sudo`, no prompt for
/// an ad-hoc-signed build.
pub const LEAKS: &str = "/usr/bin/leaks";
const S: u64 = 10_000;

/// System-library allocations `leaks` reports that are not ours, each with
/// its written reason (`ops/profiling-the-core.md`). It starts empty.
pub const EXCLUSIONS: &[(&str, &str)] = &[];

/// The parsed `Process <pid>: <n> leaks for <bytes> total leaked bytes.`
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Summary {
    pub leaks: u64,
    pub bytes: u64,
}

/// The last summary line in `leaks`' output.
pub fn summary(output: &str) -> Option<Summary> {
    output.lines().rev().find_map(|line| {
        let rest = line.trim().strip_prefix("Process ")?;
        let (_, counts) = rest.split_once(": ")?;
        let (leaks, rest) = counts.split_once(" leak")?;
        let (_, rest) = rest.split_once(" for ")?;
        let bytes = rest.strip_suffix(" total leaked bytes.").or_else(|| rest.strip_suffix(" total leaked bytes"))?;
        Some(Summary { leaks: leaks.parse().ok()?, bytes: bytes.parse().ok()? })
    })
}

/// A profile's result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Checked {
    pub profile: String,
    /// The exit code, or `sig<N>` for a `leaks` a signal killed.
    pub exit: String,
    pub summary: Option<Summary>,
    /// Whether the scan finished its walk: it writes `last-scan.json` only then.
    pub scanned: bool,
    /// `leaks`' stdout and stderr, kept for a failure.
    pub output: PathBuf,
    pub stderr: PathBuf,
    pub passed: bool,
}

/// The decision for one run: exit 0, a summary of no leak, a finished scan.
pub fn passed(exit: Option<i32>, summary: Option<Summary>, scanned: bool) -> bool {
    exit == Some(0) && summary == Some(Summary { leaks: 0, bytes: 0 }) && scanned
}

/// `leaks`' arguments: the options, each exclusion, then `-atExit` last
/// before `--` and the command.
pub fn args(cli: &Path, data: &Path, tree: &Path, exclusions: &[(&str, &str)]) -> Vec<OsString> {
    let mut args = vec![OsString::from("-quiet")];
    for (symbol, _reason) in exclusions {
        args.extend([OsString::from("-exclude"), OsString::from(symbol)]);
    }
    args.extend([OsString::from("-atExit"), OsString::from("--")]);
    args.push(cli.into());
    args.extend(["scan", "--json", "--data-dir"].iter().map(OsString::from));
    args.extend([data.into(), tree.into()]);
    args
}

/// A profile's S tree in `dir`, its settings written, and the arguments
/// for `leaks`, refused unless the run is isolated.
pub fn prepare(p: Profile, dir: &Path, cli: &Path, real_home: &Path) -> anyhow::Result<(Generated, Vec<OsString>)> {
    let g = generate(p, S, 1, dir, None)?;
    fs::write(g.layout.data.join(SETTINGS), r#"{"schema_version":1}"#)?;
    let args = args(cli, &g.layout.data, &g.layout.tree, EXCLUSIONS);
    let on = Target { dir: dir.to_owned(), device: None, entries: S };
    isolate::decide(&isolate::facts(&args, &g.home, real_home, &g.layout, &on, false)?)?;
    Ok((g, args))
}

/// Runs every profile in a marked temporary folder; outputs go to `out`.
pub fn run(cli: &Path, out: &Path) -> anyhow::Result<Vec<Checked>> {
    fs::create_dir_all(out)?;
    let real_home = isolate::real_home()?;
    let cli = cli.canonicalize()?;
    let mut checked = Vec::new();
    for p in Profile::ALL {
        let dir = target::Scratch::new()?;
        let (g, args) = prepare(p, dir.path(), &cli, &real_home)?;
        let m = measure(Path::new(LEAKS), &g.home, &args, &g.layout.data.join("leaks"))?;
        let output = out.join(format!("leaks-{}.txt", p.name()));
        let stderr = out.join(format!("leaks-{}.stderr.txt", p.name()));
        fs::copy(&m.stdout, &output)?;
        fs::copy(&m.stderr, &stderr)?;
        let summary = summary(&String::from_utf8_lossy(&fs::read(&output)?));
        let scanned = g.layout.data.join(LAST_SCAN).is_file();
        let code = m.status.code();
        let exit = match (code, crate::platform::exit_signal(&m.status)) {
            (Some(c), _) => c.to_string(),
            (None, Some(s)) => format!("sig{s}"),
            (None, None) => "-".to_owned(),
        };
        let passed = passed(code, summary, scanned);
        checked.push(Checked { profile: p.name().to_owned(), exit, summary, scanned, output, stderr, passed });
    }
    Ok(checked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_line_is_found_after_the_json() {
        let out = "{\"findings\": []}\nProcess 4242: 0 leaks for 0 total leaked bytes.\n";
        assert_eq!(summary(out), Some(Summary { leaks: 0, bytes: 0 }));
        let one = "Process 7: 1 leak for 48 total leaked bytes.\n\n    1 (48 bytes) ROOT LEAK: <malloc 48>\n";
        assert_eq!(summary(one), Some(Summary { leaks: 1, bytes: 48 }));
        let many = "header\nProcess 7: 12 leaks for 4096 total leaked bytes.\nstacks...\n";
        let nodes = "Process 7: 3021 nodes malloced for 412 KB\nProcess 7: 0 leaks for 0 total leaked bytes.\n";
        assert_eq!(summary(nodes), Some(Summary { leaks: 0, bytes: 0 }));
        assert_eq!(summary(many), Some(Summary { leaks: 12, bytes: 4096 }));
    }

    #[test]
    fn no_summary_line_is_none() {
        assert_eq!(summary(""), None);
        assert_eq!(summary("{\"Process 1: 0 leaks\": 1}\nleaks[1]: [fatal] cannot examine process\n"), None);
    }

    #[test]
    fn only_a_clean_exit_with_no_leak_and_a_finished_scan_passes() {
        let none = Some(Summary { leaks: 0, bytes: 0 });
        assert!(passed(Some(0), none, true));
        assert!(!passed(Some(1), Some(Summary { leaks: 1, bytes: 48 }), true));
        // A child a signal killed: no exit code, or `leaks` failing with no line.
        assert!(!passed(None, none, true));
        assert!(!passed(Some(2), None, true));
        assert!(!passed(Some(0), None, true));
        // A scan that stopped before its walk ended leaks nothing either.
        assert!(!passed(Some(0), none, false));
    }

    #[test]
    fn at_exit_comes_last_before_the_command() {
        let (cli, data, tree) = (Path::new("/c/deepdrivecleaner"), Path::new("/d"), Path::new("/t"));
        let a = args(cli, data, tree, &[("sym_a", "why"), ("sym_b", "why")]);
        let a: Vec<&str> = a.iter().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(
            a,
            [
                "-quiet",
                "-exclude",
                "sym_a",
                "-exclude",
                "sym_b",
                "-atExit",
                "--",
                "/c/deepdrivecleaner",
                "scan",
                "--json",
                "--data-dir",
                "/d",
                "/t"
            ]
        );
        let none = args(cli, data, tree, &[]);
        assert_eq!(none[..3], ["-quiet", "-atExit", "--"].map(OsString::from));
    }

    #[test]
    fn a_prepared_run_is_isolated() {
        // The generator builds macOS trees and reads the real home from macOS
        // (catalog split): not ported elsewhere yet.
        if crate::unsupported("a_prepared_run_is_isolated") {
            return;
        }
        let dir = target::Scratch::new().unwrap();
        let real = isolate::real_home().unwrap();
        let (g, args) = prepare(Profile::Projects, dir.path(), Path::new("/c/deepdrivecleaner"), &real).unwrap();
        assert!(g.layout.data.join(SETTINGS).is_file());
        assert!(args.contains(&g.layout.data.clone().into_os_string()));
    }
}
