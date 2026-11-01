//! Human-readable scan, disk, agents and disk-free reports.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

use ddc_core::agents::{
    AgentsReport, FootprintState, RecordsState, RunningKind, SessionFootprint, SessionState, TempRoot, UnknownReason,
};
use ddc_core::brew::{BrewPackages, NotOffered, Service, ServicesState};
use ddc_core::class::{Category, RiskClass};
use ddc_core::disk::{ActionId, DiskReport, HeldKind, HomeSource, SimulatorStatus};
use ddc_core::free::{ActionReport, ActionResult, FreeReport, Refusal, ReportEdit};
use ddc_core::model::{Finding, NotSelected, ScanReport};
use ddc_core::rules::Note;

use crate::size::{count, human};

/// Findings shown per group unless `--all`.
const PER_GROUP: usize = 8;

/// Sessions `ddc disk agents` lists per harness; `--json` has them all.
const TOP_SESSIONS: usize = 10;

pub struct Style {
    pub color: bool,
    pub all: bool,
}

impl Style {
    fn paint(&self, code: &str, text: &str) -> String {
        if self.color { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_owned() }
    }
    pub fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }
    pub fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }
    fn class(&self, class: RiskClass, text: &str) -> String {
        let code = match class {
            RiskClass::Safe => "1;32",
            RiskClass::Rebuildable => "1;36",
            RiskClass::Review => "1;33",
            RiskClass::UserData => "1;35",
        };
        self.paint(code, text)
    }
}

fn class_title(class: RiskClass) -> &'static str {
    match class {
        RiskClass::Safe => "SAFE — caches that refill themselves",
        RiskClass::Rebuildable => "REBUILDABLE — project artifacts restored by install or build",
        RiskClass::Review => "REVIEW — expensive to restore; your call, one by one",
        RiskClass::UserData => "USER DATA — never cleaned by default; files you pick go to the Trash",
    }
}

/// `/Users/me/x` → `~/x`. Control characters are escaped so a crafted file
/// name cannot move the cursor or recolor a confirmation prompt, and
/// bidirectional format characters so `cod.exe\u{202e}fdp.` cannot pass for a PDF.
pub fn tilde(path: &Path, home: &Path) -> String {
    let shown = match path.strip_prefix(home) {
        Ok(rel) if rel.as_os_str().is_empty() => "~".to_owned(),
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => path.display().to_string(),
    };
    escaped(&shown)
}

/// `text` with control and bidirectional format characters escaped, as
/// [`tilde`] shows a path: for text another program wrote.
pub fn escaped(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() || bidi(c) { c.escape_default().to_string() } else { c.to_string() })
        .collect()
}

/// Bidirectional embeddings, overrides and isolates, and the direction marks.
fn bidi(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}')
}

/// Colors only on a terminal and without `NO_COLOR`.
pub fn use_color(is_terminal: bool, no_color: bool) -> bool {
    is_terminal && !no_color
}

fn status(f: &Finding) -> String {
    let mut parts = Vec::new();
    if f.ignored {
        parts.push("ignored".to_owned());
    }
    if let Some(p) = f.private_bytes.filter(|p| *p + f.bytes / 10 < f.bytes) {
        parts.push(format!("frees ≥ {}", human(p)));
    }
    match f.not_selected {
        Some(NotSelected::ActiveProject { idle_days }) => parts.push(format!("active ({idle_days}d)")),
        Some(NotSelected::UnknownActivity) => parts.push("activity unknown".to_owned()),
        Some(NotSelected::ForeignOwner) => parts.push("another user's".to_owned()),
        _ => {}
    }
    if let Some(days) = f.project.as_ref().and_then(|p| p.idle_days).filter(|_| f.selected) {
        parts.push(format!("idle {days}d"));
    }
    for note in &f.notes {
        parts.push(
            match note {
                Note::NoLockfile => "no lock file",
                Note::AppOwned => "owned by an app",
                Note::KeptByMarker => "kept (.ddc-keep)",
                Note::Excluded => "excluded",
            }
            .to_owned(),
        );
    }
    if f.incomplete {
        parts.push("incomplete".to_owned());
    }
    parts.join(" · ")
}

pub fn report(w: &mut impl Write, r: &ScanReport, style: &Style) -> io::Result<()> {
    let home = r.home.as_path();
    let roots: Vec<String> = r.scan_roots.iter().map(|p| tilde(p, home)).collect();
    writeln!(
        w,
        "{} · scanned {} in {:.1}s · {} files · {}",
        style.bold("DeepDriveCleaner"),
        roots.join(", "),
        r.duration_ms as f64 / 1000.0,
        count(r.totals.files),
        human(r.totals.scanned_bytes)
    )?;
    writeln!(w)?;
    for t in &r.totals.classes {
        let tail = match t.class {
            RiskClass::Safe | RiskClass::Rebuildable => {
                format!("selected {}", human(t.selected_bytes))
            }
            RiskClass::Review => "manual choice".to_owned(),
            RiskClass::UserData => "pick by hand".to_owned(),
        };
        writeln!(
            w,
            "  {} {:>6} items {:>10}   {}",
            style.class(t.class, &format!("{:<12}", t.class.as_str())),
            t.count,
            human(t.bytes),
            style.dim(&tail)
        )?;
    }
    writeln!(w)?;
    writeln!(
        w,
        "  {} {} selected by default, frees ~{} → run `ddc clean` to review and confirm",
        style.bold("Can free now:"),
        human(r.totals.selected_bytes),
        human(r.totals.selected_reclaimable_bytes)
    )?;

    for class in RiskClass::ALL {
        // Hidden by settings.json: counted on one line at the end, as in the totals.
        let of: Vec<&Finding> = r.findings.iter().filter(|f| f.class == class && !f.ignored).collect();
        if of.is_empty() {
            continue;
        }
        writeln!(w)?;
        writeln!(w, "{}", style.class(class, class_title(class)))?;
        let mut by_category: BTreeMap<Category, Vec<&Finding>> = BTreeMap::new();
        for f in &of {
            by_category.entry(f.category).or_default().push(f);
        }
        let mut categories: Vec<(Category, Vec<&Finding>)> = by_category.into_iter().collect();
        categories.sort_by_key(|(_, fs)| std::cmp::Reverse(fs.iter().map(|f| f.bytes).sum::<u64>()));
        for (category, items) in categories {
            let total: u64 = items.iter().map(|f| f.bytes).sum();
            writeln!(w, "  {} · {} items · {}", style.bold(category.as_str()), items.len(), human(total))?;
            let shown = if style.all { items.len() } else { PER_GROUP.min(items.len()) };
            for f in &items[..shown] {
                let mark = if f.selected { "✓" } else { "·" };
                writeln!(
                    w,
                    "    {mark} {:>10}  {}  {}  {}",
                    human(f.bytes),
                    tilde(&f.path, home),
                    style.dim(&f.rule),
                    style.dim(&status(f))
                )?;
            }
            if shown < items.len() {
                let rest: u64 = items[shown..].iter().map(|f| f.bytes).sum();
                writeln!(
                    w,
                    "    {}",
                    style.dim(&format!("… {} more, {} (--all shows them)", items.len() - shown, human(rest)))
                )?;
            }
        }
    }

    let ignored: Vec<&Finding> = r.findings.iter().filter(|f| f.ignored).collect();
    if !ignored.is_empty() {
        writeln!(w)?;
        writeln!(
            w,
            "{} {} items ignored by settings.json ({}): switched-off types or under the minimum size.",
            style.dim("note:"),
            ignored.len(),
            human(r.totals.ignored_bytes)
        )?;
        if style.all {
            for f in ignored {
                writeln!(
                    w,
                    "    · {:>10}  {}  {}  {}",
                    human(f.bytes),
                    tilde(&f.path, home),
                    style.dim(&f.rule),
                    style.dim(&status(f))
                )?;
            }
        }
    }

    if !r.not_scanned.is_empty() {
        let skipped: Vec<String> = r.not_scanned.iter().map(|p| tilde(p, home)).collect();
        writeln!(w)?;
        writeln!(w, "{} Not scanned: {}", style.dim("note:"), skipped.join(", "))?;
    }

    if !r.large_files.is_empty() {
        writeln!(w)?;
        writeln!(w, "{}", style.bold("LARGE FILES — not covered by any rule, check by hand"))?;
        for f in &r.large_files {
            writeln!(w, "    {:>10}  {}", human(f.bytes), tilde(&f.path, home))?;
        }
    }
    if !r.inaccessible.is_empty() {
        let n = r.inaccessible.len();
        writeln!(w)?;
        let hint = if r.full_disk_access {
            String::new()
        } else {
            " Grant your terminal Full Disk Access (System Settings → Privacy & Security) for a complete scan."
                .to_owned()
        };
        writeln!(w, "{} {n} folder{} could not be read.{hint}", style.dim("note:"), if n == 1 { "" } else { "s" })?;
    }
    Ok(())
}

/// The whole-disk report as a table (`ddc disk`).
pub fn disk(w: &mut impl Write, r: &DiskReport, style: &Style) -> io::Result<()> {
    let home = r.home.path.as_path();
    match &r.container {
        Some(c) => {
            writeln!(w, "{} {} used of {}", style.bold("Startup disk"), human(c.in_use_bytes), human(c.capacity_bytes))?
        }
        None => writeln!(w, "{} (its volumes could not be read)", style.bold("Startup disk"))?,
    }
    if r.cancelled {
        writeln!(w, "{}", style.dim("Stopped: rows marked \"at least\" were not finished."))?;
    }
    writeln!(w)?;
    let row = |w: &mut dyn Write, title: &str, bytes: u64, partial: bool, note: &str| -> io::Result<()> {
        let size = if partial { format!("at least {}", human(bytes)) } else { human(bytes) };
        writeln!(w, "  {title:<28} {size:>20}  {}", style.dim(note))
    };
    let home_note = match r.home.source {
        HomeSource::LastScan => format!("from the scan at {}", r.home.as_of),
        HomeSource::Walk => "measured now".to_owned(),
    };
    row(w, &format!("Home folder {}", tilde(home, home)), r.home.bytes, r.home.partial, &home_note)?;
    // Each nested area right under the one it lies in, when that one is listed.
    let listed = |id: &Option<String>| id.as_ref().is_some_and(|id| r.areas.iter().any(|a| &a.id == id));
    let ordered = r.areas.iter().filter(|a| !listed(&a.parent)).flat_map(|top| {
        std::iter::once(top).chain(r.areas.iter().filter(move |a| a.parent.as_deref() == Some(top.id.as_str())))
    });
    for a in ordered {
        let mut note = a.frees.clone();
        if let Some(n) = a.entries_read.filter(|_| a.partial) {
            note = format!("{} entries read; {note}", count(n));
        }
        if a.unreadable > 0 {
            note = format!("{} unreadable; {note}", count(a.unreadable));
        }
        if a.id == "simulator-runtimes"
            && let Some(s) = r.simulators.as_ref().filter(|s| s.status == SimulatorStatus::Listed)
        {
            let kept = a.bytes.saturating_sub(s.listed_bytes);
            note = if kept > 0 {
                format!("listed runtimes {}; {} more in their folders", human(s.listed_bytes), human(kept))
            } else {
                format!("listed runtimes {}", human(s.listed_bytes))
            };
        }
        let title = if listed(&a.parent) { format!("  {}", a.title) } else { a.title.clone() };
        row(w, &title, a.bytes, a.partial, &note)?;
    }
    let nc = &r.not_counted;
    match nc.bytes {
        Some(b) if nc.inconsistent => writeln!(
            w,
            "  {:<28} {:>20}  {}",
            "Not counted",
            format!("-{}", human(b.unsigned_abs())),
            style
                .dim("more was counted than the volume holds: the home row may be stale, or clones were counted twice")
        )?,
        Some(b) => row(
            w,
            "Not counted",
            b.unsigned_abs(),
            false,
            "the Spotlight index, folders only root can read, APFS metadata",
        )?,
        None => writeln!(w, "  {:<28} {:>20}", "Not counted", "unknown")?,
    }
    for held in &nc.holds {
        let what = match held.kind {
            HeldKind::Cloud => "cloud folder, not read",
            HeldKind::Skip => "in your skips, not read",
        };
        writeln!(w, "    {}  {}", tilde(&held.path, home), style.dim(what))?;
    }
    if nc.unreadable > 0 {
        writeln!(w, "    {}", style.dim(&format!("{} folders could not be read", count(nc.unreadable))))?;
    }

    if let Some(s) = &r.simulators {
        writeln!(w)?;
        writeln!(w, "{}", style.bold("Simulator runtimes"))?;
        if let Some(message) = &s.message {
            writeln!(w, "  {message}")?;
        }
        for rt in &s.runtimes {
            let name = rt.runtime_identifier.as_deref().unwrap_or(&rt.identifier);
            let devices = format!(
                "{} device{}{}",
                rt.devices.len(),
                if rt.devices.len() == 1 { "" } else { "s" },
                if rt.booted { ", one running" } else { "" }
            );
            writeln!(w, "  {name:<48} {:>10}  {}", human(rt.size_bytes.unwrap_or(0)), style.dim(&devices))?;
        }
    }
    if let Some(c) = r.conda.as_ref().filter(|c| !c.installs.is_empty()) {
        writeln!(w)?;
        writeln!(w, "{}", style.bold("Conda"))?;
        let folder = |w: &mut dyn Write, title: &str, f: &ddc_core::disk::CondaFolder| -> io::Result<()> {
            let size = if f.partial { format!("at least {}", human(f.bytes)) } else { human(f.bytes) };
            let mut notes = Vec::new();
            if f.shared_bytes > 0 {
                notes.push(format!("{} shared with other folders", human(f.shared_bytes)));
            }
            if !f.writable {
                notes.push("needs an administrator".to_owned());
            }
            writeln!(w, "  {title:<48} {size:>10}  {}", style.dim(&notes.join("; ")))
        };
        for install in &c.installs {
            writeln!(w, "  {}", escaped(&install.prefix.to_string_lossy()))?;
            if let Some(p) = &install.packages {
                folder(w, "  package cache", p)?;
            }
            for e in &install.environments {
                let name = e.path.file_name().map_or_else(String::new, |n| escaped(&n.to_string_lossy()));
                folder(w, &format!("  {name}"), e)?;
            }
        }
    }
    if let Some(c) = &r.container {
        writeln!(w)?;
        writeln!(w, "{}", style.bold("Volumes"))?;
        for v in c.volumes.iter().filter(|v| v.role != "data") {
            let size = v.used_bytes.map_or_else(|| "not measured".to_owned(), human);
            let note = if v.role == "vm" { "swap; a restart frees it" } else { "" };
            writeln!(w, "  {:<28} {size:>20}  {}", format!("{} ({})", v.name, v.role), style.dim(note))?;
        }
    }
    writeln!(w)?;
    match r.snapshots.count {
        Some(n) => writeln!(w, "Local Time Machine snapshots: {n} (their size needs root to measure)")?,
        None => writeln!(w, "Local Time Machine snapshots: unknown")?,
    }
    Ok(())
}

/// `ddc disk agents`: per harness, what runs, its folders and its live
/// sessions (story "Coding agents in the Whole disk view").
pub fn agents(w: &mut impl Write, r: &AgentsReport, home: &Path, style: &Style) -> io::Result<()> {
    if r.harnesses.is_empty() {
        return writeln!(w, "No coding-agent harness keeps folders in {}.", tilde(home, home));
    }
    let size = |bytes: Option<u64>, unreadable: u64| match bytes {
        Some(b) if unreadable > 0 => format!("at least {}", human(b)),
        Some(b) => human(b),
        None => String::new(),
    };
    for (i, h) in r.harnesses.iter().enumerate() {
        if i > 0 {
            writeln!(w)?;
        }
        let mut kinds: Vec<(&str, usize)> = Vec::new();
        for p in &h.running {
            let kind = match p.kind {
                RunningKind::Cli => "CLI",
                RunningKind::AppServer => "app-server",
                RunningKind::App => "app",
                RunningKind::Other => "other",
            };
            match kinds.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, n)) => *n += 1,
                None => kinds.push((kind, 1)),
            }
        }
        let runs = if kinds.is_empty() {
            "not running".to_owned()
        } else {
            let parts: Vec<String> =
                kinds.iter().map(|(k, n)| if *n == 1 { (*k).to_owned() } else { format!("{k} ×{n}") }).collect();
            format!("running: {}", parts.join(", "))
        };
        let unreadable = h.folders.iter().filter(|f| !f.nested).map(|f| f.unreadable).sum();
        writeln!(w, "{:<44} {:>14}  {}", style.bold(&h.name), size(h.bytes, unreadable), style.dim(&runs))?;
        for f in &h.folders {
            let indent = if f.nested { "    " } else { "  " };
            let path = format!("{indent}{}", tilde(&f.path, home));
            let note = if f.skipped {
                "in your skips, not read".to_owned()
            } else if f.unreadable > 0 {
                format!("{} unreadable", count(f.unreadable))
            } else {
                String::new()
            };
            writeln!(w, "{path:<44} {:>14}  {}", size(f.bytes, f.unreadable), style.dim(&note))?;
        }
        let Some(s) = &h.sessions else { continue };
        if s.records == RecordsState::Unknown {
            let why = match s.reason {
                Some(UnknownReason::Skipped) => "they are in your skips",
                Some(UnknownReason::Missing) => "their folder is missing",
                Some(UnknownReason::Unreadable) => "their folder could not be read",
                Some(UnknownReason::Unparsed) | None => "their format is not known",
            };
            writeln!(w, "  {}", style.dim(&format!("ddc cannot read {}'s session records: {why}", h.name)))?;
        }
        for x in s.list.iter().filter(|x| x.state == SessionState::Live) {
            let place = x
                .cwd
                .as_deref()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .or_else(|| x.id.clone())
                .map(|p| escaped(&p))
                .unwrap_or_default();
            let note: Vec<String> = [&x.status, &x.name].into_iter().flatten().map(|s| escaped(s)).collect();
            let since = escaped(x.started_at.as_deref().or(x.rollout_time.as_deref()).unwrap_or_default());
            let label = if h.id == ddc_core::agents::HarnessId::Codex { "open in Codex" } else { "live" };
            writeln!(w, "  {label:<12} {place:<30} {}  {}", style.dim(&since), note.join(" · "))?;
        }
        let t = &s.transcripts;
        match (t.skipped, t.count) {
            (true, _) => writeln!(w, "  {}", style.dim("transcripts: in your skips, not read"))?,
            (false, Some(n)) => {
                let bytes = t.bytes.map(|b| format!(", {}", human(b))).unwrap_or_default();
                writeln!(w, "  {}", style.dim(&format!("{} other transcripts{bytes}", count(n))))?;
            }
            (false, None) => {}
        }
        if let Some(t) = &s.temp_root {
            temp_root(w, t, home, style)?;
        }
        if let Some(f) = &s.footprints {
            footprints(w, f, home, style)?;
        }
    }
    if !r.sizes {
        writeln!(w)?;
        writeln!(w, "{}", style.dim("Sizes were not measured (--no-sizes)."))?;
    }
    Ok(())
}

/// Claude Code's temporary folder outside the home.
fn temp_root(w: &mut impl Write, t: &TempRoot, home: &Path, style: &Style) -> io::Result<()> {
    let path = format!("  temporary folder {}", tilde(&t.path, home));
    let (bytes, note) = match t.bytes {
        _ if t.skipped => (String::new(), "in your skips, not read".to_owned()),
        Some(b) if t.unreadable > 0 => {
            (format!("at least {}", human(b)), format!("{} unreadable", count(t.unreadable)))
        }
        Some(b) => (human(b), String::new()),
        None => (String::new(), String::new()),
    };
    writeln!(w, "{path:<44} {bytes:>14}  {}", style.dim(&note))
}

/// The sessions by total, largest first: the top ten, then the rest as one
/// line. A live or unknown session's files are only described, never
/// offered; a session that is not live names its temporary folder.
fn footprints(w: &mut impl Write, list: &[SessionFootprint], home: &Path, style: &Style) -> io::Result<()> {
    if list.is_empty() {
        return Ok(());
    }
    writeln!(w, "  {}", style.dim("sessions, largest first:"))?;
    for f in list.iter().take(TOP_SESSIONS) {
        let state = match f.state {
            FootprintState::Live => "live",
            FootprintState::NotLive => "not live",
            FootprintState::Unknown => "unknown",
        };
        let mut parts: Vec<String> = Vec::new();
        if f.transcript > 0 {
            parts.push(format!("transcript {}", human(f.transcript)));
        }
        if f.checkpoints > 0 {
            parts.push(format!("checkpoints {}", human(f.checkpoints)));
        }
        match (&f.temp_path, f.state) {
            (Some(p), FootprintState::NotLive) if f.temp > 0 => {
                parts.push(format!("{} in its temporary folder ({})", human(f.temp), tilde(p, home)));
            }
            _ if f.temp > 0 => parts.push(format!("temporary {}", human(f.temp))),
            _ => {}
        }
        let id: String = f.id.chars().take(8).collect();
        let project = escaped(f.project.as_deref().unwrap_or("?"));
        writeln!(w, "    {state:<9} {id:<8}  {project:<24} {:>12}  {}", human(f.total), style.dim(&parts.join(" · ")))?;
    }
    let rest = &list[list.len().min(TOP_SESSIONS)..];
    if !rest.is_empty() {
        let bytes: u64 = rest.iter().map(|f| f.total).sum();
        writeln!(w, "    {}", style.dim(&format!("{} more, {}", count(rest.len() as u64), human(bytes))))?;
    }
    Ok(())
}

/// `ddc disk free`: one block per action, in list order.
pub fn free(w: &mut impl Write, r: &FreeReport, home: &Path, style: &Style) -> io::Result<()> {
    for (i, a) in r.actions.iter().enumerate() {
        if i > 0 {
            writeln!(w)?;
        }
        free_action(w, a, style)?;
    }
    if let Some(journal) = &r.journal {
        writeln!(w, "\n{}", style.dim(&format!("Journal: {}", tilde(journal, home))))?;
    }
    for (name, edit) in [("last-scan.json", r.last_scan), ("last-disk.json", r.last_disk)] {
        let note = match edit {
            ReportEdit::Skipped => format!("{name} was not edited: absent, or of another version or other folders."),
            ReportEdit::Failed => format!("{name} could not be saved."),
            ReportEdit::Edited | ReportEdit::Unchanged => continue,
        };
        writeln!(w, "{}", style.dim(&note))?;
    }
    Ok(())
}

/// `Simulator runtime <id>`, `Homebrew cleanup`, `Conda environment <path>`,
/// `Conda package cache of <prefix>`, `Homebrew formula <full_name>`.
pub fn action_title(action: ActionId, identifier: Option<&str>) -> String {
    match (action, identifier) {
        (ActionId::SimctlRuntimeDelete, Some(id)) => format!("Simulator runtime {id}"),
        (ActionId::SimctlRuntimeDelete, None) => "Simulator runtime".to_owned(),
        (ActionId::BrewCleanup, _) => "Homebrew cleanup".to_owned(),
        (ActionId::CondaEnvRemove, Some(path)) => format!("Conda environment {}", escaped(path)),
        (ActionId::CondaEnvRemove, None) => "Conda environment".to_owned(),
        (ActionId::CondaClean, Some(prefix)) => format!("Conda package cache of {}", escaped(prefix)),
        (ActionId::CondaClean, None) => "Conda package cache".to_owned(),
        (ActionId::BrewUninstall, Some(name)) => format!("Homebrew formula {}", escaped(name)),
        (ActionId::BrewUninstall, None) => "Homebrew formula".to_owned(),
        (ActionId::BrewUninstallOrphans, _) => "Homebrew formulae needed by nothing".to_owned(),
    }
}

fn free_action(w: &mut impl Write, a: &ActionReport, style: &Style) -> io::Result<()> {
    let freed = match (a.bytes_before, a.bytes_after) {
        (Some(before), Some(after)) => Some(before.saturating_sub(after)),
        _ => None,
    };
    let result = match &a.result {
        ActionResult::Freed => freed.map_or_else(|| "done".to_owned(), |b| format!("freed {}", human(b))),
        ActionResult::RemovedNotFreed if a.action == ActionId::CondaEnvRemove => {
            "conda finished, and the environment is still there: nothing freed".to_owned()
        }
        ActionResult::RemovedNotFreed if a.area == "homebrew" => {
            "brew finished, and a keg is still in the Cellar: not all of it was freed".to_owned()
        }
        ActionResult::RemovedNotFreed => {
            format!("runtime removed, {} not freed", a.bytes_after.map_or_else(|| "its space".to_owned(), human))
        }
        ActionResult::StillDeleting => "still deleting: CoreSimulator may finish it later".to_owned(),
        ActionResult::Refused { reason } => format!("refused: {}", refusal(*reason)),
        ActionResult::Failed { .. } => "failed".to_owned(),
        ActionResult::WouldRun => "would run".to_owned(),
        ActionResult::NotRun => "not run: stopped".to_owned(),
    };
    writeln!(w, "{}  {result}", style.bold(&action_title(a.action, a.identifier.as_deref())))?;
    match &a.result {
        ActionResult::Failed { needs_admin } => {
            let lead = if *needs_admin {
                "It needs an administrator. To run it yourself, in Terminal:"
            } else {
                "The tool failed. To run it yourself, in Terminal:"
            };
            writeln!(w, "  {lead}\n    {}", a.terminal_command)?;
        }
        ActionResult::WouldRun => {
            writeln!(w, "  {}", a.terminal_command)?;
            if let Some(p) = &a.preview {
                // A formula's preview lines are its kegs' paths, listed below.
                for line in p.lines.iter().filter(|_| a.kegs.is_empty()) {
                    writeln!(w, "    {}", style.dim(line))?;
                }
                if let Some(b) = p.estimate_bytes {
                    writeln!(w, "  frees about {}", human(b))?;
                }
            }
        }
        // A package removal names its formulae in its command.
        _ if matches!(a.action, ActionId::BrewUninstall | ActionId::BrewUninstallOrphans) => {
            writeln!(w, "  {}", style.dim(&a.terminal_command))?;
        }
        _ => {}
    }
    for k in &a.kegs {
        let size = k.bytes.map_or_else(|| "not measured".to_owned(), human);
        let keg = format!("{} {}  {}", escaped(&k.formula), escaped(&k.version), escaped(&k.path.to_string_lossy()));
        writeln!(w, "    {keg}  {size}")?;
    }
    for n in &a.needed_by {
        let casks = n.casks.iter().map(|c| format!("cask {}", escaped(c)));
        let by: Vec<String> = n.formulae.iter().map(|f| escaped(f)).chain(casks).collect();
        writeln!(w, "  {} is needed by {}", escaped(&n.formula), by.join(", "))?;
    }
    if !a.output.is_empty() && matches!(a.result, ActionResult::Failed { .. }) {
        for line in a.output.lines() {
            writeln!(w, "    {}", style.dim(line))?;
        }
    }
    Ok(())
}

/// `ddc disk brew-packages`: each formula installed on request, largest
/// first, its kegs when it has more than one, what only it holds, and why
/// a removal is not offered.
pub fn brew_packages(w: &mut impl Write, r: &BrewPackages, home: &Path, style: &Style) -> io::Result<()> {
    let size = |bytes: Option<u64>| bytes.map_or_else(|| "not measured".to_owned(), human);
    let names = |list: &[String]| list.iter().map(|n| escaped(n)).collect::<Vec<_>>().join(", ");
    if r.formulae.is_empty() {
        writeln!(w, "No formula installed on request in {}.", tilde(&r.cellar, home))?;
    }
    for p in &r.formulae {
        let note = match p.not_offered {
            None => String::new(),
            Some(NotOffered::Needed) => {
                let casks: Vec<String> = p.needed_by_casks.iter().map(|c| format!("cask {c}")).collect();
                format!("needed by {}", names(&[p.needed_by.clone(), casks].concat()))
            }
            Some(NotOffered::Pinned) => "pinned".to_owned(),
            Some(NotOffered::Service) => brew_service(p.service).to_owned(),
            Some(NotOffered::ServicesUnreadable) => "its service state could not be read".to_owned(),
            Some(NotOffered::FieldMissing) => "brew listed a formula without a field ddc needs".to_owned(),
        };
        writeln!(w, "{:<40} {:>14}  {}", style.bold(&escaped(&p.full_name)), size(p.bytes), style.dim(&note))?;
        if p.kegs.len() > 1 {
            for k in &p.kegs {
                writeln!(w, "  {:<38} {:>14}", escaped(&k.version), size(k.bytes))?;
            }
        }
        if !p.would_orphan.is_empty() {
            let alone = format!("only it needs {} ({})", names(&p.would_orphan), size(p.would_orphan_bytes));
            writeln!(w, "  {}", style.dim(&alone))?;
        }
        if !p.kept_pinned.is_empty() {
            writeln!(w, "  {}", style.dim(&format!("kept, pinned: {}", names(&p.kept_pinned))))?;
        }
    }
    if !r.orphans.is_empty() {
        writeln!(w, "\n{}", style.dim(&format!("Needed by nothing now: {}.", names(&r.orphans))))?;
    }
    if r.services == ServicesState::NotKnown {
        writeln!(
            w,
            "{}",
            style.dim("This brew has no services command: whether a service is registered is not known.")
        )?;
    }
    if !r.incomplete.is_empty() {
        let missing = format!("brew listed {} without a field ddc needs: nothing is offered.", names(&r.incomplete));
        writeln!(w, "{}", style.dim(&missing))?;
    }
    Ok(())
}

/// Why a formula's service keeps its removal from being offered.
pub fn brew_service(s: Service) -> &'static str {
    match s {
        Service::Running => "a service of yours is loaded for it; stop it with brew services first",
        Service::Root => "a root service is registered for it; its state is not known",
        Service::Registered => {
            "a service of yours is registered for it but not running; remove it with brew services first"
        }
        Service::Unreadable => "its service state could not be read",
        Service::NotKnown => "whether a service is registered for it is not known",
        Service::None => "no service of yours registered with launchd",
    }
}

pub fn refusal(r: Refusal) -> &'static str {
    match r {
        Refusal::UnknownRuntime => "no runtime with this identifier is listed",
        Refusal::NotUnderAssets => "its mount path is not under /System/Library/AssetsV2",
        Refusal::NotDeletable => "simctl lists it as not deletable",
        Refusal::Deleting => "it is already being deleted",
        Refusal::NoRuntimeIdentifier => "it has no runtime identifier, so its devices cannot be checked",
        Refusal::Booted => "a simulator on it is running or not yet shut down",
        Refusal::DevicesUnknown => "its devices could not be listed",
        Refusal::NoAssetFolder => "its folder under AssetsV2 is not known, so its size cannot be measured",
        Refusal::NoDeveloperTools => "Xcode's developer tools are not installed",
        Refusal::FirstLaunchPending => "Xcode has not finished its first launch",
        Refusal::SimctlFailed => "simctl failed",
        Refusal::BrewMissing => "/opt/homebrew/bin/brew is missing or did not answer",
        Refusal::PrefixMismatch => "brew --prefix is not /opt/homebrew",
        Refusal::CellarOutside => "brew --cellar is not inside /opt/homebrew",
        Refusal::CacheMismatch => "brew --cache is not ~/Library/Caches/Homebrew",
        Refusal::CondaInHome => "it is in the home folder: the Review window cleans it",
        Refusal::CondaNotInOpt => {
            "it is not an environment or the install of /opt/miniconda3, /opt/anaconda3 or /opt/miniforge3"
        }
        Refusal::CondaMissing => "the install's conda is missing or did not answer",
        Refusal::CondaRootMismatch => "conda's root_prefix is not the install's folder",
        Refusal::CondaBase => "it is the install's base environment",
        Refusal::CondaNotInEnvsDirs => "its folder is not in conda's envs_dirs",
        Refusal::CondaNotAnEnvironment => "it holds no conda-meta/history",
        Refusal::CondaPackagesNotListed => "the install's pkgs folder is not in conda's pkgs_dirs",
        Refusal::CondaNotWritable => "your user cannot write it: conda needs an administrator",
        Refusal::CondaInUse => "a running process uses the environment",
        Refusal::CondaSoftlinks => "an install links packages into its environments by symlink",
        Refusal::CondaNotScoped => "conda's package folders, set in a .condarc, cannot be limited to this one",
        Refusal::BrewNotInstalled => "brew no longer lists it as installed",
        Refusal::BrewPinned => "it is pinned: brew unpin it first",
        Refusal::BrewKegsChanged => "its installed versions changed since you confirmed",
        Refusal::BrewNeeded => "something installed still needs it",
        Refusal::BrewNotOrphan => "it was installed on request, so it is no longer unneeded",
        Refusal::BrewServiceRunning => brew_service(Service::Running),
        Refusal::BrewServiceRoot => brew_service(Service::Root),
        Refusal::BrewServiceRegistered => brew_service(Service::Registered),
        Refusal::BrewFieldMissing => "brew listed a formula without a field ddc needs",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_escaped() {
        let p = Path::new("/h/evil\u{1b}[2J\u{1b}[31mX\n/__pycache__");
        let shown = tilde(p, Path::new("/h"));
        assert!(!shown.chars().any(char::is_control), "{shown:?}");
        assert!(shown.starts_with("~/evil\\u{1b}[2J"));
    }

    #[test]
    fn text_a_harness_wrote_is_escaped() {
        let r: AgentsReport = serde_json::from_str(
            r#"{"schema_version":1,"ddc_version":"0","generated_at":"x","duration_ms":0,"sizes":false,"harnesses":[
              {"id":"claude-code","name":"Claude Code","running":[],"folders":[{"path":"/h/.claude","nested":false,"skipped":false,"unreadable":0}],
               "sessions":{"records":"readable","list":[{"state":"live","cwd":"/h/cod\u202efdp.exe","status":"\u001b[2Jbusy","name":"n\u001b[31m"}],
                 "transcripts":{"skipped":false}}}]}"#,
        )
        .unwrap();
        let mut out = Vec::new();
        agents(&mut out, &r, Path::new("/h"), &Style { color: false, all: false }).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.chars().any(|c| (c.is_control() && c != '\n') || bidi(c)), "{text:?}");
        assert!(text.contains("cod\\u{202e}fdp.exe") && text.contains("\\u{1b}[2Jbusy"), "{text}");
    }

    #[test]
    fn colors_only_on_a_terminal_without_no_color() {
        assert!(use_color(true, false));
        assert!(!use_color(true, true));
        assert!(!use_color(false, false));
    }
}
