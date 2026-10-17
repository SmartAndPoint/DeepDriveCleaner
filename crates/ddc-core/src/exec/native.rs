//! Tools' own cleanup commands, run only where the tool reports the finding
//! as its own location (ADR §6).

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use super::{CommandRunner, NATIVE_TIMEOUT};
use crate::class::{LocationCompare, NativeCommand};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandResult {
    pub program: String,
    pub args: Vec<String>,
    pub success: bool,
    pub code: Option<i32>,
    /// Last part of the command's output, for the journal.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub output: String,
}

fn tail(stdout: &str, stderr: &str) -> String {
    let all = format!("{stdout}{stderr}");
    let start = all.char_indices().rev().nth(1999).map_or(0, |(i, _)| i);
    all[start..].trim().to_owned()
}

pub(super) enum Native {
    Ran(CommandResult),
    Unavailable,
    LocationMismatch,
}

pub(super) fn run(cmd: &NativeCommand, target: &Path, commands: &dyn CommandRunner) -> Native {
    let Some(exe) = commands.find(cmd.program) else { return Native::Unavailable };
    if let Some(probe) = cmd.location {
        let Some(probe_exe) = commands.find(probe.program) else { return Native::Unavailable };
        let args: Vec<String> = probe.args.iter().map(|a| (*a).to_owned()).collect();
        let Ok(out) = commands.run(&probe_exe, &args, Duration::from_secs(30)) else { return Native::Unavailable };
        let reported = Path::new(out.stdout.trim()).canonicalize();
        let matches = out.success
            && reported.is_ok_and(|r| match probe.compare {
                LocationCompare::Equals => r == target,
                LocationCompare::Contains => target.starts_with(&r) && target != r,
            });
        if !matches {
            return Native::LocationMismatch;
        }
    }
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let args: Vec<String> = cmd.args.iter().map(|a| a.replace("{name}", &name)).collect();
    match commands.run(&exe, &args, NATIVE_TIMEOUT) {
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
