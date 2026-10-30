//! `ddc keep remove`: removes the `.ddc-keep` in one folder, and nothing
//! else (story "Protected by you: say what it is, and Remove Protection";
//! safety ADR, amendment of 2026-10-30; menu bar ADR §6 item 12). As `disk
//! free`: without the folder's identity a terminal asks before the lock, and
//! nothing asks without one; then the exclusive lock, and every decision
//! under it is journaled.

use std::io::{IsTerminal as _, Write as _};
use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use ddc_core::env::Env;
use ddc_core::exec::FileJournal;
use ddc_core::keep::{self, KeepRefusal, KeepReport, KeepResult, Target};
use ddc_core::platform::procs;
use ddc_core::settings::ScanFlags;
use ddc_core::store::{self, LockMode};

use crate::cli::KeepRemoveArgs;
use crate::render;
use crate::{exit, prompt};

pub fn remove(args: &KeepRemoveArgs) -> anyhow::Result<ExitCode> {
    if procs::is_root() {
        return refuse("refusing to run as root; run it as your user");
    }
    // Refused before the lock, the data directory or the journal is touched.
    if !keep::plain(&args.folder) {
        return refuse(&format!(
            "{} is not an absolute path without ..; give the folder as the scan report names it",
            render::escaped(&args.folder.display().to_string())
        ));
    }
    let identity = args.folder_dev.zip(args.folder_ino);
    if identity.is_none() && !std::io::stdin().is_terminal() {
        return refuse(
            "confirmation needs a terminal; the app passes the folder's identity with --folder-dev and --folder-ino",
        );
    }
    let env = Env::from_process(args.data_dir.clone())?;
    let resolved = super::resolve(&env, ScanFlags::default())?;
    super::note_missing(&resolved.missing);
    let roots = keep::scan_roots(&env, &resolved.options)?;

    // Asked before the exclusive lock: a declined answer leaves no trace.
    if identity.is_none() && !ask(&args.folder, env.home())? {
        return Ok(ExitCode::SUCCESS);
    }

    let _lock = store::try_lock(&env, LockMode::Exclusive)?;
    let mut journal = FileJournal::open(&env).context("cannot open the action journal")?;
    let target = Target { folder: args.folder.clone(), identity, rule: keep::kept_rule(&env, &args.folder) };
    let report = keep::unprotect(&target, &roots, &mut journal).map_err(|e| match e.kind() {
        // A platform refusal keeps its own message, and `main` still finds it (exit 69).
        std::io::ErrorKind::Unsupported => anyhow::Error::from(e),
        _ => anyhow::Error::from(e).context("cannot write the action journal"),
    })?;
    print(args, &report, env.home())?;
    let done = matches!(report.result, KeepResult::Removed | KeepResult::Absent);
    Ok(if args.json || done { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// At a terminal, without the folder's identity: one question, default No.
fn ask(folder: &Path, home: &Path) -> anyhow::Result<bool> {
    let mut out = std::io::stdout().lock();
    let question = format!(
        "Remove .ddc-keep from {}? The folder stays; it is no longer protected, and the next scan lists \
         what lies beneath in its own group, where it may be selected for cleanup.",
        render::tilde(folder, home)
    );
    if prompt::confirm_area_action(&question, std::io::stdin().lock(), &mut out)? {
        return Ok(true);
    }
    writeln!(out, "Nothing confirmed; nothing removed.")?;
    Ok(false)
}

fn print(args: &KeepRemoveArgs, report: &KeepReport, home: &Path) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    let written = if args.json {
        serde_json::to_writer_pretty(&mut out, report).map_err(std::io::Error::from).and_then(|()| writeln!(out))
    } else {
        writeln!(out, "{}", line(report, home))
    };
    match written.and_then(|()| out.flush()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other.map_err(Into::into),
    }
}

/// The one line printed without `--json`.
fn line(report: &KeepReport, home: &Path) -> String {
    let folder = render::tilde(&report.path, home);
    match &report.result {
        KeepResult::Removed => format!("Removed .ddc-keep from {folder}"),
        KeepResult::Absent => format!("No .ddc-keep in {folder}"),
        KeepResult::Refused { reason } => match reason {
            KeepRefusal::OutsideScanRoots => format!("Refused: {folder} is outside the folders ddc scans"),
            KeepRefusal::ChangedSinceScan => {
                format!("Refused: {folder} changed since the scan (another folder, or a symlink); scan again")
            }
            KeepRefusal::NoFolder => format!("Refused: {folder} does not exist"),
            KeepRefusal::NotAFile => format!(
                "Refused: {} is not a file (a symlink or a folder); remove it in Finder",
                render::tilde(&report.path.join(ddc_core::rules::KEEP_MARKER), home)
            ),
        },
        KeepResult::Failed { error } => {
            format!("Could not remove .ddc-keep from {folder}: {}", render::escaped(error))
        }
    }
}

fn refuse(why: &str) -> anyhow::Result<ExitCode> {
    eprintln!("ddc keep remove: {why}");
    Ok(ExitCode::from(exit::REFUSED))
}
