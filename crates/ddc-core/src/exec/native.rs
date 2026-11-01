//! Tools' own cleanup commands, run only where the tool reports the finding
//! as its own location (ADR §6).

use crate::platform::Canonical as _;
use std::path::Path;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{CommandRunner, ExecOptions, NATIVE_TIMEOUT, READY_TIMEOUT, SkipReason};
use crate::class::{CondaFolder, Location, LocationCompare, NativeCommand, ReadyProbe};
use crate::conda::{self, EnvironmentClaim, Installs};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommandResult {
    pub program: String,
    pub args: Vec<String>,
    pub success: bool,
    pub code: Option<i32>,
    /// Last part of the command's output, for the journal.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
}

/// The last 2000 characters of a command's output, for the journal.
pub(crate) fn tail(stdout: &str, stderr: &str) -> String {
    let all = format!("{stdout}{stderr}");
    let start = all.char_indices().rev().nth(1999).map_or(0, |(i, _)| i);
    all[start..].trim().to_owned()
}

/// The last non-empty line of a command's output.
fn last_line(output: &str) -> Option<&str> {
    output.lines().map(str::trim).rfind(|l| !l.is_empty())
}

pub(super) enum Native {
    Ran(CommandResult),
    Unavailable,
    LocationMismatch,
    /// The runtime did not answer its readiness probe; why, in words.
    NotReady(String),
    /// The tool claims the finding, but nothing may run and the fallback
    /// may not either: a conda package cache conda cannot clean alone, a
    /// base environment.
    Skip(SkipReason),
    /// A cancel arrived after a probe: the command was not started.
    Cancelled,
}

/// Runs the readiness probe; `None` when the runtime answers. A timeout or
/// no output is `down`, a known needle in the last line its own detail,
/// any other last line that line itself.
pub(super) fn ready(probe: &ReadyProbe, commands: &dyn CommandRunner, opts: &ExecOptions<'_>) -> Option<Native> {
    let Some(exe) = commands.find(probe.program) else { return Some(Native::Unavailable) };
    let args: Vec<String> = probe.args.iter().map(|a| (*a).to_owned()).collect();
    let ran = commands.run(&exe, &args, &[], READY_TIMEOUT);
    // A probe killed by a forwarded signal is a cancel, not a runtime that is down.
    if opts.cancelled() {
        return Some(Native::Cancelled);
    }
    let out = match ran {
        Ok(out) if out.success && !out.stdout.trim().is_empty() => return None,
        Ok(out) => tail(&out.stdout, &out.stderr),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => String::new(),
        Err(e) => e.to_string(),
    };
    let detail = match last_line(&out) {
        None => probe.down.to_owned(),
        Some(line) => probe
            .known
            .iter()
            .find(|(needle, _)| line.contains(needle))
            .map_or_else(|| line.to_owned(), |(_, detail)| (*detail).to_owned()),
    };
    Some(Native::NotReady(detail))
}

pub(super) fn run(
    cmd: &NativeCommand,
    target: &Path,
    commands: &dyn CommandRunner,
    installs: &Installs<'_>,
    opts: &ExecOptions<'_>,
) -> Native {
    let probe = match cmd.location {
        Some(Location::Conda(folder)) => return run_conda(cmd, folder, target, commands, installs, opts),
        Some(Location::Probe(probe)) => Some(probe),
        None => None,
    };
    let Some(exe) = commands.find(cmd.program) else { return Native::Unavailable };
    if let Some(probe) = probe {
        let Some(probe_exe) = commands.find(probe.program) else { return Native::Unavailable };
        let args: Vec<String> = probe.args.iter().map(|a| (*a).to_owned()).collect();
        let Ok(out) = commands.run(&probe_exe, &args, &[], Duration::from_secs(30)) else { return Native::Unavailable };
        let reported = Path::new(out.stdout.trim()).canonical();
        let matches = out.success
            && reported.is_ok_and(|r| match probe.compare {
                LocationCompare::Equals => r == target,
                LocationCompare::Contains => target.starts_with(&r) && target != r,
            });
        if opts.cancelled() {
            return Native::Cancelled;
        }
        if !matches {
            return Native::LocationMismatch;
        }
    }
    command(cmd.program.to_owned(), &exe, cmd, target, commands)
}

/// The conda that owns `target` runs the command (safety ADR, amendment of
/// 2026-10-29): every known install is asked once per clean, the first that
/// claims the finding runs it by its own `bin/conda`. A cache is cleaned
/// only when no install that lists it links packages by symlink, and only
/// when its claiming install, asked with `CONDA_PKGS_DIRS` set, would clean
/// that one folder and no other.
fn run_conda(
    cmd: &NativeCommand,
    folder: CondaFolder,
    target: &Path,
    commands: &dyn CommandRunner,
    installs: &Installs<'_>,
    opts: &ExecOptions<'_>,
) -> Native {
    let Some(spelled) = conda::spelled(target) else { return Native::LocationMismatch };
    let answered = installs.get();
    if opts.cancelled() {
        return Native::Cancelled;
    }
    let install = match folder {
        CondaFolder::Environment => match conda::environment(answered, target) {
            EnvironmentClaim::Install(install) => install,
            EnvironmentClaim::Base => return Native::Skip(SkipReason::Protected),
            EnvironmentClaim::Unclaimed => return Native::LocationMismatch,
        },
        CondaFolder::PackageCache => match conda::package_folder(answered, target) {
            Ok(install) => install,
            Err(conda::NOT_LISTED) => return Native::LocationMismatch,
            Err(detail) => return Native::Skip(not_ready(detail)),
        },
    };
    if folder == CondaFolder::PackageCache {
        let scoped = install.scoped_to(commands, target, spelled);
        if opts.cancelled() {
            return Native::Cancelled;
        }
        if !scoped {
            return Native::Skip(not_ready(conda::NOT_SCOPED));
        }
    }
    command(install.exe.to_string_lossy().into_owned(), &install.exe, cmd, target, commands)
}

fn not_ready(detail: &str) -> SkipReason {
    SkipReason::ToolNotReady { program: "conda".to_owned(), detail: detail.to_owned() }
}

/// Runs `cmd` for `target` as `exe`; the journal names it `program`.
fn command(program: String, exe: &Path, cmd: &NativeCommand, target: &Path, commands: &dyn CommandRunner) -> Native {
    let args = cmd.args_for(target);
    let env = cmd.env_for(target);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    match commands.run(exe, &args, &env, NATIVE_TIMEOUT) {
        Ok(out) => Native::Ran(CommandResult {
            program,
            args,
            success: out.success,
            code: out.code,
            output: tail(&out.stdout, &out.stderr),
        }),
        Err(e) => Native::Ran(CommandResult { program, args, success: false, code: None, output: e.to_string() }),
    }
}
