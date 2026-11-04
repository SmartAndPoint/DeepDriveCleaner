//! The lab's Recycle Bin test volume (safety ADR §5, the Recycle Bin on
//! Windows, "Tests"; story "Windows scanner and rule catalog", step 10b): a
//! fixed NTFS volume that is not the system's, named by
//! `DDC_RECYCLE_TEST_VOLUME` (a drive root or a mount folder). Tests that
//! need it skip without it, in the environment group of
//! `.config/native-skips/windows.txt`, and fail on a volume that is not one.
//!
//! Its `MaxCapacity` under HKCU is set through `reg.exe`. The value found is
//! kept in one restore file at the volume's root, made only where none is
//! (so two tests never take each other's value for the original; nextest
//! runs them one at a time, group `recycle`), never in a log (it describes
//! the lab machine); a guard puts it back, on a panic too, and the next run
//! refuses to start while an earlier run's restore file is still there.

use std::path::{Path, PathBuf};
use std::process::Command;

use ddc_core::platform::statfs;

/// The test volume's root, or `None` after printing why the test skips.
#[allow(clippy::disallowed_methods)] // a switch of the test harness, not of the scanner
pub fn test_volume(test: &str) -> Option<PathBuf> {
    let Some(root) = std::env::var_os("DDC_RECYCLE_TEST_VOLUME").filter(|v| !v.is_empty()) else {
        eprintln!("skipped {test}: no recycle-bin fixture (DDC_RECYCLE_TEST_VOLUME unset)");
        return None;
    };
    let st = statfs::statfs(Path::new(&root)).unwrap_or_else(|e| panic!("{test}: the test volume: {e}"));
    let system = statfs::statfs(&system_root()).expect("the system volume");
    assert_ne!(st.mounted_from, system.mounted_from, "{test}: DDC_RECYCLE_TEST_VOLUME is the system volume");
    assert_eq!(st.fs_type, "ntfs", "{test}: the test volume is not NTFS");
    Some(st.mount_point)
}

#[allow(clippy::disallowed_methods)] // where reg.exe lives
pub fn system_root() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"))
}

/// The registry key of the Recycle Bin of the volume at `root`.
pub fn bin_key(root: &Path) -> String {
    let guid = statfs::statfs(root).expect("the test volume").mounted_from;
    let braces = guid.strip_prefix(r"\\?\Volume").expect("a volume GUID path").trim_end_matches('\\').to_owned();
    format!(r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{braces}")
}

fn reg(args: &[&str]) -> std::process::Output {
    Command::new(system_root().join(r"System32\reg.exe")).args(args).output().expect("reg.exe runs")
}

/// `MaxCapacity` of `key`, `None` when absent.
pub fn max_capacity(key: &str) -> Option<u32> {
    let out = reg(&["query", key, "/v", "MaxCapacity"]);
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let hex = text.split_whitespace().skip_while(|w| *w != "REG_DWORD").nth(1).expect("a REG_DWORD value");
    Some(u32::from_str_radix(hex.trim_start_matches("0x"), 16).expect("a hexadecimal value"))
}

/// Sets `MaxCapacity` of `key`, in MiB, or removes it.
pub fn set_max_capacity(key: &str, mb: Option<u32>) {
    let out = match mb {
        Some(mb) => reg(&["add", key, "/v", "MaxCapacity", "/t", "REG_DWORD", "/d", &mb.to_string(), "/f"]),
        None => reg(&["delete", key, "/v", "MaxCapacity", "/f"]),
    };
    assert!(out.status.success() || mb.is_none(), "reg.exe refused: {}", String::from_utf8_lossy(&out.stderr));
}

/// Puts `MaxCapacity` back as its restore file says, when dropped.
pub struct Restore {
    file: PathBuf,
    key: String,
}

#[allow(clippy::disallowed_methods)] // its own restore file
impl Restore {
    /// Keeps the value of the volume at `root` in its restore file, after
    /// putting back what an earlier run left (and failing then).
    pub fn new(test: &str, root: &Path) -> Self {
        let restore = Self { file: root.join("ddc-recycle-restore"), key: bin_key(root) };
        let found = max_capacity(&restore.key);
        let made = std::fs::OpenOptions::new().write(true).create_new(true).open(&restore.file);
        let Ok(mut file) = made else {
            restore.back();
            panic!("{test}: an earlier run left MaxCapacity changed; it is restored now");
        };
        std::io::Write::write_all(&mut file, found.map_or("absent".to_owned(), |v| format!("value {v}")).as_bytes())
            .unwrap();
        restore
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    fn back(&self) {
        let Ok(text) = std::fs::read_to_string(&self.file) else { return };
        set_max_capacity(&self.key, text.trim().strip_prefix("value ").and_then(|v| v.parse().ok()));
        let _ = std::fs::remove_file(&self.file);
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        self.back();
    }
}

/// Removes a bin item the shell reported, `$R…` and its `$I…`, by name;
/// the bin is never listed.
#[allow(clippy::disallowed_methods)] // ddc's own test item, by the name the shell gave it
pub fn take_back(new: &Path) {
    if let Some(tail) = new.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix("$R")) {
        let _ = if new.is_dir() { std::fs::remove_dir_all(new) } else { std::fs::remove_file(new) };
        let _ = std::fs::remove_file(new.with_file_name(format!("$I{tail}")));
    }
}
