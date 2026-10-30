//! Story "Faster clean: re-check only what the selection needs": the pruned
//! re-check opens only what its list needs (AC 1), a list with a `Tree` item
//! takes the full walk (AC 2), and for every rule of the catalog and every
//! change after the scan, the pruned and the full re-check plan the listed
//! item alike (AC 4, AC 5). Temporary homes only.

mod common;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use common::TreeBuilder;
use common::fixtures::{TAG, rule_fixture};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::SkipReason;
use ddc_core::model::{RemovalKind, ScanReport};
use ddc_core::plan::{ActionKind, ListedItem, ListedPlan, action_for, plan_listed};
use ddc_core::report::contents::ContentsLimits;
use ddc_core::report::recheck::{self, Full, RecheckError, Walk, walk_for};
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::{Catalog, RULES, Rule, Scope};
use ddc_core::scan::{DirListing, DirReader, Progress, ReadError, StdReader, Times, WalkOptions};

const HOUR: Duration = Duration::from_secs(3_600);

struct Home {
    t: TreeBuilder,
    data: tempfile::TempDir,
    /// An hour after the fixtures were written: entries are not "recent".
    now: SystemTime,
    excludes: Vec<PathBuf>,
    /// The reviewed scan's roots; the re-checks always walk the home.
    review_roots: Vec<PathBuf>,
}

impl Home {
    fn new() -> Self {
        Self {
            t: TreeBuilder::new(),
            data: tempfile::tempdir().unwrap(),
            now: SystemTime::now() + HOUR,
            excludes: Vec::new(),
            review_roots: Vec::new(),
        }
    }

    fn env(&self) -> Env {
        Env::with_paths(self.t.root(), self.data.path(), &self.data.path().join("logs"), self.now).unwrap()
    }

    fn catalog(&self) -> Catalog {
        Catalog::builtin(self.t.root())
    }

    fn opts(&self) -> ScanOptions {
        ScanOptions {
            excludes: self.excludes.clone(),
            contents: ContentsLimits { max: 100, min_bytes: 1 },
            walk: WalkOptions { threads: Some(2), ..WalkOptions::default() },
            ..ScanOptions::default()
        }
    }

    /// Trees the list has nothing to do with; their sizes do not matter, so
    /// they are written without the fixtures' sync.
    fn unrelated(&self) {
        for i in 0..12 {
            self.t.write(&format!("big/tree{i}/deep/er/f.bin"), "x");
        }
        self.t.write("big/x/__pycache__/m.pyc", "x");
        self.t.write("Library/Caches/Unrelated/data/blob", "x");
    }

    fn review(&self) -> ScanReport {
        let opts = ScanOptions { roots: self.review_roots.clone(), ..self.opts() };
        report::scan(&self.env(), &opts, &Progress::default()).unwrap()
    }

    /// The finding at `rel` as the app lists it.
    fn listed(&self, reviewed: &ScanReport, rel: &str) -> ListedItem {
        let path = self.t.path(rel);
        let f = reviewed.findings.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("no finding at {rel}"));
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

    /// An entry of the user's own folders, as the app lists it when picked.
    fn pick(&self, rel: &str, rule: &str) -> ListedItem {
        let path = self.t.path(rel);
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

    fn full(&self, list: &[ListedItem]) -> ListedPlan {
        let (scanned, entries) =
            report::scan_with_entries(&self.env(), &self.opts(), &Progress::default(), &SystemStatFs).unwrap();
        plan_listed(&scanned, &entries, &self.catalog(), list)
    }

    fn pruned_with(&self, list: &[ListedItem], reader: Option<&dyn DirReader>) -> Result<ListedPlan, RecheckError> {
        let catalog = self.catalog();
        let (scanned, entries) =
            recheck::pruned(&self.env(), &self.opts(), &catalog, list, &Progress::default(), &SystemStatFs, reader)?;
        Ok(plan_listed(&scanned, &entries, &catalog, list))
    }

    /// Both re-checks; they must plan the list alike.
    fn agree(&self, list: &[ListedItem], what: &str) -> ListedPlan {
        let full = self.full(list);
        let pruned = self.pruned_with(list, None).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(pruned, full, "{what}: the pruned re-check differs from the full one");
        full
    }

    /// Moves `path` into a holding directory: a file or folder that disappears.
    fn take_away(&self, path: &Path) {
        let holding = self.data.path().join("holding");
        fs::create_dir_all(&holding).unwrap();
        fs::rename(path, holding.join(format!("{}-{}", path.file_name().unwrap().to_string_lossy(), rand()))).unwrap();
    }
}

fn rand() -> u128 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()
}

fn refused(plan: &ListedPlan) -> Vec<SkipReason> {
    plan.refused.iter().map(|r| r.reason.clone()).collect()
}

/// Every rule with a fixture that yields a finding, the `change` applied to
/// its item after the scan; then both re-checks plan it alike. A list with a
/// `Tree` item takes the full walk, which is then the one re-check.
fn each_rule(change: &str, apply: impl Fn(&Home, &Path, &'static Rule)) {
    for rule in RULES {
        let home = Home::new();
        let Some(rel) = rule_fixture(&home.t, rule) else { continue };
        home.unrelated();
        let reviewed = home.review();
        let item = home.listed(&reviewed, &rel);
        apply(&home, &item.path, rule);
        let what = format!("{}: {change}", rule.id);
        match walk_for(std::slice::from_ref(&item), &home.catalog(), Some(&reviewed)) {
            Walk::Pruned => {
                let plan = home.agree(&[item], &what);
                // Unchanged, an item with an action runs: a broken fixture
                // would make every change prove nothing.
                if change == "unchanged" && action_for(rule, rule.class, false).is_some() {
                    assert_eq!(plan.plan.items.len(), 1, "{what}: {:?}", refused(&plan));
                }
            }
            Walk::Full(full) => assert_eq!((rule.scope, full), (Scope::Tree, Full::Tree), "{what}"),
        }
    }
}

/// Same as [`each_rule`], with the change made through the home (excludes).
fn each_rule_mut(change: &str, apply: impl Fn(&mut Home, &Path)) {
    for rule in RULES {
        let mut home = Home::new();
        let Some(rel) = rule_fixture(&home.t, rule) else { continue };
        home.unrelated();
        let reviewed = home.review();
        let item = home.listed(&reviewed, &rel);
        apply(&mut home, &item.path);
        if walk_for(std::slice::from_ref(&item), &home.catalog(), Some(&reviewed)) == Walk::Pruned {
            home.agree(&[item], &format!("{}: {change}", rule.id));
        }
    }
}

// ---- AC 5: every rule, unchanged -------------------------------------------

#[test]
fn every_rule_unchanged_is_planned_alike_and_runs() {
    if common::unsupported("every_rule_unchanged_is_planned_alike_and_runs") {
        return;
    }
    each_rule("unchanged", |_, _, _| {});
}

// ---- AC 4: each change after the scan ---------------------------------------

#[test]
fn a_keep_marker_in_at_or_above_the_item() {
    if common::unsupported("a_keep_marker_in_at_or_above_the_item") {
        return;
    }
    each_rule("a keep marker inside", |home, item, _| {
        fs::create_dir_all(item.join("sub")).unwrap();
        fs::write(item.join("sub/.ddc-keep"), "").unwrap();
        let _ = home;
    });
    each_rule("a keep marker at it", |_, item, _| fs::write(item.join(".ddc-keep"), "").unwrap());
    each_rule("a keep marker above it", |_, item, _| fs::write(item.parent().unwrap().join(".ddc-keep"), "").unwrap());
}

#[test]
fn an_exclude_at_above_or_below_the_item() {
    if common::unsupported("an_exclude_at_above_or_below_the_item") {
        return;
    }
    each_rule_mut("an exclude at it", |home, item| home.excludes = vec![item.to_path_buf()]);
    each_rule_mut("an exclude above it", |home, item| home.excludes = vec![item.parent().unwrap().to_path_buf()]);
    each_rule_mut("an exclude below it", |home, item| {
        fs::create_dir_all(item.join("inner/x")).unwrap();
        fs::write(item.join("inner/x/f"), "x").unwrap();
        home.excludes = vec![item.join("inner")];
    });
}

#[test]
fn a_content_claim_appearing_above_the_item() {
    if common::unsupported("a_content_claim_appearing_above_the_item") {
        return;
    }
    each_rule("a CACHEDIR.TAG above it", |_, item, _| {
        fs::write(item.parent().unwrap().join("CACHEDIR.TAG"), TAG).unwrap()
    });
    each_rule("a pyvenv.cfg above it", |_, item, _| fs::write(item.parent().unwrap().join("pyvenv.cfg"), "").unwrap());
}

#[test]
fn a_lock_file_appearing_or_disappearing() {
    if common::unsupported("a_lock_file_appearing_or_disappearing") {
        return;
    }
    for rule in RULES.iter().filter(|r| r.lockfiles.is_some()) {
        let locks: Vec<&str> = rule.lockfiles.unwrap().iter().filter(|l| !l.contains('*')).copied().collect();
        for appears in [true, false] {
            let home = Home::new();
            let Some(rel) = rule_fixture(&home.t, rule) else { continue };
            home.unrelated();
            let reviewed = home.review();
            let item = home.listed(&reviewed, &rel);
            let parent = item.path.parent().unwrap();
            if appears {
                fs::write(parent.join(locks[0]), "").unwrap();
            } else {
                for lock in &locks {
                    if parent.join(lock).exists() {
                        home.take_away(&parent.join(lock));
                    }
                }
            }
            home.agree(&[item], &format!("{}: a lock file {}", rule.id, if appears { "appears" } else { "goes" }));
        }
    }
}

#[test]
fn the_item_swapped_a_manifest_inside_or_git_above() {
    if common::unsupported("the_item_swapped_a_manifest_inside_or_git_above") {
        return;
    }
    each_rule("the item swapped", |home, item, _| {
        home.take_away(item);
        fs::create_dir_all(item).unwrap();
        fs::write(item.join("f"), "new").unwrap();
    });
    each_rule("a manifest inside", |_, item, _| {
        fs::write(item.join("package.json"), "{}").unwrap();
        fs::write(item.join("Cargo.toml"), "").unwrap();
    });
    each_rule(".git above", |_, item, _| {
        fs::create_dir_all(item.parent().unwrap().join(".git")).unwrap();
        fs::write(item.parent().unwrap().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    });
}

/// Path matchers cannot appear above an existing item: the item is reviewed
/// from a narrower root, which does not see the claim, and re-checked from
/// the home, which does. Both re-checks refuse it.
#[test]
fn a_claim_above_the_item_for_each_matcher_kind() {
    if common::unsupported("a_claim_above_the_item_for_each_matcher_kind") {
        return;
    }
    // A matcher kind, the files that make the claim, and the root it sits at.
    type Case = (&'static str, &'static [(&'static str, &'static str)], &'static str);
    let cases: &[Case] = &[
        ("known paths", &[], ".cache/huggingface/p"),
        (
            "known children",
            &[("Library/Application Support/JetBrains/PyCharm2024.1/f", "")],
            "Library/Application Support/JetBrains/PyCharm2023.1/p",
        ),
        ("name with siblings", &[("w/package.json", "{}")], "w/node_modules/dep"),
        ("contains file", &[("v/.venv/pyvenv.cfg", "")], "v/.venv/lib"),
        ("CACHEDIR.TAG", &[("c/cache/CACHEDIR.TAG", TAG)], "c/cache/sub"),
        (".app suffix", &[], "A.app/Contents/p"),
    ];
    for (kind, files, root) in cases {
        let mut home = Home::new();
        for (rel, content) in *files {
            home.t.write(rel, content);
        }
        home.t.file(&format!("{root}/__pycache__/m.pyc"), 100);
        home.unrelated();
        home.review_roots = vec![home.t.path(root)];
        let reviewed = home.review();
        let item = home.listed(&reviewed, &format!("{root}/__pycache__"));
        home.review_roots.clear();
        let plan = home.agree(&[item], &format!("a {kind} claim above"));
        assert!(plan.plan.items.is_empty(), "{kind}: inside a claim the item is no finding: {:?}", refused(&plan));
    }
}

/// The one pair the plans may differ in (gate decision 1): a claim that
/// appeared above the item after the scan, with a keep marker in another of
/// its subtrees. The full walk reads the marker and keeps the claim whole;
/// the pruned walk leaves that subtree unread. Both refuse the item.
#[test]
fn a_new_claim_with_a_marker_elsewhere_refuses_the_item_for_another_reason() {
    if common::unsupported("a_new_claim_with_a_marker_elsewhere_refuses_the_item_for_another_reason") {
        return;
    }
    let home = Home::new();
    home.t.file("c/sub/__pycache__/m.pyc", 100);
    home.t.file("c/other/f.bin", 100);
    home.unrelated();
    let reviewed = home.review();
    let item = home.listed(&reviewed, "c/sub/__pycache__");
    home.t.write("c/CACHEDIR.TAG", TAG);
    home.t.write("c/other/.ddc-keep", "");
    let list = [item];
    assert_eq!(walk_for(&list, &home.catalog(), Some(&reviewed)), Walk::Pruned);
    assert_eq!(refused(&home.full(&list)), [SkipReason::Kept]);
    assert_eq!(refused(&home.pruned_with(&list, None).unwrap()), [SkipReason::ChangedSinceScan]);
}

#[test]
fn a_sibling_version_added_or_removed() {
    if common::unsupported("a_sibling_version_added_or_removed") {
        return;
    }
    let rule = RULES.iter().find(|r| r.scope == Scope::Siblings).unwrap();
    for (change, newest_goes) in [("a newer sibling added", false), ("the newer sibling removed", true)] {
        let home = Home::new();
        let rel = rule_fixture(&home.t, rule).unwrap();
        home.unrelated();
        let reviewed = home.review();
        let item = home.listed(&reviewed, &rel);
        let family = item.path.parent().unwrap();
        if newest_goes {
            home.take_away(&family.join("PyCharm2024.1"));
        } else {
            home.t.file("Library/Application Support/JetBrains/PyCharm2025.1/payload", 100);
        }
        let plan = home.agree(&[item], &format!("{}: {change}", rule.id));
        assert_eq!(plan.plan.items.is_empty(), newest_goes, "{change}: the newest version is kept");
    }
}

#[test]
fn a_picked_entry_that_became_a_finding_or_holds_a_keep_marker() {
    if common::unsupported("a_picked_entry_that_became_a_finding_or_holds_a_keep_marker") {
        return;
    }
    for (folder, rule) in [("Downloads", "user.downloads"), ("Pictures", "user.pictures")] {
        for change in ["a finding", "a keep marker", "unchanged"] {
            let home = Home::new();
            home.t.file(&format!("{folder}/picked/a/b.bin"), 3000);
            home.t.file(&format!("{folder}/other/c.bin"), 3000);
            home.unrelated();
            let _ = home.review();
            let item = home.pick(&format!("{folder}/picked"), rule);
            match change {
                "a finding" => drop(home.t.write(&format!("{folder}/picked/CACHEDIR.TAG"), TAG)),
                "a keep marker" => drop(home.t.write(&format!("{folder}/picked/a/.ddc-keep"), "")),
                _ => {}
            }
            let plan = home.agree(&[item], &format!("{folder}: {change}"));
            // Pictures is claimed whole: no rule applies inside, so a cache tag makes no finding there.
            let runs = change == "unchanged" || (folder == "Pictures" && change == "a finding");
            assert_eq!(plan.plan.items.len(), usize::from(runs), "{folder}: {change}: {:?}", refused(&plan));
        }
    }
}

// ---- AC 1: what the pruned re-check opens -----------------------------------

/// Records every directory a walk opens.
#[derive(Default)]
struct Counting {
    opened: Mutex<HashSet<PathBuf>>,
}

impl DirReader for Counting {
    fn read(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        want_private: bool,
        times: Times,
    ) -> Result<DirListing, ReadError> {
        self.opened.lock().unwrap().insert(dir.to_path_buf());
        StdReader.read(dir, expect, want_private, times)
    }
}

#[test]
fn the_pruned_recheck_opens_only_the_items_their_ancestors_and_a_siblings_family() {
    if common::unsupported("the_pruned_recheck_opens_only_the_items_their_ancestors_and_a_siblings_family") {
        return;
    }
    let home = Home::new();
    home.t.file("one/two/__pycache__/m.pyc", 100);
    home.t.file(".cache/huggingface/hub/model.bin", 100);
    let jetbrains = RULES.iter().find(|r| r.scope == Scope::Siblings).unwrap();
    let old = rule_fixture(&home.t, jetbrains).unwrap();
    home.t.file("Downloads/picked/a/b.bin", 3000);
    home.t.file("Downloads/other/deep/c.bin", 3000);
    home.t.file("Pictures/album1/p.jpg", 3000);
    home.t.file("Pictures/album2/deep/q.jpg", 3000);
    home.unrelated();
    let reviewed = home.review();
    let list = vec![
        home.listed(&reviewed, "one/two/__pycache__"),
        home.listed(&reviewed, ".cache/huggingface"),
        home.listed(&reviewed, &old),
        home.pick("Downloads/picked", "user.downloads"),
        home.pick("Pictures/album1", "user.pictures"),
    ];
    assert_eq!(walk_for(&list, &home.catalog(), Some(&reviewed)), Walk::Pruned);

    let reader = Counting::default();
    let pruned = home.pruned_with(&list, Some(&reader)).unwrap();
    assert_eq!(pruned, home.full(&list), "the pruned re-check plans the list as the full one");
    assert_eq!(pruned.plan.items.len(), list.len(), "every item runs: {:?}", refused(&pruned));

    let family = home.t.path(&old).parent().unwrap().to_path_buf();
    let opened = reader.opened.into_inner().unwrap();
    for dir in &opened {
        let above = list.iter().any(|i| i.path.starts_with(dir));
        let inside = list.iter().any(|i| dir.starts_with(&i.path)) || dir.starts_with(&family);
        assert!(above || inside, "opened {dir:?}, which the list does not need");
    }
    for unread in ["big", "big/tree3/deep", "Library/Caches/Unrelated", "Downloads/other", "Pictures/album2"] {
        assert!(!opened.contains(&home.t.path(unread)), "{unread} was opened");
    }
    assert!(opened.contains(&home.t.path("Library/Application Support/JetBrains/PyCharm2024.1")), "the family is read");
}

#[test]
fn an_unreadable_ancestor_sends_the_recheck_to_the_full_walk() {
    if common::unsupported("an_unreadable_ancestor_sends_the_recheck_to_the_full_walk") {
        return;
    }
    let home = Home::new();
    home.t.file("deep/a/b/__pycache__/m.pyc", 100);
    let reviewed = home.review();
    let item = home.listed(&reviewed, "deep/a/b/__pycache__");
    let locked = home.t.lock("deep/a");
    match home.pruned_with(&[item], None) {
        Err(RecheckError::UnreadableAncestor(path)) => assert_eq!(path, locked),
        other => panic!("expected an unreadable ancestor, got {other:?}"),
    }
}

// ---- AC 2: a `Tree` item takes the full walk --------------------------------

#[test]
fn a_toolchain_item_takes_the_full_walk_and_a_new_pin_protects_its_version() {
    if common::unsupported("a_toolchain_item_takes_the_full_walk_and_a_new_pin_protects_its_version") {
        return;
    }
    for rule in RULES.iter().filter(|r| r.scope == Scope::Tree) {
        let home = Home::new();
        let old = rule_fixture(&home.t, rule).unwrap();
        let reviewed = home.review();
        let item = home.listed(&reviewed, &old);
        assert_eq!(walk_for(std::slice::from_ref(&item), &home.catalog(), Some(&reviewed)), Walk::Full(Full::Tree));
        // A project anywhere now pins the old version.
        let version = Path::new(&old).file_name().unwrap().to_string_lossy().into_owned();
        match rule.id {
            "toolchain.nvm" => drop(home.t.write("work/app/.nvmrc", version.trim_start_matches('v'))),
            "toolchain.pyenv" => drop(home.t.write("work/app/.python-version", &version)),
            _ => drop(home.t.write("work/app/rust-toolchain", version.split('-').next().unwrap())),
        }
        let plan = home.full(&[item]);
        assert_eq!(refused(&plan), [SkipReason::ChangedSinceScan], "{}: the pinned version is no finding now", rule.id);
    }
}

/// Gate decision 10a: a report of other roots, or of another home, is not
/// the one to edit; the caller takes it for no report.
#[test]
fn only_a_report_of_the_same_roots_and_home_is_edited() {
    if common::unsupported("only_a_report_of_the_same_roots_and_home_is_edited") {
        return;
    }
    let mut home = Home::new();
    home.t.file("p/__pycache__/m.pyc", 100);
    let env = home.env();
    let whole = home.review();
    assert!(recheck::same_roots(&whole, &env, &home.opts()));
    let mut moved = whole.clone();
    moved.home = home.t.path("p");
    assert!(!recheck::same_roots(&moved, &env, &home.opts()), "another home");
    home.review_roots = vec![home.t.path("p")];
    let narrower = home.review();
    assert!(!recheck::same_roots(&narrower, &env, &home.opts()), "a scan of p alone");
    let opts = ScanOptions { roots: vec![home.t.path("p")], ..home.opts() };
    assert!(recheck::same_roots(&narrower, &env, &opts), "a clean of p alone edits it");
    assert!(!recheck::same_roots(&whole, &env, &opts));
}

#[test]
fn no_report_to_edit_takes_the_full_walk() {
    if common::unsupported("no_report_to_edit_takes_the_full_walk") {
        return;
    }
    let home = Home::new();
    home.t.file("p/__pycache__/m.pyc", 100);
    let reviewed = home.review();
    let item = home.listed(&reviewed, "p/__pycache__");
    assert_eq!(walk_for(&[item], &home.catalog(), None), Walk::Full(Full::NoReport));
}
