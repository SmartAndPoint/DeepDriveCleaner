//! `ddc rules`: the rule catalog by group, as the app's Types settings show it;
//! `--platform` prints another platform's. The text names each rule's folders
//! in the home (story "Rule catalog split: shared and per-OS sections"); the
//! JSON does not, so its contract stays version 1.

use std::io::Write as _;
use std::process::ExitCode;

use ddc_core::platform::{PLATFORM, Platform};
use ddc_core::rules::listing::RuleList;
use ddc_core::rules::{Matcher, Rule, rules_of};

use crate::cli::RulesArgs;

pub fn run(args: &RulesArgs) -> anyhow::Result<ExitCode> {
    let platform = args.platform.map_or(PLATFORM, Platform::from);
    let list = RuleList::for_platform(platform);
    let mut out = std::io::stdout().lock();
    let written = if args.json {
        serde_json::to_writer_pretty(&mut out, &list).map_err(std::io::Error::from).and_then(|()| writeln!(out))
    } else {
        text(&mut out, &list, rules_of(platform))
    };
    match written {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        other => other.map(|()| ExitCode::SUCCESS).map_err(Into::into),
    }
}

fn text(w: &mut impl std::io::Write, list: &RuleList, catalog: &[Rule]) -> std::io::Result<()> {
    for group in &list.groups {
        let rules: Vec<_> = list.rules.iter().filter(|r| &r.group == group).collect();
        let switch = if rules.iter().any(|r| r.switchable) { "" } else { "  (always on)" };
        writeln!(w, "{group}{switch}")?;
        for r in rules {
            writeln!(
                w,
                "  {:<28} {:<12} {}",
                r.id,
                r.class.as_str(),
                serde_json::to_value(r.removal)?.as_str().unwrap_or("")
            )?;
            for path in catalog.iter().filter(|rule| rule.id == r.id).flat_map(home_paths) {
                writeln!(w, "      {path}")?;
            }
        }
    }
    Ok(())
}

/// Where in the home a rule looks, as `~/…`: its known paths and regions,
/// the folders it never opens, and the folders whose children it claims
/// (`/*`) or under which it claims a name (`/…/name`). A rule matched by
/// name or content anywhere has none.
fn home_paths(rule: &Rule) -> Vec<String> {
    match rule.matcher {
        Matcher::KnownPaths(paths) | Matcher::KnownRegion(paths) | Matcher::NoTraverse(paths) => {
            paths.iter().map(|p| format!("~/{p}")).collect()
        }
        Matcher::KnownChildren(parent) => vec![format!("~/{parent}/*")],
        Matcher::ChildHolding { parents, .. } => parents.iter().map(|p| format!("~/{p}/*")).collect(),
        Matcher::Under { name, prefixes } => prefixes.iter().map(|p| format!("~/{p}/…/{name}")).collect(),
        Matcher::Name { .. }
        | Matcher::ContainsFile(_)
        | Matcher::NameSuffix(_)
        | Matcher::CmakeBuild
        | Matcher::NameContains(_)
        | Matcher::CacheDirTag
        | Matcher::Synthetic => Vec::new(),
    }
}
