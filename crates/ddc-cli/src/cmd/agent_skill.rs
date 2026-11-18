//! `ddc agent skill install` and `ddc agent skill status` (spec "Agent skill
//! install"; ADR "Agent footprint", §7, "Installing the guidance"): the Claude
//! Code skill and the `AGENTS.md` block, from the copies built into the
//! binary. It writes ddc's own text and deletes nothing; it takes no lock and
//! writes no journal line. At a terminal it asks first; without one it needs
//! `--approved`, which an agent passes only after the person said yes.

use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ddc_core::env::Env;
use ddc_core::guidance::{self, Family, FileState, FolderPlan, FolderState, FolderStatus, PlanError, SkillReport};
use ddc_core::platform::procs;

use crate::cli::{SkillInstallArgs, SkillStatusArgs};
use crate::{exit, prompt};

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn install(args: &SkillInstallArgs) -> anyhow::Result<ExitCode> {
    if procs::is_root() {
        return refuse("install", "refusing to run as root; run it as your user");
    }
    let env = Env::from_process(None)?;
    let root = match root(&env, args.project.as_deref()) {
        Ok(root) => root,
        Err(why) => return refuse("install", &why),
    };
    // Every folder planned first: a refusal in one refuses the install.
    let mut families: Vec<Family> = args.family.iter().map(|h| (*h).into()).collect();
    if families.is_empty() {
        families = Family::ALL.to_vec();
    }
    families.sort();
    families.dedup();
    let mut folders: Vec<FolderPlan> = Vec::new();
    for family in families {
        match guidance::folder_plan(&family.folder(&root), VERSION, args.replace) {
            Ok(plan) => folders.push(plan),
            Err(PlanError::Io(e)) => return Err(e.into()),
            Err(refusal) => return refuse("install", &refusal.to_string()),
        }
    }
    let agents_md = match &args.agents_md {
        None => None,
        Some(file) => match guidance::agents_md_plan(file) {
            Ok(bytes) => bytes.map(|b| (file.clone(), b)),
            Err(PlanError::Io(e)) => return Err(e.into()),
            Err(refusal) => return refuse("install", &refusal.to_string()),
        },
    };
    let mut out = std::io::stdout().lock();
    let mut writes: Vec<&Path> = folders.iter().flat_map(|f| f.writes.iter().map(PathBuf::as_path)).collect();
    writes.extend(agents_md.iter().map(|(file, _)| file.as_path()));
    if writes.is_empty() {
        for folder in &folders {
            writeln!(out, "The ddc guidance is current: {}", folder.status.path.display())?;
            others(&mut out, &folder.status)?;
        }
        return Ok(ExitCode::SUCCESS);
    }
    writeln!(out, "ddc {VERSION} will write its guidance for coding agents, and delete nothing:")?;
    for path in &writes {
        writeln!(out, "  {}", path.display())?;
    }
    for folder in &folders {
        others(&mut out, &folder.status)?;
    }
    if !args.approved {
        if !std::io::stdin().is_terminal() {
            return refuse(
                "install",
                "writing the guidance needs you at a terminal, or --approved once the person has agreed to it",
            );
        }
        if !prompt::confirm_area_action("Write these files?", std::io::stdin().lock(), &mut out)? {
            writeln!(out, "Nothing changed.")?;
            return Ok(ExitCode::FAILURE);
        }
    }
    for folder in &folders {
        guidance::write_folder(folder)?;
    }
    if let Some((file, bytes)) = &agents_md {
        guidance::write_agents_md(file, bytes)?;
    }
    writeln!(out, "Wrote the ddc guidance. A new session of the coding agent finds it.")?;
    Ok(ExitCode::SUCCESS)
}

pub fn status(args: &SkillStatusArgs) -> anyhow::Result<ExitCode> {
    let env = Env::from_process(None)?;
    // The paths first: a rejected one is exit 2 whatever the home holds.
    let project = match args.project.as_deref().map(|p| root(&env, Some(p))).transpose() {
        Ok(project) => project,
        Err(why) => return refuse("status", &why),
    };
    if let Some(file) = args.agents_md.as_deref().filter(|f| !parent_exists(f)) {
        return refuse("status", &format!("{}: its folder does not exist", file.display()));
    }
    let mut skills = Vec::new();
    for root in std::iter::once(env.home()).chain(project.as_deref()) {
        for family in Family::ALL {
            skills.push(guidance::folder_status(&family.folder(root))?);
        }
    }
    let agents_md = args.agents_md.as_deref().map(guidance::agents_md_status).transpose()?;
    let report = SkillReport::new(skills, agents_md);
    let mut stdout = std::io::stdout().lock();
    let written = if args.json {
        serde_json::to_writer_pretty(&mut stdout, &report).map_err(std::io::Error::from).and_then(|()| writeln!(stdout))
    } else {
        text(&mut stdout, &report)
    };
    match written {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        other => other.map(|()| ExitCode::SUCCESS).map_err(Into::into),
    }
}

/// The folder the skill goes under: the home folder, or an existing `DIR`.
fn root(env: &Env, project: Option<&Path>) -> Result<PathBuf, String> {
    match project {
        None => Ok(env.home().to_path_buf()),
        Some(dir) if dir.is_dir() => Ok(dir.to_path_buf()),
        Some(dir) if dir.exists() => Err(format!("{}: not a folder", dir.display())),
        Some(dir) => Err(format!("{}: no such folder", dir.display())),
    }
}

fn parent_exists(file: &Path) -> bool {
    file.parent().filter(|p| !p.as_os_str().is_empty()).is_none_or(Path::is_dir)
}

fn others(out: &mut impl std::io::Write, status: &FolderStatus) -> std::io::Result<()> {
    for name in &status.no_longer_shipped {
        writeln!(out, "  kept, no longer shipped: {name}")?;
    }
    for name in &status.extra {
        writeln!(out, "  kept, not ddc's: {name}")?;
    }
    Ok(())
}

fn text(out: &mut impl std::io::Write, report: &SkillReport) -> std::io::Result<()> {
    for skill in &report.skills {
        let state = match &skill.state {
            FolderState::Absent => "not installed".to_owned(),
            FolderState::Current => format!("current ({VERSION})"),
            FolderState::Older { version } => {
                format!("from ddc {version}; `ddc agent skill install` brings it to {VERSION}")
            }
            FolderState::Edited => "edited since ddc installed it".to_owned(),
            FolderState::NotOurs { version: Some(v) } => format!("not installed by ddc (its SKILL.md says {v})"),
            FolderState::NotOurs { version: None } => "not installed by ddc".to_owned(),
            FolderState::Link => "a symbolic link; ddc leaves it alone".to_owned(),
        };
        writeln!(out, "{}: {state}", skill.path.display())?;
        others(out, skill)?;
    }
    if let Some(file) = &report.agents_md {
        let state = match file.state {
            FileState::Absent => "no ddc block",
            FileState::Current => "its ddc block is current",
            FileState::Differs => "its ddc block differs from this ddc's",
            FileState::Malformed => "its ddc markers are broken; ddc leaves it alone",
            FileState::Link => "a symbolic link; ddc leaves it alone",
        };
        writeln!(out, "{}: {state}", file.path.display())?;
    }
    Ok(())
}

fn refuse(command: &str, why: &str) -> anyhow::Result<ExitCode> {
    eprintln!("ddc agent skill {command}: {why}");
    Ok(ExitCode::from(exit::REFUSED))
}
