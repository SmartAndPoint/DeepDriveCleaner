//! The harness's isolation rules (story "Memory and CPU under load: peak
//! memory, leaks and CPU time"): every unsafe setup is refused before a
//! spawn, one fact at a time.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use ddc_core::exec::preflight::PreflightSummary;
use ddc_loadgen::isolate::{Facts, Refusal, decide, facts, preflight};
use ddc_loadgen::items;
use ddc_loadgen::profiles::Profile;
use ddc_loadgen::target::{self, Layout};
use ddc_loadgen::{generate, isolate};

fn layout() -> Layout {
    let base = PathBuf::from("/private/var/folders/x/T/ddc-load-ab12/ppp");
    Layout { tree: base.join("tree"), home: base.join("home"), data: base.join("data") }
}

fn args(a: &[&str]) -> Vec<OsString> {
    a.iter().map(OsString::from).collect()
}

/// A safe run: the layout's home, its data folder, an accepted image target.
fn safe<'a>(args: &'a [OsString], layout: &'a Layout) -> Facts<'a> {
    Facts {
        args,
        home: &layout.home,
        home_id: Some((1, 10)),
        real_home: Path::new("/Users/owner"),
        real_home_id: Some((2, 20)),
        layout,
        target: Ok(()),
        image: true,
        trashes: false,
    }
}

#[test]
fn a_safe_run_is_allowed() {
    let l = layout();
    let a = args(&["scan", "--json", "--data-dir", l.data.to_str().unwrap()]);
    assert_eq!(decide(&safe(&a, &l)), Ok(()));
    let eq = format!("--data-dir={}", l.data.join("sub").display());
    let a = args(&["scan", "--background", "--no-notify", &eq]);
    assert_eq!(decide(&Facts { trashes: true, ..safe(&a, &l) }), Ok(()));
}

#[test]
fn the_real_home_is_refused_by_path_and_by_identity() {
    let l = layout();
    let a = args(&["scan", "--data-dir", l.data.to_str().unwrap()]);
    let real = l.home.clone();
    assert_eq!(decide(&Facts { real_home: &real, ..safe(&a, &l) }), Err(Refusal::RealHome));
    assert_eq!(decide(&Facts { real_home_id: Some((1, 10)), ..safe(&a, &l) }), Err(Refusal::RealHome));
    let elsewhere = PathBuf::from("/tmp/home");
    assert_eq!(
        decide(&Facts { home: &elsewhere, home_id: None, ..safe(&a, &l) }),
        Err(Refusal::HomeOutside(elsewhere.clone()))
    );
    // A home inside the tree (a profile's own) is allowed.
    let inner = l.tree.join("Users/u");
    assert_eq!(decide(&Facts { home: &inner, home_id: None, ..safe(&a, &l) }), Ok(()));
}

#[test]
fn a_missing_or_outside_data_dir_is_refused() {
    let l = layout();
    let none = args(&["scan", "--json"]);
    assert_eq!(decide(&safe(&none, &l)), Err(Refusal::NoDataDir));
    for outside in ["/Users/owner/Library/Application Support/DeepDriveCleaner", "relative/data"] {
        let a = args(&["scan", "--data-dir", outside]);
        assert_eq!(decide(&safe(&a, &l)), Err(Refusal::DataDirOutside(outside.into())), "{outside}");
    }
    let climbing = format!("{}/../../escape", l.data.display());
    let a = args(&["scan", "--data-dir", &climbing]);
    assert_eq!(decide(&safe(&a, &l)), Err(Refusal::DataDirOutside(climbing.into())));
}

#[test]
fn background_without_no_notify_is_refused() {
    let l = layout();
    let a = args(&["scan", "--background", "--data-dir", l.data.to_str().unwrap()]);
    assert_eq!(decide(&safe(&a, &l)), Err(Refusal::BackgroundWithoutNoNotify));
}

#[test]
fn a_target_the_guard_refuses_is_refused() {
    let l = layout();
    let a = args(&["scan", "--data-dir", l.data.to_str().unwrap()]);
    let f = Facts { target: Err(target::Refusal::Unmarked), image: false, ..safe(&a, &l) };
    assert_eq!(decide(&f), Err(Refusal::Target(target::Refusal::Unmarked)));
}

#[test]
fn a_trash_clean_on_a_temporary_target_is_refused() {
    let l = layout();
    let a = args(&["clean", "--items", "x.json", "--data-dir", l.data.to_str().unwrap()]);
    assert_eq!(decide(&Facts { trashes: true, image: false, ..safe(&a, &l) }), Err(Refusal::TrashOffImage));
    // Reading runs on a temporary target are allowed.
    assert_eq!(decide(&Facts { image: false, ..safe(&a, &l) }), Ok(()));
}

#[test]
fn a_clean_whose_dry_run_predicts_a_tool_is_refused() {
    let summary =
        |would_run| PreflightSummary { items: 3, would_delete: 1, would_trash: 1, would_run, would_skip: 0, bytes: 0 };
    assert_eq!(preflight(&summary(0)), Ok(()));
    assert_eq!(preflight(&summary(1)), Err(Refusal::ToolPredicted(1)));
}

/// The facts read from a real S tree: its own home is accepted; a temporary
/// target is never an image.
#[test]
fn facts_of_a_built_tree() {
    let dir = target::Scratch::new().unwrap();
    let g = generate(Profile::Projects, 10_000, 1, dir.path(), None).unwrap();
    let real = isolate::real_home().unwrap();
    let a = args(&["scan", "--data-dir", g.layout.data.to_str().unwrap()]);
    let on = isolate::Target { dir: dir.path().to_owned(), device: None, entries: 10_000 };
    let f = facts(&a, &g.home, &real, &g.layout, &on, false).unwrap();
    assert!(!f.image);
    assert_eq!(decide(&f), Ok(()));
    let f = facts(&a, &g.home, &real, &g.layout, &on, true).unwrap();
    assert_eq!(decide(&f), Err(Refusal::TrashOffImage));
}

/// `users` plans items a tool removes; the list a load clean passes holds
/// none of them.
#[test]
fn the_users_list_holds_no_tool_item() {
    let dir = target::Scratch::new().unwrap();
    let g = generate(Profile::Users, 10_000, 1, dir.path(), None).unwrap();
    assert!(
        g.manifest.plan.iter().any(|p| p.selected && p.action.starts_with("native")),
        "users plans no tool item any more; this test checks nothing"
    );
    let list = items::listed(&g.manifest, &g.layout.tree).unwrap().expect("trash or delete items");
    assert!(items::tool_free(&list));
    assert!(list.items.iter().all(|i| i.path.starts_with(&g.layout.tree)));
}
