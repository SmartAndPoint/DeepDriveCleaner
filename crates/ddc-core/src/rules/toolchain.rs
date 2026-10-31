//! Protected toolchain versions (story "Detection rule catalog", section
//! "Protected toolchain versions"), outdated JetBrains data folders and the
//! editors' remote servers. Every child of a toolchain directory is claimed
//! during the walk; the ones that must stay are dropped here.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::{Catalog, Pins};
use crate::scan::OwnerResult;

/// The rules whose versions pin files anywhere in the scan roots protect:
/// [`Scope::Tree`](super::Scope::Tree).
const PINNED: &[&str] = &["toolchain.nvm", "toolchain.pyenv", "toolchain.rustup"];
/// The rule whose newest sibling is kept: [`Scope::Siblings`](super::Scope::Siblings).
const NEWEST_KEPT: &str = "ide.jetbrains-old-data";
/// The rules whose server builds are kept by the editor's own record, also
/// [`Scope::Siblings`](super::Scope::Siblings).
const SERVERS: &[&str] = &["ide.vscode-server", "ide.cursor-server"];

/// Paths of claimed toolchain/IDE folders that are not findings.
pub(crate) fn dropped(owners: &[OwnerResult], catalog: &Catalog, pins: &Pins) -> HashSet<PathBuf> {
    // By rule and parent: a rule with several parents decides in each alone,
    // as a re-check that walks one parent does.
    let mut groups: HashMap<(&str, &Path), Vec<&OwnerResult>> = HashMap::new();
    for o in owners {
        let id = catalog.rule(o.tag).id;
        if PINNED.contains(&id) || id == NEWEST_KEPT || SERVERS.contains(&id) {
            groups.entry((id, o.path.parent().unwrap_or(&o.path))).or_default().push(o);
        }
    }
    let home = catalog.home();
    let mut drop = HashSet::new();
    for ((id, parent), members) in groups {
        let names: Vec<String> = members.iter().map(|o| file_name(&o.path)).collect();
        let keep: Option<HashSet<String>> = match id {
            "toolchain.nvm" => nvm_protected(&names, home, &pins.node),
            "toolchain.pyenv" => pyenv_protected(&names, home, &pins.python),
            "toolchain.rustup" => rustup_protected(&names, home, &pins.rust),
            NEWEST_KEPT => Some(jetbrains_current(&names)),
            _ => servers_kept(&names, parent),
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
            .filter(|n| crate::stats::read_dir(versions.join(n).join("envs")).is_ok_and(|mut rd| rd.next().is_some()))
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

/// `Stable-<commit>` or `Insiders-<commit>` in `cli/servers`, a bare
/// `<commit>` in `bin`, each also with a `.staging` or `.staging-<id>`
/// suffix: a build or an install of one, not the CLI's own files.
fn server_build(name: &str, current_layout: bool) -> Option<&str> {
    let name = name.split_once(".staging").map_or(name, |(build, _)| build);
    let commit =
        if current_layout { name.strip_prefix("Stable-").or_else(|| name.strip_prefix("Insiders-"))? } else { name };
    (commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit())).then_some(commit)
}

/// The server folders an editor keeps. In `cli/servers`: the head of
/// `lru.json`, the build its CLI used last, and every folder that is not a
/// build; a `.staging` folder beside the builds is an install's leftover and
/// not kept. In `bin`: the newest build and every folder that is not a
/// build. An `lru.json` that cannot be read keeps everything (`None`).
/// Builds a client still runs are held back by the in-use check, not here:
/// two clients of different versions use two builds at once.
fn servers_kept(names: &[String], parent: &Path) -> Option<HashSet<String>> {
    if parent.ends_with("cli/servers") {
        let lru: Vec<String> = serde_json::from_str(&fs::read_to_string(parent.join("lru.json")).ok()?).ok()?;
        let head = lru.first()?;
        return Some(names.iter().filter(|n| *n == head || server_build(n, true).is_none()).cloned().collect());
    }
    let modified = |n: &String| fs::symlink_metadata(parent.join(n)).and_then(|m| m.modified()).ok();
    let newest = names.iter().filter(|n| server_build(n, false).is_some()).max_by_key(|n| modified(n));
    Some(names.iter().filter(|n| Some(*n) == newest || server_build(n, false).is_none()).cloned().collect())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;

    use super::{NEWEST_KEPT, PINNED, SERVERS, servers_kept};
    use crate::platform::Platform;
    use crate::rules::{Matcher, Scope, rules_of};

    /// The scopes name exactly the rules `dropped` decides from outside their
    /// item, in the macOS catalog, which holds them all.
    #[test]
    fn the_scopes_are_the_rules_dropped_reads_beyond_their_item() {
        let rules = rules_of(Platform::Macos);
        let ids = |scope| rules.iter().filter(|r| r.scope == scope).map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids(Scope::Tree), PINNED);
        assert_eq!(ids(Scope::Siblings), [&[NEWEST_KEPT], SERVERS].concat());
        // Every Claude Code project folder is a finding of its own: nothing
        // is decided from its siblings.
        let every_child = ["agent.claude-code-transcripts"];
        for rule in rules.iter().filter(|r| matches!(r.matcher, Matcher::KnownChildren(_))) {
            if every_child.contains(&rule.id) {
                assert_eq!(rule.scope, Scope::Item, "{}", rule.id);
                continue;
            }
            assert_ne!(rule.scope, Scope::Item, "{}: a known-children rule decides from its siblings", rule.id);
        }
    }

    fn build(c: char) -> String {
        std::iter::repeat_n(c, 40).collect()
    }

    fn kept(names: &[&str], parent: &std::path::Path) -> Option<HashSet<String>> {
        let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
        servers_kept(&names, parent)
    }

    /// In `cli/servers` the head of `lru.json` stays, and every folder that
    /// is not a build; the other builds and every `.staging` folder go. No
    /// readable record keeps everything.
    #[test]
    fn the_current_layout_keeps_the_build_the_cli_used_last() {
        let d = tempfile::tempdir().unwrap();
        let servers = d.path().join(".vscode-server/cli/servers");
        fs::create_dir_all(&servers).unwrap();
        let (a, b) = (format!("Stable-{}", build('a')), format!("Stable-{}", build('b')));
        let staging = format!("{b}.staging");
        let staging_id = format!("Insiders-{}.staging-1a2b", build('c'));
        let names = [a.as_str(), b.as_str(), staging.as_str(), staging_id.as_str(), "other"];

        assert_eq!(kept(&names, &servers), None, "no lru.json");
        fs::write(servers.join("lru.json"), "not json").unwrap();
        assert_eq!(kept(&names, &servers), None, "an lru.json that is not a list");
        fs::write(servers.join("lru.json"), "[]").unwrap();
        assert_eq!(kept(&names, &servers), None, "an empty lru.json");

        fs::write(servers.join("lru.json"), format!(r#"["{b}","{a}"]"#)).unwrap();
        let want: HashSet<String> = [b.clone(), "other".to_owned()].into();
        assert_eq!(kept(&names, &servers), Some(want));
    }

    /// In `bin` the most recently changed build stays, and every folder that
    /// is not a build (`multiplex-server`).
    #[test]
    fn the_older_layout_keeps_its_newest_build() {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join(".cursor-server/bin");
        let (old, new) = (build('a'), build('b'));
        for n in [&old, &new, &"multiplex-server".to_owned(), &"short".to_owned()] {
            fs::create_dir_all(bin.join(n)).unwrap();
        }
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
        let aged = crate::platform::fsops::set_times(&bin.join(&old), past, past);
        if crate::platform::skipped_without("the_older_layout_keeps_its_newest_build", aged).is_none() {
            return;
        }
        let names = [old.as_str(), new.as_str(), "multiplex-server", "short"];
        let want: HashSet<String> = [new.clone(), "multiplex-server".to_owned(), "short".to_owned()].into();
        assert_eq!(kept(&names, &bin), Some(want));
    }
}
