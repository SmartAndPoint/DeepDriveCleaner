//! Background notifications: when to notify (pure), what the app is told
//! (`ddc_core::notify`), and how the CLI shows one itself (the platform's
//! `SystemNotifier`, reached only through a [`Notifier`]).

use std::io;
use std::path::{Path, PathBuf};

use ddc_core::env::Env;
use ddc_core::model::ScanReport;
use ddc_core::notify::{Decision, NotifyReason};
use ddc_core::store;
use serde::{Deserialize, Serialize};

use crate::size::human;

const STATE: &str = "notify-state.json";
const WEEK_SECS: u64 = 7 * 86_400;

/// Last notification and the snooze, kept in `<data-dir>/notify-state.json`.
/// The CLI is its one writer; the app calls `notify ack` / `notify snooze`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyState {
    /// Unix seconds.
    pub last_at: Option<u64>,
    pub last_bytes: u64,
    /// Unix seconds: a scan that starts before this does not notify.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snooze_until: Option<u64>,
}

/// A scanned volume as the decision sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeFacts {
    pub mount_point: PathBuf,
    pub free: u64,
    pub total: u64,
    /// What the default selection frees on this volume.
    pub reclaimable: u64,
}

#[derive(Debug, Clone)]
pub struct Facts {
    /// What the default selection frees on all volumes (estimate).
    pub reclaimable: u64,
    pub volumes: Vec<VolumeFacts>,
    /// Index in `volumes` of the first scan root's volume, when it could be read.
    pub first_root: Option<usize>,
    /// Unix seconds: when the scan started.
    pub now: u64,
}

impl Facts {
    pub fn from_report(report: &ScanReport, now: u64) -> Self {
        let volumes = report
            .volumes
            .iter()
            .map(|v| VolumeFacts {
                mount_point: v.mount_point.clone(),
                free: v.free_bytes,
                total: v.total_bytes,
                // The walk never leaves a root's volume, so a finding lies on its root's.
                reclaimable: report
                    .findings
                    .iter()
                    .filter(|f| f.selected && v.roots.iter().any(|r| f.path.starts_with(r)))
                    .map(|f| f.reclaimable_bytes())
                    .sum(),
            })
            .collect();
        let first_root =
            report.scan_roots.first().and_then(|root| report.volumes.iter().position(|v| v.roots.contains(root)));
        Self { reclaimable: report.totals.selected_reclaimable_bytes, volumes, first_root, now }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub threshold: u64,
    /// Also notify when a volume is under 10 % free (`notify_low_disk`).
    pub low_disk: bool,
}

/// Notify when a volume is under 10 % free and something on it can be freed,
/// or when the reclaimable size reaches the threshold. Not while snoozed. The
/// threshold repeats only after ≥ 20 % growth or after a week; a volume low on
/// space notifies at every scan until it is cleaned or snoozed.
pub fn decide(f: &Facts, cfg: &Config, state: &NotifyState) -> Decision {
    // Low on space where `ddc clean` can help; the fullest such volume.
    let low = f
        .volumes
        .iter()
        .filter(|v| cfg.low_disk && v.total > 0 && v.free.saturating_mul(10) < v.total && v.reclaimable > 0)
        .min_by(|a, b| (u128::from(a.free) * u128::from(b.total)).cmp(&(u128::from(b.free) * u128::from(a.total))));
    let reason = match low {
        Some(_) => Some(NotifyReason::LowDisk),
        None if f.reclaimable > 0 && f.reclaimable >= cfg.threshold => Some(NotifyReason::Threshold),
        None => None,
    };
    let snoozed = state.snooze_until.is_some_and(|until| f.now < until);
    let repeat = match state.last_at {
        None => true,
        Some(at) => {
            f.now.saturating_sub(at) >= WEEK_SECS
                || (state.last_bytes > 0 && f.reclaimable.saturating_mul(5) >= state.last_bytes.saturating_mul(6))
        }
    };
    let notify = reason.is_some() && !snoozed && (low.is_some() || repeat);
    let about = low.or_else(|| f.first_root.and_then(|i| f.volumes.get(i)));
    Decision {
        notify,
        reason: reason.filter(|_| notify),
        volume: about.map(|v| v.mount_point.clone()),
        reclaimable: f.reclaimable,
        free: about.map(|v| v.free),
        total: about.map(|v| v.total),
    }
}

pub fn message(d: &Decision) -> String {
    let junk = format!("About {} of developer junk can be freed", human(d.reclaimable));
    match (d.reason, &d.volume, d.free) {
        (Some(NotifyReason::LowDisk), Some(volume), Some(free)) => {
            format!("{} is almost full ({} free). {junk}. Run `ddc clean` to review.", volume_name(volume), human(free))
        }
        (_, _, Some(free)) => format!("{junk} ({} free now). Run `ddc clean` to review.", human(free)),
        _ => format!("{junk}. Run `ddc clean` to review."),
    }
}

fn volume_name(mount_point: &Path) -> String {
    match mount_point.to_str() {
        Some("/" | "/System/Volumes/Data") => "The startup disk".to_owned(),
        _ => mount_point.display().to_string(),
    }
}

/// Shows a notification.
pub trait Notifier {
    fn notify(&self, title: &str, body: &str) -> io::Result<()>;
}

fn state_path(env: &Env) -> PathBuf {
    env.data_dir().join(STATE)
}

pub fn load(env: &Env) -> NotifyState {
    std::fs::read(state_path(env)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(env: &Env, state: &NotifyState) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
    store::write_atomic(&state_path(env), &json).map(|_| ())
}

/// A notification of `bytes` was shown at `now`: threshold repeats wait for
/// growth or a week, and any snooze is over.
pub fn ack(env: &Env, bytes: u64, now: u64) -> io::Result<()> {
    save(env, &NotifyState { last_at: Some(now), last_bytes: bytes, snooze_until: None })
}

/// "Remind me tomorrow": no notification from scans starting before `until`;
/// the first one after it notifies whenever it is over the threshold or low on space.
pub fn snooze(env: &Env, until: u64) -> io::Result<()> {
    save(env, &NotifyState { last_at: None, last_bytes: 0, snooze_until: Some(until) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1 << 30;
    const NOW: u64 = 1_800_000_000;
    const ON: Config = Config { threshold: 10 * G, low_disk: true };

    fn volume(mount: &str, free: u64, total: u64, reclaimable: u64) -> VolumeFacts {
        VolumeFacts { mount_point: mount.into(), free, total, reclaimable }
    }

    /// One volume holding everything.
    fn facts(reclaimable: u64, free: u64, total: u64) -> Facts {
        Facts {
            reclaimable,
            volumes: vec![volume("/System/Volumes/Data", free, total, reclaimable)],
            first_root: Some(0),
            now: NOW,
        }
    }

    fn notifies(f: &Facts, state: &NotifyState) -> bool {
        decide(f, &ON, state).notify
    }

    #[test]
    fn threshold_or_low_free_space_triggers() {
        let none = NotifyState::default();
        let d = decide(&facts(12 * G, 500 * G, 1000 * G), &ON, &none);
        assert_eq!((d.notify, d.reason), (true, Some(NotifyReason::Threshold)));
        assert_eq!((d.volume.as_deref(), d.free), (Some(Path::new("/System/Volumes/Data")), Some(500 * G)));
        let quiet = decide(&facts(5 * G, 500 * G, 1000 * G), &ON, &none);
        assert_eq!((quiet.notify, quiet.reason), (false, None), "reason is null exactly when not notifying");
        let low = decide(&facts(G, 50 * G, 1000 * G), &ON, &none);
        assert_eq!((low.notify, low.reason), (true, Some(NotifyReason::LowDisk)), "under 10 % free");
        assert!(!decide(&facts(G, 50 * G, 1000 * G), &Config { low_disk: false, ..ON }, &none).notify, "switched off");
        assert_eq!(decide(&facts(12 * G, 50 * G, 1000 * G), &ON, &none).reason, Some(NotifyReason::LowDisk), "wins");
    }

    #[test]
    fn low_disk_names_the_volume_where_something_can_be_freed() {
        let f = Facts {
            reclaimable: G,
            volumes: vec![volume("/System/Volumes/Data", 30 * G, 1000 * G, 0), volume("/Volumes/Work", 8, 100, G)],
            first_root: Some(0),
            now: NOW,
        };
        let d = decide(&f, &ON, &NotifyState::default());
        assert_eq!((d.reason, d.volume.as_deref()), (Some(NotifyReason::LowDisk), Some(Path::new("/Volumes/Work"))));
        assert_eq!((d.free, d.total), (Some(8), Some(100)), "the triggering volume, not the fullest one");

        let nothing_there = Facts { volumes: vec![volume("/Volumes/Work", 8, 100, 0)], ..f.clone() };
        assert!(!decide(&nothing_there, &ON, &NotifyState::default()).notify, "ddc clean cannot help there");

        let unread =
            Facts { reclaimable: 20 * G, volumes: vec![volume("/Volumes/Work", 80, 100, 0)], first_root: None, ..f };
        let d = decide(&unread, &ON, &NotifyState::default());
        assert_eq!(
            (d.notify, d.volume, d.free),
            (true, None, None),
            "the first root's volume is unknown: no other stands in"
        );
    }

    #[test]
    fn the_threshold_repeats_only_after_growth_or_a_week() {
        let last = NotifyState { last_at: Some(NOW - 86_400), last_bytes: 20 * G, snooze_until: None };
        assert!(!notifies(&facts(21 * G, 500 * G, 1000 * G), &last), "5 % growth, 1 day");
        assert!(notifies(&facts(24 * G, 500 * G, 1000 * G), &last), "20 % growth");
        let old = NotifyState { last_at: Some(NOW - 8 * 86_400), ..last };
        assert!(notifies(&facts(20 * G, 500 * G, 1000 * G), &old), "a week later");
        let empty = NotifyState { last_at: Some(NOW - 86_400), last_bytes: 0, snooze_until: None };
        assert!(!notifies(&facts(20 * G, 500 * G, 1000 * G), &empty), "1 day after a zero baseline");
        assert!(!notifies(&facts(0, 50 * G, 1000 * G), &NotifyState::default()), "nothing to clean");
    }

    #[test]
    fn low_disk_notifies_at_every_scan_until_snoozed() {
        let last = NotifyState { last_at: Some(NOW - 86_400), last_bytes: 20 * G, snooze_until: None };
        assert!(notifies(&facts(20 * G, 50 * G, 1000 * G), &last), "a day later, nothing grew, still almost full");
        assert!(notifies(&facts(G, 50 * G, 1000 * G), &last), "less to free than last time");
        let snoozed = NotifyState { snooze_until: Some(NOW + 60), ..last };
        assert!(!notifies(&facts(20 * G, 50 * G, 1000 * G), &snoozed), "Remind me tomorrow holds it back");

        let off = Config { low_disk: false, ..ON };
        assert!(!decide(&facts(20 * G, 50 * G, 1000 * G), &off, &last).notify, "switched off: the threshold's rule");
        let nothing_on_the_low_one = Facts {
            reclaimable: 20 * G,
            volumes: vec![
                volume("/System/Volumes/Data", 500 * G, 1000 * G, 20 * G),
                volume("/Volumes/Work", 8, 100, 0),
            ],
            first_root: Some(0),
            now: NOW,
        };
        assert!(!notifies(&nothing_on_the_low_one, &last), "a low volume with nothing to clean lifts no rule");
    }

    #[test]
    fn ack_then_snooze_then_after_the_snooze() {
        let data = tempfile::tempdir().unwrap();
        let env =
            Env::with_paths(data.path(), data.path(), &data.path().join("logs"), std::time::SystemTime::now()).unwrap();
        ack(&env, 20 * G, NOW).unwrap();
        let day_later = Facts { now: NOW + 86_400, ..facts(20 * G, 500 * G, 1000 * G) };
        assert!(!notifies(&day_later, &load(&env)), "acknowledged: the same amount a day later");

        snooze(&env, NOW + 2 * 86_400).unwrap();
        assert_eq!(load(&env).last_at, None);
        assert!(!notifies(&day_later, &load(&env)), "starts before the snooze ends");
        let after = Facts { now: NOW + 2 * 86_400 + 60, ..facts(21 * G, 500 * G, 1000 * G) };
        assert!(notifies(&after, &load(&env)), "after it: 5 % growth and a day are enough");
    }

    #[test]
    fn a_0_1_0_state_file_still_loads() {
        let state: NotifyState = serde_json::from_str(r#"{"last_at": 5, "last_bytes": 7}"#).unwrap();
        assert_eq!(state, NotifyState { last_at: Some(5), last_bytes: 7, snooze_until: None });
    }

    #[test]
    fn message_names_the_size_the_command_and_a_full_volume() {
        let m = message(&decide(&facts(37 * G, 74 * G, 1000 * G), &ON, &NotifyState::default()));
        assert!(m.contains("37.0 GiB") && m.contains("ddc clean") && m.contains("startup disk"), "{m}");
        let threshold = message(&decide(&facts(37 * G, 500 * G, 1000 * G), &ON, &NotifyState::default()));
        assert!(threshold.contains("500.0 GiB free now"), "{threshold}");
    }
}
