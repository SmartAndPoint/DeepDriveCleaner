//! `ddc rules`: the rule catalog by group, as the app's Types settings show it.

use std::io::Write as _;
use std::process::ExitCode;

use ddc_core::rules::listing::RuleList;

use crate::cli::RulesArgs;

pub fn run(args: &RulesArgs) -> anyhow::Result<ExitCode> {
    let list = RuleList::builtin();
    let mut out = std::io::stdout().lock();
    let written = if args.json {
        serde_json::to_writer_pretty(&mut out, &list).map_err(std::io::Error::from).and_then(|()| writeln!(out))
    } else {
        text(&mut out, &list)
    };
    match written {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        other => other.map(|()| ExitCode::SUCCESS).map_err(Into::into),
    }
}

fn text(w: &mut impl std::io::Write, list: &RuleList) -> std::io::Result<()> {
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
        }
    }
    Ok(())
}
