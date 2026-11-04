//! The two reports, read as bytes and decoded whatever their
//! `schema_version`: an older report usually decodes with today's types
//! (bumps 3 to 7 only added enum variants; 8 changed `dev` on Linux btrfs
//! and ZFS, which a report from before 0.31 never reaches, since it stops at
//! `predates-fields`). `store::read_last_scan` refuses another version, so
//! it is not used here.

use std::fs;
use std::io::{self, ErrorKind};

use super::Reason;
use crate::env::Env;
use crate::model::ScanReport;
use crate::store::{LAST_SCAN, PREVIOUS_SCAN};

/// The pair as read, and why it cannot be compared as far as reading tells.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// `previous-scan.json`, when it decoded.
    pub baseline: Option<ScanReport>,
    /// `last-scan.json`, when it decoded.
    pub current: Option<ScanReport>,
    /// `no-report`, `no-previous`, `other-schema` and `out-of-step`, when they
    /// hold; [`super::compare_loaded`] adds the reasons the reports tell.
    pub reasons: Vec<Reason>,
}

/// Reads the pair. A missing file is a reason; any other error reading one
/// is an error. When both decode and `last-scan.json` does not name the
/// other as its pair, both are read once more (a save may have been between
/// the two reads) before the pair is `out-of-step`.
pub fn load(env: &Env) -> io::Result<Loaded> {
    let first = read_pair(env)?;
    if !first.reasons.contains(&Reason::OutOfStep) {
        return Ok(first);
    }
    read_pair(env)
}

/// Reads `previous-scan.json` against `current`, the report a scan has just
/// saved (`ddc scan`'s block). Not read again when out of step: `current`
/// is this save's own, so another save came between.
pub fn load_previous(env: &Env, current: ScanReport) -> io::Result<Loaded> {
    let mut reasons = Vec::new();
    let baseline = decode(read(env, PREVIOUS_SCAN)?, Reason::NoPrevious, &mut reasons);
    Ok(pair(baseline, Some(current), reasons))
}

fn read_pair(env: &Env) -> io::Result<Loaded> {
    let mut reasons = Vec::new();
    let current = decode(read(env, LAST_SCAN)?, Reason::NoReport, &mut reasons);
    let baseline = decode(read(env, PREVIOUS_SCAN)?, Reason::NoPrevious, &mut reasons);
    Ok(pair(baseline, current, reasons))
}

fn pair(baseline: Option<ScanReport>, current: Option<ScanReport>, mut reasons: Vec<Reason>) -> Loaded {
    if let (Some(b), Some(c)) = (&baseline, &current)
        && b.written_at.is_some()
        && c.previous_written_at != b.written_at
    {
        reasons.push(Reason::OutOfStep);
    }
    Loaded { baseline, current, reasons }
}

fn read(env: &Env, name: &str) -> io::Result<Option<Vec<u8>>> {
    match fs::read(env.data_dir().join(name)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io::Error::new(e.kind(), format!("cannot read {name}: {e}"))),
    }
}

fn decode(bytes: Option<Vec<u8>>, missing: Reason, reasons: &mut Vec<Reason>) -> Option<ScanReport> {
    let Some(bytes) = bytes else {
        reasons.push(missing);
        return None;
    };
    let report = serde_json::from_slice::<ScanReport>(&bytes).ok();
    if report.is_none() {
        reasons.push(Reason::OtherSchema);
    }
    report
}
