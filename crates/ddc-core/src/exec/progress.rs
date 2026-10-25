//! Live counters of a clean, shared with the caller as `scan::Progress` is
//! with the scanner (story "Progress output while ddc clean runs"). The
//! terminal's progress line and the app's progress events read one snapshot.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use crate::class::Category;
use crate::plan::{Action, ActionKind, PlanItem};
use crate::scan::{Progress, ProgressSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecPhase {
    /// The local-snapshot count, the keep-marker walk and the process snapshot.
    Preparing,
    /// The item loop.
    Cleaning,
}

/// The item being cleaned now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentItem {
    /// 1-based, of `items_total`.
    pub index: u64,
    pub path: PathBuf,
    pub category: Category,
    pub action: ActionKind,
    pub bytes: u64,
    /// The tool whose command (or location probe) runs now.
    pub tool: Option<&'static str>,
}

/// One per run: `execute` sets the totals, never resets the counts.
#[derive(Debug, Default)]
pub struct ExecProgress {
    cleaning: AtomicBool,
    walk: Progress,
    walk_expected: AtomicU64,
    items_total: AtomicU64,
    items_done: AtomicU64,
    bytes_total: AtomicU64,
    bytes_done: AtomicU64,
    current: Mutex<Option<CurrentItem>>,
}

/// Point-in-time copy of [`ExecProgress`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecSnapshot {
    pub phase: ExecPhase,
    /// The keep-marker walk of the chosen items.
    pub walked: ProgressSnapshot,
    /// The chosen items' files as the scan counted them: an estimate.
    pub walk_expected: u64,
    pub items_total: u64,
    /// Items with an outcome; the `not-run` items of a cancel are not counted.
    pub items_done: u64,
    /// The chosen items' bytes, `native-only` items left out: their space is
    /// freed inside a VM image and is not in any total (safety ADR §5).
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub current: Option<CurrentItem>,
}

impl ExecProgress {
    pub fn snapshot(&self) -> ExecSnapshot {
        ExecSnapshot {
            phase: if self.cleaning.load(Relaxed) { ExecPhase::Cleaning } else { ExecPhase::Preparing },
            walked: self.walk.snapshot(),
            walk_expected: self.walk_expected.load(Relaxed),
            items_total: self.items_total.load(Relaxed),
            items_done: self.items_done.load(Relaxed),
            bytes_total: self.bytes_total.load(Relaxed),
            bytes_done: self.bytes_done.load(Relaxed),
            current: self.current().clone(),
        }
    }

    pub(crate) fn walk(&self) -> &Progress {
        &self.walk
    }

    pub(crate) fn start(&self, chosen: &[&PlanItem]) {
        self.items_total.store(chosen.len() as u64, Relaxed);
        self.bytes_total.store(chosen.iter().map(|i| counted_bytes(i)).sum(), Relaxed);
        self.walk_expected.store(chosen.iter().map(|i| i.finding.files).sum(), Relaxed);
    }

    pub(crate) fn begin_cleaning(&self) {
        self.cleaning.store(true, Relaxed);
    }

    pub(crate) fn item_started(&self, index: u64, item: &PlanItem) {
        *self.current() = Some(CurrentItem {
            index,
            path: item.finding.path.clone(),
            category: item.finding.category,
            action: item.action.kind(),
            bytes: item.finding.bytes,
            tool: None,
        });
    }

    pub(crate) fn tool(&self, tool: Option<&'static str>) {
        if let Some(current) = self.current().as_mut() {
            current.tool = tool;
        }
    }

    /// Counts an item with an outcome; a `not-run` item only leaves `current`.
    pub(crate) fn item_finished(&self, item: &PlanItem, counted: bool) {
        *self.current() = None;
        if counted {
            self.items_done.fetch_add(1, Relaxed);
            self.bytes_done.fetch_add(counted_bytes(item), Relaxed);
        }
    }

    fn current(&self) -> std::sync::MutexGuard<'_, Option<CurrentItem>> {
        self.current.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn counted_bytes(item: &PlanItem) -> u64 {
    if matches!(item.action, Action::NativeOnly(_)) { 0 } else { item.finding.bytes }
}
