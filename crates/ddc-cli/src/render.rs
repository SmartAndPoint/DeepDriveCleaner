//! Human-readable scan and disk reports.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

use ddc_core::class::{Category, RiskClass};
use ddc_core::disk::{DiskReport, HeldKind, HomeSource, SimulatorStatus};
use ddc_core::model::{Finding, NotSelected, ScanReport};
use ddc_core::rules::Note;

use crate::size::{count, human};

/// Findings shown per group unless `--all`.
const PER_GROUP: usize = 8;

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
    shown
        .chars()
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
    fn colors_only_on_a_terminal_without_no_color() {
        assert!(use_color(true, false));
        assert!(!use_color(true, true));
        assert!(!use_color(false, false));
    }
}
