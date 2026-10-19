//! `ddc notify ack|snooze`: the app records what it did with a notification.
//! The CLI stays the one writer of `notify-state.json`. No lock: the write is
//! atomic, and a running clean must not block it.

use std::process::ExitCode;
use std::time::UNIX_EPOCH;

use ddc_core::env::Env;

use crate::cli::NotifyAction;
use crate::notify;

pub fn run(action: NotifyAction) -> anyhow::Result<ExitCode> {
    match action {
        NotifyAction::Ack { bytes, data_dir } => {
            let env = Env::from_process(data_dir)?;
            let now = env.now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
            notify::ack(&env, bytes, now)?;
        }
        NotifyAction::Snooze { until, data_dir } => notify::snooze(&Env::from_process(data_dir)?, until)?,
    }
    Ok(ExitCode::SUCCESS)
}
