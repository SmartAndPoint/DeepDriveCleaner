//! Another process's command line, working folder and environment, on
//! Windows (safety ADR §5, amendment of 2026-11-02, the process check;
//! story "Windows scanner and rule catalog", step 9a): what Linux reads from
//! `/proc/<pid>/{cmdline,cwd,environ}`.
//!
//! - The command line comes from `NtQueryInformationProcess` with
//!   `ProcessCommandLineInformation`, which a limited handle may ask for, and
//!   is split by the Microsoft C runtime's rules ([`split_args`]).
//! - The working folder and the environment come from the process
//!   parameters its PEB points to, read with `ReadProcessMemory`. The
//!   parameters' layout is not documented past its first fields; the
//!   offsets below are the 64-bit ones every Windows since Vista keeps, and
//!   a test reads this process through the same path and compares.
//! - A 32-bit process on 64-bit Windows keeps its current folder and
//!   environment in its 32-bit block; the 64-bit block holds the copy made
//!   at its start. Its fields are not read ([`Fields::wow64`]).
//!
//! Every read is bounded: a name at most 32767 units, the environment at
//! most [`ENV_MAX`] bytes. A read that fails (the process exited, or
//! changed its block meanwhile) leaves the field empty, never retried.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt as _;
use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
use std::path::PathBuf;

use windows_sys::Wdk::System::Threading::{
    NtQueryInformationProcess, ProcessBasicInformation, ProcessCommandLineInformation,
};
use windows_sys::Win32::Foundation::{RtlNtStatusToDosError, UNICODE_STRING};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_UNKNOWN;
use windows_sys::Win32::System::Threading::{IsWow64Process2, PROCESS_BASIC_INFORMATION};

// The offsets are those of a 64-bit process read by a 64-bit ddc.
const _: () = assert!(size_of::<usize>() == 8, "the PEB offsets are the 64-bit ones");

/// `PEB.ProcessParameters`.
const PEB_PARAMETERS: usize = 0x20;
/// `RTL_USER_PROCESS_PARAMETERS.CurrentDirectory.DosPath`.
const PARAMETERS_CWD: usize = 0x38;
/// `RTL_USER_PROCESS_PARAMETERS.Environment`.
const PARAMETERS_ENVIRONMENT: usize = 0x80;
/// `RTL_USER_PROCESS_PARAMETERS.EnvironmentSize`.
const PARAMETERS_ENVIRONMENT_SIZE: usize = 0x3F0;
/// The longest name Windows has, in UTF-16 units.
const NAME_MAX: usize = 32_767;
/// The most of an environment that is read.
pub(super) const ENV_MAX: usize = 4 << 20;

/// What the process parameters give; `None` where they could not be read.
#[derive(Debug, Default)]
pub(super) struct Fields {
    pub cwd: Option<PathBuf>,
    /// The environment block: `NAME=value` strings, each ended by a NUL.
    pub env: Option<Vec<u16>>,
    /// The block is longer than [`ENV_MAX`]: only its start was read.
    pub env_cut: bool,
    /// A 32-bit process on 64-bit Windows: neither field was read.
    pub wow64: bool,
}

/// The command line of the process `handle` is open on (a limited handle
/// is enough), as one string.
pub(super) fn command_line(handle: &OwnedHandle) -> io::Result<Vec<u16>> {
    let mut len = 0u32;
    // SAFETY: a null buffer of length 0 asks for the size only, written to
    // `len`; the call fails with STATUS_INFO_LENGTH_MISMATCH, as expected.
    unsafe {
        NtQueryInformationProcess(
            handle.as_raw_handle(),
            ProcessCommandLineInformation,
            std::ptr::null_mut(),
            0,
            &mut len,
        )
    };
    if (len as usize) < size_of::<UNICODE_STRING>() || len as usize > size_of::<UNICODE_STRING>() + 2 * NAME_MAX + 2 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "no command line"));
    }
    // Aligned for the UNICODE_STRING the buffer starts with.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    let size = u32::try_from(buf.len() * 8).expect("a bounded length");
    // SAFETY: `buf` is writable for `size` bytes and outlives the call.
    let status = unsafe {
        NtQueryInformationProcess(
            handle.as_raw_handle(),
            ProcessCommandLineInformation,
            buf.as_mut_ptr().cast(),
            size,
            &mut len,
        )
    };
    nt(status)?;
    // SAFETY: the call wrote a UNICODE_STRING at the start of `buf`, which
    // is aligned for it.
    let header = unsafe { buf.as_ptr().cast::<UNICODE_STRING>().read() };
    let units = usize::from(header.Length) / 2;
    let start = buf.as_ptr().cast::<u8>() as usize;
    let text = header.Buffer as usize;
    // The string lies in the same buffer, after the header, aligned for u16.
    let end = text.checked_add(units * 2);
    if !text.is_multiple_of(2)
        || text < start + size_of::<UNICODE_STRING>()
        || end.is_none_or(|e| e > start + buf.len() * 8)
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a command line outside its buffer"));
    }
    // SAFETY: `units` UTF-16 units at `text`, inside `buf` (checked above).
    Ok(unsafe { std::slice::from_raw_parts(header.Buffer, units) }.to_vec())
}

/// The working folder and the environment of the process `handle` is open
/// on, which needs `PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ`.
pub(super) fn fields(handle: &OwnedHandle) -> Fields {
    if wow64(handle).unwrap_or(true) {
        return Fields { wow64: true, ..Fields::default() };
    }
    let Ok(parameters) = parameters(handle) else { return Fields::default() };
    let (env, env_cut) = environment(handle, parameters).map_or((None, false), |(e, cut)| (Some(e), cut));
    Fields { cwd: cwd(handle, parameters), env, env_cut, wow64: false }
}

/// Whether the process runs under WOW64: a 32-bit process on 64-bit
/// Windows. An x64 process emulated on ARM64 is not (`IsWow64Process2`
/// gives it no WOW64 machine), and keeps a native block.
fn wow64(handle: &OwnedHandle) -> io::Result<bool> {
    let (mut process, mut native) = (0, 0);
    // SAFETY: the handle is open for the call; both outputs are locals.
    if unsafe { IsWow64Process2(handle.as_raw_handle(), &mut process, &mut native) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(process != IMAGE_FILE_MACHINE_UNKNOWN)
}

/// The address of the process parameters in the other process.
fn parameters(handle: &OwnedHandle) -> io::Result<usize> {
    // SAFETY: plain data; all zeroes is a valid value.
    let mut basic: PROCESS_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let size = u32::try_from(size_of::<PROCESS_BASIC_INFORMATION>()).expect("a small struct");
    let mut len = 0u32;
    // SAFETY: `basic` is the class's struct, writable for `size` bytes.
    let status = unsafe {
        NtQueryInformationProcess(
            handle.as_raw_handle(),
            ProcessBasicInformation,
            (&raw mut basic).cast(),
            size,
            &mut len,
        )
    };
    nt(status)?;
    let peb = basic.PebBaseAddress as usize;
    if peb == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "no PEB"));
    }
    let parameters = read_usize(handle, offset(peb, PEB_PARAMETERS)?)?;
    if parameters == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "no process parameters"));
    }
    Ok(parameters)
}

/// `base + by`, an address the other process gave: an error when it wraps.
fn offset(base: usize, by: usize) -> io::Result<usize> {
    base.checked_add(by).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "an address past the end"))
}

fn cwd(handle: &OwnedHandle, parameters: usize) -> Option<PathBuf> {
    let name = read_unicode_string(handle, offset(parameters, PARAMETERS_CWD).ok()?).ok()?;
    let mut path = OsString::from_wide(&name);
    // `C:\work\` names `C:\work`; a drive's root keeps its separator.
    let text = path.to_string_lossy();
    if text.len() > 3 && text.ends_with('\\') {
        path = OsString::from_wide(&name[..name.len() - 1]);
    }
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// The environment block, and whether it was cut at [`ENV_MAX`].
fn environment(handle: &OwnedHandle, parameters: usize) -> Option<(Vec<u16>, bool)> {
    let at = read_usize(handle, offset(parameters, PARAMETERS_ENVIRONMENT).ok()?).ok()?;
    let size = read_usize(handle, offset(parameters, PARAMETERS_ENVIRONMENT_SIZE).ok()?).ok()?;
    if at == 0 || size == 0 {
        return None;
    }
    let bytes = read_bytes(handle, at, size.min(ENV_MAX)).ok()?;
    Some((bytes.as_chunks::<2>().0.iter().map(|u| u16::from_le_bytes(*u)).collect(), size > ENV_MAX))
}

/// A `UNICODE_STRING` at `at` in the other process, and the text it points to.
fn read_unicode_string(handle: &OwnedHandle, at: usize) -> io::Result<Vec<u16>> {
    let header = read_bytes(handle, at, 16)?;
    let len = usize::from(u16::from_le_bytes([header[0], header[1]]));
    let text = usize::from_le_bytes(header[8..16].try_into().expect("8 bytes"));
    offset(text, len)?;
    if len == 0 || len % 2 != 0 || len / 2 > NAME_MAX || text == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "no string"));
    }
    let bytes = read_bytes(handle, text, len)?;
    Ok(bytes.as_chunks::<2>().0.iter().map(|u| u16::from_le_bytes(*u)).collect())
}

fn read_usize(handle: &OwnedHandle, at: usize) -> io::Result<usize> {
    let bytes = read_bytes(handle, at, 8)?;
    Ok(usize::from_le_bytes(bytes.try_into().expect("8 bytes")))
}

/// `len` bytes at `at` in the other process, all of them or an error.
fn read_bytes(handle: &OwnedHandle, at: usize, len: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let mut read = 0usize;
    // SAFETY: `buf` is writable for `len` bytes; the address is the other
    // process's, which the call checks; `read` is a local.
    let ok =
        unsafe { ReadProcessMemory(handle.as_raw_handle(), at as *const _, buf.as_mut_ptr().cast(), len, &mut read) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if read != len {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "a short read"));
    }
    Ok(buf)
}

fn nt(status: i32) -> io::Result<()> {
    if status >= 0 {
        return Ok(());
    }
    // SAFETY: a pure conversion of a status code.
    let code = unsafe { RtlNtStatusToDosError(status) };
    Err(io::Error::from_raw_os_error(i32::try_from(code).unwrap_or(i32::MAX)))
}

/// A command line split into its arguments by the Microsoft C runtime's
/// rules ("Parsing C command-line arguments"): white space separates
/// arguments outside quotes; `"` opens and closes a quoted part; `""`
/// inside quotes is one `"`; `2n` backslashes before a `"` are `n`
/// backslashes and the quote is a delimiter, `2n+1` are `n` and a literal
/// `"`; other backslashes are literal. The first argument, the program,
/// takes quotes only, with no escape.
pub(super) fn split_args(line: &[u16]) -> Vec<OsString> {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let white = |c: u16| c == u16::from(b' ') || c == u16::from(b'\t');
    let mut args = Vec::new();
    let mut i = 0;
    // The program.
    let mut program = Vec::new();
    let mut quoted = false;
    while i < line.len() {
        let c = line[i];
        if c == QUOTE {
            quoted = !quoted;
        } else if white(c) && !quoted {
            break;
        } else {
            program.push(c);
        }
        i += 1;
    }
    args.push(OsString::from_wide(&program));
    loop {
        while i < line.len() && white(line[i]) {
            i += 1;
        }
        if i >= line.len() {
            break;
        }
        let mut arg = Vec::new();
        let mut quoted = false;
        while i < line.len() {
            let c = line[i];
            if c == BACKSLASH {
                let run = line[i..].iter().take_while(|&&b| b == BACKSLASH).count();
                if line.get(i + run) == Some(&QUOTE) {
                    arg.extend(std::iter::repeat_n(BACKSLASH, run / 2));
                    i += run;
                    if run % 2 == 1 {
                        arg.push(QUOTE);
                        i += 1;
                    }
                } else {
                    arg.extend(std::iter::repeat_n(BACKSLASH, run));
                    i += run;
                }
                continue;
            }
            if c == QUOTE {
                if quoted && line.get(i + 1) == Some(&QUOTE) {
                    arg.push(QUOTE);
                    i += 2;
                    continue;
                }
                quoted = !quoted;
                i += 1;
                continue;
            }
            if white(c) && !quoted {
                break;
            }
            arg.push(c);
            i += 1;
        }
        args.push(OsString::from_wide(&arg));
    }
    args
}

/// `VIRTUAL_ENV` and `CONDA_PREFIX` from an environment block, by name in
/// any case; empty values and the hidden `=C:=…` entries are skipped.
pub(super) fn env_prefixes(env: &[u16]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in env.split(|&u| u == 0) {
        let text = String::from_utf16_lossy(entry);
        let Some((name, value)) = text.split_once('=') else { continue };
        if name.is_empty() || value.is_empty() {
            continue;
        }
        if name.eq_ignore_ascii_case("VIRTUAL_ENV") || name.eq_ignore_ascii_case("CONDA_PREFIX") {
            // The value as its units, a name no UTF-16 can spell included.
            out.push(PathBuf::from(OsString::from_wide(&entry[name.encode_utf16().count() + 1..])));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn split(s: &str) -> Vec<String> {
        split_args(&wide(s)).into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    /// Microsoft's examples ("Parsing C command-line arguments"), after the
    /// program.
    #[test]
    fn the_command_line_splits_by_the_c_runtimes_rules() {
        assert_eq!(split(r#"p "a b c" d e"#), ["p", "a b c", "d", "e"]);
        assert_eq!(split(r#"p "ab\"c" "\\" d"#), ["p", r#"ab"c"#, r"\", "d"]);
        assert_eq!(split(r#"p a\\\b d"e f"g h"#), ["p", r"a\\\b", "de fg", "h"]);
        assert_eq!(split(r#"p a\\\"b c d"#), ["p", r#"a\"b"#, "c", "d"]);
        assert_eq!(split(r#"p a\\\\"b c" d e"#), ["p", r"a\\b c", "d", "e"]);
        assert_eq!(split(r#"p a"b"" c d"#), ["p", r#"ab" c d"#]);
        // The program takes quotes, never an escape.
        assert_eq!(split(r#""C:\Program Files\x\x.exe" --flag"#), [r"C:\Program Files\x\x.exe", "--flag"]);
        assert_eq!(split(r"C:\tools\x.exe  "), [r"C:\tools\x.exe"]);
        assert_eq!(split(""), [""]);
    }

    #[test]
    fn the_environment_gives_its_prefixes_by_name_in_any_case() {
        let mut env = Vec::new();
        for entry in
            ["=C:=C:\\x", "Path=C:\\Windows", "virtual_env=C:\\p\\.venv", "CONDA_PREFIX=", "Conda_Prefix=D:\\c"]
        {
            env.extend(wide(entry));
            env.push(0);
        }
        env.push(0);
        assert_eq!(env_prefixes(&env), [PathBuf::from(r"C:\p\.venv"), PathBuf::from(r"D:\c")]);
    }
}
