//! Human-readable scan report.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

use ddc_core::class::{Category, RiskClass};
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
        RiskClass::UserData => "USER DATA — reported only, never cleaned",
    }
}

/// `/Users/me/x` → `~/x`. Control characters are escaped so a crafted file
/// name cannot move the cursor or recolor a confirmation prompt.
pub fn tilde(path: &Path, home: &Path) -> String {
    let shown = match path.strip_prefix(home) {
        Ok(rel) if rel.as_os_str().is_empty() => "~".to_owned(),
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => path.display().to_string(),
    };
    shown.chars().map(|c| if c.is_control() { c.escape_default().to_string() } else { c.to_string() }).collect()
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
            RiskClass::UserData => "report only".to_owned(),
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
