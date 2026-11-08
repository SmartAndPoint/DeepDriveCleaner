//! `ddc lock status`: who holds the data-dir lock now (story "Busy: name the
//! scan or clean that holds the lock, and offer to stop it"; ADR "Menu bar
//! app: Swift shell over the ddc CLI", §6 item 16). It reads the holders'
//! records only: it never probes the lock, takes none and removes nothing.

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::SystemTime;

use ddc_core::env::Env;
use ddc_core::holders::{self, LOCK_SCHEMA_VERSION, LockStatus};

use crate::cli::LockStatusArgs;
use crate::render;

pub fn status(args: &LockStatusArgs) -> anyhow::Result<ExitCode> {
    let env = Env::from_process(args.data_dir.clone())?;
    let report = LockStatus { schema_version: LOCK_SCHEMA_VERSION, holders: holders::holders(&env) };
    let mut out = io::stdout().lock();
    let written = if args.json {
        serde_json::to_writer_pretty(&mut out, &report).map_err(io::Error::from).and_then(|()| writeln!(out))
    } else if report.holders.is_empty() {
        writeln!(out, "No scan or clean holds the lock.")
    } else {
        let now = humantime::format_rfc3339_seconds(SystemTime::now()).to_string();
        report.holders.iter().try_for_each(|h| writeln!(out, "{}", render::holder(h, &now)))
    };
    match written {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        other => other.map(|()| ExitCode::SUCCESS).map_err(Into::into),
    }
}
