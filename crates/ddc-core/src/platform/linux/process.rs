//! How ddc starts a native command, on Linux.

use std::os::unix::process::CommandExt as _;
use std::process::Command;

/// Starts `cmd` in a process group of its own (`setpgid(0, 0)`), whose id is
/// the child's pid: `signals::kill_group` then reaches everything it started.
pub fn own_process_group(cmd: &mut Command) -> &mut Command {
    cmd.process_group(0)
}
