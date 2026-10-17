//! Background notifications: when to notify (pure) and how (osascript).

use std::io;
use std::path::PathBuf;
use std::process::Command;

use ddc_core::env::Env;
use ddc_core::store;
use serde::{Deserialize, Serialize};

use crate::size::human;

const STATE: &str = "notify-state.json";
const WEEK_SECS: u64 = 7 * 86_400;

/// Last notification, kept in `<data-dir>/notify-state.json`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyState {
    /// Unix seconds.
    pub last_at: Option<u64>,
    pub last_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Facts {
    /// What the default selection frees (estimate).
    pub reclaimable: u64,
    pub free: Option<u64>,
    pub total: Option<u64>,
    /// Unix seconds.
    pub now: u64,
}

/// Notify when the reclaimable size reaches `threshold` or less than 10 % of
/// the volume is free; repeat only after ≥ 20 % growth or after a week.
pub fn decide(f: &Facts, threshold: u64, state: &NotifyState) -> bool {
    let low_free = matches!((f.free, f.total), (Some(free), Some(total)) if total > 0 && free * 10 < total);
    // Nothing to clean: `ddc clean` cannot help, even when the disk is full.
    if f.reclaimable == 0 || (f.reclaimable < threshold && !low_free) {
        return false;
    }
    match state.last_at {
        None => true,
        Some(at) => {
            f.now.saturating_sub(at) >= WEEK_SECS
                || (state.last_bytes > 0 && f.reclaimable.saturating_mul(5) >= state.last_bytes.saturating_mul(6))
        }
    }
}

pub fn message(f: &Facts) -> String {
    let free = f.free.map(|b| format!(" ({} free now)", human(b))).unwrap_or_default();
    format!("About {} of developer junk can be freed{free}. Run `ddc clean` to review.", human(f.reclaimable))
}

/// Shows a notification. Texts are passed as script arguments, never spliced into AppleScript.
pub fn send(title: &str, body: &str) -> io::Result<()> {
    let status = Command::new("/usr/bin/osascript")
        .args(["-e", "on run argv", "-e", "display notification (item 1 of argv) with title (item 2 of argv)"])
        .args(["-e", "end run", body, title])
        .status()?;
    if status.success() { Ok(()) } else { Err(io::Error::other(format!("osascript exited with {status}"))) }
}

fn state_path(env: &Env) -> PathBuf {
    env.data_dir().join(STATE)
}

pub fn load(env: &Env) -> NotifyState {
    std::fs::read(state_path(env)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save(env: &Env, state: &NotifyState) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
    store::write_atomic(&state_path(env), &json).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1 << 30;
    const NOW: u64 = 1_800_000_000;

    fn facts(reclaimable: u64, free: u64, total: u64) -> Facts {
        Facts { reclaimable, free: Some(free), total: Some(total), now: NOW }
    }

    #[test]
    fn threshold_or_low_free_space_triggers() {
        let none = NotifyState::default();
        assert!(decide(&facts(12 * G, 500 * G, 1000 * G), 10 * G, &none));
        assert!(!decide(&facts(5 * G, 500 * G, 1000 * G), 10 * G, &none));
        assert!(decide(&facts(G, 50 * G, 1000 * G), 10 * G, &none), "under 10 % free");
    }

    #[test]
    fn repeats_only_after_growth_or_a_week() {
        let last = NotifyState { last_at: Some(NOW - 86_400), last_bytes: 20 * G };
        assert!(!decide(&facts(21 * G, 500 * G, 1000 * G), 10 * G, &last), "5 % growth, 1 day");
        assert!(decide(&facts(24 * G, 500 * G, 1000 * G), 10 * G, &last), "20 % growth");
        let old = NotifyState { last_at: Some(NOW - 8 * 86_400), last_bytes: 20 * G };
        assert!(decide(&facts(20 * G, 500 * G, 1000 * G), 10 * G, &old), "a week later");
        let empty = NotifyState { last_at: Some(NOW - 86_400), last_bytes: 0 };
        assert!(!decide(&facts(G, 50 * G, 1000 * G), 10 * G, &empty), "low on space, 1 day after a zero baseline");
        assert!(!decide(&facts(0, 50 * G, 1000 * G), 10 * G, &NotifyState::default()), "nothing to clean");
    }

    #[test]
    fn message_names_the_size_and_the_command() {
        let m = message(&facts(37 * G, 74 * G, 1000 * G));
        assert!(m.contains("37.0 GiB") && m.contains("ddc clean"), "{m}");
    }
}
