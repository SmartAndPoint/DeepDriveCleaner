//! `ddc-loadgen`: builds a load tree and prints its manifest as JSON.

use std::path::PathBuf;

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
        Cmd::Manifest { profile, entries, seed } => {
            let caps = FsCaps { case_insensitive: true, normalization_insensitive: true, invalid_utf8: false };
            let m = manifest::manifest(profile.name(), seed, &profile.desc(entries, seed, caps));
            serde_json::json!({ "hash": m.hash(), "manifest": m })
        }
        Cmd::Build { profile, entries, seed, into, device, verify: check } => {
            let g = generate(profile, entries, seed, &into, device.as_deref())?;
            let diffs = if check { verify::verify(&g.manifest, &g.layout, &g.home)? } else { Vec::new() };
            let out = serde_json::json!({
                "hash": g.manifest.hash(),
                "tree": g.layout.tree,
                "home": g.home,
                "data": g.layout.data,
                "build_ms": g.build_ms,
                "differences": diffs,
                "manifest": g.manifest,
            });
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
