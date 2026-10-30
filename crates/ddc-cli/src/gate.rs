//! The platform's refusals at dispatch (ADR "Platform seam: one platform
//! module per OS with declared capabilities", §3, §5–§7): after clap, before
//! `init_process`, an `Env`, a lock or a signal handler. On a platform that
//! is not supported only `rules` runs (with `--help` and `--version`, which
//! clap answers first). A command that needs a capability the platform does
//! not declare is refused whole. Either exits 69, with the JSON envelope on
//! stdout when the command's JSON output was asked for.

use std::io::Write as _;
use std::process::ExitCode;

use ddc_core::platform::{self, Capability, Refusal, Unsupported};
use ddc_core::refusal::ErrorReport;

use crate::cli::{Command, DiskAction, DiskArgs};
use crate::exit;

/// The exit of a command this build refuses; `None` lets it run.
pub fn check(cmd: &Command) -> Option<ExitCode> {
    let refusal = refusal_for(cmd, platform::require_supported(), platform::CAPABILITIES)?;
    Some(refuse(&refusal, &message(&refusal), cmd.prints_json()))
}

/// What refuses `cmd` on a platform that is `supported` (or not) and
/// declares `declared`. The match is exhaustive, with no wildcard, so a new
/// command has to choose.
fn refusal_for(cmd: &Command, supported: Result<(), Unsupported>, declared: &[Capability]) -> Option<Refusal> {
    let needs = match cmd {
        Command::Rules(_) => return None,
        Command::Scan(_) | Command::Clean(_) | Command::Du(_) | Command::Notify { .. } | Command::Diag => None,
        Command::Schedule { .. } => Some(Capability::Schedule),
        Command::Disk(DiskArgs { action: None, .. }) => Some(Capability::WholeDisk),
        Command::Disk(DiskArgs { action: Some(DiskAction::Agents(_)), .. }) => Some(Capability::CodingAgents),
        Command::Disk(DiskArgs { action: Some(DiskAction::BrewPackages(_)), .. }) => Some(Capability::Homebrew),
        // Gated per action in the core (`free::run`, `dry_run`, `available`);
        // refused whole only when no action could ever run.
        Command::Disk(DiskArgs { action: Some(DiskAction::Free(_)), .. }) => {
            (!declared.contains(&Capability::WholeDisk) && !declared.contains(&Capability::Homebrew))
                .then_some(Capability::WholeDisk)
        }
    };
    if let Err(unsupported) = supported {
        return Some(Refusal::Unsupported(unsupported));
    }
    let needs = needs?;
    platform::require(needs, declared, None).err().map(Refusal::CapabilityAbsent)
}

/// The `ddc: …` line and, under JSON output, the message of the envelope.
pub fn message(refusal: &Refusal) -> String {
    match refusal {
        Refusal::Unsupported(u) => format!("{u}: only --version, --help and rules run"),
        Refusal::CapabilityAbsent(c) => c.to_string(),
    }
}

/// Writes `message` to stderr and, when `json`, the envelope to stdout;
/// the exit is 69.
pub fn refuse(refusal: &Refusal, message: &str, json: bool) -> ExitCode {
    eprintln!("ddc: {message}");
    if json {
        let mut out = std::io::stdout().lock();
        let report = ErrorReport::new(refusal, message);
        // A closed pipe loses only the envelope; the exit code still says it.
        let _ = serde_json::to_writer_pretty(&mut out, &report)
            .map_err(std::io::Error::from)
            .and_then(|()| writeln!(out))
            .and_then(|()| out.flush());
    }
    ExitCode::from(exit::UNSUPPORTED)
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;
    use ddc_core::disk::ActionId;
    use ddc_core::platform::{CapabilityAbsent, PLATFORM, Platform};

    use super::*;
    use crate::cli::Cli;

    fn command(args: &[&str]) -> Command {
        Cli::try_parse_from(std::iter::once("deepdrivecleaner").chain(args.iter().copied())).unwrap().command
    }

    const UNPORTED: Result<(), Unsupported> = Err(Unsupported { platform: Platform::Linux, simulated: false });

    /// Every command, with each kind of JSON output it has.
    const EVERY: &[&[&str]] = &[
        &["scan"],
        &["scan", "--json"],
        &["scan", "--background"],
        &["scan", "--background", "--notify", "json"],
        &["clean", "--dry-run"],
        &["clean", "--items", "-", "--json"],
        &["du"],
        &["disk"],
        &["disk", "--json"],
        &["disk", "agents", "--json"],
        &["disk", "free", "--json"],
        &["disk", "brew-packages", "--json"],
        &["schedule", "status"],
        &["notify", "ack", "--bytes", "1"],
        &["diag"],
    ];

    #[test]
    fn an_unported_platform_runs_only_rules() {
        for args in EVERY {
            let refusal = refusal_for(&command(args), UNPORTED, &Capability::ALL);
            assert_eq!(refusal, Some(Refusal::Unsupported(UNPORTED.unwrap_err())), "{args:?}");
        }
        for args in [&["rules"][..], &["rules", "--json"]] {
            assert_eq!(refusal_for(&command(args), UNPORTED, &[]), None, "{args:?}");
        }
    }

    #[test]
    fn a_supported_platform_with_every_capability_runs_everything() {
        for args in EVERY {
            assert_eq!(refusal_for(&command(args), Ok(()), &Capability::ALL), None, "{args:?}");
        }
    }

    #[test]
    fn a_command_whose_capability_is_absent_is_refused_with_its_name() {
        let absent = |capability| {
            Some(Refusal::CapabilityAbsent(CapabilityAbsent { platform: PLATFORM, capability, action: None }))
        };
        let all_but = |c: Capability| Capability::ALL.into_iter().filter(|x| *x != c).collect::<Vec<_>>();
        assert_eq!(
            refusal_for(&command(&["disk"]), Ok(()), &all_but(Capability::WholeDisk)),
            absent(Capability::WholeDisk)
        );
        assert_eq!(
            refusal_for(&command(&["disk", "agents"]), Ok(()), &all_but(Capability::CodingAgents)),
            absent(Capability::CodingAgents)
        );
        assert_eq!(
            refusal_for(&command(&["disk", "brew-packages"]), Ok(()), &all_but(Capability::Homebrew)),
            absent(Capability::Homebrew)
        );
        assert_eq!(
            refusal_for(&command(&["schedule", "status"]), Ok(()), &all_but(Capability::Schedule)),
            absent(Capability::Schedule)
        );
        // `disk agents` needs only its own: Linux can declare it without the whole-disk report.
        assert_eq!(refusal_for(&command(&["disk", "agents"]), Ok(()), &[Capability::CodingAgents]), None);
        // The scan reads its absent capabilities in the report instead.
        assert_eq!(refusal_for(&command(&["scan"]), Ok(()), &[]), None);
    }

    #[test]
    fn disk_free_is_refused_whole_only_when_no_action_could_run() {
        let free = command(&["disk", "free", "--json"]);
        assert_eq!(refusal_for(&free, Ok(()), &[Capability::Homebrew]), None);
        assert_eq!(refusal_for(&free, Ok(()), &[Capability::WholeDisk]), None);
        assert_eq!(
            refusal_for(&free, Ok(()), &[Capability::CodingAgents]),
            Some(Refusal::CapabilityAbsent(CapabilityAbsent {
                platform: PLATFORM,
                capability: Capability::WholeDisk,
                action: None
            }))
        );
    }

    #[test]
    fn the_envelope_names_the_refusal() {
        let unsupported = Refusal::Unsupported(UNPORTED.unwrap_err());
        let report = ErrorReport::new(&unsupported, message(&unsupported));
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["error_schema_version"], 1);
        assert_eq!(json["error"]["code"], "platform-not-supported");
        assert_eq!(json["error"]["platform"], "linux");
        assert_eq!(
            json["error"]["message"],
            "deepdrivecleaner does not support linux yet: only --version, --help and rules run"
        );
        let absent = Refusal::CapabilityAbsent(CapabilityAbsent {
            platform: Platform::Linux,
            capability: Capability::Homebrew,
            action: Some(ActionId::BrewUninstall),
        });
        let json = serde_json::to_value(ErrorReport::new(&absent, message(&absent))).unwrap();
        assert_eq!(json["error"]["code"], "capability-absent");
        assert_eq!(json["error"]["capability"], "homebrew");
        assert_eq!(json["error"]["action"], "brew-uninstall");
        assert_eq!(json["error"]["simulated"], false);
    }
}
