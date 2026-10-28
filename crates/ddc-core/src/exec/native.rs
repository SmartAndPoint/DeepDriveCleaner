//! Tools' own cleanup commands, run only where the tool reports the finding
//! as its own location (ADR §6).

use std::path::Path;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{CommandRunner, ExecOptions, NATIVE_TIMEOUT, READY_TIMEOUT};
use crate::class::{LocationCompare, NativeCommand, ReadyProbe};

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

pub(super) fn run(cmd: &NativeCommand, target: &Path, commands: &dyn CommandRunner, opts: &ExecOptions<'_>) -> Native {
    let Some(exe) = commands.find(cmd.program) else { return Native::Unavailable };
    if let Some(probe) = cmd.location {
        let Some(probe_exe) = commands.find(probe.program) else { return Native::Unavailable };
        let args: Vec<String> = probe.args.iter().map(|a| (*a).to_owned()).collect();
        let Ok(out) = commands.run(&probe_exe, &args, &[], Duration::from_secs(30)) else { return Native::Unavailable };
        let reported = Path::new(out.stdout.trim()).canonicalize();
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
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let args: Vec<String> = cmd.args.iter().map(|a| a.replace("{name}", &name)).collect();
    match commands.run(&exe, &args, cmd.env, NATIVE_TIMEOUT) {
        Ok(out) => Native::Ran(CommandResult {
            program: cmd.program.to_owned(),
            args,
            success: out.success,
            code: out.code,
            output: tail(&out.stdout, &out.stderr),
        }),
        Err(e) => Native::Ran(CommandResult {
            program: cmd.program.to_owned(),
            args,
            success: false,
            code: None,
            output: e.to_string(),
        }),
    }
}
