//! `ddc-loadgen heap`: one `scan --json` of a profile's tree by a CLI built
//! with `ddc-core/dhat-heap` (story "Memory and CPU under load: peak memory,
//! leaks and CPU time"), isolated like a load run. The CLI writes
//! `dhat-heap.json` in its working directory, the tree's home; it is kept
//! as `dhat-<size>-<profile>.json` in `out` for `bench/dhat_top.py`.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use ddc_core::settings::SETTINGS;
use ddc_core::store::LAST_SCAN;

use crate::generate;
use crate::isolate::{self, Target};
use crate::measure::measure;
use crate::profiles::Profile;
use crate::run::tail;

/// What dhat writes in the working directory.
pub const DHAT_FILE: &str = "dhat-heap.json";

#[derive(Debug, serde::Serialize)]
pub struct Heap {
    pub profile: String,
    pub entries: u64,
    pub build_ms: u128,
    pub wall_ms: u128,
    pub max_rss_bytes: u64,
    /// The kept profile.
    pub file: PathBuf,
}

pub struct HeapArgs<'a> {
    pub profile: Profile,
    pub entries: u64,
    pub seed: u64,
    pub into: &'a Path,
    pub device: Option<&'a str>,
    pub cli: &'a Path,
    pub size: &'a str,
    pub out: &'a Path,
}

pub fn run(a: &HeapArgs) -> anyhow::Result<Heap> {
    let real_home = isolate::real_home()?;
    let cli = a.cli.canonicalize()?;
    let g = generate(a.profile, a.entries, a.seed, a.into, a.device)?;
    fs::write(g.layout.data.join(SETTINGS), r#"{"schema_version":1}"#)?;
    let mut args: Vec<OsString> = ["scan", "--json", "--data-dir"].iter().map(OsString::from).collect();
    args.extend([g.layout.data.clone().into(), g.layout.tree.clone().into()]);
    let on = Target { dir: a.into.to_owned(), device: a.device.map(str::to_owned), entries: a.entries };
    isolate::decide(&isolate::facts(&args, &g.home, &real_home, &g.layout, &on, false)?)?;
    let m = measure(&cli, &g.home, &args, &g.layout.data.join("heap"))?;
    if !m.status.success() {
        anyhow::bail!("the scan exited {}; stderr: {}", m.status, tail(&m.stderr));
    }
    if !g.layout.data.join(LAST_SCAN).is_file() {
        anyhow::bail!("the scan wrote no {LAST_SCAN}: it did not finish its walk");
    }
    let written = g.home.join(DHAT_FILE);
    if !written.is_file() {
        anyhow::bail!("no {DHAT_FILE} in {}: is {} built with ddc-core/dhat-heap?", g.home.display(), cli.display());
    }
    fs::create_dir_all(a.out)?;
    let file = a.out.join(format!("dhat-{}-{}.json", a.size, a.profile.name()));
    fs::copy(&written, &file)?;
    Ok(Heap {
        profile: a.profile.name().to_owned(),
        entries: g.manifest.entries,
        build_ms: g.build_ms,
        wall_ms: m.wall.as_millis(),
        max_rss_bytes: m.usage.max_rss_bytes,
        file,
    })
}
