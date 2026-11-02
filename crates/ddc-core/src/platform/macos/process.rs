//! How ddc starts a native command, on macOS.

use std::io;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command};

/// Starts `cmd` in a process group of its own (`setpgid(0, 0)`), whose id is
/// the child's pid: `signals::kill_group` then reaches everything it started.
pub fn spawn_grouped(cmd: &mut Command) -> io::Result<Child> {
    cmd.process_group(0).spawn()
}

/// The file names `program` is looked for by, in each folder of the search
/// path: the name itself.
pub fn program_names(program: &str) -> Vec<String> {
    vec![program.to_owned()]
}
