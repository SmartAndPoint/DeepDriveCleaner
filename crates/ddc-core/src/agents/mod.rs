//! `ddc disk agents`: what coding-agent harnesses keep under the home
//! folder, and which of their sessions are live (story "Coding agents in
//! the Whole disk view"). Read-only and lock-free. It reads the named fields
//! of Claude Code's session records, the names of Codex's lock files and
//! rollouts, the process list with the open files of the user's processes,
//! and allocated sizes; never a transcript's content, a `.key` file or a
//! harness database. With sizes it also measures Claude Code's temporary
//! folder outside the home, and each Claude Code session's bytes (story
//! "Per-session footprint: what each agent session keeps in its
//! transcripts, temporary folders and worktrees").

mod claude;
mod codex;
mod footprint;
pub mod liveness;
mod model;
mod running;
mod sizes;

use crate::platform::Canonical as _;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Instant;

pub use claude::utc_ctime;
pub use model::*;
pub use running::Installs;

/// Where Cursor's agent CLI installs its versions, from the home folder.
pub const CURSOR_AGENT: &str = ".local/share/cursor-agent";

use crate::env::Env;
use crate::platform::procs::{self, ProcInfo};
use crate::rules::Catalog;
use sizes::{Shape, TranscriptBytes};

/// One folder of a harness, relative to the home folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderDef {
    pub path: &'static str,
    /// Lies inside the folder before it.
    pub nested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessDef {
    pub id: HarnessId,
    pub name: &'static str,
    pub folders: &'static [FolderDef],
}

const fn top(path: &'static str) -> FolderDef {
    FolderDef { path, nested: false }
}

const fn nested(path: &'static str) -> FolderDef {
    FolderDef { path, nested: true }
}

/// Every harness and the folders it keeps, from the research note "Coding-agent
/// harnesses on macOS: where their data lives and how to tell a live
/// session". The rest of `Library/Application Support/Claude` is the
/// desktop chat app's, and `Library/Application Support/Cursor` the
/// editor's state: neither is listed.
pub const HARNESSES: &[HarnessDef] = &[
    HarnessDef {
        id: HarnessId::ClaudeCode,
        name: "Claude Code",
        folders: &[top(".claude"), nested(".claude/projects"), top("Library/Application Support/Claude/claude-code")],
    },
    HarnessDef {
        id: HarnessId::Codex,
        name: "Codex",
        folders: &[top(".codex"), nested(".codex/sessions"), nested(".codex/worktrees"), top(".cache/codex-runtimes")],
    },
    HarnessDef { id: HarnessId::Cursor, name: "Cursor", folders: &[top(".cursor")] },
    HarnessDef {
        id: HarnessId::Opencode,
        name: "opencode",
        folders: &[top(".local/share/opencode"), top(".cache/opencode"), top(".config/opencode")],
    },
];

#[derive(Debug, Clone, Default)]
pub struct AgentsOptions {
    /// Walk the folders; without it every `bytes` is absent.
    pub sizes: bool,
    /// Worker threads; `None` = available parallelism.
    pub threads: Option<usize>,
    /// The user's skips, canonical: never opened, at or below.
    pub user_skips: Vec<PathBuf>,
}

/// A harness's existing folder, canonical.
struct Found {
    path: PathBuf,
    nested: bool,
    skipped: bool,
}

/// The report. `procs` are the user's processes (their open files are not
/// needed); a set `cancel` stops the walk.
pub fn report(env: &Env, opts: &AgentsOptions, procs: &[ProcInfo], cancel: &AtomicBool) -> AgentsReport {
    let started = Instant::now();
    let home = env.home();
    let catalog = Catalog::builtin(home);
    let skipped = |path: &Path| catalog.never_open(path, &opts.user_skips).is_some();
    let pids: Vec<i32> = procs.iter().map(|p| p.pid).collect();
    let packages = home.join(".codex/packages");
    let cursor_agent = home.join(CURSOR_AGENT);
    let installs = Installs { codex_packages: &packages, cursor_agent: &cursor_agent };

    let found: Vec<Vec<Found>> = HARNESSES
        .iter()
        .map(|def| {
            def.folders
                .iter()
                .filter_map(|f| {
                    let path = home.join(f.path);
                    path.metadata().ok().filter(std::fs::Metadata::is_dir)?;
                    let path = path.canonical().ok()?;
                    Some(Found { skipped: skipped(&path), path, nested: f.nested })
                })
                .collect()
        })
        .collect();

    // Sessions first: the transcripts the walk sums leave the live ones out.
    let mut transcripts: Vec<TranscriptBytes> = Vec::new();
    let mut sessions: Vec<Option<(Sessions, Option<usize>)>> = Vec::new();
    for (def, folders) in HARNESSES.iter().zip(&found) {
        let base = |name: &str| folders.first().map(|f| &f.path).filter(|p| p.ends_with(name));
        let read = match def.id {
            HarnessId::ClaudeCode => base(".claude").map(|b| claude_sessions(b, &skipped)),
            HarnessId::Codex => base(".codex").map(|b| codex_sessions(b, &skipped, &pids)),
            HarnessId::Cursor | HarnessId::Opencode => None,
        };
        sessions.push(read.map(|(s, t)| {
            let i = t.filter(|_| opts.sizes).map(|t| {
                transcripts.push(t);
                transcripts.len() - 1
            });
            (s, i)
        }));
    }

    // Claude Code's sessions by id, and its temporary folder: with sizes only.
    let claude_base = HARNESSES
        .iter()
        .zip(&found)
        .find(|(def, _)| def.id == HarnessId::ClaudeCode)
        .and_then(|(_, folders)| folders.first())
        .map(|f| &f.path)
        .filter(|p| p.ends_with(".claude"));
    // Opened only when it is the current user's own folder; refused, it is never listed.
    let (temp, refused) = match claude_base.filter(|_| opts.sizes).and_then(|_| env.claude_temp_base()) {
        Some(base) => match temp_base(&base, procs::uid()) {
            Some(Ok(p)) => (Some((skipped(&p), p)), None),
            Some(Err(why)) => (None, Some((base, why))),
            None => (None, None),
        },
        None => (None, None),
    };
    let walked_temp = temp.as_ref().filter(|(s, _)| !s).map(|(_, p)| p.as_path());
    let footprints =
        claude_base.filter(|_| opts.sizes).map(|base| (base, footprint::collect(base, walked_temp, &skipped)));

    let mut measured: Vec<(PathBuf, bool)> =
        found.iter().flatten().filter(|f| !f.skipped).map(|f| (f.path.clone(), f.nested)).collect();
    // A root of its own, in no harness's folders: the harness bytes stay the home's.
    let harness_folders = measured.len();
    measured.extend(walked_temp.map(|p| (p.to_path_buf(), false)));
    let footprint_bytes = footprints.as_ref().map(|(_, (_, bytes))| bytes);
    let sizes = opts.sizes.then(|| {
        sizes::measure(&measured, &catalog, &opts.user_skips, &transcripts, footprint_bytes, opts.threads, cancel)
    });
    let temp_root = temp.map(|(skipped, path)| {
        let size = sizes.as_ref().and_then(|s| s.get(harness_folders)).filter(|_| !skipped);
        TempRoot {
            path,
            skipped,
            refused: None,
            bytes: size.map(|s| s.bytes),
            unreadable: size.map_or(0, |s| s.unreadable),
        }
    });
    let temp_root = temp_root.or_else(|| {
        refused.map(|(path, why)| TempRoot {
            skipped: skipped(&path),
            path,
            refused: Some(why),
            bytes: None,
            unreadable: 0,
        })
    });
    let mut footprints = footprints.map(|(base, (found, bytes))| {
        let records = base.join("sessions");
        footprint::assemble(found, &bytes, &records, skipped(&records), &pids, env.now())
    });
    let mut temp_root = Some(temp_root);
    let size_of = |path: &Path| {
        let i = measured[..harness_folders].iter().position(|(p, _)| p == path)?;
        sizes.as_ref().map(|s| s[i])
    };

    let mut harnesses = Vec::new();
    for ((def, folders), sessions) in HARNESSES.iter().zip(&found).zip(sessions) {
        if folders.is_empty() {
            continue;
        }
        let folders: Vec<Folder> = folders
            .iter()
            .map(|f| {
                let size = if f.skipped { None } else { size_of(&f.path) };
                Folder {
                    path: f.path.clone(),
                    nested: f.nested,
                    skipped: f.skipped,
                    bytes: size.map(|s| s.bytes),
                    unreadable: size.map_or(0, |s| s.unreadable),
                }
            })
            .collect();
        // None when no top-level folder was measured: all skipped is not 0 bytes.
        let tops = || folders.iter().filter(|f| !f.nested).filter_map(|f| f.bytes);
        let bytes = (opts.sizes && tops().next().is_some()).then(|| tops().sum());
        let sessions = sessions.map(|(mut s, i)| {
            s.transcripts.bytes = i.map(|i| transcripts[i].bytes.load(Relaxed));
            if def.id == HarnessId::ClaudeCode {
                s.footprints = footprints.take();
                s.temp_root = temp_root.take().flatten();
            }
            s
        });
        harnesses.push(Harness {
            id: def.id,
            name: def.name.to_owned(),
            running: running::of(def.id, procs, installs),
            folders,
            bytes,
            sessions,
        });
    }

    AgentsReport {
        schema_version: AGENTS_SCHEMA_VERSION,
        ddc_version: env!("CARGO_PKG_VERSION").to_owned(),
        generated_at: humantime::format_rfc3339_seconds(env.now()).to_string(),
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        sizes: opts.sizes,
        harnesses,
    }
}

/// Claude Code's sessions under `base` (`~/.claude`), and its transcripts
/// to sum when they are not skipped.
fn claude_sessions(base: &Path, skipped: &dyn Fn(&Path) -> bool) -> (Sessions, Option<TranscriptBytes>) {
    let records = base.join("sessions");
    let (state, reason, list) = if skipped(&records) {
        (RecordsState::Unknown, Some(UnknownReason::Skipped), Vec::new())
    } else {
        claude::sessions(&records)
    };
    let projects = base.join("projects");
    let names = claude_transcripts(&projects, skipped);
    finish(state, reason, list, names, projects, Shape::Claude)
}

/// Codex's threads under `base` (`~/.codex`); the open files of `pids` say
/// which are open.
fn codex_sessions(base: &Path, skipped: &dyn Fn(&Path) -> bool, pids: &[i32]) -> (Sessions, Option<TranscriptBytes>) {
    let rollouts_dir = base.join("sessions");
    let rollouts = match codex::rollouts(&rollouts_dir, skipped) {
        Ok(rollouts) => Some(rollouts),
        Err(codex::Absent::Unreadable) => Some(HashMap::new()),
        Err(codex::Absent::Skipped) => None,
    };
    let locks = base.join("thread-writer-locks");
    let (state, reason, list) = if skipped(&locks) {
        (RecordsState::Unknown, Some(UnknownReason::Skipped), Vec::new())
    } else {
        codex::threads(&locks, pids, procs::open_vnodes, rollouts.as_ref().unwrap_or(&HashMap::new()))
    };
    let names = rollouts.map(|r| r.into_keys().collect());
    finish(state, reason, list, names, rollouts_dir, Shape::Codex)
}

/// Sorts the sessions, live first, and counts the transcripts of `names`
/// that no live session writes; `None` names are a skipped folder.
fn finish(
    records: RecordsState,
    reason: Option<UnknownReason>,
    mut list: Vec<Session>,
    names: Option<HashSet<String>>,
    dir: PathBuf,
    shape: Shape,
) -> (Sessions, Option<TranscriptBytes>) {
    list.sort_by(|a, b| {
        let key =
            |s: &Session| (s.state != SessionState::Live, s.started_at.clone(), s.rollout_time.clone(), s.id.clone());
        key(a).cmp(&key(b))
    });
    let live: HashSet<String> =
        list.iter().filter(|s| s.state == SessionState::Live).filter_map(|s| s.id.clone()).collect();
    let skipped = names.is_none();
    let count = names.map(|ids| ids.difference(&live).count() as u64);
    let sessions = Sessions {
        records,
        reason,
        list,
        transcripts: Transcripts { count, bytes: None, skipped },
        footprints: None,
        temp_root: None,
    };
    (sessions, (!skipped).then(|| TranscriptBytes::new(dir, shape, live)))
}

/// The session ids of the transcripts under `projects`: `<p>/<id>.jsonl`
/// and `<p>/<id>/`. Names only. `None` when a skip covers `projects` or a
/// folder inside it: that folder is never opened, and a count without it
/// would be wrong.
fn claude_transcripts(projects: &Path, skipped: &dyn Fn(&Path) -> bool) -> Option<HashSet<String>> {
    if skipped(projects) {
        return None;
    }
    let mut ids = HashSet::new();
    for project in crate::stats::read_dir(projects).into_iter().flatten().flatten() {
        if !project.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let dir = project.path();
        if skipped(&dir) {
            return None;
        }
        for entry in crate::stats::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if entry.file_type().is_ok_and(|t| t.is_dir()) && skipped(&entry.path()) {
                return None;
            }
            let id = name.strip_suffix(".jsonl").unwrap_or(name);
            if is_uuid(id) {
                // Per project: one session's id never names two projects.
                ids.insert(id.to_owned());
            }
        }
    }
    Some(ids)
}

/// Claude Code's temporary folder at `base`, canonical, when it is a folder
/// that `me` owns; why not otherwise, from `base`'s own metadata, never
/// followed; `None` when nothing is there. In a shared `/tmp` anyone may
/// make `claude-<uid>` first: a link or another user's folder is not opened.
fn temp_base(base: &Path, me: u32) -> Option<Result<PathBuf, TempRefusal>> {
    let md = std::fs::symlink_metadata(base).ok()?;
    let why = if md.file_type().is_symlink() {
        TempRefusal::Symlink
    } else if !md.is_dir() {
        TempRefusal::NotAFolder
    } else if crate::platform::meta::uid(&md) != Some(me) {
        TempRefusal::NotOwned
    } else {
        return base.canonical().ok().map(Ok);
    };
    Some(Err(why))
}

/// `8-4-4-4-12` hexadecimal digits.
fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes()
            .enumerate()
            .all(|(i, b)| if matches!(i, 8 | 13 | 18 | 23) { b == b'-' } else { b.is_ascii_hexdigit() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{skipped_without, testing};

    /// A link to a folder elsewhere, a file and another user's folder are
    /// refused from their own metadata; only the user's own folder opens.
    #[test]
    fn the_temporary_base_opens_only_as_the_users_own_folder() {
        let d = tempfile::tempdir().unwrap();
        // A folder's owner, which Windows does not report yet (`meta::uid`).
        let owner = crate::platform::meta::uid(&std::fs::metadata(d.path()).unwrap())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Unsupported, "no file owner on this platform"));
        if skipped_without("the_temporary_base_opens_only_as_the_users_own_folder", owner).is_none() {
            return;
        }
        let elsewhere = d.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let link = d.path().join("claude-501");
        let made = testing::symlink(&elsewhere, &link);
        if skipped_without("the_temporary_base_opens_only_as_the_users_own_folder", made).is_none() {
            return;
        }
        let me = procs::uid();
        assert_eq!(temp_base(&link, me), Some(Err(TempRefusal::Symlink)));
        let file = d.path().join("claude-file");
        std::fs::write(&file, "").unwrap();
        assert_eq!(temp_base(&file, me), Some(Err(TempRefusal::NotAFolder)));
        assert_eq!(temp_base(&elsewhere, me.wrapping_add(1)), Some(Err(TempRefusal::NotOwned)));
        assert_eq!(temp_base(&elsewhere, me), Some(Ok(elsewhere.canonical().unwrap())));
        assert_eq!(temp_base(&d.path().join("none"), me), None);
    }
}
