//! One fixture per rule of the catalog, derived from its declaration (the
//! matcher, siblings, children and lock files). It follows `rule_fixture`
//! in `ddc-core/tests/common/fixtures.rs` but stays a copy: the core's tests
//! cannot depend on this crate, which depends on the core. A rule with no
//! way to build it panics here, so a new rule fails the generator's tests.

use ddc_core::rules::{Matcher, RULES, Rule};

use crate::desc::{Desc, join};
use crate::rng::Rng;

/// A valid `CACHEDIR.TAG`.
pub const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# cache\n";

/// Where a profile builds a rule's fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Matched by what a folder holds, wherever it is.
    Project,
    /// Anchored at the home folder.
    Home,
    /// Assigned by the visitor, never matched (`user.kept`).
    Synthetic,
    /// Not listed: it only stops other rules inside it.
    Unlisted,
}

/// Every matcher kind is placed here, with no catch-all: a new kind fails
/// to compile, and a listed rule of a kind no profile can build panics.
pub fn place(rule: &Rule) -> Place {
    let place = match rule.matcher {
        Matcher::Name { .. }
        | Matcher::ContainsFile(_)
        | Matcher::CmakeBuild
        | Matcher::NameContains(_)
        | Matcher::CacheDirTag => Place::Project,
        Matcher::KnownPaths(_)
        | Matcher::KnownRegion(_)
        | Matcher::KnownChildren(_)
        | Matcher::NoTraverse(_)
        | Matcher::ChildHolding { .. } => Place::Home,
        Matcher::Synthetic => Place::Synthetic,
        Matcher::NameSuffix(_) | Matcher::Under { .. } => Place::Unlisted,
    };
    match (rule.listed, place) {
        (false, _) => Place::Unlisted,
        (true, Place::Unlisted) => panic!("no fixture for the listed rule {}: add one here", rule.id),
        (true, place) => place,
    }
}

/// The project profiles plant these.
pub fn project_rules() -> Vec<&'static Rule> {
    RULES.iter().filter(|r| place(r) == Place::Project).collect()
}

/// The profiles with a home plant these.
pub fn home_rules() -> Vec<&'static Rule> {
    RULES.iter().filter(|r| place(r) == Place::Home).collect()
}

/// Builds `rule`'s artifact in the project folder `base` and returns its
/// path. `lock` writes the rule's first lock file beside it.
pub fn project(d: &mut Desc, base: &[u8], rule: &Rule, lock: bool) -> Vec<u8> {
    d.dir(base);
    if lock && let Some(locks) = rule.lockfiles {
        d.file(join(base, locks[0].replace('*', "").as_bytes()), 0);
    }
    match rule.matcher {
        Matcher::Name { names, siblings, children_any } => {
            if let Some(s) = siblings.first() {
                d.file(join(base, s.as_bytes()), 0);
            }
            let at = join(base, names[0].as_bytes());
            d.dir(&at);
            if let Some(c) = children_any.first() {
                d.dir(join(&at, c.as_bytes()));
            }
            at
        }
        Matcher::ContainsFile(file) => {
            let at = join(base, b".venv");
            d.text(join(&at, file.as_bytes()), "home = /usr/bin\n");
            at
        }
        Matcher::CmakeBuild => {
            d.file(join(base, b"CMakeLists.txt"), 0);
            let at = join(base, b"cmake-build-debug");
            let source = format!("CMAKE_HOME_DIRECTORY:INTERNAL={{tree}}/{}\n", String::from_utf8_lossy(base));
            d.text(join(&at, b"CMakeCache.txt"), &source);
            at
        }
        Matcher::NameContains(text) => {
            let at = join(base, format!("x{text}1-1").as_bytes());
            d.dir(&at);
            at
        }
        Matcher::CacheDirTag => {
            let at = join(base, b"cache");
            d.text(join(&at, b"CACHEDIR.TAG"), TAG);
            at
        }
        _ => panic!("rule {} is not a project rule", rule.id),
    }
}

/// Builds `rule`'s fixture under the home folder `home` and plants what it
/// must find. A version folder of a toolchain the home pins is built too,
/// and must not be found.
pub fn home(d: &mut Desc, home: &[u8], rule: &'static Rule, rng: &mut Rng) {
    let at = |rel: &str| join(home, rel.as_bytes());
    match rule.matcher {
        Matcher::KnownPaths(paths) | Matcher::KnownRegion(paths) => {
            for p in paths {
                payload(d, rng, &at(p), 6);
                d.plant(at(p), rule.id);
            }
        }
        Matcher::NoTraverse(paths) => {
            // Never opened, so neither it nor its payload is counted.
            for p in paths {
                payload(d, rng, &at(p), 3);
                d.unopened.push(at(p));
            }
        }
        Matcher::KnownChildren(parent) => {
            let (old, new, pins): (&str, &str, &[(&str, &str)]) = match rule.id {
                "toolchain.nvm" => ("v18.0.0", "v20.0.0", &[(".nvm/alias/default", "20")]),
                "toolchain.pyenv" => ("3.11.0", "3.12.0", &[(".pyenv/version", "3.12.0")]),
                "toolchain.rustup" => (
                    "nightly-aarch64-apple-darwin",
                    "stable-aarch64-apple-darwin",
                    &[(".rustup/settings.toml", "default_toolchain = \"stable-aarch64-apple-darwin\"\n")],
                ),
                "ide.jetbrains-old-data" => ("PyCharm2023.1", "PyCharm2024.1", &[]),
                other => panic!("no fixture for the KnownChildren rule {other}: add one here"),
            };
            for v in [old, new] {
                payload(d, rng, &at(&format!("{parent}/{v}")), 4);
            }
            for (rel, text) in pins {
                d.text(at(rel), text);
            }
            d.plant(at(&format!("{parent}/{old}")), rule.id);
        }
        Matcher::ChildHolding { parents, file } => {
            let child = at(&format!("{}/old", parents[0]));
            payload(d, rng, &child, 4);
            d.text(join(&child, file.as_bytes()), "");
            d.plant(child, rule.id);
        }
        _ => panic!("rule {} is not anchored at the home folder", rule.id),
    }
}

/// About `n` entries under `dir`: files of 0–2 blocks in folders up to 3
/// deep, named so that no rule matches them.
pub fn payload(d: &mut Desc, rng: &mut Rng, dir: &[u8], n: u64) {
    d.dir(dir);
    let mut dirs = vec![dir.to_vec()];
    for i in 0..n {
        let at = rng.pick(&dirs).clone();
        if rng.chance(20) && at.iter().filter(|&&c| c == b'/').count() < dir.iter().filter(|&&c| c == b'/').count() + 3
        {
            let sub = join(&at, format!("d{i}").as_bytes());
            d.dir(&sub);
            dirs.push(sub);
        } else {
            let blocks = *rng.pick(&[0, 0, 1, 1, 2]);
            d.file(join(&at, format!("f{i}").as_bytes()), blocks);
        }
    }
}

#[cfg(test)]
mod tests {
    use ddc_core::rules::KEPT;

    use super::*;

    #[test]
    fn every_listed_rule_has_a_fixture() {
        for rule in RULES.iter().filter(|r| r.listed) {
            let placed = place(rule);
            assert!(
                matches!(placed, Place::Project | Place::Home) || rule.id == KEPT,
                "{} is placed {placed:?}",
                rule.id
            );
        }
    }
}
