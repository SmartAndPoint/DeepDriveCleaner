//! `ddc-loadgen`: builds a load tree and prints its manifest as JSON.

use std::path::PathBuf;
use std::time::Instant;

use clap::{Parser, Subcommand};
use ddc_loadgen::desc::FsCaps;
use ddc_loadgen::profiles::Profile;
use ddc_loadgen::{generate, manifest, target, verify};

#[derive(Parser)]
#[command(about = "Deterministic load trees for DeepDriveCleaner's load tests")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a marked temporary folder and print its path.
    Mktemp,
    /// Remove every marked temporary folder of this user.
    Clean,
    /// Print every profile's name, one per line.
    Profiles,
    /// Build a tree and print its manifest.
    Build {
        profile: Profile,
        #[arg(long, default_value_t = 10_000)]
        entries: u64,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// A marked temporary folder (`mktemp`) or an attached image's mount point.
        #[arg(long)]
        into: PathBuf,
        /// The device `hdiutil attach` returned, for an image target.
        #[arg(long)]
        device: Option<String>,
        /// Scan the tree in process and fail on any difference from the manifest.
        #[arg(long)]
        verify: bool,
        /// Leave the manifest out of the output (its totals stay).
        #[arg(long)]
        brief: bool,
    },
    /// Print a manifest without building (default APFS capabilities).
    Manifest {
        profile: Profile,
        #[arg(long, default_value_t = 10_000)]
        entries: u64,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
}

fn main() -> anyhow::Result<()> {
    let out = match Cli::parse().cmd {
        Cmd::Mktemp => serde_json::json!({ "path": target::mktemp()? }),
        Cmd::Clean => serde_json::json!({ "removed": target::clean()? }),
        Cmd::Profiles => {
            Profile::ALL.iter().for_each(|p| println!("{}", p.name()));
            return Ok(());
        }
        Cmd::Manifest { profile, entries, seed } => {
            let caps = FsCaps { case_insensitive: true, normalization_insensitive: true, invalid_utf8: false };
            let m = manifest::manifest(profile.name(), seed, &profile.desc(entries, seed, caps));
            serde_json::json!({ "hash": m.hash(), "manifest": m })
        }
        Cmd::Build { profile, entries, seed, into, device, verify: check, brief } => {
            let g = generate(profile, entries, seed, &into, device.as_deref())?;
            let started = Instant::now();
            let diffs = if check { verify::verify(&g.manifest, &g.layout, &g.home)? } else { Vec::new() };
            let verify_ms = check.then(|| started.elapsed().as_millis());
            let mut out = serde_json::json!({
                "profile": profile.name(),
                "entries": g.manifest.entries,
                "hash": g.manifest.hash(),
                "tree": g.layout.tree,
                "home": g.home,
                "data": g.layout.data,
                "build_ms": g.build_ms,
                "verify_ms": verify_ms,
                "differences": diffs,
                "difference_count": diffs.len(),
                "totals": g.manifest.totals,
                "manifest": g.manifest,
            });
            if brief {
                out["manifest"] = serde_json::Value::Null;
            }
            if !diffs.is_empty() {
                println!("{}", serde_json::to_string_pretty(&out)?);
                anyhow::bail!("{} differences from the manifest", diffs.len());
            }
            out
        }
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
