//! Terminal width, so a progress line can be cut to fit one row.

/// Columns of the terminal on stderr; `None` when stderr is not a terminal
/// or reports no width.
pub fn stderr_columns() -> Option<u16> {
    let size = rustix::termios::tcgetwinsize(rustix::stdio::stderr()).ok()?;
    (size.ws_col != 0).then_some(size.ws_col)
}
