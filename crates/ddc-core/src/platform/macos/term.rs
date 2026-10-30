//! Terminal width, so a progress line can be cut to fit one row.

/// Columns of the terminal on stderr; `None` when stderr is not a terminal
/// or reports no width.
pub fn stderr_columns() -> Option<u16> {
    let mut size = libc::winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: `size` is a valid out-pointer for TIOCGWINSZ.
    if unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut size) } != 0 || size.ws_col == 0 {
        return None;
    }
    Some(size.ws_col)
}
