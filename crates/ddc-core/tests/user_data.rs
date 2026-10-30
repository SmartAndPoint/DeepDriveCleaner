//! Story "User data: pick your own files for the Trash": what the scan lists
//! in the user's own folders, and what `clean --items` does with a picked
//! entry. Temporary homes only; the Trash is always `FakeTrash`.

mod common;

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use common::fakes::{FakeCommands, FakeProcs, FakeTrash};
use common::{TreeBuilder, alloc, reference_alloc};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::preflight::{self, Prediction, PreflightLine, PreflightPorts};
use ddc_core::exec::report::CleanReport;
use ddc_core::exec::{self, Consent, ExecOptions, MemoryJournal, Outcome, Ports, SkipReason};
use ddc_core::model::{Contents, FolderEntry, FolderEntryKind, RemovalKind, ScanReport};
use ddc_core::plan::{ActionKind, ListError, ListedItem, ListedItems, PlanOptions, build_plan, plan_listed};
use ddc_core::platform::procs::ProcInfo;
use ddc_core::report::contents::{ContentsLimits, EntryIndex, EntryState};
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::rules::entries::{ENTRY_RULES, EntryLock};
use ddc_core::scan::{Progress, StdReader, WalkOptions};

const HOUR: Duration = Duration::from_secs(3_600);

const STATUS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>SnapshotState</key>
	<string>STATE</string>
</dict>
</plist>
"#;

const INFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>Device Name</key>
	<string>Example's iPhone</string>
	<key>Last Backup Date</key>
	<date>2026-10-01T12:30:45Z</date>
</dict>
</plist>
"#;

fn set_mtime(path: &Path, when: SystemTime) {
    File::options().write(true).open(path).unwrap().set_modified(when).unwrap();
}

struct Home {
    tree: TreeBuilder,
    data: tempfile::TempDir,
    /// The scan's start: an hour after the fixtures were written, so they
    /// are not "recent" unless a test makes them so.
    now: SystemTime,
    limits: ContentsLimits,
    excludes: Vec<PathBuf>,
}

impl Home {
    fn new() -> Self {
        Self {
            tree: TreeBuilder::new(),
            data: tempfile::tempdir().unwrap(),
            now: SystemTime::now() + HOUR,
            limits: ContentsLimits { max: 100, min_bytes: 1 },
            excludes: Vec::new(),
        }
    }

    fn env(&self) -> Env {
        Env::with_paths(self.tree.root(), self.data.path(), &self.data.path().join("logs"), self.now).unwrap()
    }

    fn scan(&self) -> (ScanReport, EntryIndex) {
        let opts = ScanOptions {
            excludes: self.excludes.clone(),
            contents: self.limits,
            walk: WalkOptions { threads: Some(4), ..WalkOptions::default() },
            ..ScanOptions::default()
        };
        report::scan_with_entries(&self.env(), &opts, &Progress::default(), &SystemStatFs).unwrap()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.tree.path(rel)
    }

    fn catalog(&self) -> Catalog {
        Catalog::builtin(self.tree.root())
    }

    /// An entry as the app lists it when picked: to the Trash, never permanent.
    fn pick(&self, rel: &str, rule: &str) -> ListedItem {
        let path = self.path(rel);
        let (dev, ino) = common::id(&path);
        ListedItem {
            path,
            dev,
            ino,
            rule: rule.to_owned(),
            class: RiskClass::UserData,
            action: ActionKind::Trash,
            permanent: false,
        }
    }

    fn trash(&self) -> PathBuf {
        self.data.path().join("FakeTrash")
    }

    /// Rescans, plans the list and runs it as `clean --items` does; the
    /// outcomes in list order, refused items included.
    fn clean(&self, items: &[ListedItem], procs: Vec<ProcInfo>) -> Vec<Outcome> {
        let (scanned, entries) = self.scan();
        let catalog = self.catalog();
        let listed = plan_listed(&scanned, &entries, &catalog, items);
        let mut journal = MemoryJournal::default();
        let run = exec::execute(
            &self.env(),
            &catalog,
            &listed.plan,
            &Consent::Listed,
            Ports {
                commands: &FakeCommands::default(),
                trash: &FakeTrash(self.trash()),
                procs: &FakeProcs::new(procs),
                journal: &mut journal,
            },
            &ExecOptions { cancel: &AtomicBool::new(false), ..ExecOptions::default() },
        )
        .unwrap();
        let report = CleanReport::new(items, &listed.refused, &run, PathBuf::from("/journal"));
        report.items.into_iter().map(|i| i.outcome).collect()
    }

    fn dry_run(&self, items: &[ListedItem], procs: Vec<ProcInfo>) -> Vec<Prediction> {
        let mut out = Vec::new();
        let ports =
            PreflightPorts { commands: &FakeCommands::default(), procs: &FakeProcs::new(procs), reader: &StdReader };
        preflight::preflight(&self.env(), &self.catalog(), &[], &self.excludes, items, &ports, &mut |line| {
            if let PreflightLine::Item(i) = line {
                out.push(i.prediction.clone());
            }
            Ok(())
        })
        .unwrap();
        out
    }

    /// The dry run, then the clean, over the same tree: each item's
    /// prediction must be its outcome.
    fn predict_and_clean(&self, items: &[ListedItem], procs: Vec<ProcInfo>) -> Vec<Outcome> {
        let predicted = self.dry_run(items, procs.clone());
        let outcomes = self.clean(items, procs);
        for ((item, p), o) in items.iter().zip(&predicted).zip(&outcomes) {
            match (p, o) {
                (Prediction::WouldTrash, Outcome::Trashed) | (Prediction::WouldDelete, Outcome::Deleted) => {}
                (Prediction::WouldSkip(a), Outcome::Skipped(b)) if a == b => {}
                _ => panic!("{}: the dry run said {p:?}, the clean did {o:?}", item.path.display()),
            }
        }
        outcomes
    }
}

fn skipped(reason: SkipReason) -> Outcome {
    Outcome::Skipped(reason)
}

fn kept() -> Outcome {
    skipped(SkipReason::EntryLocked { lock: EntryLock::Kept })
}

/// A finding of the scan as the app lists it.
fn finding_item(r: &ScanReport, path: &Path) -> ListedItem {
    let f = r.findings.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("no finding at {path:?}"));
    let action = match f.removal {
        RemovalKind::Delete | RemovalKind::None => ActionKind::Delete,
        RemovalKind::Trash => ActionKind::Trash,
        RemovalKind::Native => ActionKind::Native,
        RemovalKind::NativeOnly => ActionKind::NativeOnly,
    };
    ListedItem {
        path: f.path.clone(),
        dev: f.dev,
        ino: f.ino,
        rule: f.rule.clone(),
        class: f.class,
        action,
        permanent: false,
    }
}

fn contents<'a>(r: &'a ScanReport, folder: &Path) -> &'a Contents {
    let f = r.findings.iter().find(|f| f.path == folder).unwrap_or_else(|| panic!("no finding {folder:?}"));
    f.contents.as_ref().unwrap_or_else(|| panic!("{folder:?} ({}) lists no contents", f.rule))
}

fn listed<'a>(r: &'a ScanReport, path: &Path) -> &'a FolderEntry {
    let folder = path.parent().unwrap();
    contents(r, folder).entries.iter().find(|e| e.path == path).unwrap_or_else(|| panic!("{path:?} is not listed"))
}

fn rule_of(r: &ScanReport, path: &Path) -> Option<String> {
    r.findings.iter().find(|f| f.path == path).map(|f| f.rule.clone())
}

#[test]
fn the_scan_lists_entries_largest_first_with_the_cap_and_the_floor_and_sums_the_rest() {
    if common::unsupported("the_scan_lists_entries_largest_first_with_the_cap_and_the_floor_and_sums_the_rest") {
        return;
    }
    let mut h = Home::new();
    h.limits = ContentsLimits { max: 2, min_bytes: 50_000 };
    let t = &h.tree;
    for (name, len) in [("a.iso", 400_000), ("b.zip", 300_000), ("c.dmg", 200_000), ("tiny.txt", 30_000)] {
        t.file(&format!("Downloads/{name}"), len);
    }
    t.file("Downloads/folder/inside", 100_000);
    t.symlink("a.iso", "Downloads/link");
    t.file("Pictures/Trip/one.jpg", 70_000);
    t.file("Movies/clip.mov", 90_000);

    let (r, index) = h.scan();

    let d = contents(&r, &h.path("Downloads"));
    let names: Vec<_> = d.entries.iter().map(|e| e.path.file_name().unwrap().to_owned()).collect();
    assert_eq!(names, ["a.iso", "b.zip"]);
    assert_eq!(d.entries[0].bytes, alloc(&h.path("Downloads/a.iso")));
    assert_eq!(d.entries[0].kind, FolderEntryKind::File);
    assert!(d.entries[0].modified.is_some() && d.entries[0].lock.is_none());
    // c.dmg and folder are over the floor but past the cap; tiny under it; the symlink is no entry.
    assert_eq!(d.rest_count, 4);
    let rest: u64 = ["c.dmg", "tiny.txt", "folder", "link"]
        .iter()
        .map(|n| reference_alloc_or_file(&h.path(&format!("Downloads/{n}"))))
        .sum();
    assert_eq!(d.rest_bytes, rest);
    let link = &index.folders[&h.path("Downloads")].entries[std::ffi::OsStr::new("link")];
    assert_eq!(link.state, EntryState::NotAnEntry(ddc_core::report::contents::NotAnEntry::Symlink));
    // The full list knows every entry, listed or not.
    let c = &index.folders[&h.path("Downloads")].entries[std::ffi::OsStr::new("c.dmg")];
    assert_eq!(c.state, EntryState::Pickable);
    assert_eq!(listed(&r, &h.path("Pictures/Trip")).kind, FolderEntryKind::Folder);
    assert_eq!(listed(&r, &h.path("Pictures/Trip")).bytes, reference_alloc(&h.path("Pictures/Trip")));
    assert!(listed(&r, &h.path("Movies/clip.mov")).lock.is_none());
}

fn reference_alloc_or_file(path: &Path) -> u64 {
    let md = std::fs::symlink_metadata(path).unwrap();
    if md.is_dir() { reference_alloc(path) } else { alloc(path) }
}

#[test]
fn the_default_limits_list_a_hundred_entries_of_at_least_ten_mebibytes() {
    if common::unsupported("the_default_limits_list_a_hundred_entries_of_at_least_ten_mebibytes") {
        return;
    }
    let h = Home { limits: ContentsLimits::default(), ..Home::new() };
    let t = &h.tree;
    let first = t.file("Downloads/c000.bin", 10 << 20);
    for i in 1..=101 {
        t.clone_file(&first, &format!("Downloads/c{i:03}.bin"));
    }
    t.file("Downloads/small.bin", (10 << 20) - 64 * 1024);

    let (r, _) = h.scan();

    let d = contents(&r, &h.path("Downloads"));
    assert_eq!(d.entries.len(), 100);
    assert!(d.entries.iter().all(|e| e.bytes >= 10 << 20));
    assert_eq!(d.entries[0].path, h.path("Downloads/c000.bin"), "ties by path");
    assert_eq!(d.rest_count, 3);
}

#[test]
fn locked_entries_are_listed_with_their_reason() {
    if common::unsupported("locked_entries_are_listed_with_their_reason") {
        return;
    }
    let mut h = Home::new();
    let t = &h.tree;
    t.file("Pictures/Photos Library.photoslibrary/database/Photos.sqlite", 4_000);
    t.file("Pictures/Kept/inner/a.jpg", 4_000);
    t.write("Pictures/Kept/inner/.ddc-keep", "");
    t.file("Pictures/Free/a.jpg", 4_000);
    t.file("Music/Music/Media/song.m4a", 4_000);
    t.file("Downloads/big.iso.part", 4_000);
    t.file("Downloads/new.zip", 4_000);
    t.file("Downloads/future.mov", 4_000);
    t.file("Downloads/excluded.bin", 4_000);
    t.file("Downloads/deep/sub/x", 4_000);
    t.write("Downloads/deep/sub/.ddc-keep", "");
    t.file("Downloads/topkept/x", 4_000);
    t.write("Downloads/topkept/.ddc-keep", "");
    t.file("Downloads/exdir/x", 4_000);
    set_mtime(&h.path("Downloads/new.zip"), h.now - Duration::from_secs(60));
    set_mtime(&h.path("Downloads/future.mov"), h.now + 24 * HOUR);
    h.excludes = vec![h.path("Downloads/excluded.bin"), h.path("Downloads/exdir")];

    let (r, _) = h.scan();

    let lock = |rel: &str| listed(&r, &h.path(rel)).lock.clone();
    assert_eq!(
        rule_of(&r, &h.path("Pictures")).as_deref(),
        Some("user.pictures"),
        "a marker inside an entry keeps the folder"
    );
    assert_eq!(lock("Pictures/Photos Library.photoslibrary"), Some(EntryLock::AppManaged { app: "Photos".to_owned() }));
    assert_eq!(lock("Pictures/Kept"), Some(EntryLock::Kept));
    assert_eq!(lock("Pictures/Free"), None);
    assert_eq!(lock("Music/Music"), Some(EntryLock::AppManaged { app: "Music".to_owned() }));
    assert_eq!(lock("Downloads/big.iso.part"), Some(EntryLock::PartialDownload));
    assert_eq!(lock("Downloads/new.zip"), Some(EntryLock::Recent));
    assert_eq!(lock("Downloads/future.mov"), None, "a day ahead is a wrong clock");
    assert_eq!(lock("Downloads/excluded.bin"), Some(EntryLock::Kept), "an excluded file is locked");
    assert_eq!(lock("Downloads/deep"), Some(EntryLock::Kept), "a marker deeper inside");
    // A child folder with a marker at its top, or excluded, is its own finding.
    for rel in ["Downloads/topkept", "Downloads/exdir"] {
        assert_eq!(rule_of(&r, &h.path(rel)).as_deref(), Some("user.kept"), "{rel}");
        assert!(contents(&r, &h.path("Downloads")).entries.iter().all(|e| e.path != h.path(rel)));
    }
}

#[test]
fn a_fresh_entry_is_recent_against_the_scans_start() {
    if common::unsupported("a_fresh_entry_is_recent_against_the_scans_start") {
        return;
    }
    let mut h = Home::new();
    h.now = SystemTime::now();
    h.tree.file("Downloads/fresh.zip", 4_000);

    let (r, index) = h.scan();

    assert_eq!(listed(&r, &h.path("Downloads/fresh.zip")).lock, Some(EntryLock::Recent));
    let e = &index.folders[&h.path("Downloads")].entries[std::ffi::OsStr::new("fresh.zip")];
    assert_eq!(e.state, EntryState::Locked(EntryLock::Recent));
}

#[test]
fn a_marker_at_the_folders_top_or_an_exclude_inside_still_keeps_the_whole_folder() {
    if common::unsupported("a_marker_at_the_folders_top_or_an_exclude_inside_still_keeps_the_whole_folder") {
        return;
    }
    let h = Home::new();
    h.tree.file("Pictures/Trip/a.jpg", 4_000);
    h.tree.write("Pictures/.ddc-keep", "");
    let (r, _) = h.scan();
    assert_eq!(rule_of(&r, &h.path("Pictures")).as_deref(), Some("user.kept"));
    assert!(r.findings.iter().all(|f| f.contents.is_none()));

    let mut h = Home::new();
    h.tree.file("Movies/Trip/a.mov", 4_000);
    h.tree.file("Movies/Other/b.mov", 4_000);
    h.excludes = vec![h.path("Movies/Trip")];
    let (r, _) = h.scan();
    assert_eq!(rule_of(&r, &h.path("Movies")).as_deref(), Some("user.kept"));
    assert!(r.findings.iter().all(|f| f.contents.is_none()));
}

#[test]
fn a_downloads_folder_holds_findings_with_their_size_and_an_app_is_an_ordinary_entry() {
    if common::unsupported("a_downloads_folder_holds_findings_with_their_size_and_an_app_is_an_ordinary_entry") {
        return;
    }
    let h = Home::new();
    let t = &h.tree;
    t.write("Downloads/repo/package.json", "{}");
    t.write("Downloads/repo/package-lock.json", "{}");
    t.file("Downloads/repo/node_modules/x/index.js", 50_000);
    t.file("Downloads/repo/src/main.js", 4_000);
    t.dir("Downloads/repo/.git");
    t.file("Downloads/Tool.app/Contents/MacOS/tool", 80_000);

    let (r, _) = h.scan();

    assert_eq!(rule_of(&r, &h.path("Downloads/repo/node_modules")).as_deref(), Some("node.node_modules"));
    let repo = listed(&r, &h.path("Downloads/repo"));
    assert_eq!(repo.bytes, reference_alloc(&h.path("Downloads/repo")), "the finding inside is folded back");
    assert_eq!(repo.findings, 1);
    assert!(repo.git && repo.lock.is_none());
    let app = listed(&r, &h.path("Downloads/Tool.app"));
    assert_eq!(app.bytes, reference_alloc(&h.path("Downloads/Tool.app")));
    assert_eq!((app.findings, app.lock.clone()), (0, None));
    // The Downloads finding itself is still the remainder.
    let downloads = r.findings.iter().find(|f| f.path == h.path("Downloads")).unwrap();
    assert!(downloads.bytes < reference_alloc(&h.path("Downloads")));
}

#[test]
fn backups_show_their_device_and_lock_until_they_finished() {
    if common::unsupported("backups_show_their_device_and_lock_until_they_finished") {
        return;
    }
    let h = Home::new();
    let t = &h.tree;
    let backups = "Library/Application Support/MobileSync/Backup";
    t.write(&format!("{backups}/done/Status.plist"), &STATUS.replace("STATE", "finished"));
    t.write(&format!("{backups}/done/Info.plist"), INFO);
    t.file(&format!("{backups}/done/Manifest.db"), 8_000);
    t.write(&format!("{backups}/busy/Status.plist"), &STATUS.replace("STATE", "new"));
    t.file(&format!("{backups}/bare/Manifest.db"), 8_000);

    let (r, _) = h.scan();

    let done = listed(&r, &h.path(&format!("{backups}/done")));
    assert_eq!(done.lock, None);
    let b = done.backup.as_ref().unwrap();
    assert_eq!(b.device_name.as_deref(), Some("Example's iPhone"));
    assert_eq!(b.last_backup.as_deref(), Some("2026-10-01T12:30:45Z"));
    for name in ["busy", "bare"] {
        assert_eq!(listed(&r, &h.path(&format!("{backups}/{name}"))).lock, Some(EntryLock::UnfinishedBackup), "{name}");
    }
}

#[test]
fn messengers_imazing_kept_and_the_trash_list_nothing_and_cloud_folders_stay_closed() {
    if common::unsupported("messengers_imazing_kept_and_the_trash_list_nothing_and_cloud_folders_stay_closed") {
        return;
    }
    let h = Home::new();
    let t = &h.tree;
    t.file("Library/Group Containers/6N38VWS5BX.ru.keepcoder.Telegram/media/a", 8_000);
    t.file("Library/Group Containers/group.net.whatsapp.WhatsApp.shared/Media/a", 8_000);
    t.file("Library/Application Support/iMazing/Backups/0001/Manifest.db", 8_000);
    t.file(".Trash/old.zip", 8_000);
    t.file("Documents/keep/a", 8_000);
    t.write("Documents/keep/.ddc-keep", "");
    t.file("Library/CloudStorage/GoogleDrive-x/doc", 8_000);
    t.file("Library/Mobile Documents/com~apple~CloudDocs/doc", 8_000);

    let (r, index) = h.scan();

    let rules: Vec<&str> = r.findings.iter().map(|f| f.rule.as_str()).collect();
    for rule in ["user.telegram", "user.whatsapp", "user.imazing-backups", "trash", "user.kept"] {
        assert!(rules.contains(&rule), "{rule} is found: {rules:?}");
    }
    for f in &r.findings {
        assert!(f.contents.is_none(), "{} ({}) lists contents", f.path.display(), f.rule);
    }
    assert!(index.folders.is_empty());
    for cloud in ["Library/CloudStorage", "Library/Mobile Documents"] {
        assert!(r.skipped.iter().any(|s| s.path == h.path(cloud)), "{cloud} is only in skipped");
        assert!(r.findings.iter().all(|f| !f.path.starts_with(h.path(cloud))));
    }
}

#[test]
fn an_entry_with_an_unreadable_part_is_listed_partly_unreadable_and_stays_pickable() {
    if common::unsupported("an_entry_with_an_unreadable_part_is_listed_partly_unreadable_and_stays_pickable") {
        return;
    }
    let h = Home::new();
    h.tree.file("Downloads/archive/readable", 8_000);
    h.tree.file("Downloads/archive/closed/secret", 8_000);
    h.tree.lock("Downloads/archive/closed");

    let (r, index) = h.scan();

    let e = listed(&r, &h.path("Downloads/archive"));
    assert!(e.incomplete && e.lock.is_none());
    let indexed = &index.folders[&h.path("Downloads")].entries[std::ffi::OsStr::new("archive")];
    assert_eq!(indexed.state, EntryState::Pickable);
}

#[test]
fn two_scans_list_the_same_contents() {
    if common::unsupported("two_scans_list_the_same_contents") {
        return;
    }
    let h = Home::new();
    for i in 0..30 {
        h.tree.file(&format!("Downloads/d{i}/f"), 1_000 * (i % 7 + 1));
        h.tree.file(&format!("Downloads/f{i}"), 2_000 * (i % 5 + 1));
    }
    let strip = |mut r: ScanReport| {
        r.duration_ms = 0;
        r.findings.into_iter().filter_map(|f| f.contents).collect::<Vec<_>>()
    };
    assert_eq!(strip(h.scan().0), strip(h.scan().0));
}

#[test]
fn a_picked_file_and_folder_go_to_the_trash() {
    if common::unsupported("a_picked_file_and_folder_go_to_the_trash") {
        return;
    }
    let h = Home::new();
    h.tree.file("Downloads/report.pdf", 40_000);
    h.tree.file("Downloads/Album/song.mp3", 60_000);
    h.tree.file("Pictures/Trip/one.jpg", 70_000);
    let items = [
        h.pick("Downloads/report.pdf", "user.downloads"),
        h.pick("Downloads/Album", "user.downloads"),
        h.pick("Pictures/Trip", "user.pictures"),
    ];

    assert_eq!(h.predict_and_clean(&items, vec![]), [Outcome::Trashed, Outcome::Trashed, Outcome::Trashed]);
    for name in ["report.pdf", "Album", "Trip"] {
        assert!(h.trash().join(name).exists(), "{name} is in the Trash");
    }
    assert!(!h.path("Downloads/report.pdf").exists() && h.path("Downloads").is_dir());
}

#[test]
fn a_symlink_a_replaced_entry_a_locked_one_and_the_folder_itself_are_skipped() {
    if common::unsupported("a_symlink_a_replaced_entry_a_locked_one_and_the_folder_itself_are_skipped") {
        return;
    }
    let h = Home::new();
    h.tree.file("Downloads/a.iso", 40_000);
    h.tree.symlink("a.iso", "Downloads/link");
    h.tree.file("Downloads/old.zip", 30_000);
    h.tree.file("Downloads/Project/sub/x.bin", 20_000);
    h.tree.file("Pictures/Trip/one.jpg", 70_000);
    let items = [
        h.pick("Downloads/link", "user.downloads"),
        h.pick("Downloads/old.zip", "user.downloads"),
        h.pick("Downloads/Project", "user.downloads"),
        h.pick("Pictures/Trip", "user.pictures"),
        h.pick("Downloads", "user.downloads"),
    ];
    // After the review: a new file under the old name, and markers put in.
    fs::rename(h.path("Downloads/old.zip"), h.path("old.zip.bak")).unwrap();
    h.tree.file("Downloads/old.zip", 30_000);
    h.tree.write("Downloads/Project/sub/.ddc-keep", "");
    h.tree.write("Pictures/Trip/.ddc-keep", "");

    assert_eq!(
        h.predict_and_clean(&items, vec![]),
        [
            skipped(SkipReason::Symlink),
            skipped(SkipReason::ChangedSinceScan),
            kept(),
            kept(),
            skipped(SkipReason::ActionNotAllowed),
        ]
    );
    assert!(h.path("Downloads/old.zip").exists() && h.path("Pictures/Trip/one.jpg").exists());
    assert!(!h.trash().exists(), "nothing went to the Trash");
}

#[test]
fn an_exclude_locks_a_downloads_entry_and_keeps_a_folder_claimed_whole() {
    if common::unsupported("an_exclude_locks_a_downloads_entry_and_keeps_a_folder_claimed_whole") {
        return;
    }
    let mut h = Home::new();
    h.tree.file("Downloads/Kept/a.bin", 20_000);
    h.tree.file("Downloads/Other/notes.txt", 20_000);
    h.tree.file("Pictures/Trip/one.jpg", 70_000);
    h.excludes = vec![h.path("Downloads/Kept"), h.path("Downloads/Other/notes.txt"), h.path("Pictures/Trip/one.jpg")];
    let items = [
        h.pick("Downloads/Kept", "user.downloads"),
        h.pick("Downloads/Other", "user.downloads"),
        h.pick("Pictures/Trip", "user.pictures"),
    ];

    // In Downloads the entry is locked; Pictures, which the scan claims
    // whole, is all kept by an exclude anywhere inside.
    assert_eq!(h.predict_and_clean(&items, vec![]), [kept(), kept(), skipped(SkipReason::Kept)]);
    assert!(!h.trash().exists());
}

#[test]
fn a_path_that_is_no_entry_is_skipped_and_one_beyond_the_listed_hundred_is_not() {
    if common::unsupported("a_path_that_is_no_entry_is_skipped_and_one_beyond_the_listed_hundred_is_not") {
        return;
    }
    let mut h = Home::new();
    h.limits = ContentsLimits { max: 1, min_bytes: 1 };
    h.tree.file("Downloads/big.iso", 90_000);
    h.tree.file("Downloads/small.zip", 20_000);
    h.tree.file("Downloads/Album/song.mp3", 60_000);
    assert_eq!(contents(&h.scan().0, &h.path("Downloads")).entries.len(), 1, "small.zip is not listed");
    let items = [h.pick("Downloads/Album/song.mp3", "user.downloads"), h.pick("Downloads/small.zip", "user.downloads")];

    assert_eq!(h.predict_and_clean(&items, vec![]), [skipped(SkipReason::NotAnEntry), Outcome::Trashed]);
    assert!(h.path("Downloads/Album/song.mp3").exists());
}

#[test]
fn another_rules_finding_in_downloads_is_no_entry_in_the_clean() {
    if common::unsupported("another_rules_finding_in_downloads_is_no_entry_in_the_clean") {
        return;
    }
    let h = Home::new();
    h.tree.write("Downloads/package.json", "{}");
    h.tree.file("Downloads/node_modules/dep/index.js", 40_000);
    let (r, _) = h.scan();
    assert_ne!(rule_of(&r, &h.path("Downloads/node_modules")).as_deref(), Some("user.downloads"));

    // The dry run has no scan to see it; the clean's rescan does.
    let items = [h.pick("Downloads/node_modules", "user.downloads")];
    assert_eq!(h.clean(&items, vec![]), [skipped(SkipReason::NotAnEntry)]);
    assert!(h.path("Downloads/node_modules/dep/index.js").exists());
}

#[test]
fn delete_and_permanent_are_refused_for_a_picked_entry() {
    if common::unsupported("delete_and_permanent_are_refused_for_a_picked_entry") {
        return;
    }
    let h = Home::new();
    h.tree.file("Downloads/report.pdf", 40_000);
    let mut delete = h.pick("Downloads/report.pdf", "user.downloads");
    delete.action = ActionKind::Delete;

    assert_eq!(h.predict_and_clean(&[delete], vec![]), [skipped(SkipReason::ActionNotAllowed)]);
    assert!(h.path("Downloads/report.pdf").exists());

    let mut permanent = serde_json::to_value(h.pick("Downloads/report.pdf", "user.downloads")).unwrap();
    permanent["permanent"] = true.into();
    let list = serde_json::json!({ "schema_version": 1, "items": [permanent] }).to_string();
    assert!(matches!(ListedItems::parse(list.as_bytes()), Err(ListError::Permanent(_))));
}

#[test]
fn a_finding_inside_a_trashed_entry_is_skipped_and_runs_when_the_entry_was_skipped() {
    if common::unsupported("a_finding_inside_a_trashed_entry_is_skipped_and_runs_when_the_entry_was_skipped") {
        return;
    }
    let build = |h: &Home| {
        h.tree.write("Downloads/Project/package.json", "{}");
        h.tree.file("Downloads/Project/node_modules/dep/index.js", 40_000);
        let (r, _) = h.scan();
        let inner = finding_item(&r, &h.path("Downloads/Project/node_modules"));
        [h.pick("Downloads/Project", "user.downloads"), inner]
    };

    let h = Home::new();
    let items = build(&h);
    assert_eq!(h.predict_and_clean(&items, vec![]), [Outcome::Trashed, skipped(SkipReason::InsideAnotherItem)]);
    assert!(h.trash().join("Project/node_modules/dep/index.js").exists(), "Put Back brings it back whole");

    // A shell working in the folder: the entry is in use, the finding inside still runs.
    let h = Home::new();
    let items = build(&h);
    let shell = ProcInfo { pid: 7, name: "zsh".into(), cwd: Some(h.path("Downloads/Project")), ..ProcInfo::default() };
    let outcomes = h.predict_and_clean(&items, vec![shell]);
    assert_eq!(outcomes[0], skipped(SkipReason::InUse { pid: 7, name: "zsh".into() }));
    assert!(matches!(outcomes[1], Outcome::Trashed | Outcome::Deleted), "{:?}", outcomes[1]);
    assert!(h.path("Downloads/Project/package.json").exists());
}

#[test]
fn a_symlink_above_the_folder_or_a_path_outside_the_roots_is_skipped_as_for_any_item() {
    if common::unsupported("a_symlink_above_the_folder_or_a_path_outside_the_roots_is_skipped_as_for_any_item") {
        return;
    }
    let h = Home::new();
    h.tree.file("Downloads/report.pdf", 40_000);
    h.tree.symlink(h.tree.root(), "alias");
    let outside = tempfile::tempdir().unwrap();
    let outside = outside.path().canonicalize().unwrap();
    fs::create_dir(outside.join("Downloads")).unwrap();
    fs::write(outside.join("Downloads/x.pdf"), [1u8; 100]).unwrap();
    let mut through_alias = h.pick("Downloads/report.pdf", "user.downloads");
    through_alias.path = h.path("alias/Downloads/report.pdf");
    let mut elsewhere = h.pick("Downloads/report.pdf", "user.downloads");
    elsewhere.path = outside.join("Downloads/x.pdf");

    assert_eq!(
        h.predict_and_clean(&[through_alias, elsewhere], vec![]),
        [skipped(SkipReason::ParentChanged), skipped(SkipReason::OutsideScanRoots)]
    );
    assert!(h.path("Downloads/report.pdf").exists() && outside.join("Downloads/x.pdf").exists());
}

#[test]
fn a_plan_never_holds_an_entry_and_yes_touches_none() {
    if common::unsupported("a_plan_never_holds_an_entry_and_yes_touches_none") {
        return;
    }
    let h = Home::new();
    h.tree.file("Downloads/report.pdf", 40_000);
    h.tree.file("Pictures/Trip/one.jpg", 70_000);
    let (r, _) = h.scan();
    assert_eq!(contents(&r, &h.path("Downloads")).entries.len(), 1);
    let catalog = h.catalog();
    let all = vec![RiskClass::Safe, RiskClass::Rebuildable, RiskClass::Review, RiskClass::UserData];
    let plan = build_plan(&r, &catalog, &PlanOptions { classes: all, ..PlanOptions::default() });
    assert!(
        plan.items
            .iter()
            .all(|i| i.finding.class != RiskClass::UserData && !ENTRY_RULES.contains(&i.finding.rule.as_str())),
        "{:?}",
        plan.items.iter().map(|i| &i.finding.path).collect::<Vec<_>>()
    );

    let mut journal = MemoryJournal::default();
    exec::execute(
        &h.env(),
        &catalog,
        &plan,
        // `--yes` refuses `review`; every class it accepts.
        &Consent::AssumeYes { classes: vec![RiskClass::Safe, RiskClass::Rebuildable, RiskClass::UserData] },
        Ports {
            commands: &FakeCommands::default(),
            trash: &FakeTrash(h.trash()),
            procs: &FakeProcs::new(vec![]),
            journal: &mut journal,
        },
        &ExecOptions { cancel: &AtomicBool::new(false), ..ExecOptions::default() },
    )
    .unwrap();
    assert!(h.path("Downloads/report.pdf").exists() && h.path("Pictures/Trip/one.jpg").exists());
    assert!(!h.trash().exists());
}

#[test]
fn the_executor_derives_an_entrys_lock_again_right_before_the_trash() {
    if common::unsupported("the_executor_derives_an_entrys_lock_again_right_before_the_trash") {
        return;
    }
    let h = Home::new();
    h.tree.file("Downloads/a.iso", 40_000);
    h.tree.file("Downloads/Project/sub/x.bin", 20_000);
    h.tree.file("Downloads/fine.zip", 20_000);
    let items = [
        h.pick("Downloads/a.iso", "user.downloads"),
        h.pick("Downloads/Project", "user.downloads"),
        h.pick("Downloads/fine.zip", "user.downloads"),
    ];
    let (scanned, entries) = h.scan();
    let catalog = h.catalog();
    let listed = plan_listed(&scanned, &entries, &catalog, &items);
    assert!(listed.refused.is_empty() && listed.plan.items.len() == 3);

    // After the plan, before the action: one file changes now, a folder gets a marker deep inside.
    let minute_ago = h.now - Duration::from_secs(60);
    ddc_core::platform::fsops::set_times(&h.path("Downloads/a.iso"), minute_ago, minute_ago).unwrap();
    h.tree.write("Downloads/Project/sub/.ddc-keep", "");
    let mut journal = MemoryJournal::default();
    let run = exec::execute(
        &h.env(),
        &catalog,
        &listed.plan,
        &Consent::Listed,
        Ports {
            commands: &FakeCommands::default(),
            trash: &FakeTrash(h.trash()),
            procs: &FakeProcs::new(vec![]),
            journal: &mut journal,
        },
        &ExecOptions { cancel: &AtomicBool::new(false), ..ExecOptions::default() },
    )
    .unwrap();

    let outcome = |rel: &str| run.items.iter().find(|i| i.path == h.path(rel)).map(|i| i.outcome.clone());
    assert_eq!(outcome("Downloads/a.iso"), Some(skipped(SkipReason::EntryLocked { lock: EntryLock::Recent })));
    assert_eq!(outcome("Downloads/Project"), Some(kept()));
    assert_eq!(outcome("Downloads/fine.zip"), Some(Outcome::Trashed));
    assert!(h.path("Downloads/a.iso").exists() && h.path("Downloads/Project/sub/x.bin").exists());
}
