//! `ddc-loadgen run`: builds a tree, checks it against its manifest, then
//! runs the release CLI on it and measures every run (story "Memory and CPU
//! under load: peak memory, leaks and CPU time"). Every command that reads
//! the tree runs before the one that changes it; the clean runs once, on an
//! attached image only. Each spawn is checked by [`isolate::decide`] first.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use ddc_core::exec::Outcome;
use ddc_core::exec::preflight::{Prediction, PreflightLine, PreflightSummary};
use ddc_core::exec::report::CleanReport;
use ddc_core::plan::ListedItems;
use ddc_core::platform::{self, Capability, procs, statfs, usage};
use ddc_core::settings::SETTINGS;
use ddc_core::store::{LAST_DISK, LAST_SCAN};

use crate::isolate::{self, Target};
use crate::limits::{self, Mode};
use crate::measure::{Measured, measure};
use crate::profiles::Profile;
use crate::table::{Meta, Row};
use crate::target::Layout;
use crate::{generate, items, verify};

#[derive(Debug, Clone)]
pub struct RunArgs {
    pub profile: Profile,
    pub entries: u64,
    pub seed: u64,
    /// A marked temporary folder or an attached image's mount point.
    pub into: PathBuf,
    pub device: Option<String>,
    /// The CLI to measure: the release build.
    pub cli: PathBuf,
    /// Runs of each reading command.
    pub repeat: u32,
    /// The size's name (`S`, `M`, `L`, `XL`), for the rows.
    pub size: String,
    pub date: String,
    pub commit: String,
    /// Where `<size>-<profile>.tsv` is written.
    pub out: PathBuf,
    pub mode: Mode,
    /// This platform's limits ([`limits::PATH`]), read in `enforce` mode.
    pub limits: PathBuf,
    /// The 1-minute load average when the whole run started, before any
    /// tree was built; read here when not given.
    pub load_start: Option<f64>,
}

#[derive(Debug, serde::Serialize)]
pub struct Report {
    pub profile: String,
    pub entries: u64,
    pub hash: String,
    pub tree: PathBuf,
    pub build_ms: u128,
    pub verify_ms: u128,
    pub differences: Vec<String>,
    pub difference_count: usize,
    pub rows: Vec<Row>,
    pub clean: Option<Clean>,
    /// What failed after the tree matched: a run's exit, the clean's check.
    pub failures: Vec<String>,
    pub failure_count: usize,
    /// The owner's state that changed during the run, by modification time.
    pub owner_changes: Vec<String>,
    /// Whether CPU time was judged: `enforce` on a quiet machine.
    pub cpu_judged: bool,
    pub tsv: Option<PathBuf>,
}

/// The clean's check.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Clean {
    pub listed: usize,
    pub trashed: usize,
    pub deleted: usize,
    /// Entries the clean added to `<mount>/.Trashes/<uid>`.
    pub trash_entries: usize,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.difference_count == 0 && self.failure_count == 0
    }
}

/// The reading commands, by name: every one passes `--data-dir`.
fn commands(profile: Profile, tree: &Path, data: &Path) -> Vec<(&'static str, Vec<OsString>)> {
    let args = |a: &[&str]| -> Vec<OsString> {
        let mut v: Vec<OsString> = a.iter().map(OsString::from).collect();
        v.extend([OsString::from("--data-dir"), data.into()]);
        v
    };
    let with_tree = |mut v: Vec<OsString>| {
        v.push(tree.into());
        v
    };
    let mut list = vec![
        ("scan", with_tree(args(&["scan", "--json"]))),
        ("clean-dry-run", with_tree(args(&["clean", "--dry-run"]))),
        ("scan-background", with_tree(args(&["scan", "--background", "--no-notify"]))),
    ];
    // `ddc disk` exits 69 where the platform does not declare it: nothing to measure.
    if profile == Profile::SystemRoot && platform::CAPABILITIES.contains(&Capability::WholeDisk) {
        let mut disk = args(&["disk", "--json", "--system-root"]);
        disk.insert(3, tree.into());
        list.push(("disk", disk));
    }
    list
}

/// One run's fixed parts: what every spawn is checked against.
struct Session<'a> {
    a: &'a RunArgs,
    home: &'a Path,
    real_home: PathBuf,
    layout: &'a Layout,
    out: PathBuf,
}

impl Session<'_> {
    fn target(&self) -> Target {
        Target { dir: self.a.into.clone(), device: self.a.device.clone(), entries: self.a.entries }
    }

    /// Checks the run, then measures it.
    fn spawn(&self, name: &str, run: u32, args: &[OsString], trashes: bool) -> anyhow::Result<Measured> {
        let facts = isolate::facts(args, self.home, &self.real_home, self.layout, &self.target(), trashes)?;
        isolate::decide(&facts)?;
        Ok(measure(&self.a.cli, self.home, args, &self.out.join(format!("{name}-{run}")))?)
    }

    fn row(&self, name: &str, run: u32, m: &Measured) -> Row {
        let exit = match (m.status.code(), crate::platform::exit_signal(&m.status)) {
            (Some(c), _) => c.to_string(),
            (None, Some(s)) => format!("sig{s}"),
            (None, None) => "-".to_owned(),
        };
        Row {
            profile: self.a.profile.name().to_owned(),
            size: self.a.size.clone(),
            seed: self.a.seed,
            entries: self.a.entries,
            command: name.to_owned(),
            run,
            wall_ms: m.wall.as_millis(),
            user_ms: m.usage.user.as_millis(),
            sys_ms: m.usage.system.as_millis(),
            cpu_ms: m.usage.cpu().as_millis(),
            max_rss_bytes: m.usage.max_rss_bytes,
            threads_max: m.threads_max,
            exit,
        }
    }

    fn image(&self) -> bool {
        isolate::facts(&[], self.home, &self.real_home, self.layout, &self.target(), false).is_ok_and(|f| f.image)
    }
}

pub fn run(a: &RunArgs) -> anyhow::Result<Report> {
    let real_home = isolate::real_home()?;
    let before = owner_state(&real_home);
    let load_start = a.load_start.unwrap_or_else(|| usage::load_average().unwrap_or(f64::NAN));
    let g = generate(a.profile, a.entries, a.seed, &a.into, a.device.as_deref())?;
    let started = Instant::now();
    let differences = verify::verify(&g.manifest, &g.layout, &g.home)?;
    let mut report = Report {
        profile: a.profile.name().to_owned(),
        entries: g.manifest.entries,
        hash: g.manifest.hash(),
        tree: g.layout.tree.clone(),
        build_ms: g.build_ms,
        verify_ms: started.elapsed().as_millis(),
        difference_count: differences.len(),
        differences,
        rows: Vec::new(),
        clean: None,
        failures: Vec::new(),
        failure_count: 0,
        owner_changes: Vec::new(),
        cpu_judged: false,
        tsv: None,
    };
    // A tree that does not match is not measured.
    if report.difference_count > 0 {
        return Ok(report);
    }
    // Default settings; the folder holds no lock, since `prepare` created it.
    fs::write(g.layout.data.join(SETTINGS), r#"{"schema_version":1}"#)?;
    let out = g.layout.data.join("out");
    fs::create_dir_all(&out)?;
    let s = Session { a, home: &g.home, real_home: real_home.clone(), layout: &g.layout, out };

    for run in 1..=a.repeat.max(1) {
        for (name, args) in commands(a.profile, &g.layout.tree, &g.layout.data) {
            let m = s.spawn(name, run, &args, false)?;
            if !m.status.success() {
                report.failures.push(format!("{name} run {run}: {}; stderr: {}", m.status, tail(&m.stderr)));
            }
            report.rows.push(s.row(name, run, &m));
        }
    }
    if s.image() {
        // A failure here keeps the rows already measured.
        match items::listed(&g.manifest, &g.layout.tree).and_then(|l| l.map(|l| clean(&s, &l)).transpose()) {
            Ok(Some((m, c, problems))) => {
                report.rows.push(s.row("clean-items", 1, &m));
                report.failures.extend(problems);
                report.clean = Some(c);
            }
            Ok(None) => {}
            Err(e) => report.failures.push(format!("clean-items: {e:#}")),
        }
    }

    let ncpu = std::thread::available_parallelism().map_or(0, usize::from);
    // CPU is judged on medians of enough runs only.
    report.cpu_judged =
        a.mode == Mode::Enforce && a.repeat as usize >= limits::CPU_RUNS && limits::cpu_judged(load_start, ncpu);
    if a.mode == Mode::Enforce {
        match fs::read_to_string(&a.limits).map_err(anyhow::Error::from).and_then(|t| limits::parse(&t)) {
            Ok(l) => report.failures.extend(limits::check(&l, &report.rows, a.mode, report.cpu_judged)),
            Err(e) => report.failures.push(format!("limits: {}: {e:#}", a.limits.display())),
        }
    }
    let meta = Meta {
        date: a.date.clone(),
        commit: a.commit.clone(),
        model: usage::machine_model().unwrap_or_else(|| "-".to_owned()),
        ncpu,
        load_start,
        load_end: usage::load_average().unwrap_or(f64::NAN),
        // `enforce-memory`: a busy machine recorded CPU and did not judge it.
        mode: match (a.mode, report.cpu_judged) {
            (Mode::Record, _) => "record",
            (Mode::Enforce, true) => "enforce",
            (Mode::Enforce, false) => "enforce-memory",
        }
        .to_owned(),
        seed: a.seed,
    };
    fs::create_dir_all(&a.out)?;
    let tsv = a.out.join(format!("{}-{}.tsv", a.size, a.profile.name()));
    let lines: Vec<String> = report.rows.iter().map(Row::line).collect();
    fs::write(&tsv, crate::table::render(&meta, &lines))?;
    report.tsv = Some(tsv);
    report.owner_changes = changed(&before, &owner_state(&real_home));
    report.failure_count = report.failures.len();
    Ok(report)
}

/// The clean of `list`: its dry run first, which must predict no tool; then
/// the clean, which must trash or delete every item, and the image's
/// `.Trashes` must grow by the trashed ones.
fn clean(s: &Session, list: &ListedItems) -> anyhow::Result<(Measured, Clean, Vec<String>)> {
    if !items::tool_free(list) {
        anyhow::bail!("the item list holds an item a tool removes");
    }
    let path = s.layout.data.join("items.json");
    fs::write(&path, serde_json::to_vec(list)?)?;
    let tree = s.layout.tree.as_os_str().to_owned();
    let data = s.layout.data.as_os_str().to_owned();
    let base = |extra: &[&str]| -> Vec<OsString> {
        let mut v: Vec<OsString> = ["clean", "--items"].iter().map(OsString::from).collect();
        v.push(path.clone().into());
        v.extend(extra.iter().map(OsString::from));
        v.extend([OsString::from("--data-dir"), data.clone(), tree.clone()]);
        v
    };

    let dry = s.spawn("clean-items-dry-run", 1, &base(&["--dry-run", "--json"]), false)?;
    if !dry.status.success() {
        anyhow::bail!("the dry run exited {}; stderr: {}", dry.status, tail(&dry.stderr));
    }
    let lines: Vec<PreflightLine> =
        fs::read_to_string(&dry.stdout)?.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let summary = lines
        .iter()
        .find_map(|l| match l {
            PreflightLine::Summary(s) => Some(s),
            PreflightLine::Item(_) => None,
        })
        .ok_or_else(|| anyhow::anyhow!("the dry run printed no summary; stderr: {}", tail(&dry.stderr)))?;
    isolate::preflight(summary)?;
    check_prediction(summary, &lines, list)?;

    let trash_dir = trashes(&s.layout.tree)?;
    let before = names(&trash_dir);
    let m = s.spawn("clean-items", 1, &base(&["--json", "--local-snapshots", "0"]), true)?;
    if !m.status.success() {
        anyhow::bail!("the clean exited {}; stderr: {}", m.status, tail(&m.stderr));
    }
    let report: CleanReport = serde_json::from_slice(&fs::read(&m.stdout)?)?;
    let mut problems = Vec::new();
    let (mut trashed, mut deleted) = (0, 0);
    for item in &report.items {
        match &item.outcome {
            Outcome::Trashed => trashed += 1,
            Outcome::Deleted => deleted += 1,
            other => problems.push(format!("clean-items: {} ended {other:?}", item.path.display())),
        }
    }
    if report.items.len() != list.items.len() {
        problems.push(format!("clean-items: {} items listed, {} reported", list.items.len(), report.items.len()));
    }
    let added: Vec<OsString> = names(&trash_dir).into_iter().filter(|n| !before.contains(n)).collect();
    let trash_entries = added.len();
    if trash_entries != trashed {
        let some: Vec<_> = added.iter().take(5).map(|n| n.to_string_lossy()).collect();
        problems.push(format!(
            "clean-items: {trashed} trashed, {trash_entries} new entries in {} ({}, ...)",
            trash_dir.display(),
            some.join(", ")
        ));
    }
    Ok((m, Clean { listed: list.items.len(), trashed, deleted, trash_entries }, problems))
}

/// The dry run predicts what the list asks: every item trashed or deleted.
/// A refusal names the first skipped items with their reasons.
fn check_prediction(summary: &PreflightSummary, lines: &[PreflightLine], list: &ListedItems) -> anyhow::Result<()> {
    let listed = list.items.len() as u64;
    if summary.would_skip == 0 && summary.would_delete + summary.would_trash == listed {
        return Ok(());
    }
    let skipped: Vec<String> = lines
        .iter()
        .filter_map(|l| match l {
            PreflightLine::Item(i) => match &i.prediction {
                Prediction::WouldSkip(r) => Some(format!("{} ({r:?})", i.path.display())),
                _ => None,
            },
            PreflightLine::Summary(_) => None,
        })
        .take(5)
        .collect();
    anyhow::bail!(
        "the dry run predicts {} trashed, {} deleted, {} skipped of {listed} items; first skipped: {}",
        summary.would_trash,
        summary.would_delete,
        summary.would_skip,
        skipped.join(", ")
    )
}

/// This user's trash folder on the volume of `tree`: an image's own.
fn trashes(tree: &Path) -> anyhow::Result<PathBuf> {
    let mount = statfs::statfs(tree)?.mount_point;
    Ok(mount.join(".Trashes").join(procs::uid().to_string()))
}

/// The last lines of a run's output: the files go with the image's detach.
pub(crate) fn tail(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(20)..].join("\n")
}

/// Names of the entries directly in `dir`; none while it does not exist.
/// The first item moved to a new trash folder brings a `.DS_Store` with it,
/// which no rule ever cleans.
fn names(dir: &Path) -> std::collections::BTreeSet<OsString> {
    fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.file_name()).filter(|n| n != ".DS_Store").collect())
        .unwrap_or_default()
}

/// The owner's state, by modification time only: the data folder, the
/// files the CLI writes there, `~/Library/LaunchAgents` and the log folder.
fn owner_state(home: &Path) -> Vec<(PathBuf, Option<SystemTime>)> {
    let data = home.join("Library/Application Support/DeepDriveCleaner");
    let logs = home.join("Library/Logs/DeepDriveCleaner");
    let mut paths = vec![data.clone()];
    paths.extend([SETTINGS, LAST_SCAN, LAST_DISK, "lock", "notify-state.json"].map(|f| data.join(f)));
    // The background scan appends to its log in place, which leaves the folder's time as it was.
    paths.extend([home.join("Library/LaunchAgents"), logs.clone()]);
    paths.extend(["actions.jsonl", "background.log"].map(|f| logs.join(f)));
    paths.into_iter().map(|p| (p.clone(), fs::symlink_metadata(&p).and_then(|m| m.modified()).ok())).collect()
}

fn changed(before: &[(PathBuf, Option<SystemTime>)], after: &[(PathBuf, Option<SystemTime>)]) -> Vec<String> {
    before
        .iter()
        .zip(after)
        .filter(|(b, a)| b.1 != a.1)
        .map(|(_, (p, t))| match t {
            Some(t) => {
                let secs = t.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                format!("{} modified at {secs} (Unix time)", p.display())
            }
            None => format!("{} removed", p.display()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_root_profile_measures_disk_only_where_the_platform_has_it() {
        let names: Vec<&str> =
            commands(Profile::SystemRoot, Path::new("/t"), Path::new("/d")).into_iter().map(|(n, _)| n).collect();
        let whole_disk = platform::CAPABILITIES.contains(&Capability::WholeDisk);
        assert_eq!(names.contains(&"disk"), whole_disk, "{names:?}");
        assert!(names.contains(&"scan") && names.contains(&"clean-dry-run"), "{names:?}");
    }
}
