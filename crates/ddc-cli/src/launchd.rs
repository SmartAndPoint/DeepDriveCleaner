//! The scheduled scan as a per-user LaunchAgent.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cli::IntervalArg;

pub const LABEL: &str = "com.smartandpoint.deepdrivecleaner";

#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub exe: PathBuf,
    pub interval: IntervalArg,
    pub threshold: u64,
    pub log_dir: PathBuf,
}

pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

pub fn plist(spec: &AgentSpec) -> String {
    let args = [
        spec.exe.to_string_lossy().into_owned(),
        "scan".into(),
        "--background".into(),
        "--threshold".into(),
        spec.threshold.to_string(),
    ];
    let args: String = args.iter().map(|a| format!("\n        <string>{}</string>", xml(a))).collect();
    let schedule = match spec.interval {
        IntervalArg::Daily => "<key>Hour</key><integer>11</integer><key>Minute</key><integer>30</integer>",
        IntervalArg::Weekly => {
            "<key>Weekday</key><integer>1</integer><key>Hour</key><integer>11</integer><key>Minute</key><integer>30</integer>"
        }
    };
    let out = xml(&spec.log_dir.join("background.out.log").to_string_lossy());
    let err = xml(&spec.log_dir.join("background.err.log").to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>{args}
    </array>
    <key>StartCalendarInterval</key>
    <dict>{schedule}</dict>
    <key>RunAtLoad</key>
    <false/>
    <key>ProcessType</key>
    <string>Background</string>
    <key>LowPriorityIO</key>
    <true/>
    <key>Nice</key>
    <integer>10</integer>
    <key>SoftResourceLimits</key>
    <dict><key>NumberOfFiles</key><integer>10240</integer></dict>
    <key>StandardOutPath</key>
    <string>{out}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
</dict>
</plist>
"#
    )
}

/// `ProgramArguments[0]` of an installed plist.
pub fn installed_program(plist_text: &str) -> Option<PathBuf> {
    let after = plist_text.split("<key>ProgramArguments</key>").nth(1)?;
    let first = after.split("<string>").nth(1)?.split("</string>").next()?;
    let unescaped = first
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    Some(PathBuf::from(unescaped))
}

/// A binary built by cargo (`…/target/<profile>/deepdrivecleaner`) disappears with
/// `cargo clean` or `ddc clean` (rule `rust.target`): refuse to schedule it.
pub fn inside_cargo_target(exe: &Path) -> bool {
    exe.ancestors().any(|a| {
        a.file_name().is_some_and(|n| n == "target") && a.parent().is_some_and(|p| p.join("Cargo.toml").exists())
    })
}

pub trait Launchctl {
    /// Unloads; `Ok(false)` when it was not loaded.
    fn bootout(&self, target: &str) -> io::Result<bool>;
    fn bootstrap(&self, domain: &str, plist: &Path) -> io::Result<()>;
    /// Whether the service is loaded.
    fn loaded(&self, target: &str) -> bool;
}

pub struct SystemLaunchctl;

impl Launchctl for SystemLaunchctl {
    fn bootout(&self, target: &str) -> io::Result<bool> {
        let out = Command::new("/bin/launchctl").args(["bootout", target]).output()?;
        match out.status.code() {
            Some(0) => Ok(true),
            // Not loaded: "Boot-out failed: 3: No such process" (113 on some macOS versions).
            Some(3 | 113) => Ok(false),
            _ => Err(io::Error::other(format!("launchctl bootout: {}", String::from_utf8_lossy(&out.stderr).trim()))),
        }
    }

    fn bootstrap(&self, domain: &str, plist: &Path) -> io::Result<()> {
        let out = Command::new("/bin/launchctl").arg("bootstrap").arg(domain).arg(plist).output()?;
        if out.status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("launchctl bootstrap: {}", String::from_utf8_lossy(&out.stderr).trim())))
        }
    }

    fn loaded(&self, target: &str) -> bool {
        Command::new("/bin/launchctl").args(["print", target]).output().is_ok_and(|o| o.status.success())
    }
}

pub fn domain(uid: u32) -> String {
    format!("gui/{uid}")
}

pub fn target(uid: u32) -> String {
    format!("gui/{uid}/{LABEL}")
}

/// Writes the plist and (re)loads the agent: idempotent.
pub fn install(spec: &AgentSpec, home: &Path, uid: u32, lc: &dyn Launchctl) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        !inside_cargo_target(&spec.exe),
        "{} is a cargo build artifact and may be deleted; install a stable copy first: \
         make install (or cargo install --path crates/ddc-cli)",
        spec.exe.display()
    );
    std::fs::create_dir_all(&spec.log_dir)?;
    let path = ddc_core::store::write_atomic(&plist_path(home), plist(spec).as_bytes())?;
    lc.bootout(&target(uid))?;
    lc.bootstrap(&domain(uid), &path)?;
    Ok(path)
}

/// Unloads and removes the agent: idempotent. Returns whether a plist existed.
pub fn uninstall(home: &Path, uid: u32, lc: &dyn Launchctl) -> anyhow::Result<bool> {
    lc.bootout(&target(uid))?;
    // Keep the plist while the job is still loaded: without it `status` would say "not installed".
    anyhow::ensure!(!lc.loaded(&target(uid)), "the background scan is still loaded; the plist was kept");
    let path = plist_path(home);
    let existed = path.exists();
    if existed {
        #[allow(clippy::disallowed_methods)] // our own plist
        std::fs::remove_file(&path)?;
    }
    Ok(existed)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashSet;

    use super::*;

    #[derive(Default)]
    struct FakeLaunchctl(RefCell<HashSet<String>>, bool);

    impl Launchctl for FakeLaunchctl {
        fn bootout(&self, target: &str) -> io::Result<bool> {
            if self.1 {
                return Err(io::Error::other("launchctl bootout: Boot-out failed: 5: Input/output error"));
            }
            Ok(self.0.borrow_mut().remove(target))
        }
        fn bootstrap(&self, domain: &str, plist: &Path) -> io::Result<()> {
            assert!(plist.exists());
            let target = format!("{domain}/{LABEL}");
            if !self.0.borrow_mut().insert(target) {
                return Err(io::Error::other("Bootstrap failed: 5: Input/output error"));
            }
            Ok(())
        }
        fn loaded(&self, target: &str) -> bool {
            self.0.borrow().contains(target)
        }
    }

    fn spec(dir: &Path) -> AgentSpec {
        AgentSpec {
            exe: PathBuf::from("/Users/me/.cargo/bin/deepdrivecleaner & <tools>"),
            interval: IntervalArg::Daily,
            threshold: 10 << 30,
            log_dir: dir.join("logs"),
        }
    }

    #[test]
    fn generated_plist_passes_plutil() {
        let tmp = tempfile::tempdir().unwrap();
        for interval in [IntervalArg::Daily, IntervalArg::Weekly] {
            let path = tmp.path().join("agent.plist");
            std::fs::write(&path, plist(&AgentSpec { interval, ..spec(tmp.path()) })).unwrap();
            let out = Command::new("/usr/bin/plutil").arg("-lint").arg(&path).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
        }
        let text = plist(&spec(tmp.path()));
        assert_eq!(installed_program(&text), Some(PathBuf::from("/Users/me/.cargo/bin/deepdrivecleaner & <tools>")));
    }

    #[test]
    fn install_and_uninstall_are_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let lc = FakeLaunchctl::default();
        install(&spec(tmp.path()), tmp.path(), 501, &lc).unwrap();
        install(&spec(tmp.path()), tmp.path(), 501, &lc).unwrap();
        assert_eq!(lc.0.borrow().len(), 1, "one agent");
        assert!(lc.loaded(&target(501)));
        assert!(uninstall(tmp.path(), 501, &lc).unwrap());
        assert!(!uninstall(tmp.path(), 501, &lc).unwrap());
        assert!(lc.0.borrow().is_empty() && !plist_path(tmp.path()).exists());
    }

    #[test]
    fn a_failed_bootout_keeps_the_agent_and_its_plist() {
        let tmp = tempfile::tempdir().unwrap();
        let lc = FakeLaunchctl::default();
        install(&spec(tmp.path()), tmp.path(), 501, &lc).unwrap();
        let stuck = FakeLaunchctl(RefCell::new(lc.0.take()), true);

        assert!(uninstall(tmp.path(), 501, &stuck).is_err());
        assert!(install(&spec(tmp.path()), tmp.path(), 501, &stuck).is_err());
        assert!(stuck.loaded(&target(501)) && plist_path(tmp.path()).exists());
    }

    #[test]
    fn a_cargo_build_is_never_scheduled() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        let exe = tmp.path().join("target/release/deepdrivecleaner");
        assert!(inside_cargo_target(&exe));
        let err =
            install(&AgentSpec { exe, ..spec(tmp.path()) }, tmp.path(), 501, &FakeLaunchctl::default()).unwrap_err();
        assert!(err.to_string().contains("cargo install"), "{err}");
        assert!(!plist_path(tmp.path()).exists());
    }
}
