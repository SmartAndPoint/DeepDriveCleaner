//! Many projects, each a git work tree with one artifact of a catalog rule:
//! lock files present or missing, recent or old activity, keep markers
//! (planted here, not derived from the catalog). One monorepo and one build
//! folder with no project around it cover the rest of the selection model.

use ddc_core::class::RiskClass;
use ddc_core::rules::KEPT;

use crate::desc::{Desc, join};
use crate::fixtures;
use crate::rng::Rng;

pub fn desc(entries: u64, rng: &mut Rng) -> Desc {
    let rules = fixtures::project_rules();
    let mut d = Desc::new();
    monorepo(&mut d, rng);
    unowned_build(&mut d);
    let mut i = 0;
    while (d.len() as u64) < entries {
        let base = format!("projects/g{:02}/p{i:05}", i / 100).into_bytes();
        // The first projects cover every rule; the rest are drawn.
        let rule = if i < rules.len() { rules[i] } else { *rng.pick(&rules) };
        d.file(join(&base, b".git/HEAD"), 0);
        d.file(join(&base, b".git/index"), 0);
        let at = fixtures::project(&mut d, &base, rule, rng.chance(80));
        let n = rng.range(5, 60);
        fixtures::payload(&mut d, rng, &at, n);
        if rng.chance(15) {
            d.fresh(join(&base, b".git/index"));
        }
        if rule.class == RiskClass::Rebuildable && rng.chance(5) {
            d.file(join(&at, b".ddc-keep"), 0);
            d.plant(&at, KEPT);
        } else {
            d.plant(&at, rule.id);
        }
        i += 1;
    }
    d
}

/// Packages under one git root that holds the lock file: activity is read in
/// the root and in the package, and a lock file in the root counts.
fn monorepo(d: &mut Desc, rng: &mut Rng) {
    let root: &[u8] = b"projects/mono";
    d.file(join(root, b".git/HEAD"), 0);
    d.file(join(root, b".git/index"), 0);
    d.file(join(root, b"yarn.lock"), 0);
    for (package, fresh) in [("web", true), ("api", false)] {
        let base = join(root, format!("packages/{package}").as_bytes());
        d.file(join(&base, b"package.json"), 0);
        if fresh {
            // Edited today: its modules stay, though the root is old.
            d.fresh(join(&base, b"package.json"));
        }
        let at = join(&base, b"node_modules");
        fixtures::payload(d, rng, &at, 8);
        d.plant(&at, "node.node_modules");
    }
    // The lock file is in the package only.
    let base = join(root, b"packages/py");
    d.file(join(&base, b"requirements-dev.txt"), 0);
    let at = join(&base, b".venv");
    d.text(join(&at, b"pyvenv.cfg"), "home = /usr/bin\n");
    d.plant(&at, "python.venv");
}

/// A CMake build folder with no project around it: no `CMakeLists.txt`
/// beside it and no `.git` above, its source elsewhere. Its activity is
/// unknown, so it is not selected.
fn unowned_build(d: &mut Desc) {
    d.file(b"projects/cmake-src/CMakeLists.txt", 0);
    let at: &[u8] = b"projects/loose/cmake-build-debug";
    d.text(join(at, b"CMakeCache.txt"), "CMAKE_HOME_DIRECTORY:INTERNAL={tree}/projects/cmake-src\n");
    d.plant(at, "cmake.build");
}
