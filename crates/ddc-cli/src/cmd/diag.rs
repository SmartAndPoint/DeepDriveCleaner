//! `ddc diag`: process state after startup, for tests and bug reports.

use std::process::ExitCode;

use ddc_core::sys::{iopolicy, rlimit, statfs};

pub fn run() -> anyhow::Result<ExitCode> {
    let dataless = match iopolicy::dataless_materialization_disabled() {
        Ok(true) => "off".to_owned(),
        Ok(false) => "on".to_owned(),
        Err(err) => format!("unknown ({err})"),
    };
    println!("dataless_materialization: {dataless}");
    println!("open_files_limit: {}", rlimit::nofile_limit()?);
    let vol = statfs::statfs(&std::env::current_dir()?)?;
    println!(
        "volume: {} on {} ({} of {} bytes available)",
        vol.fs_type,
        vol.mount_point.display(),
        vol.available_bytes,
        vol.total_bytes
    );
    Ok(ExitCode::SUCCESS)
}
