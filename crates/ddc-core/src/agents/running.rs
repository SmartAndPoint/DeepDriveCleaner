//! Which harness each process is, by its exact executable name or its app
//! bundle, never by a substring: the Claude desktop chat app runs as
//! `Claude`, and is not Claude Code.

use std::path::Path;

use crate::platform::procs::{ProcInfo, app_bundle};

use super::{HarnessId, Running, RunningKind};

/// The harness `proc` belongs to, and what it is of it. `codex_packages`
/// is `~/.codex/packages`, where Codex's app-server daemon lives.
pub(super) fn classify(proc: &ProcInfo, codex_packages: &Path) -> Option<(HarnessId, RunningKind)> {
    if let Some(exe) = proc.exe.as_deref().filter(|e| e.is_absolute()) {
        if exe.starts_with(codex_packages) {
            return Some((HarnessId::Codex, RunningKind::AppServer));
        }
        if in_macos_of(exe, "Cursor") {
            return Some((HarnessId::Cursor, RunningKind::App));
        }
    }
    // Anywhere in an app bundle: ChatGPT ships a `codex` under `Contents/Resources`.
    let in_app = proc.app_bundle().is_some() || proc.exe.as_deref().is_some_and(in_some_app);
    let kind = |cli: RunningKind| if in_app { RunningKind::Other } else { cli };
    match proc.name.as_str() {
        "claude" => Some((HarnessId::ClaudeCode, kind(RunningKind::Cli))),
        "codex" if proc.args.iter().skip(1).any(|a| a == "app-server") => {
            Some((HarnessId::Codex, kind(RunningKind::AppServer)))
        }
        "codex" => Some((HarnessId::Codex, kind(RunningKind::Cli))),
        "opencode" => Some((HarnessId::Opencode, kind(RunningKind::Cli))),
        _ => None,
    }
}

/// Whether `exe` lies anywhere inside a `<Name>.app` bundle.
fn in_some_app(exe: &Path) -> bool {
    exe.components().any(|c| c.as_os_str().to_str().is_some_and(|s| s.len() > 4 && s.ends_with(".app")))
}

/// An executable directly in `<app>.app/Contents/MacOS`: the app itself,
/// not one of its helpers under `Contents/Frameworks`.
fn in_macos_of(exe: &Path, app: &str) -> bool {
    app_bundle(exe).as_deref() == Some(app)
        && exe.parent().is_some_and(|dir| dir.ends_with(format!("{app}.app/Contents/MacOS")))
}

/// The running processes of `harness`, by pid.
pub(super) fn of(harness: HarnessId, procs: &[ProcInfo], codex_packages: &Path) -> Vec<Running> {
    let mut out: Vec<Running> = procs
        .iter()
        .filter_map(|p| match classify(p, codex_packages) {
            Some((h, kind)) if h == harness => Some(Running { pid: p.pid, kind }),
            _ => None,
        })
        .collect();
    out.sort_by_key(|r| r.pid);
    out
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::*;

    fn proc(name: &str, exe: &str, args: &[&str]) -> ProcInfo {
        ProcInfo {
            pid: 1,
            name: name.to_owned(),
            exe: Some(PathBuf::from(exe)),
            args: args.iter().map(OsString::from).collect(),
            ..ProcInfo::default()
        }
    }

    const PACKAGES: &str = "/Users/example/.codex/packages";

    fn classify(p: &ProcInfo) -> Option<(HarnessId, RunningKind)> {
        super::classify(p, Path::new(PACKAGES))
    }

    #[test]
    fn the_claude_desktop_app_and_a_name_containing_claude_are_not_claude_code() {
        let cli = proc("claude", "/Users/example/.local/bin/claude", &["claude"]);
        let desktop = proc("Claude", "/Applications/Claude.app/Contents/MacOS/Claude", &[]);
        let lookalike = proc("claude-helper", "/usr/local/bin/claude-helper", &[]);
        let procs = [cli.clone(), desktop.clone(), lookalike.clone()];
        assert_eq!(classify(&cli), Some((HarnessId::ClaudeCode, RunningKind::Cli)));
        assert_eq!(classify(&desktop), None);
        assert_eq!(classify(&lookalike), None);
        assert_eq!(of(HarnessId::ClaudeCode, &procs, Path::new(PACKAGES)).len(), 1);
    }

    #[test]
    fn codex_is_a_cli_or_its_app_server() {
        assert_eq!(
            classify(&proc("codex", "/opt/homebrew/bin/codex", &["codex"])),
            Some((HarnessId::Codex, RunningKind::Cli))
        );
        assert_eq!(
            classify(&proc("codex", "/opt/homebrew/bin/codex", &["codex", "app-server"])),
            Some((HarnessId::Codex, RunningKind::AppServer))
        );
        assert_eq!(
            classify(&proc("node", &format!("{PACKAGES}/0.160.0/bin/node"), &["node"])),
            Some((HarnessId::Codex, RunningKind::AppServer))
        );
        // Inside another app's bundle: Codex, but not one ddc can name.
        assert_eq!(
            classify(&proc("codex", "/Applications/ChatGPT.app/Contents/MacOS/codex", &["codex"])),
            Some((HarnessId::Codex, RunningKind::Other))
        );
        assert_eq!(
            classify(&proc("codex", "/Applications/ChatGPT.app/Contents/Resources/codex", &["codex", "app-server"])),
            Some((HarnessId::Codex, RunningKind::Other))
        );
    }

    #[test]
    fn cursor_is_its_main_binary_only() {
        assert_eq!(
            classify(&proc("Cursor", "/Applications/Cursor.app/Contents/MacOS/Cursor", &[])),
            Some((HarnessId::Cursor, RunningKind::App))
        );
        let helper = "/Applications/Cursor.app/Contents/Frameworks/Cursor Helper.app/Contents/MacOS/Cursor Helper";
        assert_eq!(classify(&proc("Cursor Helper", helper, &[])), None);
        assert_eq!(classify(&proc("cursor", "/usr/local/bin/cursor", &[])), None);
    }

    #[test]
    fn opencode_runs_by_its_name_and_node_is_nobody() {
        assert_eq!(
            classify(&proc("opencode", "/opt/homebrew/bin/opencode", &[])),
            Some((HarnessId::Opencode, RunningKind::Cli))
        );
        // An npm-installed Claude Code runs as `node`.
        assert_eq!(classify(&proc("node", "/opt/homebrew/bin/node", &["node", "/x/claude"])), None);
    }
}
