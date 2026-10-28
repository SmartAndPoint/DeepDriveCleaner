//! A tree that fights the scan: folders it cannot open, inside a finding
//! and outside any, and a `churn/` folder of filler that [`mutate`] deletes,
//! renames and re-creates while a scan runs. The rest is `projects`.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use crate::desc::{Desc, join};
use crate::fixtures;
use crate::profiles::projects;
use crate::rng::Rng;

/// The folder [`mutate`] works in; nothing in it matches a rule.
pub const CHURN: &str = "churn";

pub fn fill(d: &mut Desc, entries: u64, rng: &mut Rng) {
    let budget = entries.saturating_sub(d.len() as u64);
    d.file(b"hostile/locked/secret/f", 1);
    d.lock(b"hostile/locked/secret");
    // A finding with a folder it cannot open: one error, incomplete.
    let sealed = b"hostile/sealed/node_modules";
    d.file(b"hostile/sealed/package.json", 0);
    d.file(b"hostile/sealed/yarn.lock", 0);
    d.file(join(sealed, b"a/f"), 1);
    d.file(join(sealed, b"x/f"), 1);
    d.lock(join(sealed, b"x"));
    d.plant(sealed, "node.node_modules");
    // Findings whose entries a test makes vanish while they are read.
    for project in [&b"hostile/gone"[..], b"hostile/flaky"] {
        d.file(join(project, b"package.json"), 0);
        d.file(join(project, b"yarn.lock"), 0);
        let at = join(project, b"node_modules");
        for name in [&b"a"[..], b"keep"] {
            d.file(join(&join(&at, name), b"f"), 1);
        }
        d.file(join(&at, b"x"), 1);
        d.plant(&at, "node.node_modules");
    }
    fixtures::payload(d, rng, CHURN.as_bytes(), budget / 4);
    projects::fill(d, entries, rng);
}

/// Deletes, renames and creates filler in `<tree>/churn` until `stop` is
/// set; returns how many changes it made. Every change races the scan on
/// purpose, so failures (an entry already gone) are ignored.
#[allow(clippy::disallowed_methods)] // it deletes filler of its own tree, never a finding
pub fn mutate(tree: &Path, seed: u64, stop: &AtomicBool) -> u64 {
    let churn = tree.join(CHURN);
    let mut rng = Rng::new(seed);
    let mut changes = 0;
    while !stop.load(Relaxed) {
        // A folder of churn, sometimes one level down.
        let mut dir = churn.clone();
        if rng.chance(50)
            && let Some(sub) = list(&dir).into_iter().filter(|p| is_dir(p)).nth(rng.range(0, 7) as usize)
        {
            dir = sub;
        }
        let entries = list(&dir);
        let n = changes;
        let done = match (rng.range(0, 3), entries.is_empty()) {
            (0, false) => {
                let victim = rng.pick(&entries);
                if is_dir(victim) { fs::remove_dir_all(victim) } else { fs::remove_file(victim) }
            }
            (1, false) => fs::rename(rng.pick(&entries), dir.join(format!("r{n}"))),
            (2, _) => {
                fs::create_dir(dir.join(format!("n{n}"))).and_then(|()| fs::write(dir.join(format!("n{n}/f")), b"x"))
            }
            _ => fs::write(dir.join(format!("c{n}")), b"x"),
        };
        changes += u64::from(done.is_ok());
    }
    changes
}

/// Without following a link, so a change never leaves `churn/`.
fn is_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

fn list(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<_> = fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    out.sort();
    out
}
