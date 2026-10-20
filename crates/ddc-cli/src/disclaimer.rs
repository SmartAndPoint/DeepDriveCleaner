//! The warning a person at a terminal reads before anything else: what this
//! tool is, and whose risk it is (`DISCLAIMER.md`; story "Distribution: the
//! 0.2.0 cask with the app").

use std::io::{IsTerminal as _, Write as _};

use ddc_core::env::Env;

use crate::render;

/// Absolute: a link in a terminal is clickable, and the copies of the tarball and
/// of the app (`Contents/Resources`) live in different places.
pub const URL: &str = "https://github.com/SmartAndPoint/DeepDriveCleaner/blob/main/DISCLAIMER.md";

/// Two lines, the second indented under the first's text.
pub fn text(color: bool) -> String {
    let label = if color { "\x1b[1;33mwarning:\x1b[0m" } else { "warning:" };
    format!(
        "ddc: {label} DeepDriveCleaner deletes files and can remove almost anything it is told to.\n              \
         Use it at your own risk: {URL}\n"
    )
}

/// Only when stderr is a terminal: the app, launchd, pipes and redirects never
/// see it, so `--json` output and scripts are unchanged.
pub fn warn_if_terminal() {
    let stderr = std::io::stderr();
    if stderr.is_terminal() {
        let _ = stderr.lock().write_all(text(render::use_color(true, Env::no_color())).as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_lines_with_the_link_and_color_only_on_the_label() {
        let plain = text(false);
        assert_eq!(plain.lines().count(), 2);
        assert!(plain.contains(URL) && plain.contains("own risk") && plain.contains("deletes files"));
        assert!(!plain.contains('\x1b'));
        assert_eq!(text(true).replace("\x1b[1;33m", "").replace("\x1b[0m", ""), plain);
    }
}
