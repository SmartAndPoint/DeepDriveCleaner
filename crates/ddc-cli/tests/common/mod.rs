//! What the CLI tests share across platforms (ADR "Platform seam: one
//! platform module per OS with declared capabilities", §9): the skip of a
//! command the build refuses, and the Unix-only helpers, which keep one
//! signature on every OS and panic elsewhere, where only a test that has
//! already skipped reaches them.
#![allow(dead_code)]

use std::path::Path;

/// A test that runs `scan`, `clean`, `du`, `disk`, `schedule`, `notify` or
/// `diag`, which a build with `SUPPORTED` false refuses, starts with
/// `if common::unsupported("name") { return; }`.
pub use ddc_loadgen::unsupported;

/// A test of a command or action that needs a capability (`ddc disk`, `ddc
/// disk agents`, Homebrew) starts with
/// `if common::lacks("name", Capability::WholeDisk) { return; }`.
#[allow(unused_imports)] // only the capability tests use it
pub use ddc_loadgen::lacks;

/// The data folder a CLI started with a clean environment and its home at
/// `home` (`ClearEnv::home_env`) uses when nothing names one:
/// `~/Library/Application Support/DeepDriveCleaner` on macOS, the XDG state
/// folder on Linux, `%LOCALAPPDATA%\DeepDriveCleaner` on Windows.
pub fn default_data(home: &Path) -> std::path::PathBuf {
    use ddc_core::platform::{PLATFORM, Platform};
    match PLATFORM {
        Platform::Linux => home.join(".local/state/deepdrivecleaner"),
        Platform::Windows => home.join("AppData").join("Local").join("DeepDriveCleaner"),
        Platform::Macos => home.join("Library/Application Support/DeepDriveCleaner"),
    }
}

/// `script`, set to run `program args` on a pseudo-terminal of its own as
/// its foreground process group, with no record file: BSD's on macOS,
/// util-linux's on Linux (which passes the exit status on with `-e`).
/// `Unsupported` where there is no pseudo-terminal driver (Windows), and the
/// test skips through [`skipped_without`].
pub fn script(program: &std::ffi::OsStr, args: &[&std::ffi::OsStr]) -> std::io::Result<std::process::Command> {
    use ddc_core::platform::{PLATFORM, Platform};
    let mut script = std::process::Command::new("/usr/bin/script");
    match PLATFORM {
        Platform::Macos => {
            script.args(["-q", "/dev/null"]).arg(program).args(args);
        }
        Platform::Linux => {
            let words: Vec<String> =
                std::iter::once(program).chain(args.iter().copied()).map(|w| sh_quoted(&w.to_string_lossy())).collect();
            script.args(["-qec", &format!("exec {}", words.join(" ")), "/dev/null"]);
        }
        Platform::Windows => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "no pseudo-terminal driver on this platform",
            ));
        }
    }
    Ok(script)
}

/// `w` as one word for `sh`.
pub fn sh_quoted(w: &str) -> String {
    format!("'{}'", w.replace('\'', r"'\''"))
}

/// The value of a harness tool (the pseudo-terminal driver), or `None`
/// after printing why when the tool is missing here
/// (`io::ErrorKind::Unsupported`), and the test skips (ADR "Platform seam",
/// §9, amended). Any other error fails the test.
pub fn skipped_without<T>(test: &str, made: std::io::Result<T>) -> Option<T> {
    match made {
        Ok(value) => Some(value),
        Err(why) if why.kind() == std::io::ErrorKind::Unsupported => {
            eprintln!("skipped {test}: {why}");
            None
        }
        Err(why) => panic!("{test}: {why}"),
    }
}

/// `(dev, ino)` of one path, without following symlinks.
pub fn id(path: impl AsRef<Path>) -> (u64, u64) {
    ddc_core::platform::meta::lstat(path.as_ref()).unwrap().id()
}

/// Sets the Unix permission bits of `path`.
pub fn set_mode(path: impl AsRef<Path>, mode: u32) {
    let path = path.as_ref();
    try_set_mode(path, mode).unwrap_or_else(|e| panic!("chmod {mode:o} {}: {e}", path.display()));
}

#[cfg(unix)]
fn try_set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn try_set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Unix permission bits"))
}

/// A symlink at `link` to `target`; Unix only, as [`set_mode`].
#[cfg(unix)]
pub fn symlink(target: impl AsRef<Path>, link: impl AsRef<Path>) {
    let link = link.as_ref();
    std::os::unix::fs::symlink(target, link).unwrap_or_else(|e| panic!("symlink {}: {e}", link.display()));
}

#[cfg(not(unix))]
pub fn symlink(_target: impl AsRef<Path>, link: impl AsRef<Path>) {
    panic!("symlink {}: Unix only", link.as_ref().display());
}
