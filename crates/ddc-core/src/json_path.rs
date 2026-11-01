//! Paths in JSON output (ADR "Rust core library and CLI with JSON output",
//! amendment of 2026-10-31). Linux allows names that are not UTF-8, which
//! serde refuses to write, so an output path is written as a string with
//! U+FFFD for each invalid sequence; valid UTF-8 is written unchanged. The
//! scan and clean reports count such paths in `lossy_paths`. A store that
//! is read back and must match (`settings.json`) keeps serde's own writer,
//! which refuses a non-UTF-8 path.

use std::borrow::Cow;
use std::cell::Cell;
use std::path::{Path, PathBuf};

use serde::{Serialize, Serializer};

thread_local! {
    /// Paths written lossily on this thread since [`count_lossy`] started.
    static LOSSY: Cell<u64> = const { Cell::new(0) };
}

fn lossy(p: &Path) -> Cow<'_, str> {
    match p.to_str() {
        Some(s) => Cow::Borrowed(s),
        None => {
            LOSSY.with(|n| n.set(n.get() + 1));
            p.to_string_lossy()
        }
    }
}

struct Lossy<'a>(&'a Path);

impl Serialize for Lossy<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&lossy(self.0))
    }
}

/// `serialize_with` for a `PathBuf`.
pub fn path<S: Serializer>(p: &Path, s: S) -> Result<S::Ok, S::Error> {
    Lossy(p).serialize(s)
}

/// `serialize_with` for an `Option<PathBuf>`.
pub fn opt_path<S: Serializer>(p: &Option<PathBuf>, s: S) -> Result<S::Ok, S::Error> {
    match p {
        Some(p) => s.serialize_some(&Lossy(p)),
        None => s.serialize_none(),
    }
}

/// `serialize_with` for a `Vec<PathBuf>`.
pub fn paths<S: Serializer>(v: &[PathBuf], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(v.iter().map(|p| Lossy(p)))
}

/// How many paths `value` writes lossily: `None` when every one is UTF-8,
/// so the report leaves `lossy_paths` out.
pub fn count_lossy<T: Serialize>(value: &T) -> Option<u64> {
    LOSSY.with(|n| n.set(0));
    let _ = serde_json::to_writer(std::io::sink(), value);
    Some(LOSSY.with(|n| n.replace(0))).filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Out {
        #[serde(serialize_with = "path")]
        one: PathBuf,
        #[serde(serialize_with = "opt_path")]
        maybe: Option<PathBuf>,
        #[serde(serialize_with = "paths")]
        many: Vec<PathBuf>,
    }

    #[test]
    fn a_path_that_is_not_utf8_is_written_lossily_and_counted() {
        // Windows spells the bad byte as an unpaired surrogate (WTF-8).
        let bad = crate::platform::testing::name_from_bytes(b"bad-\xff").expect("a name that is not UTF-8");
        let bad = PathBuf::from("/h").join(bad);
        // The separator `join` puts in: `/` on Unix, `\` on Windows.
        let lossy = PathBuf::from("/h").join("bad-\u{fffd}").to_string_lossy().into_owned();
        let out = Out { one: bad.clone(), maybe: Some(bad.clone()), many: vec![PathBuf::from("/ok"), bad] };
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["one"], lossy.as_str());
        assert_eq!(json["maybe"], lossy.as_str());
        assert_eq!(json["many"], serde_json::json!(["/ok", lossy]));
        assert_eq!(count_lossy(&out), Some(3));
        let fine = Out { one: "/a".into(), maybe: None, many: vec![] };
        assert_eq!(count_lossy(&fine), None);
        assert_eq!(serde_json::to_value(&fine).unwrap()["maybe"], serde_json::Value::Null);
    }
}
