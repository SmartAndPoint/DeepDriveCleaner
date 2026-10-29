//! What Homebrew says is installed (story "Whole disk: Homebrew packages,
//! their sizes, and brew uninstall"): fixtures of `brew info --json=v2
//! --installed` and `brew services list --json`. No brew runs here.

mod common;

use std::collections::BTreeSet;

use common::TreeBuilder;
use ddc_core::brew::{self, Graph, Service, Services, info, services};
use serde_json::{Value, json};

/// A formula of `brew info --json=v2 --installed`: each keg's version and
/// whether it was installed on request; `deps` its kegs' runtime
/// dependencies by `full_name`, `None` for a blank tab.
fn formula(full_name: &str, kegs: &[(&str, bool)], deps: Option<&[&str]>) -> Value {
    let runtime = deps.map(|d| {
        d.iter().map(|n| json!({"full_name": n, "version": "1", "declared_directly": true})).collect::<Vec<_>>()
    });
    json!({
        "name": brew::short(full_name),
        "full_name": full_name,
        "tap": "homebrew/core",
        "pinned": false,
        "dependencies": [],
        "recommended_dependencies": [],
        "installed": kegs.iter().map(|(v, on_request)| json!({
            "version": v, "used_options": [], "built_as_bottle": true, "poured_from_bottle": true,
            "time": 1_700_000_000, "runtime_dependencies": runtime, "installed_on_request": on_request,
        })).collect::<Vec<_>>(),
    })
}

fn with(mut v: Value, edit: impl FnOnce(&mut Value)) -> Value {
    edit(&mut v);
    v
}

/// The fixture Homebrew:
/// - wget (on request) needs openssl@3, ca-certificates, libidn2 and libunistring;
/// - curl (on request) needs openssl@3 and ca-certificates;
/// - bar and baz (on request) need the tap formula `user/tap/foo`;
/// - blank (on request) has a blank tab and recommends gettext;
/// - colima (on request) needs lima, which the cask `tools` also needs;
/// - git (on request) needs pcre2 (pinned) and zstd, which pcre2 needs too;
/// - python@3.13 has two kegs, one of them on request.
fn fixture() -> String {
    json!({
        "formulae": [
            formula("wget", &[("1.25.0", true)], Some(&["openssl@3", "ca-certificates", "libidn2", "libunistring"])),
            formula("curl", &[("8.11.0", true)], Some(&["openssl@3", "ca-certificates"])),
            formula("openssl@3", &[("3.4.0", false)], Some(&["ca-certificates"])),
            formula("ca-certificates", &[("2024-11-26", false)], Some(&[])),
            formula("libidn2", &[("2.3.7", false)], Some(&["libunistring"])),
            formula("libunistring", &[("1.3", false)], Some(&[])),
            formula("user/tap/foo", &[("0.1", false)], Some(&[])),
            formula("bar", &[("1.0", true)], Some(&["user/tap/foo"])),
            formula("baz", &[("1.0", true)], Some(&["user/tap/foo"])),
            with(formula("blank", &[("2.0", true)], None), |f| f["recommended_dependencies"] = json!(["gettext"])),
            formula("gettext", &[("0.22.5", false)], Some(&[])),
            formula("colima", &[("0.8.1", true)], Some(&["lima"])),
            formula("lima", &[("1.0.2", false)], Some(&[])),
            formula("git", &[("2.47.1", true)], Some(&["pcre2", "zstd"])),
            with(formula("pcre2", &[("10.44", false)], Some(&["zstd"])), |f| f["pinned"] = json!(true)),
            formula("zstd", &[("1.5.6", false)], Some(&[])),
            formula("python@3.13", &[("3.13.0", false), ("3.13.1", true)], Some(&[])),
        ],
        "casks": [
            {"token": "tools", "full_token": "tools", "depends_on": {"formula": ["lima"], "macos": {">=": ["12"]}}},
            {"token": "plain", "full_token": "plain", "depends_on": {}},
        ],
    })
    .to_string()
}

fn graph(json: &str) -> Graph {
    Graph::new(&info::parse(json).expect("brew info parses"))
}

fn names(g: &Graph, set: &BTreeSet<usize>) -> Vec<String> {
    set.iter().map(|&i| g.nodes()[i].full_name.clone()).collect()
}

fn at(g: &Graph, full_name: &str) -> usize {
    g.find(full_name).unwrap_or_else(|| panic!("{full_name} is listed"))
}

#[test]
fn every_installed_formula_is_read_with_its_kegs_and_what_it_needs() {
    let g = graph(&fixture());
    assert!(g.incomplete.is_empty(), "{:?}", g.incomplete);
    let wget = &g.nodes()[at(&g, "wget")];
    assert!(wget.on_request && !wget.pinned);
    assert_eq!(wget.versions, ["1.25.0"]);
    assert_eq!(
        wget.needs,
        BTreeSet::from(["openssl@3", "ca-certificates", "libidn2", "libunistring"].map(str::to_owned))
    );
    let python = &g.nodes()[at(&g, "python@3.13")];
    assert_eq!(python.versions, ["3.13.0", "3.13.1"]);
}

#[test]
fn a_formula_another_formula_or_a_cask_needs_names_what_needs_it() {
    let g = graph(&fixture());
    let openssl = g.needed_by(at(&g, "openssl@3"), &BTreeSet::new());
    assert_eq!(openssl.formula, "openssl@3");
    assert_eq!(openssl.formulae, ["curl", "wget"]);
    assert!(openssl.casks.is_empty());
    let lima = g.needed_by(at(&g, "lima"), &BTreeSet::new());
    assert_eq!(lima.formulae, ["colima"]);
    assert_eq!(lima.casks, ["tools"]);
    // A leaf nothing needs.
    assert!(g.needed_by(at(&g, "wget"), &BTreeSet::new()).is_empty());
}

#[test]
fn a_tap_formula_is_found_through_a_core_dependents_full_name() {
    let g = graph(&fixture());
    let foo = at(&g, "user/tap/foo");
    assert_eq!(g.nodes()[foo].name, "foo");
    assert_eq!(g.by_name("foo"), Some(foo));
    let needed = g.needed_by(foo, &BTreeSet::new());
    assert_eq!(needed.formula, "user/tap/foo");
    assert_eq!(needed.formulae, ["bar", "baz"]);
    // By `full_name` only: the short name finds no formula.
    assert_eq!(g.find("foo"), None);
}

#[test]
fn a_keg_with_a_blank_tab_is_read_through_its_declared_dependencies() {
    let g = graph(&fixture());
    assert_eq!(g.nodes()[at(&g, "blank")].needs, BTreeSet::from(["gettext".to_owned()]));
    assert_eq!(g.needed_by(at(&g, "gettext"), &BTreeSet::new()).formulae, ["blank"]);
    assert_eq!(names(&g, &g.would_orphan(at(&g, "blank")).orphans), ["gettext"]);
}

#[test]
fn the_orphans_of_a_removal_are_recursive_and_leave_what_another_formula_needs() {
    let g = graph(&fixture());
    let wget = g.would_orphan(at(&g, "wget"));
    // openssl@3 and ca-certificates stay for curl; libunistring goes through libidn2.
    assert_eq!(names(&g, &wget.orphans), ["libidn2", "libunistring"]);
    assert!(wget.kept_pinned.is_empty());
    // curl and wget together: removing one leaves the other's needs.
    assert!(names(&g, &g.would_orphan(at(&g, "curl")).orphans).is_empty());
}

#[test]
fn an_orphan_a_cask_or_a_remaining_tap_dependent_needs_is_kept() {
    let g = graph(&fixture());
    // The cask `tools` needs lima.
    assert!(g.would_orphan(at(&g, "colima")).orphans.is_empty());
    // bar still needs user/tap/foo after baz goes.
    assert!(g.would_orphan(at(&g, "baz")).orphans.is_empty());
}

#[test]
fn a_pinned_dependency_is_kept_and_so_is_what_it_needs() {
    let g = graph(&fixture());
    let git = g.would_orphan(at(&g, "git"));
    assert!(git.orphans.is_empty(), "{:?}", names(&g, &git.orphans));
    assert_eq!(names(&g, &git.kept_pinned), ["pcre2"]);
}

#[test]
fn what_is_orphaned_now_is_a_fixpoint_of_the_dependencies_nothing_needs() {
    let mut doc: Value = serde_json::from_str(&fixture()).unwrap();
    // A dependency left behind with its own dependency, and a cycle of two.
    let formulae = doc["formulae"].as_array_mut().unwrap();
    formulae.push(formula("left", &[("1", false)], Some(&["under"])));
    formulae.push(formula("under", &[("1", false)], Some(&[])));
    formulae.push(formula("ring-a", &[("1", false)], Some(&["ring-b"])));
    formulae.push(formula("ring-b", &[("1", false)], Some(&["ring-a"])));
    let g = graph(&doc.to_string());
    assert_eq!(names(&g, &g.orphans()), ["left", "under", "ring-a", "ring-b"]);
}

#[test]
fn installed_on_request_false_or_absent_is_a_dependency_and_one_keg_on_request_is_enough() {
    let doc = json!({"formulae": [
        formula("python@3.13", &[("3.13.0", false), ("3.13.1", true)], Some(&[])),
        with(formula("old", &[("1", false)], Some(&[])), |f| {
            // An older brew's field, still printed: ignored.
            f["installed"][0]["installed_as_dependency"] = json!(true);
        }),
        with(formula("older", &[("1", false)], Some(&[])), |f| {
            f["installed"][0].as_object_mut().unwrap().remove("installed_on_request");
        }),
    ]});
    let g = graph(&doc.to_string());
    assert!(g.incomplete.is_empty());
    assert!(g.nodes()[at(&g, "python@3.13")].on_request);
    assert!(!g.nodes()[at(&g, "old")].on_request);
    assert!(!g.nodes()[at(&g, "older")].on_request);
    assert_eq!(names(&g, &g.orphans()), ["old", "older"]);
}

#[test]
fn a_formula_without_a_field_the_graph_needs_is_named_as_incomplete() {
    let doc = json!({"formulae": [
        formula("wget", &[("1", true)], Some(&[])),
        with(formula("nopin", &[("1", true)], Some(&[])), |f| { f.as_object_mut().unwrap().remove("pinned"); }),
        with(formula("noversion", &[("1", true)], Some(&[])), |f| {
            f["installed"][0].as_object_mut().unwrap().remove("version");
        }),
    ], "casks": []});
    let g = graph(&doc.to_string());
    assert_eq!(g.incomplete, ["nopin", "noversion"]);
    assert!(g.find("wget").is_some());
}

#[test]
fn output_that_is_not_brews_json_is_not_read() {
    assert!(info::parse("Error: no internet").is_none());
    assert!(info::parse("[]").is_none());
    assert_eq!(info::parse(r#"{"formulae": [], "casks": [], "new": 1}"#).unwrap(), info::Installed::default());
    assert!(services::parse("{}").is_none());
    assert_eq!(services::parse("[]").unwrap(), []);
}

#[test]
fn brew_has_services_built_in_or_from_the_old_tap() {
    let t = TreeBuilder::new();
    let prefix = t.dir("homebrew");
    assert!(!brew::has_services(&prefix));
    t.dir("homebrew/Library/Homebrew/services");
    assert!(brew::has_services(&prefix));
    let old = t.dir("old");
    t.dir("old/Library/Taps/homebrew/homebrew-services");
    assert!(brew::has_services(&old));
}

fn entry(name: &str, status: &str, user: Option<&str>, file: Option<&std::path::Path>) -> Value {
    json!({"name": name, "status": status, "user": user, "file": file, "exit_code": null})
}

#[test]
fn a_service_job_is_judged_by_its_status_its_user_and_its_plist() {
    let t = TreeBuilder::new();
    let home = t.dir("home");
    let registered = t.file("home/Library/LaunchAgents/homebrew.mxcl.redis.plist", 10);
    let missing = home.join("Library/LaunchAgents/homebrew.mxcl.postgresql@17.plist");
    let keg_plist = t.file("Cellar/memcached/1.6/homebrew.mxcl.memcached.plist", 10);
    let list = json!([
        entry("nginx", "started", Some("someone"), Some(&registered)),
        entry("cron-job", "scheduled", Some("someone"), None),
        entry("odd", "a-status-not-yet-known", None, None),
        // `sudo brew services start`: seen from the user's domain.
        entry("postgresql@17", "none", Some("root"), Some(&missing)),
        entry("redis", "none", Some("someone"), Some(&registered)),
        entry("stopped-user", "none", Some("someone"), Some(&missing)),
        entry("memcached", "none", None, Some(&keg_plist)),
    ]);
    let services = Services::Listed(services::parse(&list.to_string()).unwrap());
    assert_eq!(services.of("nginx", &home), Service::Running);
    assert_eq!(services.of("cron-job", &home), Service::Running);
    assert_eq!(services.of("odd", &home), Service::Running);
    assert_eq!(services.of("postgresql@17", &home), Service::Root);
    assert_eq!(services.of("redis", &home), Service::Registered);
    assert_eq!(services.of("stopped-user", &home), Service::None);
    assert_eq!(services.of("memcached", &home), Service::None);
    assert_eq!(services.of("wget", &home), Service::None);
    assert!(Service::None.allows_removal() && Service::NotKnown.allows_removal());
    assert!(!Service::Unreadable.allows_removal() && !Service::Root.allows_removal());
    assert_eq!(Services::NotKnown.of("nginx", &home), Service::NotKnown);
    assert_eq!(Services::Unreadable.of("nginx", &home), Service::Unreadable);
}
