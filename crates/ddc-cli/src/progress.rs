//! Live progress line on stderr while a scan or a clean runs (terminals only).

use std::io::{IsTerminal as _, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ddc_core::exec::{ExecPhase, ExecProgress, ExecSnapshot};
use ddc_core::scan::Progress;

use crate::{render, size};

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
pub fn with_clean_line<T>(progress: &ExecProgress, home: &Path, f: impl FnOnce() -> T) -> T {
    spin(
        |frame| {
            let columns = ddc_core::sys::term::stderr_columns().map_or(80, usize::from);
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
    let done = AtomicBool::new(false);
    /// Stops the spinner even if `f` panics (the scope waits for the thread),
    /// and wakes it so the line clears at once.
    struct Stop<'a>(&'a AtomicBool, std::thread::Thread);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
            self.1.unpark();
        }
    }
    fn draw(text: &str) {
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(text.as_bytes());
        let _ = err.flush();
    }
    std::thread::scope(|s| {
        let spinner = s.spawn(|| {
            const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let mut i = 0;
            while !done.load(Ordering::Relaxed) {
                draw(&format!("\r\x1b[2K{}", line(FRAMES[i % FRAMES.len()])));
                i += 1;
                std::thread::park_timeout(Duration::from_millis(120));
            }
            draw("\r\x1b[2K");
        });
        let _stop = Stop(&done, spinner.thread().clone());
        f()
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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
}
