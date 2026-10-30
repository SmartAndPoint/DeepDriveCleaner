//! Deterministic load trees for DeepDriveCleaner (epic DDC-005, story
//! "Synthetic load trees: large, wide, deep and hostile"). A profile, a size
//! and a seed give a tree with the same shape on every run, and a manifest
//! of what a scan of it must report. Trees are built only in a marked
//! temporary folder or on a disk image the generator attached ([`target`]).

pub mod build;
pub mod caps;
pub mod desc;
pub mod fixtures;
pub mod heap;
pub mod isolate;
pub mod items;
pub mod leaks;
pub mod limits;
pub mod manifest;
pub mod measure;
pub mod platform;
pub mod profiles;
pub mod rng;
pub mod run;
pub mod scaling;
pub mod table;
pub mod target;
pub mod verify;

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::manifest::Manifest;
use crate::profiles::Profile;
use crate::target::Layout;

/// A tree on disk and what a scan of it must report.
#[derive(Debug)]
pub struct Generated {
    pub manifest: Manifest,
    pub layout: Layout,
    /// `HOME` for every run on this tree: inside it, or the empty one beside it.
    pub home: PathBuf,
    pub build_ms: u128,
}

/// Checks `target` ([`target::prepare`]), then builds `profile` in it.
pub fn generate(
    profile: Profile,
    entries: u64,
    seed: u64,
    target: &Path,
    device: Option<&str>,
) -> anyhow::Result<Generated> {
    platform::builds()?;
    let layout = target::prepare(target, device, entries)?;
    let caps = caps::probe(&layout.data.join("caps"))?;
    let desc = profile.desc(entries, seed, caps);
    let started = Instant::now();
    build::build(&desc, &layout.tree)?;
    let build_ms = started.elapsed().as_millis();
    let home = match &desc.home {
        Some(rel) => layout.tree.join(
            platform::name(rel).ok_or_else(|| anyhow::anyhow!("{}: not a name here", String::from_utf8_lossy(rel)))?,
        ),
        None => layout.home.clone(),
    };
    Ok(Generated { manifest: manifest::manifest(profile.name(), seed, &desc), layout, home, build_ms })
}

/// What a test that reaches the builder through [`generate`] gets: the tree,
/// or `None` (after saying so) on a platform with no builder yet, where it
/// skips. Any other error fails the test.
pub fn skip_unported(generated: anyhow::Result<Generated>) -> Option<Generated> {
    match generated {
        Ok(g) => Some(g),
        Err(e) if e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::Unsupported) => {
            eprintln!("skipped: {e:#}");
            None
        }
        Err(e) => panic!("generate: {e:#}"),
    }
}
