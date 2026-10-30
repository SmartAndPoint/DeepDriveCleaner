//! The CLI on macOS.

use std::io;
use std::process::Command;

use crate::notify::Notifier;

/// The CLI's own notification, through AppleScript.
pub struct SystemNotifier;

impl Notifier for SystemNotifier {
    /// Texts are passed as script arguments, never spliced into AppleScript.
    fn notify(&self, title: &str, body: &str) -> io::Result<()> {
        let status = Command::new("/usr/bin/osascript")
            .args(["-e", "on run argv", "-e", "display notification (item 1 of argv) with title (item 2 of argv)"])
            .args(["-e", "end run", body, title])
            .status()?;
        if status.success() { Ok(()) } else { Err(io::Error::other(format!("osascript exited with {status}"))) }
    }
}
