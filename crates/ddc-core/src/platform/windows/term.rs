//! Terminal width, so a progress line can be cut to fit one row.

use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_ERROR_HANDLE,
};

/// Columns of the console window on stderr; `None` when stderr is not a
/// console (a pipe, a file, a terminal emulator without a console) or
/// reports no width.
pub fn stderr_columns() -> Option<u16> {
    // SAFETY: GetStdHandle takes a constant and returns a handle or null/-1,
    // which GetConsoleScreenBufferInfo refuses with 0. `info` is a plain C
    // struct the call fills; it is read only after a non-zero return.
    let info = unsafe {
        let handle = GetStdHandle(STD_ERROR_HANDLE);
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(handle, &mut info) == 0 {
            return None;
        }
        info
    };
    let width = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
    u16::try_from(width).ok().filter(|w| *w > 0)
}
