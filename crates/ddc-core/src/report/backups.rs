//! iPhone and iPad backups (story "User data: pick your own files for the
//! Trash"): whether a backup finished, and which device it is of. A small
//! bounds-checked reader of the top-level string and date values of a
//! binary (`bplist00`) or XML property list: the scan needs three keys, not
//! a plist library. Never through a symlink, with capped reads; anything it
//! cannot read means "not finished" and "unknown device".

use std::fs::OpenOptions;
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `Info.plist` may carry megabytes of app data before its trailer.
const MAX_PLIST: u64 = 32 << 20;
/// Objects in a binary plist this reader looks at, at most.
const MAX_OBJECTS: u64 = 1 << 20;
/// Seconds from 1970-01-01 to 2001-01-01, the epoch of binary plist dates.
const APPLE_EPOCH: f64 = 978_307_200.0;
/// 10000-01-01: RFC 3339 has four-digit years. A later date is garbage.
pub(crate) const YEAR_10000: u64 = 253_402_300_800;

/// What `Info.plist` says about the backed-up device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    pub device_name: Option<String>,
    pub last_backup: Option<SystemTime>,
}

/// A top-level value this reader understands.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    String(String),
    Date(SystemTime),
}

/// Whether the backup in `dir` finished: its `Status.plist` says
/// `SnapshotState = finished`. A missing or unreadable file says no.
pub fn finished(dir: &Path) -> bool {
    read(&dir.join("Status.plist"))
        .and_then(|bytes| top_level(&bytes, "SnapshotState"))
        .is_some_and(|v| v == Value::String("finished".to_owned()))
}

/// The device name and the date of the last backup, from `Info.plist`.
pub fn info(dir: &Path) -> Info {
    let Some(bytes) = read(&dir.join("Info.plist")) else { return Info::default() };
    Info {
        device_name: match top_level(&bytes, "Device Name") {
            Some(Value::String(name)) if !name.is_empty() => Some(name),
            _ => None,
        },
        last_backup: match top_level(&bytes, "Last Backup Date") {
            Some(Value::Date(t)) => Some(t),
            _ => None,
        },
    }
}

/// A regular file, opened without following a symlink or blocking on a FIFO.
fn read(path: &Path) -> Option<Vec<u8>> {
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path).ok()?;
    let md = file.metadata().ok()?;
    if !md.is_file() || md.len() > MAX_PLIST {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_PLIST).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// The value of `key` in the plist's top-level dictionary.
pub(crate) fn top_level(bytes: &[u8], key: &str) -> Option<Value> {
    if bytes.starts_with(b"bplist00") { binary(bytes, key) } else { xml(bytes, key) }
}

// ---- binary -------------------------------------------------------------

struct Binary<'a> {
    bytes: &'a [u8],
    offset_size: usize,
    ref_size: usize,
    objects: u64,
    table: usize,
}

fn be(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b))
}

fn binary(bytes: &[u8], key: &str) -> Option<Value> {
    let trailer = bytes.get(bytes.len().checked_sub(32)?..)?;
    let (offset_size, ref_size) = (usize::from(trailer[6]), usize::from(trailer[7]));
    let objects = be(&trailer[8..16]);
    let top = be(&trailer[16..24]);
    let table = usize::try_from(be(&trailer[24..32])).ok()?;
    if !(1..=8).contains(&offset_size) || !(1..=8).contains(&ref_size) || objects > MAX_OBJECTS || top >= objects {
        return None;
    }
    let p = Binary { bytes, offset_size, ref_size, objects, table };
    let (count, refs) = p.dict(top)?;
    for i in 0..count {
        let key_ref = p.reference(refs, i)?;
        if p.is(key_ref, key) {
            let value_ref = p.reference(refs, count + i)?;
            return p.string(value_ref).map(Value::String).or_else(|| p.date(value_ref).map(Value::Date));
        }
    }
    None
}

impl Binary<'_> {
    fn slice(&self, at: usize, len: usize) -> Option<&[u8]> {
        self.bytes.get(at..at.checked_add(len)?)
    }

    /// Where object `n` starts.
    fn offset(&self, n: u64) -> Option<usize> {
        if n >= self.objects {
            return None;
        }
        let at = self.table.checked_add(usize::try_from(n).ok()?.checked_mul(self.offset_size)?)?;
        usize::try_from(be(self.slice(at, self.offset_size)?)).ok()
    }

    /// The object's marker, its length (an extended length follows `0xF` as an
    /// int object) and where its contents start.
    fn header(&self, n: u64) -> Option<(u8, usize, usize)> {
        let at = self.offset(n)?;
        let marker = *self.bytes.get(at)?;
        let low = usize::from(marker & 0x0F);
        if low != 0x0F {
            return Some((marker >> 4, low, at + 1));
        }
        let int = *self.bytes.get(at + 1)?;
        if int >> 4 != 0x1 {
            return None;
        }
        let size = 1usize << (int & 0x0F);
        if size > 8 {
            return None;
        }
        let len = usize::try_from(be(self.slice(at + 2, size)?)).ok()?;
        Some((marker >> 4, len, at + 2 + size))
    }

    fn dict(&self, n: u64) -> Option<(usize, usize)> {
        match self.header(n)? {
            (0xD, count, refs) => {
                self.slice(refs, count.checked_mul(2)?.checked_mul(self.ref_size)?)?;
                Some((count, refs))
            }
            _ => None,
        }
    }

    fn reference(&self, refs: usize, i: usize) -> Option<u64> {
        Some(be(self.slice(refs.checked_add(i.checked_mul(self.ref_size)?)?, self.ref_size)?))
    }

    /// Whether object `n` is the string `key` (ASCII), compared in place:
    /// a crafted plist whose keys all point at one huge string costs nothing.
    fn is(&self, n: u64, key: &str) -> bool {
        let key = key.as_bytes();
        match self.header(n) {
            Some((0x5, len, at)) => len == key.len() && self.slice(at, len) == Some(key),
            Some((0x6, len, at)) => {
                len == key.len()
                    && self.slice(at, len * 2).is_some_and(|units| {
                        units.as_chunks::<2>().0.iter().zip(key).all(|(u, &k)| u16::from_be_bytes(*u) == u16::from(k))
                    })
            }
            _ => false,
        }
    }

    fn string(&self, n: u64) -> Option<String> {
        match self.header(n)? {
            (0x5, len, at) => Some(self.slice(at, len)?.iter().map(|&b| char::from(b)).collect()),
            (0x6, len, at) => {
                let units: Vec<u16> = self
                    .slice(at, len.checked_mul(2)?)?
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_be_bytes(*c))
                    .collect();
                String::from_utf16(&units).ok()
            }
            _ => None,
        }
    }

    fn date(&self, n: u64) -> Option<SystemTime> {
        let at = self.offset(n)?;
        if *self.bytes.get(at)? != 0x33 {
            return None;
        }
        let secs = f64::from_be_bytes(self.slice(at + 1, 8)?.try_into().ok()?) + APPLE_EPOCH;
        // A flipped bit makes any double: only a date between 1970 and 9999 is one.
        if !(secs.is_finite() && (0.0..YEAR_10000 as f64).contains(&secs)) {
            return None;
        }
        UNIX_EPOCH.checked_add(Duration::try_from_secs_f64(secs).ok()?)
    }
}

// ---- XML ----------------------------------------------------------------

/// Walks the tags, tracking the depth of dictionaries and arrays; a key at
/// depth 1 is a top-level key, and the element after it is its value.
fn xml(bytes: &[u8], key: &str) -> Option<Value> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut rest = text;
    let mut depth = 0usize;
    while let Some(open) = rest.find('<') {
        rest = &rest[open..];
        let close = rest.find('>')?;
        let tag = &rest[1..close];
        rest = &rest[close + 1..];
        match tag {
            "dict" | "array" => depth += 1,
            "/dict" | "/array" => depth = depth.checked_sub(1)?,
            "key" if depth == 1 => {
                let end = rest.find("</key>")?;
                let found = unescape(&rest[..end])? == key;
                rest = &rest[end + "</key>".len()..];
                if found {
                    return value(rest);
                }
            }
            _ => {}
        }
    }
    None
}

/// The element at the start of `rest` (after whitespace), as a string or a date.
fn value(rest: &str) -> Option<Value> {
    let rest = rest.trim_start();
    for (open, close) in [("<string>", "</string>"), ("<date>", "</date>")] {
        if let Some(body) = rest.strip_prefix(open) {
            let inner = unescape(&body[..body.find(close)?])?;
            return if open == "<string>" {
                Some(Value::String(inner))
            } else {
                humantime::parse_rfc3339(inner.trim())
                    .ok()
                    .filter(|t| t.duration_since(UNIX_EPOCH).is_ok_and(|d| d.as_secs() < YEAR_10000))
                    .map(Value::Date)
            };
        }
    }
    if rest.starts_with("<string/>") {
        return Some(Value::String(String::new()));
    }
    None
}

fn unescape(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp + 1..];
        let semi = rest.find(';')?;
        let entity = &rest[..semi];
        out.push(match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = match entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => entity.strip_prefix('#')?.parse().ok()?,
                };
                char::from_u32(code)?
            }
        });
        rest = &rest[semi + 1..];
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use super::*;

    const STATUS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>BackupState</key>
	<string>new</string>
	<key>Date</key>
	<date>2026-09-30T08:15:00Z</date>
	<key>IsFullBackup</key>
	<false/>
	<key>SnapshotState</key>
	<string>STATE</string>
	<key>UUID</key>
	<string>7D2F0B8E-0000-4000-8000-000000000000</string>
	<key>Version</key>
	<string>3.3</string>
</dict>
</plist>
"#;

    const INFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Applications</key>
	<dict>
		<key>Device Name</key>
		<string>nested, not this one</string>
	</dict>
	<key>Build Version</key>
	<string>22A3354</string>
	<key>Device Name</key>
	<string>Example&apos;s iPhone &amp; more — ✓</string>
	<key>Installed Applications</key>
	<array>
		<string>com.example.app</string>
	</array>
	<key>Last Backup Date</key>
	<date>2026-10-01T12:30:45Z</date>
	<key>iTunes Files</key>
	<dict>
		<key>IC-Info.sidv</key>
		<data>AAECAwQFBgcICQ==</data>
	</dict>
</dict>
</plist>
"#;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    /// Writes `xml` as `name` in `dir`, converted to a binary plist when asked.
    fn plist(dir: &Path, name: &str, xml: &str, binary: bool) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, xml).unwrap();
        if binary {
            let ok = Command::new("/usr/bin/plutil").args(["-convert", "binary1"]).arg(&path).status().unwrap();
            assert!(ok.success());
            assert!(std::fs::read(&path).unwrap().starts_with(b"bplist00"));
        }
        path
    }

    #[test]
    fn a_finished_backup_reads_as_finished_in_both_formats() {
        for binary in [false, true] {
            let d = dir();
            plist(d.path(), "Status.plist", &STATUS.replace("STATE", "finished"), binary);
            assert!(finished(d.path()), "binary: {binary}");
            let d = dir();
            plist(d.path(), "Status.plist", &STATUS.replace("STATE", "new"), binary);
            assert!(!finished(d.path()), "binary: {binary}");
        }
    }

    #[test]
    fn the_device_and_last_backup_come_from_the_top_level_only() {
        let when = humantime::parse_rfc3339("2026-10-01T12:30:45Z").unwrap();
        for binary in [false, true] {
            let d = dir();
            plist(d.path(), "Info.plist", INFO, binary);
            let info = info(d.path());
            assert_eq!(info.device_name.as_deref(), Some("Example's iPhone & more — ✓"), "binary: {binary}");
            assert_eq!(info.last_backup, Some(when), "binary: {binary}");
        }
    }

    #[test]
    fn missing_truncated_or_garbage_files_read_as_not_finished_without_a_panic() {
        let d = dir();
        assert!(!finished(d.path()), "missing");
        assert_eq!(info(d.path()), Info::default());

        let full = plist(d.path(), "Status.plist", &STATUS.replace("STATE", "finished"), true);
        let bytes = std::fs::read(&full).unwrap();
        let done = Some(Value::String("finished".to_owned()));
        for cut in 0..bytes.len() {
            assert_ne!(top_level(&bytes[..cut], "SnapshotState"), done, "cut at {cut}");
        }
        for garbage in
            [&b"bplist00"[..], b"bplist00\xff\xff\xff\xff", b"<dict><key>SnapshotState", b"\x00\x01\x02", b""]
        {
            assert_eq!(top_level(garbage, "SnapshotState"), None);
        }
        // Every single-bit change of a valid binary plist is read or refused,
        // never a panic: the date's exponent bits too.
        let info = std::fs::read(plist(d.path(), "Info.plist", INFO, true)).unwrap();
        for (bytes, key) in [(&bytes, "SnapshotState"), (&info, "Last Backup Date"), (&info, "Device Name")] {
            for i in 0..bytes.len() {
                for bit in 0..8 {
                    let mut bent = bytes.clone();
                    bent[i] ^= 1 << bit;
                    if let Some(Value::Date(t)) = top_level(&bent, key) {
                        assert!(crate::report::rfc3339(t).is_some(), "byte {i} bit {bit}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_date_past_year_9999_or_before_1970_is_no_date() {
        let at = |secs: f64| {
            // A one-object binary plist: { "Last Backup Date": <date> }.
            let mut b = b"bplist00".to_vec();
            let dict = b.len();
            b.extend([0xD1, 0x01, 0x02]);
            let key = b.len();
            // An ASCII string of 16 bytes: its length as a one-byte int.
            b.extend([0x5F, 0x10, 16]);
            b.extend(b"Last Backup Date");
            let date = b.len();
            b.push(0x33);
            b.extend((secs - APPLE_EPOCH).to_be_bytes());
            let table = b.len();
            b.extend([dict as u8, key as u8, date as u8]);
            b.extend([0, 0, 0, 0, 0, 0, 1, 1]);
            b.extend(3u64.to_be_bytes());
            b.extend(0u64.to_be_bytes());
            b.extend((table as u64).to_be_bytes());
            top_level(&b, "Last Backup Date")
        };
        assert!(matches!(at(1_700_000_000.0), Some(Value::Date(_))));
        for secs in [YEAR_10000 as f64, 1e19, 1e300, f64::MAX, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(at(secs), None, "{secs}");
        }
        let xml = INFO.replace("2026-10-01T12:30:45Z", "9999-12-31T23:59:59Z");
        assert!(matches!(top_level(xml.as_bytes(), "Last Backup Date"), Some(Value::Date(_))));
    }

    #[test]
    fn a_symlink_or_a_fifo_is_never_read() {
        let d = dir();
        let real = plist(d.path(), "real.plist", &STATUS.replace("STATE", "finished"), false);
        std::os::unix::fs::symlink(&real, d.path().join("Status.plist")).unwrap();
        assert!(!finished(d.path()), "a symlink");

        let d = dir();
        let fifo = Command::new("/usr/bin/mkfifo").arg(d.path().join("Status.plist")).status().unwrap();
        assert!(fifo.success());
        assert!(!finished(d.path()), "a FIFO, without blocking");
    }
}
