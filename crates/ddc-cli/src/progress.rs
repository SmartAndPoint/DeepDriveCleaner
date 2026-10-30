//! Progress on stderr while a scan or a clean runs: a live line on a terminal,
//! or with `--progress json` one event per line for the app (story "CLI:
//! progress events"; schema `docs/progress.schema.json`).

use std::io::{IsTerminal as _, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ddc_core::exec::{ExecEvents, ExecPhase, ExecProgress, ExecSnapshot, Outcome};
use ddc_core::model::Totals;
use ddc_core::plan::PlanItem;
use ddc_core::progress::{ItemEvent, ItemsEvent, Phase, PhaseEvent, ProgressEvent, ProgressLine, WalkEvent, WalkStep};
use ddc_core::scan::Progress;

use crate::{render, size};

/// How often the JSON ticker looks; the emitter sends at most one tick per [`GAP`].
const TICK: Duration = Duration::from_millis(50);
/// The least time between two ticks; phase and item lines are never held back.
const GAP: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    /// The live line, on a terminal.
    Line,
    /// `--progress json`.
    Json,
}

/// The progress of one command, in the mode its stderr and flags call for.
#[derive(Debug)]
pub struct Reporter {
    mode: Mode,
    emitter: Emitter,
}

impl Reporter {
    /// JSON when asked, else the live line when stderr is a terminal, else nothing.
    pub fn new(json: bool) -> Self {
        let mode = if json {
            Mode::Json
        } else if std::io::stderr().is_terminal() {
            Mode::Line
        } else {
            Mode::Off
        };
        Self { mode, emitter: Emitter::to(Box::new(std::io::stderr())) }
    }

    pub fn phase(&self, phase: Phase) {
        if self.mode == Mode::Json {
            self.emitter.phase(phase);
        }
    }

    /// Runs `f`, a walk counted by `progress`: the spinner with `label`, or
    /// `walk` ticks of `step`.
    pub fn walk<T>(
        &self,
        progress: &Progress,
        label: &str,
        step: WalkStep,
        files_expected: Option<u64>,
        f: impl FnOnce() -> T,
    ) -> T {
        match self.mode {
            Mode::Off => f(),
            Mode::Line => with_spinner(progress, label, f),
            Mode::Json => ticker(TICK, |_| self.walk_tick(progress, step, files_expected), || {}, f),
        }
    }

    /// One look of the JSON ticker during a walk.
    fn walk_tick(&self, progress: &Progress, step: WalkStep, files_expected: Option<u64>) {
        self.emitter.walk(WalkEvent::new(step, progress.snapshot(), files_expected), Instant::now(), false);
    }

    /// The walk's final tick, from the finished report's totals: exact, where
    /// the live byte count counted a hard-linked file once per link.
    pub fn walk_done(&self, step: WalkStep, totals: &Totals, files_expected: Option<u64>) {
        self.walk_done_counts(WalkEvent {
            step,
            files: totals.files,
            dirs: totals.dirs,
            bytes: totals.scanned_bytes,
            files_expected,
        });
    }

    /// The walk's final tick from counts other than a scan's totals.
    pub fn walk_done_counts(&self, event: WalkEvent) {
        if self.mode == Mode::Json {
            self.emitter.walk(event, Instant::now(), true);
        }
    }

    /// Runs `f`, the executor, with the hook it passes on to `execute`. A
    /// cancel seen by the ticker also stops the keep-marker walk mid-tree.
    pub fn clean<T>(
        &self,
        progress: &ExecProgress,
        home: &Path,
        cancel: &AtomicBool,
        f: impl FnOnce(Option<&dyn ExecEvents>) -> T,
    ) -> T {
        match self.mode {
            Mode::Off => f(None),
            Mode::Line => with_clean_line(progress, home, cancel, || f(None)),
            Mode::Json => {
                let sink = JsonEvents { emitter: &self.emitter, progress };
                let result = ticker(TICK, |_| self.clean_tick(progress, cancel), || {}, || f(Some(&sink)));
                self.emitter.exec_tick(progress, Instant::now(), true);
                result
            }
        }
    }

    /// One look of the JSON ticker during a clean.
    fn clean_tick(&self, progress: &ExecProgress, cancel: &AtomicBool) {
        if cancel.load(Ordering::SeqCst) {
            progress.cancel_walk();
        }
        self.emitter.exec_tick(progress, Instant::now(), false);
    }
}

/// The executor's hook in JSON mode: the lines that must reach stderr in
/// order, written on the executing thread.
#[derive(Debug)]
struct JsonEvents<'a> {
    emitter: &'a Emitter,
    progress: &'a ExecProgress,
}

impl ExecEvents for JsonEvents<'_> {
    fn cleaning(&self) {
        self.emitter.cleaning(self.progress);
    }
    fn item(&self, i: u64, n: u64, item: &PlanItem, outcome: &Outcome) {
        self.emitter.item(ItemEvent::new(i, n, item, outcome));
    }
}

/// Writes the events, one JSON line each, in one order. All of its state
/// sits under one lock, taken before any counter's snapshot: the ticker's
/// ticks and the hook's lines never interleave, a tick never repeats, no
/// `items` tick comes before `phase cleaning` and no `walk` tick after it.
pub struct Emitter {
    state: Mutex<EmitterState>,
}

struct EmitterState {
    out: Box<dyn Write + Send>,
    last_tick: Option<Instant>,
    last_walk: Option<WalkEvent>,
    last_items: Option<ItemsEvent>,
    cleaning: bool,
}

impl std::fmt::Debug for Emitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emitter").finish_non_exhaustive()
    }
}

impl Emitter {
    pub fn to(out: Box<dyn Write + Send>) -> Self {
        Self {
            state: Mutex::new(EmitterState {
                out,
                last_tick: None,
                last_walk: None,
                last_items: None,
                cleaning: false,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, EmitterState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn phase(&self, phase: Phase) {
        let mut state = self.lock();
        state.cleaning |= phase == Phase::Cleaning;
        state.write(ProgressEvent::Phase(PhaseEvent { phase }));
    }

    /// A `walk` tick: dropped once `cleaning` is out, when unchanged, or,
    /// unless it is a step's last, within [`GAP`] of the previous tick.
    pub fn walk(&self, event: WalkEvent, now: Instant, last: bool) {
        self.lock().walk(event, now, last);
    }

    /// The ticker's look at the executor: `walk markers` while it prepares,
    /// `items` once `phase cleaning` is out.
    pub fn exec_tick(&self, progress: &ExecProgress, now: Instant, last: bool) {
        let mut state = self.lock();
        let snap = progress.snapshot();
        if state.cleaning {
            state.items(ItemsEvent::from(&snap), now, last);
        } else if snap.phase == ExecPhase::Preparing && !last {
            state.walk(markers(&snap), now, false);
        }
    }

    /// The hook's `cleaning`: the marker walk's final tick, then the phase.
    fn cleaning(&self, progress: &ExecProgress) {
        let mut state = self.lock();
        let snap = progress.snapshot();
        state.walk(markers(&snap), Instant::now(), true);
        state.cleaning = true;
        state.write(ProgressEvent::Phase(PhaseEvent { phase: Phase::Cleaning }));
    }

    pub fn item(&self, event: ItemEvent) {
        self.lock().write(ProgressEvent::Item(event));
    }
}

fn markers(snap: &ExecSnapshot) -> WalkEvent {
    WalkEvent::new(WalkStep::Markers, snap.walked, Some(snap.walk_expected))
}

impl EmitterState {
    fn walk(&mut self, event: WalkEvent, now: Instant, last: bool) {
        if self.cleaning || self.last_walk.as_ref() == Some(&event) || !(last || self.due(now)) {
            return;
        }
        self.last_tick = Some(now);
        self.last_walk = Some(event.clone());
        self.write(ProgressEvent::Walk(event));
    }

    fn items(&mut self, event: ItemsEvent, now: Instant, last: bool) {
        if self.last_items.as_ref() == Some(&event) || !(last || self.due(now)) {
            return;
        }
        self.last_tick = Some(now);
        self.last_items = Some(event.clone());
        self.write(ProgressEvent::Items(event));
    }

    fn due(&self, now: Instant) -> bool {
        self.last_tick.is_none_or(|t| now.saturating_duration_since(t) >= GAP)
    }

    /// One line, one write, flushed; errors ignored: a closed stderr never
    /// fails a clean.
    fn write(&mut self, event: ProgressEvent) {
        let Ok(mut line) = serde_json::to_vec(&ProgressLine::new(event)) else { return };
        line.push(b'\n');
        let _ = self.out.write_all(&line);
        let _ = self.out.flush();
    }
}

/// Runs `f`, drawing a spinner with counters from `progress` when stderr is a terminal.
pub fn with_spinner<T>(progress: &Progress, label: &str, f: impl FnOnce() -> T) -> T {
    spin(
        |frame| {
            let snap = progress.snapshot();
            format!("{frame} {label} {} files · {}", size::count(snap.files), size::human(snap.bytes))
        },
        f,
    )
}

/// Runs `f` (the executor), drawing the clean's progress from `progress`
/// when stderr is a terminal (story "Progress output while ddc clean runs").
/// A cancel it sees also stops the keep-marker walk mid-tree.
pub fn with_clean_line<T>(progress: &ExecProgress, home: &Path, cancel: &AtomicBool, f: impl FnOnce() -> T) -> T {
    spin(
        |frame| {
            if cancel.load(Ordering::SeqCst) {
                progress.cancel_walk();
            }
            let columns = ddc_core::platform::term::stderr_columns().map_or(80, usize::from);
            // The frame and its space, counted wide, and one spare column so a
            // terminal that wraps at the last column early never wraps the line.
            format!("{frame} {}", clean_line(&progress.snapshot(), home, columns.saturating_sub(4)))
        },
        f,
    )
}

/// The clean's progress in at most `width` columns, so it never wraps:
/// `\r\x1b[2K` clears only the last row of a wrapped line.
pub fn clean_line(snap: &ExecSnapshot, home: &Path, width: usize) -> String {
    let line = match snap.phase {
        ExecPhase::Preparing => {
            let walked = snap.walked.files;
            if snap.walk_expected == 0 {
                format!("preparing · {} files checked", size::count(walked))
            } else {
                // The scan's count is an estimate: a picked entry counts no files.
                let expected = snap.walk_expected.max(walked);
                format!("preparing · {} of {} files checked", size::count(walked), size::count(expected))
            }
        }
        ExecPhase::Cleaning => {
            let index = snap.current.as_ref().map_or(snap.items_done, |c| c.index);
            let head = format!(
                "{index}/{} · {} of {}",
                snap.items_total,
                size::human(snap.bytes_done),
                size::human(snap.bytes_total)
            );
            match &snap.current {
                None => head,
                Some(current) => {
                    let what = match current.tool {
                        Some(tool) => format!("running {tool}"),
                        None => current.category.as_str().to_owned(),
                    };
                    let head = format!("{head} · {what} · ");
                    let room = width.saturating_sub(columns(&head));
                    format!("{head}{}", middle(&render::tilde(&current.path, home), room))
                }
            }
        }
    };
    if columns(&line) > width { fit(line.chars(), width).into_iter().collect() } else { line }
}

/// Columns a character takes, counted cautiously without a width table: any
/// non-ASCII character as two, since CJK is wide and some terminals draw
/// `·` and `…` wide too. A line may come out shorter, never wider.
fn column(c: char) -> usize {
    if c.is_ascii() { 1 } else { 2 }
}

fn columns(text: &str) -> usize {
    text.chars().map(column).sum()
}

/// The leading characters of `chars` that fit in `room` columns.
fn fit(chars: impl Iterator<Item = char>, room: usize) -> Vec<char> {
    let mut used = 0;
    chars.take_while(|&c| (used + column(c) <= room).then(|| used += column(c)).is_some()).collect()
}

/// `text` cut in the middle to `room` columns, with `…` where it was cut.
fn middle(text: &str, room: usize) -> String {
    if columns(text) <= room {
        return text.to_owned();
    }
    let ellipsis = column('…');
    if room < ellipsis {
        return String::new();
    }
    let tail_room = (room - ellipsis) / 2;
    let head = fit(text.chars(), room - ellipsis - tail_room);
    let mut tail = fit(text.chars().rev(), tail_room);
    tail.reverse();
    head.into_iter().chain(['…']).chain(tail).collect()
}

/// Redraws `line(frame)` every 120 ms while `f` runs, when stderr is a terminal.
/// Write errors are ignored: a closed terminal must not end the run.
fn spin<T>(line: impl Fn(char) -> String + Sync, f: impl FnOnce() -> T) -> T {
    if !std::io::stderr().is_terminal() {
        return f();
    }
    fn draw(text: &str) {
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(text.as_bytes());
        let _ = err.flush();
    }
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    ticker(
        Duration::from_millis(120),
        |i| draw(&format!("\r\x1b[2K{}", line(FRAMES[i % FRAMES.len()]))),
        || draw("\r\x1b[2K"),
        f,
    )
}

/// Calls `tick(i)` every `period` on its own thread while `f` runs, then
/// `end()` there once `f` returned.
fn ticker<T>(period: Duration, tick: impl Fn(usize) + Sync, end: impl FnOnce() + Send, f: impl FnOnce() -> T) -> T {
    let done = AtomicBool::new(false);
    let done = &done;
    /// Stops the ticker even if `f` panics (the scope waits for the thread),
    /// and wakes it so it ends at once.
    struct Stop<'a>(&'a AtomicBool, std::thread::Thread);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
            self.1.unpark();
        }
    }
    std::thread::scope(|s| {
        let tick = &tick;
        let thread = s.spawn(move || {
            let mut i = 0;
            while !done.load(Ordering::Relaxed) {
                tick(i);
                i += 1;
                std::thread::park_timeout(period);
            }
            end();
        });
        let _stop = Stop(done, thread.thread().clone());
        f()
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use ddc_core::class::Category;
    use ddc_core::exec::CurrentItem;
    use ddc_core::plan::ActionKind;
    use ddc_core::scan::ProgressSnapshot;

    use super::*;

    const HOME: &str = "/Users/example";

    fn snap(phase: ExecPhase) -> ExecSnapshot {
        ExecSnapshot {
            phase,
            walked: ProgressSnapshot { files: 1204, dirs: 80, bytes: 0, errors: 0 },
            walk_expected: 2210,
            items_total: 12,
            items_done: 2,
            bytes_total: 4 * 1024 * 1024 * 1024,
            bytes_done: 1024 * 1024 * 1024,
            current: None,
        }
    }

    fn current(path: &str, tool: Option<&'static str>) -> CurrentItem {
        CurrentItem {
            index: 3,
            path: PathBuf::from(path),
            category: Category::PackageCache,
            action: ActionKind::Native,
            bytes: 1024,
            tool,
        }
    }

    #[test]
    fn preparing_counts_the_files_against_the_estimate() {
        assert_eq!(
            clean_line(&snap(ExecPhase::Preparing), Path::new(HOME), 80),
            "preparing · 1.2K of 2.2K files checked"
        );
        let mut s = snap(ExecPhase::Preparing);
        s.walk_expected = 0;
        assert_eq!(clean_line(&s, Path::new(HOME), 80), "preparing · 1.2K files checked");
        // A walk past the estimate never reads "1,500 of 1,000".
        s.walk_expected = 1000;
        assert_eq!(clean_line(&s, Path::new(HOME), 80), "preparing · 1.2K of 1.2K files checked");
    }

    #[test]
    fn cleaning_names_the_item_under_home() {
        let mut s = snap(ExecPhase::Cleaning);
        s.current = Some(current("/Users/example/Library/Caches/Homebrew", None));
        assert_eq!(
            clean_line(&s, Path::new(HOME), 100),
            "3/12 · 1.0 GiB of 4.0 GiB · package-cache · ~/Library/Caches/Homebrew"
        );
        s.current = None;
        assert_eq!(clean_line(&s, Path::new(HOME), 100), "2/12 · 1.0 GiB of 4.0 GiB");
    }

    #[test]
    fn a_running_tool_is_named() {
        let mut s = snap(ExecPhase::Cleaning);
        s.current = Some(current("/Users/example/.cache/uv", Some("uv")));
        assert_eq!(clean_line(&s, Path::new(HOME), 100), "3/12 · 1.0 GiB of 4.0 GiB · running uv · ~/.cache/uv");
    }

    #[test]
    fn a_control_character_is_escaped() {
        let mut s = snap(ExecPhase::Cleaning);
        s.current = Some(current("/Users/example/a\u{1b}[2Jb", None));
        let line = clean_line(&s, Path::new(HOME), 100);
        assert!(!line.contains('\u{1b}'), "{line:?}");
        assert!(line.ends_with("~/a\\u{1b}[2Jb"), "{line:?}");
    }

    #[test]
    fn a_long_path_is_cut_in_the_middle_to_the_width() {
        let mut s = snap(ExecPhase::Cleaning);
        let long = format!("/Users/example/Projects/{}node_modules", "deep/".repeat(30));
        s.current = Some(current(&long, None));
        let line = clean_line(&s, Path::new(HOME), 80);
        assert!(columns(&line) <= 80 && columns(&line) >= 79, "{line:?}");
        assert!(line.contains("~/Projects/deep/") && line.contains('…') && line.ends_with("/node_modules"), "{line:?}");
        // Narrower than the head: cut at the end, never wider than asked.
        assert!(columns(&clean_line(&s, Path::new(HOME), 10)) <= 10);
    }

    #[test]
    fn wide_characters_count_two_columns() {
        let mut s = snap(ExecPhase::Cleaning);
        s.current = Some(current("/Users/example/プロジェクト/とても長いフォルダの名前です/__pycache__", None));
        for width in [80, 100, 120] {
            let line = clean_line(&s, Path::new(HOME), width);
            assert!(columns(&line) <= width, "{width}: {line:?}");
            assert!(line.ends_with("__pycache__"), "{width}: {line:?}");
        }
    }

    /// A stderr the test can read back.
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl Write for Buf {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Buf {
        fn events(&self) -> Vec<serde_json::Value> {
            let bytes = self.0.lock().unwrap().clone();
            String::from_utf8(bytes).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
        }
    }

    fn walk(files: u64) -> WalkEvent {
        WalkEvent { step: WalkStep::Scan, files, dirs: 1, bytes: 0, files_expected: None }
    }

    fn item(i: u64) -> ItemEvent {
        let path = PathBuf::from("/Users/example/p/node_modules");
        ItemEvent { i, n: 9, path, action: ActionKind::Delete, bytes: 1, outcome: Outcome::Deleted }
    }

    #[test]
    fn ticks_are_at_least_100_ms_apart_but_last_ticks_phases_and_items_never_wait() {
        let buf = Buf::default();
        let emitter = Emitter::to(Box::new(buf.clone()));
        let start = Instant::now();
        let mut sent = Vec::new();
        // A changing count every 10 ms for 2 s.
        for k in 0..200 {
            let now = start + Duration::from_millis(10 * k);
            let before = buf.events().len();
            emitter.walk(walk(k + 1), now, false);
            if buf.events().len() > before {
                sent.push(now);
                // An item line right after a tick: written at once, and the gap is left alone.
                if sent.len() == 5 {
                    emitter.item(item(5));
                    assert_eq!(buf.events().last().unwrap()["event"], "item");
                }
            }
        }
        assert!((15..=21).contains(&sent.len()), "{} ticks", sent.len());
        assert!(sent.windows(2).all(|w| w[1] - w[0] >= GAP));
        assert_eq!(sent[5] - sent[4], GAP, "the item line did not delay the next tick");

        let after = start + Duration::from_millis(1995);
        emitter.walk(walk(5000), after, true);
        emitter.walk(walk(5000), after, true);
        emitter.phase(Phase::Cleaning);
        let events = buf.events();
        let tail: Vec<_> = events[events.len() - 2..].iter().map(|e| e["event"].as_str().unwrap()).collect();
        assert_eq!(tail, ["walk", "phase"], "the last tick at once, and once; the phase at once");
        assert_eq!(events[events.len() - 2]["files"], 5000);
    }

    #[test]
    fn no_items_tick_before_cleaning_and_no_walk_tick_after_it() {
        let buf = Buf::default();
        let emitter = Emitter::to(Box::new(buf.clone()));
        let progress = ExecProgress::default();
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);

        emitter.exec_tick(&progress, at(0), false);
        emitter.exec_tick(&progress, at(500), false);
        emitter.phase(Phase::Cleaning);
        emitter.walk(walk(9), at(1000), true);
        emitter.exec_tick(&progress, at(1500), false);
        emitter.exec_tick(&progress, at(2000), true);

        let kinds: Vec<_> = buf.events().iter().map(|e| e["event"].as_str().unwrap().to_owned()).collect();
        assert_eq!(kinds, ["walk", "phase", "items"], "an unchanged tick is not repeated, even as the last one");
        assert_eq!(buf.events()[0]["step"], "markers");
    }

    /// The CPU the ticker's own thread uses over a 2 s step during which
    /// nothing changes, as a share of the wall time: the thread's clock, so
    /// other tests in the process do not count. `None` where the platform
    /// has no thread clock yet.
    fn idle_share(test: &str, tick: impl Fn() + Sync) -> Option<f64> {
        use ddc_core::platform::usage::thread_cpu_time;
        if let Err(why) = thread_cpu_time() {
            eprintln!("skipped {test}: {why}");
            return None;
        }
        let first = Mutex::new(None);
        let span = Mutex::new(None);
        ticker(
            TICK,
            |_| {
                first.lock().unwrap().get_or_insert_with(|| (thread_cpu_time().unwrap(), Instant::now()));
                tick();
            },
            || {
                let (cpu, at) = first.lock().unwrap().expect("ticked");
                *span.lock().unwrap() = Some((thread_cpu_time().unwrap() - cpu, at.elapsed()));
            },
            || std::thread::sleep(Duration::from_secs(2)),
        );
        let (cpu, wall) = span.into_inner().unwrap().expect("ended");
        assert!(wall >= Duration::from_millis(1500), "{wall:?}");
        Some(cpu.as_secs_f64() / wall.as_secs_f64())
    }

    fn json_reporter() -> Reporter {
        Reporter { mode: Mode::Json, emitter: Emitter::to(Box::new(Buf::default())) }
    }

    #[test]
    fn the_ticker_does_not_spin_between_ticks() {
        let Some(share) = idle_share("the_ticker_does_not_spin_between_ticks", || {}) else { return };
        assert!(share < 0.01, "{:.2}% CPU", share * 100.0);
    }

    #[test]
    fn json_walk_ticks_do_not_spin_while_the_count_stands() {
        let reporter = json_reporter();
        let progress = Progress::default();
        let Some(share) = idle_share("json_walk_ticks_do_not_spin_while_the_count_stands", || {
            reporter.walk_tick(&progress, WalkStep::Scan, None)
        }) else {
            return;
        };
        assert!(share < 0.01, "{:.2}% CPU", share * 100.0);
    }

    #[test]
    fn json_clean_ticks_do_not_spin_while_the_clean_waits() {
        let reporter = json_reporter();
        let (progress, cancel) = (ExecProgress::default(), AtomicBool::new(false));
        let Some(share) = idle_share("json_clean_ticks_do_not_spin_while_the_clean_waits", || {
            reporter.clean_tick(&progress, &cancel)
        }) else {
            return;
        };
        assert!(share < 0.01, "{:.2}% CPU", share * 100.0);
    }
}
