//! `ddc keep remove` in the core (story "Protected by you: say what it is,
//! and Remove Protection"): the checks of `keep::unprotect`, its journal
//! line and its schema. Temporary folders only; the CLI tests cover the
//! command end to end.

mod common;

use std::path::{Path, PathBuf};

use common::TreeBuilder;
use ddc_core::exec::{JOURNAL_SCHEMA_VERSION, JournalKind, MemoryKeepJournal};
use ddc_core::keep::{self, KEEP_SCHEMA_VERSION, KeepRefusal, KeepReport, KeepResult, Target};

fn target(folder: PathBuf, identity: Option<(u64, u64)>) -> Target {
    Target { folder, identity, rule: Some("node.node_modules".into()) }
}

#[test]
fn a_regular_marker_is_unlinked_and_the_decision_journaled() {
    if common::unsupported("a_regular_marker_is_unlinked_and_the_decision_journaled") {
        return;
    }
    let t = TreeBuilder::new();
    t.write("p/node_modules/.ddc-keep", "");
    t.file("p/node_modules/dep/index.js", 10);
    let folder = t.path("p/node_modules");
    let id = common::id(&folder);
    let mut journal = MemoryKeepJournal::default();
    let r = keep::unprotect(&target(folder.clone(), Some(id)), &[t.root().to_path_buf()], &mut journal).unwrap();
    assert_eq!(r.result, KeepResult::Removed);
    assert_eq!((r.dev, r.ino), (Some(id.0), Some(id.1)));
    assert!(!folder.join(".ddc-keep").exists() && folder.join("dep/index.js").exists(), "only the marker went");

    let line = serde_json::to_value(&journal.0[..]).unwrap();
    assert_eq!(journal.0.len(), 1);
    assert_eq!(line[0]["schema_version"], JOURNAL_SCHEMA_VERSION);
    assert_eq!(line[0]["kind"], "keep-remove");
    assert_eq!(line[0]["path"], folder.to_str().unwrap());
    assert_eq!((&line[0]["dev"], &line[0]["ino"]), (&id.0.into(), &id.1.into()));
    assert_eq!(line[0]["rule"], "node.node_modules");
    assert_eq!(line[0]["result"], "removed");
    assert!(line[0].get("class").is_none() && line[0].get("bytes").is_none(), "{line}");
    assert_eq!(serde_json::to_value(JournalKind::KeepRemove).unwrap(), "keep-remove");
}

/// A symlink above the folder cannot carry the open outside the roots, and
/// a folder that is now a file or a symlink is not the scan's.
#[test]
fn the_folder_is_judged_where_it_really_is() {
    if common::unsupported("the_folder_is_judged_where_it_really_is") {
        return;
    }
    let t = TreeBuilder::new();
    t.write("root/p/.ddc-keep", "");
    t.write("outside/q/.ddc-keep", "");
    t.symlink(t.path("outside"), "root/link");
    t.write("root/file", "");
    t.symlink(t.path("root/p"), "root/alias");
    let roots = [t.path("root")];
    let mut journal = MemoryKeepJournal::default();
    let mut run =
        |rel: &str, identity| keep::unprotect(&target(t.path(rel), identity), &roots, &mut journal).unwrap().result;
    let refused = |reason| KeepResult::Refused { reason };
    assert_eq!(run("root/link/q", None), refused(KeepRefusal::OutsideScanRoots));
    assert!(t.path("outside/q/.ddc-keep").exists());
    assert_eq!(run("root/alias", None), refused(KeepRefusal::ChangedSinceScan), "the folder is a symlink");
    assert!(t.path("root/p/.ddc-keep").exists());
    assert_eq!(run("root/file", Some((1, 2))), refused(KeepRefusal::ChangedSinceScan));
    assert_eq!(run("root/file", None), refused(KeepRefusal::NoFolder));
    assert_eq!(run("root/gone", None), refused(KeepRefusal::NoFolder));
    assert_eq!(journal.0.len(), 5, "every decision is journaled");
}

#[test]
fn the_keep_report_and_the_journal_have_their_own_versions() {
    assert_eq!(KEEP_SCHEMA_VERSION, 1);
    assert_eq!(JOURNAL_SCHEMA_VERSION, 4);
}

#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_keep_schema_is_current() {
    let schema = serde_json::to_string_pretty(&schemars::schema_for!(KeepReport)).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/keep.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &schema).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/keep.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}
