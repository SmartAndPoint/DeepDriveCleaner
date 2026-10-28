//! Cleanup plan display, interactive selection and the result summary.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use ddc_core::class::Fallback;
use ddc_core::class::RiskClass;
use ddc_core::exec::{ExecReport, Outcome, SkipReason};
use ddc_core::plan::{Action, Plan, PlanItem};
use ddc_core::rules::DOCKER_DOWN;
use ddc_core::rules::entries::EntryLock;

use crate::render::{Style, tilde};
use crate::size::human;

pub enum Decision {
    Proceed,
    Abort,
}

/// Groups: (class, rule) → item indices, in plan order of first appearance.
fn groups(plan: &Plan) -> Vec<(RiskClass, String, Vec<usize>)> {
    let mut map: BTreeMap<(RiskClass, String), Vec<usize>> = BTreeMap::new();
    for (i, item) in plan.items.iter().enumerate() {
        map.entry((item.finding.class, item.finding.rule.clone())).or_default().push(i);
    }
    let mut groups: Vec<_> = map.into_iter().map(|((c, r), idx)| (c, r, idx)).collect();
    groups.sort_by(|a, b| {
        let size = |g: &(RiskClass, String, Vec<usize>)| g.2.iter().map(|&i| plan.items[i].finding.bytes).sum::<u64>();
        a.0.cmp(&b.0).then(size(b).cmp(&size(a)))
    });
    groups
}

fn action_text(item: &PlanItem) -> String {
    match item.action {
        Action::Delete { .. } => "delete".to_owned(),
        Action::Trash | Action::TrashEntry => "→ Trash".to_owned(),
        Action::Native { cmd, fallback } => {
            let otherwise = match fallback {
                Fallback::Trash => "else → Trash",
                Fallback::Delete | Fallback::DeleteWritable => "else delete",
            };
            format!("`{} {}` ({otherwise})", cmd.program, cmd.args.join(" "))
        }
        Action::NativeOnly(cmds) => {
            let commands =
                cmds.iter().map(|c| format!("`{} {}`", c.program, c.args.join(" "))).collect::<Vec<_>>().join(" + ");
            format!("{commands} — frees space inside the VM image; {}", item.finding.hint)
        }
    }
}

/// What cleaning an item frees: nothing on disk for native-only VM pruning
/// (space is freed inside the VM image, the image may not shrink).
fn frees(item: &PlanItem) -> u64 {
    if matches!(item.action, Action::NativeOnly(_)) { 0 } else { item.finding.reclaimable_bytes() }
}

pub fn print_plan(w: &mut impl Write, plan: &Plan, style: &Style) -> io::Result<()> {
    let selected: Vec<&PlanItem> = plan.items.iter().filter(|i| i.selected).collect();
    writeln!(
        w,
        "\n{} {} items selected, {} (frees ~{})",
        style.bold("Cleanup plan:"),
        selected.len(),
        human(selected.iter().map(|i| i.finding.bytes).sum()),
        human(selected.iter().map(|i| frees(i)).sum())
    )?;
    for (n, (class, rule, idx)) in groups(plan).iter().enumerate() {
        let picked = idx.iter().filter(|&&i| plan.items[i].selected).count();
        let mark = match picked {
            0 => "·",
            p if p == idx.len() => "✓",
            _ => "~",
        };
        let bytes: u64 = idx.iter().map(|&i| plan.items[i].finding.bytes).sum();
        writeln!(
            w,
            "  [{:>2}] {mark} {:<11} {:<26} {:>4}/{:<4} {:>10}  {}",
            n + 1,
            class.as_str(),
            rule,
            picked,
            idx.len(),
            human(bytes),
            style.dim(&action_text(&plan.items[idx[0]]))
        )?;
    }
    Ok(())
}

fn list_group(w: &mut impl Write, plan: &Plan, n: usize, idx: &[usize], home: &Path) -> io::Result<()> {
    for (k, &i) in idx.iter().enumerate() {
        let item = &plan.items[i];
        let mark = if item.selected { "✓" } else { "·" };
        writeln!(
            w,
            "    {n}.{:<3} {mark} {:>10}  {}",
            k + 1,
            human(item.finding.bytes),
            tilde(&item.finding.path, home)
        )?;
    }
    Ok(())
}

fn ask(input: &mut impl BufRead, w: &mut impl Write, question: &str) -> io::Result<Option<String>> {
    write!(w, "{question}")?;
    w.flush()?;
    let mut line = String::new();
    Ok((input.read_line(&mut line)? > 0).then(|| line.trim().to_owned()))
}

/// `--exclude` replaces the folders `settings.json` protects: names those it
/// would no longer protect and asks before going on.
pub fn confirm_dropped_excludes(
    dropped: &[PathBuf],
    home: &Path,
    mut input: impl BufRead,
    w: &mut impl Write,
) -> io::Result<bool> {
    writeln!(w, "--exclude replaces the folders settings.json protects; these would no longer be protected:")?;
    for path in dropped {
        writeln!(w, "  {}", tilde(path, home))?;
    }
    Ok(yes(ask(&mut input, w, "Go on without protecting them? [y/N] ")?))
}

/// The dropped folders on one line, for a warning or a refusal.
pub fn dropped_excludes(dropped: &[PathBuf], home: &Path) -> String {
    let names: Vec<String> = dropped.iter().map(|p| tilde(p, home)).collect();
    format!("--exclude replaces the folders settings.json protects; no longer protected: {}", names.join(", "))
}

/// Asks for one area action after its dry run, default No.
pub fn confirm_area_action(question: &str, mut input: impl BufRead, w: &mut impl Write) -> io::Result<bool> {
    Ok(yes(ask(&mut input, w, &format!("{question} [y/N] "))?))
}

fn yes(answer: Option<String>) -> bool {
    answer.is_some_and(|a| matches!(a.to_lowercase().as_str(), "y" | "yes"))
}

/// Lets the user adjust the plan, confirms every `review` / native-only item
/// one by one, then asks for a final confirmation.
pub fn interactive(
    plan: &mut Plan,
    home: &Path,
    mut input: impl BufRead,
    w: &mut impl Write,
    style: &Style,
) -> io::Result<Decision> {
    loop {
        let groups = groups(plan);
        let Some(line) = ask(
            &mut input,
            w,
            "\nToggle groups by number (\"2 5\"), \"l N\" to list a group, \"t N.M\" to toggle one item, \
             Enter to continue, \"q\" to quit: ",
        )?
        else {
            return Ok(Decision::Abort);
        };
        match line.as_str() {
            "" => break,
            "q" | "quit" => return Ok(Decision::Abort),
            l if l.starts_with('l') => {
                if let Some((n, (_, _, idx))) =
                    l[1..].trim().parse::<usize>().ok().and_then(|n| Some((n, groups.get(n.checked_sub(1)?)?)))
                {
                    list_group(w, plan, n, idx, home)?;
                }
                continue;
            }
            t if t.starts_with('t') => {
                let parsed = t[1..]
                    .trim()
                    .split_once('.')
                    .and_then(|(g, k)| Some((g.parse::<usize>().ok()?, k.parse::<usize>().ok()?)));
                if let Some(i) =
                    parsed.and_then(|(g, k)| groups.get(g.checked_sub(1)?)?.2.get(k.checked_sub(1)?).copied())
                {
                    plan.items[i].selected = !plan.items[i].selected;
                }
            }
            numbers => {
                for n in numbers.split_whitespace().filter_map(|n| n.parse::<usize>().ok()) {
                    if let Some((_, _, idx)) = n.checked_sub(1).and_then(|n| groups.get(n)) {
                        let on = !idx.iter().any(|&i| plan.items[i].selected);
                        idx.iter().for_each(|&i| plan.items[i].selected = on);
                    }
                }
            }
        }
        print_plan(w, plan, style)?;
    }

    for item in plan.items.iter_mut().filter(|i| i.selected) {
        let question = match item.action {
            _ if item.finding.class == RiskClass::Review && matches!(item.action, Action::NativeOnly(_)) => {
                format!("Run {} for {}? [y/N] ", action_text(item), tilde(&item.finding.path, home))
            }
            Action::Trash => {
                format!("Move {} ({}) to the Trash? [y/N] ", tilde(&item.finding.path, home), human(item.finding.bytes))
            }
            Action::Native { .. } if item.finding.class == RiskClass::Review => {
                format!("Run {} for {}? [y/N] ", action_text(item), tilde(&item.finding.path, home))
            }
            _ if item.finding.class == RiskClass::Review => format!(
                "Delete {} ({}) permanently? [y/N] ",
                tilde(&item.finding.path, home),
                human(item.finding.bytes)
            ),
            _ => continue,
        };
        item.selected = yes(ask(&mut input, w, &question)?);
    }
    let chosen: Vec<&PlanItem> = plan.items.iter().filter(|i| i.selected).collect();
    if chosen.is_empty() {
        return Ok(Decision::Proceed);
    }
    if chosen.iter().any(|i| matches!(i.action, Action::Trash | Action::TrashEntry)) {
        writeln!(w, "{}", style.dim("note: the Trash keeps the space until you empty it."))?;
    }
    let question = format!(
        "Clean {} items, {} (frees ~{})? [y/N] ",
        chosen.len(),
        human(chosen.iter().map(|i| i.finding.bytes).sum()),
        human(chosen.iter().map(|i| frees(i)).sum())
    );
    Ok(if yes(ask(&mut input, w, &question)?) { Decision::Proceed } else { Decision::Abort })
}

pub fn reason(r: &SkipReason) -> String {
    match r {
        SkipReason::InUse { pid, name } => format!("in use by {name} (pid {pid})"),
        SkipReason::InUseUnknownCwd { pid, name } => format!("possibly in use by {name} (pid {pid})"),
        SkipReason::OwnerRunning { pid, name } => format!("{name} is running (pid {pid})"),
        SkipReason::NativeUnavailable { program } => format!("{program} is not installed"),
        SkipReason::ToolNotReady { detail, .. } if detail == DOCKER_DOWN => {
            format!("{detail} — start it and run again")
        }
        SkipReason::ToolNotReady { detail, .. } => detail.clone(),
        SkipReason::Missing => "already gone".to_owned(),
        SkipReason::NotADirectory => "no longer a directory".to_owned(),
        SkipReason::Symlink => "replaced by a symlink".to_owned(),
        SkipReason::ChangedSinceScan => "changed since the scan".to_owned(),
        SkipReason::ParentChanged => "a folder above it changed since the scan".to_owned(),
        SkipReason::RuleMismatch => "no longer matches its rule".to_owned(),
        SkipReason::OutsideScanRoots => "outside the scanned folders".to_owned(),
        SkipReason::ScanRoot => "it is a scanned folder itself".to_owned(),
        SkipReason::Protected => "protected folder".to_owned(),
        SkipReason::ProjectRoot => "it is a project root".to_owned(),
        SkipReason::Kept => "protected by .ddc-keep or --exclude".to_owned(),
        SkipReason::NeedsConfirmation => "needs an interactive confirmation".to_owned(),
        SkipReason::ActionNotAllowed => "the rule does not allow this action".to_owned(),
        SkipReason::Incomplete => "partly unreadable or has another filesystem mounted inside".to_owned(),
        SkipReason::NotAnEntry => "no longer a file or folder you can pick in its folder".to_owned(),
        SkipReason::EntryLocked { lock } => format!("locked: {}", lock_text(lock)),
        SkipReason::InsideAnotherItem => "inside a folder that went to the Trash".to_owned(),
    }
}

fn lock_text(lock: &EntryLock) -> String {
    match lock {
        EntryLock::AppManaged { app } => format!("{app} manages it; clean it in {app}"),
        EntryLock::Kept => "protected by .ddc-keep or an exclude".to_owned(),
        EntryLock::Protected => "holds a protected folder".to_owned(),
        EntryLock::OtherVolume => "another volume is mounted inside".to_owned(),
        EntryLock::Recent => "added or changed in the last 10 minutes".to_owned(),
        EntryLock::PartialDownload => "a download still arriving".to_owned(),
        EntryLock::UnfinishedBackup => "the backup did not finish".to_owned(),
    }
}

pub fn print_result(w: &mut impl Write, r: &ExecReport, home: &Path, journal: &Path, style: &Style) -> io::Result<()> {
    let count = |pred: &dyn Fn(&Outcome) -> bool| r.items.iter().filter(|i| pred(&i.outcome)).count();
    writeln!(
        w,
        "\n{} deleted {} ({}) · moved to Trash {} ({}) · ran tools {} · skipped {} · failed {}",
        style.bold("Done:"),
        count(&|o| matches!(o, Outcome::Deleted)),
        human(r.bytes_where(|o| matches!(o, Outcome::Deleted))),
        count(&|o| matches!(o, Outcome::Trashed)),
        human(r.bytes_where(|o| matches!(o, Outcome::Trashed))),
        count(&|o| matches!(o, Outcome::Native { .. })),
        count(&|o| matches!(o, Outcome::Skipped(_))),
        count(&|o| matches!(o, Outcome::Failed { .. })),
    )?;
    if r.cancelled {
        writeln!(w, "Cancelled: {} items were not started.", count(&|o| matches!(o, Outcome::NotRun)))?;
    }
    for v in &r.volumes {
        let on = if r.volumes.len() > 1 { format!(" on {}", v.mount_point.display()) } else { String::new() };
        writeln!(
            w,
            "Free space{on}: {} → {} (APFS may take a moment to reclaim)",
            human(v.free_before),
            human(v.free_after)
        )?;
    }
    for item in &r.items {
        match &item.outcome {
            Outcome::Skipped(why) => writeln!(w, "  skipped  {}: {}", tilde(&item.path, home), reason(why))?,
            Outcome::Failed { error } => writeln!(w, "  failed   {}: {error}", tilde(&item.path, home))?,
            Outcome::Native { commands } if commands.iter().any(|c| !c.success) => {
                writeln!(w, "  tool failed for {}", tilde(&item.path, home))?
            }
            _ => {}
        }
    }
    if r.local_snapshots > 0 {
        writeln!(
            w,
            "{} {} local Time Machine snapshots keep deleted data until macOS thins them.",
            style.dim("note:"),
            r.local_snapshots
        )?;
    }
    writeln!(w, "Journal: {}", tilde(journal, home))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::PathBuf;

    use ddc_core::class::Category;
    use ddc_core::model::{Finding, RemovalKind};

    use super::*;

    fn item(path: &str, class: RiskClass, rule: &str, action: Action, selected: bool) -> PlanItem {
        PlanItem {
            finding: Finding {
                path: PathBuf::from(path),
                rule: rule.to_owned(),
                category: Category::ProjectArtifact,
                class,
                rule_class: class,
                removal: RemovalKind::Delete,
                bytes: 1 << 20,
                shared_bytes: 0,
                private_bytes: None,
                files: 1,
                dirs: 1,
                incomplete: false,
                project: None,
                selected,
                not_selected: None,
                ignored: false,
                notes: vec![],
                hint: String::new(),
                dev: 1,
                ino: 1,
                parent_dev: None,
                parent_ino: None,
                contents: None,
            },
            action,
            default_selected: selected,
            selected,
        }
    }

    fn plan() -> Plan {
        Plan {
            roots: vec![PathBuf::from("/h")],
            items: vec![
                item(
                    "/h/a/__pycache__",
                    RiskClass::Safe,
                    "python.pycache",
                    Action::Delete { make_writable: false },
                    true,
                ),
                item(
                    "/h/b/node_modules",
                    RiskClass::Rebuildable,
                    "node.node_modules",
                    Action::Delete { make_writable: false },
                    false,
                ),
                item("/h/c/node_modules", RiskClass::Review, "node.node_modules-orphan", Action::Trash, false),
            ],
        }
    }

    fn run(input: &str) -> (Decision, Plan) {
        let mut p = plan();
        let mut out = Vec::new();
        let d = interactive(&mut p, Path::new("/h"), Cursor::new(input), &mut out, &Style { color: false, all: false })
            .unwrap();
        (d, p)
    }

    #[test]
    fn enter_then_yes_keeps_the_defaults() {
        let (d, p) = run("\ny\n");
        assert!(matches!(d, Decision::Proceed));
        assert_eq!(p.items.iter().map(|i| i.selected).collect::<Vec<_>>(), [true, false, false]);
    }

    #[test]
    fn review_needs_its_own_yes() {
        // groups: 1 = safe pycache, 2 = rebuildable, 3 = review orphan
        let (_, p) = run("3\n\nn\ny\n");
        assert!(!p.items[2].selected, "declined review item is dropped");
        let (_, p) = run("3\n\ny\ny\n");
        assert!(p.items[2].selected);
    }

    #[test]
    fn toggling_groups_and_items() {
        let (_, p) = run("1 2\n\ny\n");
        assert_eq!(p.items.iter().map(|i| i.selected).collect::<Vec<_>>(), [false, true, false]);
        let (_, p) = run("t 2.1\n\ny\n");
        assert!(p.items[1].selected);
    }

    #[test]
    fn quit_eof_or_no_aborts() {
        assert!(matches!(run("q\n").0, Decision::Abort));
        assert!(matches!(run("").0, Decision::Abort));
        assert!(matches!(run("\nn\n").0, Decision::Abort));
    }

    #[test]
    fn a_runtime_that_does_not_answer_says_what_to_do() {
        let detail = "Docker Desktop is not running or not answering".to_owned();
        assert_eq!(
            reason(&SkipReason::ToolNotReady { program: "docker".into(), detail }),
            "Docker Desktop is not running or not answering — start it and run again"
        );
        let detail = "the desktop-linux context does not exist".to_owned();
        assert_eq!(
            reason(&SkipReason::ToolNotReady { program: "docker".into(), detail }),
            "the desktop-linux context does not exist",
            "starting Docker Desktop does not create a missing context"
        );
        assert_eq!(reason(&SkipReason::NativeUnavailable { program: "docker".into() }), "docker is not installed");
    }
}
