//! What Homebrew says is installed (story "Whole disk: Homebrew packages,
//! their sizes, and brew uninstall"): fixtures of `brew info --json=v2
//! --installed` and `brew services list --json`, and the listing of `ddc
//! disk brew-packages` from a fake `brew` and kegs in a temporary folder.
//! No real brew runs here.

mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use common::fakes::{FakeCommands, FakeProcs};
use common::{TreeBuilder, reference_alloc};
use ddc_core::brew::listing::{self, ListError};
use ddc_core::brew::{
    self, BrewPackages, Graph, NotOffered, Package, Service, Services, ServicesState, info, services,
};
use ddc_core::disk::{ActionId, Sources};
use ddc_core::exec::MemoryAreaJournal;
use ddc_core::free::{
    self, ActionList, ActionReport, ActionResult, FREE_SCHEMA_VERSION, FreeContext, ListedAction, Refusal,
};
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

    // A runtime dependency without its name is never read as the declared ones.
    let doc = json!({"formulae": [
        with(formula("nameless", &[("1", true)], Some(&["zstd"])), |f| {
            f["installed"][0]["runtime_dependencies"] = json!([{"full_name": "zstd"}, {"version": "1"}]);
            f["dependencies"] = json!(["zstd"]);
        }),
        formula("zstd", &[("1.5.6", false)], Some(&[])),
    ], "casks": []});
    assert_eq!(graph(&doc.to_string()).incomplete, ["nameless"]);
}

#[test]
fn a_casks_formula_from_another_tap_still_keeps_the_rack_of_that_name() {
    let doc = json!({"formulae": [formula("lima", &[("1.0.2", false)], Some(&[]))], "casks": [
        {"token": "tools", "depends_on": {"formula": ["other/tap/lima"]}},
    ]});
    let g = graph(&doc.to_string());
    assert_eq!(g.needed_by(at(&g, "lima"), &BTreeSet::new()).casks, ["tools"]);
    assert!(g.orphans().is_empty(), "a cask needs it");
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

/// The Homebrew area under a temporary system root: `/opt` is the firmlink
/// of `/System/Volumes/Data/opt`, as in `tests/disk_free.rs`.
struct Area {
    t: TreeBuilder,
    sources: Sources,
    home: PathBuf,
}

const CELLAR: &str = "sys/System/Volumes/Data/opt/homebrew/Cellar";

impl Area {
    fn new() -> Self {
        let t = TreeBuilder::new();
        t.dir("home/Library/Caches/Homebrew");
        t.dir(CELLAR);
        t.symlink("System/Volumes/Data/opt", "sys/opt");
        let sources = Sources::under(&t.path("sys"));
        let home = t.path("home");
        Self { t, sources, home }
    }

    /// A keg of `len` bytes at `Cellar/<name>/<version>`.
    fn keg(&self, name: &str, version: &str, len: usize) -> PathBuf {
        self.t.file(&format!("{CELLAR}/{name}/{version}/bin/{name}"), len);
        self.t.path(&format!("{CELLAR}/{name}/{version}"))
    }

    /// `brew services` is part of this brew.
    fn with_services(&self) {
        self.t.dir("sys/System/Volumes/Data/opt/homebrew/Library/Homebrew/services");
    }

    /// The area's `brew`, answering the probe, `info` with `installed` and
    /// `services list` with `services`.
    fn brew(&self, installed: &str, services: &str) -> FakeCommands {
        let prefix = self.t.path("sys/opt/homebrew");
        FakeCommands::default()
            .with("brew", false, "")
            .answer("brew", "--prefix", true, &format!("{}\n", prefix.display()))
            .answer("brew", "--cellar", true, &format!("{}\n", prefix.join("Cellar").display()))
            .answer("brew", "--cache", true, &format!("{}\n", self.home.join("Library/Caches/Homebrew").display()))
            .answer("brew", "info --json=v2 --installed", true, installed)
            .answer("brew", "services list --json", true, services)
    }

    fn list(&self, commands: &FakeCommands, skips: &[PathBuf]) -> Result<BrewPackages, ListError> {
        listing::list(commands, &self.sources, &self.home, skips)
    }
}

fn package<'a>(listed: &'a BrewPackages, full_name: &str) -> &'a Package {
    listed.formulae.iter().find(|p| p.full_name == full_name).unwrap_or_else(|| panic!("{full_name} is listed"))
}

/// The fixture with `lima` and the tap's `foo` installed on request too,
/// `curl` pinned, and `leftover` installed as a dependency nothing needs.
fn listing_fixture() -> String {
    let mut doc: Value = serde_json::from_str(&fixture()).unwrap();
    for f in doc["formulae"].as_array_mut().unwrap() {
        match f["full_name"].as_str().unwrap() {
            "lima" | "user/tap/foo" => f["installed"][0]["installed_on_request"] = json!(true),
            "curl" => f["pinned"] = json!(true),
            _ => {}
        }
    }
    doc["formulae"].as_array_mut().unwrap().push(formula("leftover", &[("1", false)], Some(&[])));
    doc.to_string()
}

#[test]
fn the_listing_measures_each_keg_and_offers_what_nothing_needs() {
    let a = Area::new();
    a.with_services();
    let wget = a.keg("wget", "1.25.0", 96 * 1024);
    let old = a.keg("python@3.13", "3.13.0", 64 * 1024);
    let new = a.keg("python@3.13", "3.13.1", 48 * 1024);
    let idn = a.keg("libidn2", "2.3.7", 16 * 1024);
    let unistring = a.keg("libunistring", "1.3", 8 * 1024);
    let commands = a.brew(&listing_fixture(), "[]");
    let listed = a.list(&commands, &[]).unwrap();

    assert_eq!(listed.schema_version, brew::BREW_PACKAGES_SCHEMA_VERSION);
    assert_eq!(listed.services, ServicesState::Listed);
    assert_eq!(listed.cellar, a.t.path("sys/opt/homebrew/Cellar"));
    // On request only, largest first.
    let names: Vec<&str> = listed.formulae.iter().map(|p| p.full_name.as_str()).collect();
    assert_eq!(&names[..2], ["python@3.13", "wget"]);
    assert!(!names.contains(&"openssl@3") && !names.contains(&"leftover"));
    assert_eq!(listed.orphans, ["leftover"]);

    // Sizes are ddc's, from the kegs; brew's JSON has none.
    let w = package(&listed, "wget");
    assert_eq!(w.kegs[0].path, a.t.path("sys/opt/homebrew/Cellar/wget/1.25.0"));
    assert_eq!(w.bytes, Some(reference_alloc(&wget)));
    assert_eq!(w.not_offered, None);
    assert_eq!(w.service, Service::None);
    assert_eq!(w.would_orphan, ["libidn2", "libunistring"]);
    assert_eq!(w.would_orphan_bytes, Some(reference_alloc(&idn) + reference_alloc(&unistring)));
    let openssl = w.dependencies.iter().find(|d| d.full_name == "openssl@3").unwrap();
    assert!(!openssl.alone && !openssl.on_request);
    assert_eq!(openssl.bytes, Some(0), "a keg not on disk measures 0");
    assert!(w.dependencies.iter().find(|d| d.full_name == "libidn2").unwrap().alone);

    // Two kegs: offered, each measured.
    let p = package(&listed, "python@3.13");
    assert_eq!(p.kegs.iter().map(|k| k.version.as_str()).collect::<Vec<_>>(), ["3.13.0", "3.13.1"]);
    assert_eq!(p.kegs[0].bytes, Some(reference_alloc(&old)));
    assert_eq!(p.kegs[1].bytes, Some(reference_alloc(&new)));
    assert_eq!(p.bytes, Some(reference_alloc(&old) + reference_alloc(&new)));
    assert_eq!(p.not_offered, None);

    // Needed by a formula and a cask; a tap formula named by its full_name.
    let lima = package(&listed, "lima");
    assert_eq!(lima.not_offered, Some(NotOffered::Needed));
    assert_eq!(lima.needed_by, ["colima"]);
    assert_eq!(lima.needed_by_casks, ["tools"]);
    let foo = package(&listed, "user/tap/foo");
    assert_eq!(foo.name, "foo");
    assert_eq!(foo.needed_by, ["bar", "baz"]);
    assert_eq!(foo.not_offered, Some(NotOffered::Needed));
    assert_eq!(package(&listed, "curl").not_offered, Some(NotOffered::Pinned));
    let git = package(&listed, "git");
    assert_eq!(git.not_offered, None);
    assert_eq!(git.kept_pinned, ["pcre2"]);

    // Every call ran under the four variables, the probe's included.
    assert_eq!(
        *commands.calls.borrow(),
        [
            "brew --prefix",
            "brew --cellar",
            "brew --cache",
            "brew info --json=v2 --installed",
            "brew services list --json"
        ]
    );
    for env in commands.envs.borrow().iter() {
        assert_eq!(*env, brew::ENV.map(|(k, v)| format!("{k}={v}")), "every brew call's environment");
    }
}

#[test]
fn without_brews_services_command_none_runs_and_the_state_is_not_known() {
    let a = Area::new();
    let commands = a.brew(&listing_fixture(), "[]");
    let listed = a.list(&commands, &[]).unwrap();
    assert_eq!(listed.services, ServicesState::NotKnown);
    assert!(!commands.ran("brew services list --json"));
    let w = package(&listed, "wget");
    assert_eq!((w.service, w.not_offered), (Service::NotKnown, None));
}

#[test]
fn a_service_job_keeps_its_formula_from_being_offered() {
    let a = Area::new();
    a.with_services();
    let missing = a.home.join("Library/LaunchAgents/homebrew.mxcl.wget.plist");
    let list = json!([
        {"name": "wget", "status": "none", "user": "root", "file": missing, "exit_code": null},
        {"name": "python@3.13", "status": "scheduled", "user": "someone", "file": null, "exit_code": null},
    ]);
    let listed = a.list(&a.brew(&listing_fixture(), &list.to_string()), &[]).unwrap();
    let w = package(&listed, "wget");
    assert_eq!((w.service, w.not_offered), (Service::Root, Some(NotOffered::Service)));
    let p = package(&listed, "python@3.13");
    assert_eq!((p.service, p.not_offered), (Service::Running, Some(NotOffered::Service)));
    assert_eq!(package(&listed, "bar").not_offered, None);
}

#[test]
fn services_that_cannot_be_read_offer_nothing() {
    let a = Area::new();
    a.with_services();
    let listed = a.list(&a.brew(&listing_fixture(), "Error: not JSON"), &[]).unwrap();
    assert_eq!(listed.services, ServicesState::Unreadable);
    assert!(listed.formulae.iter().all(|p| p.not_offered.is_some()), "{:?}", listed.formulae);
    assert_eq!(package(&listed, "wget").not_offered, Some(NotOffered::ServicesUnreadable));
}

#[test]
fn a_keg_under_a_skip_is_not_measured() {
    let a = Area::new();
    let wget = a.keg("wget", "1.25.0", 16 * 1024);
    let listed = a.list(&a.brew(&listing_fixture(), "[]"), &[wget]).unwrap();
    let w = package(&listed, "wget");
    assert_eq!((w.kegs[0].bytes, w.bytes), (None, None));
    assert_eq!(listed.formulae.last().unwrap().full_name, "wget", "one not measured comes last");
}

#[test]
fn a_formula_missing_a_field_offers_nothing() {
    let a = Area::new();
    let mut doc: Value = serde_json::from_str(&listing_fixture()).unwrap();
    doc["formulae"][0].as_object_mut().unwrap().remove("pinned");
    let listed = a.list(&a.brew(&doc.to_string(), "[]"), &[]).unwrap();
    assert_eq!(listed.incomplete, ["wget"]);
    assert!(listed.formulae.iter().all(|p| p.not_offered == Some(NotOffered::FieldMissing)));
}

#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_brew_packages_schema_is_current() {
    let schema = serde_json::to_string_pretty(&schemars::schema_for!(BrewPackages)).unwrap() + "\n";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/brew-packages.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &schema).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/brew-packages.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}

#[test]
fn nothing_is_listed_when_brew_fails_or_is_not_the_areas() {
    let a = Area::new();
    let failing = a.brew("", "[]").answer("brew", "info --json=v2 --installed", false, "Error: offline");
    assert!(matches!(a.list(&failing, &[]), Err(ListError::Info(_))));
    let elsewhere = a.brew(&listing_fixture(), "[]").answer("brew", "--prefix", true, "/usr/local\n");
    assert_eq!(a.list(&elsewhere, &[]), Err(ListError::Probe(Refusal::PrefixMismatch)));
    assert!(!elsewhere.ran("brew info --json=v2 --installed"));
}

/// `brew-uninstall` of `full_name`, its kegs as confirmed.
fn uninstall_of(full_name: &str, kegs: &[&str]) -> ActionList {
    let mut listed = ListedAction::new(ActionId::BrewUninstall, Some(full_name.to_owned()));
    listed.kegs = Some(kegs.iter().map(|k| (*k).to_owned()).collect());
    ActionList { schema_version: FREE_SCHEMA_VERSION, actions: vec![listed] }
}

/// `brew-uninstall-orphans` of `names`.
fn orphans_of(names: &[&str]) -> ActionList {
    let mut listed = ListedAction::new(ActionId::BrewUninstallOrphans, None);
    listed.names = Some(names.iter().map(|n| (*n).to_owned()).collect());
    ActionList { schema_version: FREE_SCHEMA_VERSION, actions: vec![listed] }
}

impl Area {
    /// The dry run of `list` by `commands`: the re-read, never the command.
    fn dry_run(&self, commands: &FakeCommands, list: &ActionList, skips: &[PathBuf]) -> ActionReport {
        let (procs, cancel) = (FakeProcs(vec![]), AtomicBool::new(false));
        let ctx = FreeContext::new(commands, &self.sources, &self.home, skips, &procs, &cancel);
        list.validate().unwrap();
        let mut reports = free::dry_run(&ctx, list);
        assert_eq!(reports.len(), 1);
        reports.remove(0)
    }
}

#[test]
fn the_dry_run_rereads_brew_and_lists_every_keg_it_would_remove() {
    let a = Area::new();
    a.with_services();
    let old = a.keg("python@3.13", "3.13.0", 64 * 1024);
    let new = a.keg("python@3.13", "3.13.1", 48 * 1024);
    let commands = a.brew(&listing_fixture(), "[]");
    let report = a.dry_run(&commands, &uninstall_of("python@3.13", &["3.13.1", "3.13.0"]), &[]);

    assert_eq!(report.result, ActionResult::WouldRun, "{report:?}");
    assert_eq!(report.area, "homebrew");
    assert_eq!(report.identifier.as_deref(), Some("python@3.13"));
    let exe = a.t.path("sys/opt/homebrew/bin/brew");
    let mut want = vec![exe.to_string_lossy().into_owned()];
    want.extend(["uninstall", "--formula", "--force", "python@3.13"].map(str::to_owned));
    assert_eq!(report.command, want, "every keg: --force");
    assert_eq!(report.env, brew::ENV.map(|(k, v)| format!("{k}={v}")));
    assert!(report.terminal_command.starts_with("HOMEBREW_NO_AUTOREMOVE=1 "), "{}", report.terminal_command);
    let kegs: Vec<(&str, &str, Option<u64>)> =
        report.kegs.iter().map(|k| (k.formula.as_str(), k.version.as_str(), k.bytes)).collect();
    assert_eq!(
        kegs,
        [
            ("python@3.13", "3.13.0", Some(reference_alloc(&old))),
            ("python@3.13", "3.13.1", Some(reference_alloc(&new)))
        ]
    );
    assert_eq!(report.kegs[0].path, a.t.path("sys/opt/homebrew/Cellar/python@3.13/3.13.0"));
    let preview = report.preview.as_ref().unwrap();
    assert_eq!(preview.lines.len(), 2);
    assert_eq!(preview.estimate_bytes, report.bytes_before);
    assert!(report.bytes_before.unwrap() >= reference_alloc(&old) + reference_alloc(&new));

    // The re-read under brew's four variables; nothing removed.
    assert_eq!(
        *commands.calls.borrow(),
        [
            "brew --prefix",
            "brew --cellar",
            "brew --cache",
            "brew info --json=v2 --installed",
            "brew services list --json"
        ]
    );
    for env in commands.envs.borrow().iter() {
        assert_eq!(*env, brew::ENV.map(|(k, v)| format!("{k}={v}")));
    }

    // One keg: no --force.
    a.keg("wget", "1.25.0", 16 * 1024);
    let one = a.dry_run(&a.brew(&listing_fixture(), "[]"), &uninstall_of("wget", &["1.25.0"]), &[]);
    assert_eq!(one.result, ActionResult::WouldRun);
    assert_eq!(one.command[1..], ["uninstall", "--formula", "wget"]);
}

#[test]
fn the_rereading_refuses_what_changed_since_it_was_confirmed() {
    let a = Area::new();
    a.with_services();
    let refused = |commands: &FakeCommands, list: &ActionList| {
        let report = a.dry_run(commands, list, &[]);
        assert!(!commands.calls.borrow().iter().any(|c| c.contains("uninstall")), "nothing removed");
        match report.result {
            ActionResult::Refused { reason } => (reason, report.needed_by),
            other => panic!("{list:?}: {other:?}"),
        }
    };
    let fixture = listing_fixture();
    let plain = a.brew(&fixture, "[]");
    assert_eq!(refused(&plain, &uninstall_of("wget", &["1.24.0"])).0, Refusal::BrewKegsChanged);
    assert_eq!(refused(&plain, &uninstall_of("python@3.13", &["3.13.1"])).0, Refusal::BrewKegsChanged);
    let gone = uninstall_of("python@3.13", &["3.12.9", "3.13.0", "3.13.1"]);
    assert_eq!(refused(&plain, &gone).0, Refusal::BrewKegsChanged, "a confirmed keg is gone");
    assert_eq!(refused(&plain, &uninstall_of("gone", &["1"])).0, Refusal::BrewNotInstalled);
    assert_eq!(refused(&plain, &uninstall_of("curl", &["8.11.0"])).0, Refusal::BrewPinned);

    let (reason, needed) = refused(&plain, &uninstall_of("openssl@3", &["3.4.0"]));
    assert_eq!(reason, Refusal::BrewNeeded);
    assert_eq!(needed.len(), 1);
    assert_eq!(
        (needed[0].formula.as_str(), &needed[0].formulae[..]),
        ("openssl@3", &["curl", "wget"].map(str::to_owned)[..])
    );
    let (reason, needed) = refused(&plain, &uninstall_of("lima", &["1.0.2"]));
    assert_eq!(reason, Refusal::BrewNeeded);
    assert_eq!(needed[0].casks, ["tools"]);

    // The tap's formula was replaced by core's `foo` of the same version.
    let mut doc: Value = serde_json::from_str(&fixture).unwrap();
    for f in doc["formulae"].as_array_mut().unwrap() {
        if f["full_name"] == "user/tap/foo" {
            f["full_name"] = json!("foo");
        }
    }
    let replaced = a.brew(&doc.to_string(), "[]");
    assert_eq!(refused(&replaced, &uninstall_of("user/tap/foo", &["0.1"])).0, Refusal::BrewNotInstalled);

    // A service job, by its kind.
    let plist = a.home.join("Library/LaunchAgents/homebrew.mxcl.wget.plist");
    let job = |status: &str, user: &str, file: Option<&std::path::Path>| {
        a.brew(&fixture, &json!([{"name": "wget", "status": status, "user": user, "file": file}]).to_string())
    };
    assert_eq!(
        refused(&job("started", "someone", None), &uninstall_of("wget", &["1.25.0"])).0,
        Refusal::BrewServiceRunning
    );
    assert_eq!(
        refused(&job("scheduled", "someone", None), &uninstall_of("wget", &["1.25.0"])).0,
        Refusal::BrewServiceRunning
    );
    // Not root, and its plist absent: nothing registered, no refusal.
    let absent = a.dry_run(&job("none", "someone", Some(&plist)), &uninstall_of("wget", &["1.25.0"]), &[]);
    assert_ne!(absent.result, ActionResult::Refused { reason: Refusal::BrewServiceRegistered }, "{absent:?}");
    assert!(!matches!(absent.result, ActionResult::Refused { .. }), "{absent:?}");
    assert_eq!(
        refused(&job("none", "root", Some(&plist)), &uninstall_of("wget", &["1.25.0"])).0,
        Refusal::BrewServiceRoot
    );
    a.t.file("home/Library/LaunchAgents/homebrew.mxcl.wget.plist", 10);
    assert_eq!(
        refused(&job("none", "someone", Some(&plist)), &uninstall_of("wget", &["1.25.0"])).0,
        Refusal::BrewServiceRegistered
    );

    // A formula listed without a field: what needs what is not known.
    let mut doc: Value = serde_json::from_str(&fixture).unwrap();
    doc["formulae"][1].as_object_mut().unwrap().remove("pinned");
    assert_eq!(
        refused(&a.brew(&doc.to_string(), "[]"), &uninstall_of("wget", &["1.25.0"])).0,
        Refusal::BrewFieldMissing
    );

    // A cask came to need an orphan since the dry run.
    let mut doc: Value = serde_json::from_str(&fixture).unwrap();
    doc["casks"].as_array_mut().unwrap().push(json!({"token": "newer", "depends_on": {"formula": "leftover"}}));
    let (reason, needed) = refused(&a.brew(&doc.to_string(), "[]"), &orphans_of(&["leftover"]));
    assert_eq!(reason, Refusal::BrewNeeded);
    assert_eq!((needed[0].formula.as_str(), &needed[0].casks[..]), ("leftover", &["newer".to_owned()][..]));

    // A runtime dependency without its name: brew listed a formula without a field.
    let mut doc: Value = serde_json::from_str(&fixture).unwrap();
    doc["formulae"][1]["installed"][0]["runtime_dependencies"] = json!([{"version": "3.4.0"}]);
    assert_eq!(
        refused(&a.brew(&doc.to_string(), "[]"), &uninstall_of("wget", &["1.25.0"])).0,
        Refusal::BrewFieldMissing
    );

    // The orphans: each still needed by nothing and not on request.
    assert_eq!(refused(&plain, &orphans_of(&["leftover", "wget"])).0, Refusal::BrewNotOrphan);
    let (reason, needed) = refused(&plain, &orphans_of(&["leftover", "libunistring"]));
    assert_eq!(reason, Refusal::BrewNeeded);
    assert_eq!(needed[0].formulae, ["libidn2", "wget"]);
    let orphans = a.dry_run(&plain, &orphans_of(&["leftover"]), &[]);
    assert_eq!(orphans.result, ActionResult::WouldRun);
    assert_eq!(orphans.identifier, None);
    assert_eq!(orphans.command[1..], ["uninstall", "--formula", "--force", "leftover"]);
    // Two that leave together are not needed by each other.
    let pair = a.dry_run(&plain, &orphans_of(&["libidn2", "libunistring"]), &[]);
    assert!(matches!(pair.result, ActionResult::Refused { reason: Refusal::BrewNeeded }), "wget needs both");
}

#[test]
fn a_rereading_brew_cannot_make_is_failed_and_nothing_starts() {
    let a = Area::new();
    a.with_services();
    let not_started = |commands: &FakeCommands| {
        let report = a.dry_run(commands, &uninstall_of("wget", &["1.25.0"]), &[]);
        assert_eq!(report.result, ActionResult::Failed { needs_admin: false });
        assert!(report.output.starts_with(free::NOT_STARTED), "{}", report.output);
        assert!(!commands.calls.borrow().iter().any(|c| c.contains("uninstall")));
        report.output
    };
    let offline = a.brew("", "[]").answer("brew", "info --json=v2 --installed", false, "Error: offline");
    not_started(&offline);
    let unreadable = a.brew(&listing_fixture(), "Error: not JSON");
    assert!(not_started(&unreadable).contains("services"));
    // Elsewhere: the probe refuses before brew info is asked.
    let elsewhere = a.brew(&listing_fixture(), "[]").answer("brew", "--prefix", true, "/usr/local\n");
    let report = a.dry_run(&elsewhere, &uninstall_of("wget", &["1.25.0"]), &[]);
    assert_eq!(report.result, ActionResult::Refused { reason: Refusal::PrefixMismatch });
    assert!(!elsewhere.ran("brew info --json=v2 --installed"));
}

impl Area {
    /// `list` run by `commands` with a 5 s timeout and `floor`, journaled.
    fn run(&self, commands: &FakeCommands, list: &ActionList, floor: Duration) -> (ActionReport, MemoryAreaJournal) {
        let (procs, cancel) = (FakeProcs(vec![]), AtomicBool::new(false));
        let ctx = FreeContext {
            timeout: Duration::from_secs(5),
            floor,
            ..FreeContext::new(commands, &self.sources, &self.home, &[], &procs, &cancel)
        };
        list.validate().unwrap();
        let mut journal = MemoryAreaJournal(Vec::new());
        let mut reports = free::run(&ctx, list, &mut journal).unwrap();
        assert_eq!(reports.len(), 1);
        (reports.remove(0), journal)
    }

    /// Moves `Cellar/<name>` out of the Cellar, as brew's uninstall does.
    fn uninstalled(&self, name: &str) -> impl Fn() + 'static {
        let rack = self.t.path(&format!("{CELLAR}/{name}"));
        let holding = self.t.dir("gone").join(name);
        move || std::fs::rename(&rack, &holding).unwrap()
    }
}

const UNINSTALL_PYTHON: &str = "brew uninstall --formula --force python@3.13";

#[test]
fn a_confirmed_formula_is_uninstalled_by_the_areas_brew_and_what_it_freed_is_measured() {
    let a = Area::new();
    a.with_services();
    let old = a.keg("python@3.13", "3.13.0", 64 * 1024);
    let new = a.keg("python@3.13", "3.13.1", 48 * 1024);
    let both = reference_alloc(&old) + reference_alloc(&new);
    let gone = a.uninstalled("python@3.13");
    let commands = FakeCommands {
        on_call: Some(Box::new(move |call| {
            if call == UNINSTALL_PYTHON {
                gone();
            }
        })),
        ..a.brew(&listing_fixture(), "[]").answer("brew", &UNINSTALL_PYTHON[5..], true, "Uninstalling python@3.13")
    };
    let (report, journal) = a.run(&commands, &uninstall_of("python@3.13", &["3.13.0", "3.13.1"]), Duration::ZERO);

    assert_eq!(report.result, ActionResult::Freed, "{report:?}");
    assert!(report.bytes_before.unwrap() >= both);
    assert_eq!(report.bytes_after, Some(0));
    assert_eq!(report.output, "Uninstalling python@3.13");
    assert_eq!(report.kegs.len(), 2);
    let calls = commands.calls.borrow();
    assert_eq!(calls.last().map(String::as_str), Some(UNINSTALL_PYTHON));
    assert_eq!(calls.iter().filter(|c| c.contains("uninstall")).count(), 1);
    assert_eq!(*commands.envs.borrow().last().unwrap(), brew::ENV.map(|(k, v)| format!("{k}={v}")));
    let timeout = *commands.timeouts.borrow().last().unwrap();
    assert!(timeout > Duration::ZERO && timeout <= Duration::from_secs(5), "{timeout:?}: what the check left");
    assert_eq!(journal.0.len(), 1);
    assert_eq!(journal.0[0].action, ActionId::BrewUninstall);
    assert!(journal.0[0].command.ends_with("uninstall --formula --force python@3.13"), "{}", journal.0[0].command);
}

#[test]
fn brew_exiting_0_with_a_keg_left_has_freed_nothing_and_a_failure_says_why() {
    let a = Area::new();
    a.keg("wget", "1.25.0", 16 * 1024);
    let call = "uninstall --formula wget";
    let kept = a.brew(&listing_fixture(), "[]").answer("brew", call, true, "");
    let (report, _) = a.run(&kept, &uninstall_of("wget", &["1.25.0"]), Duration::ZERO);
    assert_eq!(report.result, ActionResult::RemovedNotFreed);

    let failing = a.brew(&listing_fixture(), "[]").answer("brew", call, false, "Error: Permission denied @ rb_sysopen");
    let (report, _) = a.run(&failing, &uninstall_of("wget", &["1.25.0"]), Duration::ZERO);
    assert_eq!(report.result, ActionResult::Failed { needs_admin: true });
    assert!(report.output.contains("Permission denied"));

    let slow = a.brew(&listing_fixture(), "[]").error_on("brew", call, std::io::ErrorKind::TimedOut);
    let (report, _) = a.run(&slow, &uninstall_of("wget", &["1.25.0"]), Duration::ZERO);
    assert_eq!(report.result, ActionResult::Failed { needs_admin: false });
    assert!(report.output.starts_with("timed out after "), "{}", report.output);
}

#[test]
fn too_little_time_left_after_the_check_starts_nothing() {
    // A 5 s timeout and a 4 s floor: a check over 1 s leaves too little.
    let a = Area::new();
    a.keg("wget", "1.25.0", 16 * 1024);
    let slow = FakeCommands {
        on_call: Some(Box::new(|call| {
            if call == "brew info --json=v2 --installed" {
                std::thread::sleep(Duration::from_millis(1_200));
            }
        })),
        ..a.brew(&listing_fixture(), "[]").answer("brew", "uninstall --formula wget", true, "")
    };
    let (report, journal) = a.run(&slow, &uninstall_of("wget", &["1.25.0"]), Duration::from_secs(4));
    assert_eq!(report.result, ActionResult::Failed { needs_admin: false });
    let mut lines = report.output.lines();
    assert_eq!(lines.next(), Some(free::NOT_STARTED));
    assert!(lines.next().unwrap().ends_with("of 5 s; try again"), "{}", report.output);
    assert!(!slow.calls.borrow().iter().any(|c| c.contains("uninstall")));
    assert_eq!(journal.0.len(), 1, "journaled as not started");
    assert_ne!(journal.0[0].result, ActionResult::Freed);

    let quick = a.brew(&listing_fixture(), "[]").answer("brew", "uninstall --formula wget", true, "");
    let (report, _) = a.run(&quick, &uninstall_of("wget", &["1.25.0"]), Duration::from_secs(4));
    assert!(quick.ran("brew uninstall --formula wget"), "{report:?}");

    let never = a.brew(&listing_fixture(), "[]");
    let (report, _) = a.run(&never, &uninstall_of("wget", &["1.25.0"]), Duration::from_secs(6));
    assert!(report.output.starts_with(free::NOT_STARTED), "a floor above the timeout never starts brew");
}

#[test]
fn the_orphans_are_uninstalled_together_and_a_refusal_is_journaled_with_what_needs_one() {
    let a = Area::new();
    a.keg("leftover", "1", 8 * 1024);
    let gone = a.uninstalled("leftover");
    let call = "brew uninstall --formula --force leftover";
    let commands = FakeCommands {
        on_call: Some(Box::new(move |c| {
            if c == call {
                gone();
            }
        })),
        ..a.brew(&listing_fixture(), "[]").answer("brew", &call[5..], true, "")
    };
    let (report, _) = a.run(&commands, &orphans_of(&["leftover"]), Duration::ZERO);
    assert_eq!(report.result, ActionResult::Freed);
    assert!(commands.ran(call));

    let plain = a.brew(&listing_fixture(), "[]");
    let (report, journal) = a.run(&plain, &orphans_of(&["libunistring"]), Duration::ZERO);
    assert_eq!(report.result, ActionResult::Refused { reason: Refusal::BrewNeeded });
    assert_eq!(journal.0[0].needed_by, report.needed_by);
    assert_eq!(journal.0[0].needed_by[0].formulae, ["libidn2", "wget"]);
}

#[test]
fn two_orphans_one_with_two_kegs_go_in_one_command_journaled_with_every_name() {
    let a = Area::new();
    let mut doc: Value = serde_json::from_str(&listing_fixture()).unwrap();
    let formulae = doc["formulae"].as_array_mut().unwrap();
    formulae.retain(|f| f["full_name"] != "leftover");
    formulae.push(formula("leftover", &[("1", false), ("2", false)], Some(&[])));
    formulae.push(formula("stray", &[("0.1", false)], Some(&[])));
    let installed = doc.to_string();
    a.keg("leftover", "1", 8 * 1024);
    a.keg("leftover", "2", 8 * 1024);
    a.keg("stray", "0.1", 4 * 1024);
    let (leftover, stray) = (a.uninstalled("leftover"), a.uninstalled("stray"));
    let call = "brew uninstall --formula --force leftover stray";
    let commands = FakeCommands {
        on_call: Some(Box::new(move |c| {
            if c == call {
                leftover();
                stray();
            }
        })),
        ..a.brew(&installed, "[]").answer("brew", &call[5..], true, "")
    };
    let (report, journal) = a.run(&commands, &orphans_of(&["leftover", "stray"]), Duration::ZERO);
    assert_eq!(report.result, ActionResult::Freed, "{report:?}");
    let kegs: Vec<(&str, &str)> = report.kegs.iter().map(|k| (k.formula.as_str(), k.version.as_str())).collect();
    assert_eq!(kegs, [("leftover", "1"), ("leftover", "2"), ("stray", "0.1")]);
    assert_eq!(commands.calls.borrow().iter().filter(|c| c.contains("uninstall")).count(), 1);
    assert!(commands.ran(call));
    assert_eq!(*commands.envs.borrow().last().unwrap(), brew::ENV.map(|(k, v)| format!("{k}={v}")));
    assert_eq!(journal.0.len(), 1);
    assert_eq!(journal.0[0].action, ActionId::BrewUninstallOrphans);
    assert!(journal.0[0].command.ends_with("uninstall --formula --force leftover stray"), "{}", journal.0[0].command);
}

#[test]
fn a_stop_before_the_run_leaves_it_not_run() {
    let a = Area::new();
    let commands = a.brew(&listing_fixture(), "[]");
    let (procs, cancel) = (FakeProcs(vec![]), AtomicBool::new(true));
    let ctx = FreeContext::new(&commands, &a.sources, &a.home, &[], &procs, &cancel);
    let mut journal = MemoryAreaJournal(Vec::new());
    let reports = free::run(&ctx, &uninstall_of("wget", &["1.25.0"]), &mut journal).unwrap();
    assert_eq!(reports[0].result, ActionResult::NotRun);
    assert_eq!(reports[0].command[1..], ["uninstall", "--formula", "wget"]);
    assert!(commands.calls.borrow().is_empty());
}

#[test]
fn a_service_entry_of_the_kegs_own_plist_and_no_user_is_not_a_refusal() {
    let a = Area::new();
    a.with_services();
    let keg = a.keg("wget", "1.25.0", 16 * 1024);
    a.t.file(&format!("{CELLAR}/wget/1.25.0/homebrew.mxcl.wget.plist"), 10);
    let own = keg.join("homebrew.mxcl.wget.plist");
    let services = json!([{"name": "wget", "status": "none", "user": null, "file": own}]).to_string();
    let report = a.dry_run(&a.brew(&listing_fixture(), &services), &uninstall_of("wget", &["1.25.0"]), &[]);
    assert_eq!(report.result, ActionResult::WouldRun, "{report:?}");
}

/// The fixture with nothing needing the tap's `foo`: a leaf.
fn tap_leaf_fixture() -> String {
    let mut doc: Value = serde_json::from_str(&listing_fixture()).unwrap();
    doc["formulae"].as_array_mut().unwrap().retain(|f| f["full_name"] != "bar" && f["full_name"] != "baz");
    doc.to_string()
}

#[test]
fn a_tap_leaf_is_uninstalled_by_its_full_name_and_nothing_else() {
    let a = Area::new();
    a.keg("foo", "0.1", 8 * 1024);
    let installed = tap_leaf_fixture();
    let dry = a.dry_run(&a.brew(&installed, "[]"), &uninstall_of("user/tap/foo", &["0.1"]), &[]);
    assert_eq!(dry.result, ActionResult::WouldRun, "{dry:?}");
    assert_eq!(dry.command[1..], ["uninstall", "--formula", "user/tap/foo"]);
    assert_eq!(dry.kegs[0].path, a.t.path("sys/opt/homebrew/Cellar/foo/0.1"));

    let gone = a.uninstalled("foo");
    let call = "brew uninstall --formula user/tap/foo";
    let commands = FakeCommands {
        on_call: Some(Box::new(move |c| {
            if c == call {
                gone();
            }
        })),
        ..a.brew(&installed, "[]").answer("brew", &call[5..], true, "")
    };
    let (report, journal) = a.run(&commands, &uninstall_of("user/tap/foo", &["0.1"]), Duration::ZERO);
    assert_eq!(report.result, ActionResult::Freed, "{report:?}");
    let uninstalls: Vec<String> = commands.calls.borrow().iter().filter(|c| c.contains("uninstall")).cloned().collect();
    assert_eq!(uninstalls, [call]);
    assert_eq!(journal.0[0].identifier.as_deref(), Some("user/tap/foo"));
}

#[test]
fn a_run_the_rereading_refuses_never_reaches_brew_uninstall() {
    let a = Area::new();
    a.keg("python@3.13", "3.13.0", 8 * 1024);
    a.keg("python@3.13", "3.13.1", 8 * 1024);
    a.keg("wget", "1.25.0", 8 * 1024);
    let mut doc: Value = serde_json::from_str(&listing_fixture()).unwrap();
    for f in doc["formulae"].as_array_mut().unwrap() {
        if f["full_name"] == "python@3.13" {
            f["pinned"] = json!(true);
        }
    }
    let pinned = doc.to_string();
    let cases: [(&str, ActionList, Refusal); 3] = [
        (&pinned, uninstall_of("python@3.13", &["3.13.0", "3.13.1"]), Refusal::BrewPinned),
        ("", uninstall_of("wget", &["1.24.0"]), Refusal::BrewKegsChanged),
        ("", uninstall_of("gone", &["1"]), Refusal::BrewNotInstalled),
    ];
    let plain = listing_fixture();
    for (installed, list, reason) in cases {
        let installed = if installed.is_empty() { &plain } else { installed };
        let commands = a
            .brew(installed, "[]")
            .answer("brew", "uninstall --formula --force python@3.13", true, "")
            .answer("brew", "uninstall --formula wget", true, "")
            .answer("brew", "uninstall --formula gone", true, "");
        let (report, journal) = a.run(&commands, &list, Duration::ZERO);
        assert_eq!(report.result, ActionResult::Refused { reason }, "{list:?}");
        assert!(!commands.calls.borrow().iter().any(|c| c.contains("uninstall")), "{list:?}: no uninstall ran");
        assert_eq!(journal.0[0].result, ActionResult::Refused { reason });
    }
}

#[test]
fn an_empty_set_of_orphans_is_refused_and_never_a_nameless_uninstall() {
    let a = Area::new();
    let list = orphans_of(&[]);
    assert!(list.validate().is_err(), "the list is refused whole first");
    let commands = a.brew(&listing_fixture(), "[]").answer("brew", "uninstall --formula --force", true, "");
    let (procs, cancel) = (FakeProcs(vec![]), AtomicBool::new(false));
    let ctx = FreeContext::new(&commands, &a.sources, &a.home, &[], &procs, &cancel);
    let report = free::dry_run(&ctx, &list).remove(0);
    assert_eq!(report.result, ActionResult::Refused { reason: Refusal::BrewNotInstalled });
    let mut journal = MemoryAreaJournal(Vec::new());
    let report = free::run(&ctx, &list, &mut journal).unwrap().remove(0);
    assert_eq!(report.result, ActionResult::Refused { reason: Refusal::BrewNotInstalled });
    assert!(!commands.calls.borrow().iter().any(|c| c.contains("uninstall")));
}
