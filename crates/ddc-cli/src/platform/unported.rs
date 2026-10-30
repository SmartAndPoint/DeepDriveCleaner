//! The CLI on a platform that is not ported yet: the background scan, the
//! one caller of [`SystemNotifier`], is refused at dispatch before it runs.

use std::io;

use crate::notify::Notifier;

/// Shows nothing yet; the port gives it the platform's notification.
pub struct SystemNotifier;

impl Notifier for SystemNotifier {
    fn notify(&self, _title: &str, _body: &str) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "notifications are not available on this platform yet"))
    }
}
