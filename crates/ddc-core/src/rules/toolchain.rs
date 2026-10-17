//! Protected toolchain versions (story "Detection rule catalog", section
//! "Protected toolchain versions") and outdated JetBrains data folders.
//! Every child of a toolchain directory is claimed during the walk; the ones
//! that must stay are dropped here.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::{Catalog, Pins};
use crate::scan::OwnerResult;

/// Paths of claimed toolchain/IDE folders that are not findings.
pub(crate) fn dropped(owners: &[OwnerResult], catalog: &Catalog, pins: &Pins) -> HashSet<PathBuf> {
    let mut groups: HashMap<&str, Vec<&OwnerResult>> = HashMap::new();
    for o in owners {
        let id = catalog.rule(o.tag).id;
        if matches!(id, "toolchain.nvm" | "toolchain.pyenv" | "toolchain.rustup" | "ide.jetbrains-old-data") {
            groups.entry(id).or_default().push(o);
        }
    }
    let home = catalog.home();
    let mut drop = HashSet::new();
    for (id, members) in groups {
        let names: Vec<String> = members.iter().map(|o| file_name(&o.path)).collect();
        let keep: Option<HashSet<String>> = match id {
            "toolchain.nvm" => nvm_protected(&names, home, &pins.node),
            "toolchain.pyenv" => pyenv_protected(&names, home, &pins.python),
            "toolchain.rustup" => rustup_protected(&names, home, &pins.rust),
            _ => Some(jetbrains_current(&names)),
        };
        for (o, name) in members.iter().zip(&names) {
            if keep.as_ref().is_none_or(|k| k.contains(name)) {
                drop.insert(o.path.clone());
            }
        }
    }
    drop
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn read_trimmed(path: &Path) -> String {
    fs::read_to_string(path).map(|s| s.trim().to_owned()).unwrap_or_default()
}

pub(crate) fn version_key(v: &str) -> Vec<u64> {
    v.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// Resolves an nvm alias to one installed version name; `None` if unresolvable.
/// `installed` must be sorted ascending by version.
pub(crate) fn nvm_resolve(alias: &str, installed: &[String], home: &Path, depth: u8) -> Option<String> {
    let alias = alias.trim();
    if alias.is_empty() || alias == "system" || depth > 2 {
        return None;
    }
    if alias == "node" || alias == "stable" {
        return installed.last().cloned();
    }
    if alias.starts_with("lts/") {
        let target = read_trimmed(&home.join(".nvm/alias").join(alias));
        return nvm_resolve(&target, installed, home, depth + 1);
    }
    let prefix = alias.trim_start_matches('v');
    installed
        .iter()
        .filter(|v| {
            let v = v.trim_start_matches('v');
            v == prefix || v.starts_with(&format!("{prefix}."))
        })
        .max_by_key(|v| version_key(v))
        .cloned()
}

/// Versions nvm must keep; `None` = the default cannot be resolved (rule yields nothing).
pub(crate) fn nvm_protected(names: &[String], home: &Path, pins: &[String]) -> Option<HashSet<String>> {
    let mut installed = names.to_vec();
    installed.sort_by_key(|v| version_key(v));
    if installed.len() <= 1 {
        return None;
    }
    let default = nvm_resolve(&read_trimmed(&home.join(".nvm/alias/default")), &installed, home, 0)?;
    let mut keep: HashSet<String> = pins.iter().filter_map(|p| nvm_resolve(p, &installed, home, 0)).collect();
    keep.insert(default);
    Some(keep)
}

fn pyenv_protected(names: &[String], home: &Path, pins: &[String]) -> Option<HashSet<String>> {
    if names.len() <= 1 {
        return None;
    }
    let versions = home.join(".pyenv/versions");
    let global = read_trimmed(&home.join(".pyenv/version"));
    let mut keep = HashSet::new();
    for pin in global.split_whitespace().map(str::to_owned).chain(pins.iter().cloned()) {
        if names.contains(&pin) {
            keep.insert(pin.clone());
        }
        // pyenv-virtualenv: versions/<env> → <version>/envs/<env>.
        if let Ok(target) = fs::read_link(versions.join(&pin)) {
            let parts: Vec<String> =
                target.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            if let Some(i) = parts.iter().position(|p| p == "envs") {
                keep.extend(i.checked_sub(1).map(|j| parts[j].clone()));
            }
        }
        // `3.12` (what `uv init` writes) → the newest installed 3.12.x.
        let prefix = format!("{pin}.");
        keep.extend(names.iter().filter(|n| n.starts_with(&prefix)).max_by_key(|n| version_key(n)).cloned());
    }
    // A version with virtualenvs inside is in use.
    keep.extend(
        names
            .iter()
            .filter(|n| fs::read_dir(versions.join(n).join("envs")).is_ok_and(|mut rd| rd.next().is_some()))
            .cloned(),
    );
    Some(keep)
}

fn rustup_protected(names: &[String], home: &Path, pins: &[String]) -> Option<HashSet<String>> {
    if names.len() <= 1 {
        return None;
    }
    let settings = fs::read_to_string(home.join(".rustup/settings.toml")).unwrap_or_default();
    let default = settings.lines().find_map(|l| {
        let l = l.trim();
        l.strip_prefix("default_toolchain")
            .map(|r| r.trim_start().trim_start_matches('=').trim().trim_matches('"').to_owned())
    })?;
    let wanted: Vec<&str> = std::iter::once(default.as_str()).chain(pins.iter().map(String::as_str)).collect();
    Some(
        names
            .iter()
            .filter(|n| wanted.iter().any(|w| !w.is_empty() && (n.as_str() == *w || n.starts_with(&format!("{w}-")))))
            .cloned()
            .collect(),
    )
}

/// `PyCharm2024.3` → (`PyCharm`, [2024, 3]).
fn jetbrains_version(name: &str) -> Option<(&str, Vec<u64>)> {
    let split = name.find(|c: char| c.is_ascii_digit())?;
    let (product, version) = name.split_at(split);
    let ok = !product.is_empty()
        && product.chars().all(|c| c.is_ascii_alphabetic())
        && version.split('.').count() == 2
        && version.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    ok.then(|| (product, version_key(version)))
}

/// Names that are NOT outdated (no newer version of the same product), plus
/// every non-versioned folder — all of these are kept.
fn jetbrains_current(names: &[String]) -> HashSet<String> {
    let mut newest: HashMap<&str, Vec<u64>> = HashMap::new();
    for (product, v) in names.iter().filter_map(|n| jetbrains_version(n)) {
        let e = newest.entry(product).or_default();
        if v > *e {
            *e = v;
        }
    }
    names
        .iter()
        .filter(|n| match jetbrains_version(n) {
            Some((product, v)) => newest.get(product).is_some_and(|max| *max == v),
            None => true,
        })
        .cloned()
        .collect()
}
