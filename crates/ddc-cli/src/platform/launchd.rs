//! The scheduled scan as a per-user LaunchAgent.

use ddc_core::platform::Canonical as _;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cli::IntervalArg;

pub const LABEL: &str = "com.smartandpoint.deepdrivecleaner";
/// The Mac app's bundle id: the same string as [`LABEL`], a different meaning.
pub const APP_BUNDLE_ID: &str = "com.smartandpoint.deepdrivecleaner";

#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub exe: PathBuf,
    pub interval: IntervalArg,
    /// Written only when given, so `notify_threshold` in settings.json applies otherwise.
    pub threshold: Option<u64>,
    pub log_dir: PathBuf,
    /// Lists the agent under this app in Login Items (`AssociatedBundleIdentifiers`).
    pub associated_bundle: Option<&'static str>,
}

/// The app bundle a binary in `X.app/Contents/Helpers/` belongs to.
pub fn app_bundle_of(exe: &Path) -> Option<PathBuf> {
    let helpers = exe.parent().filter(|p| p.file_name().is_some_and(|n| n == "Helpers"))?;
    let contents = helpers.parent().filter(|p| p.file_name().is_some_and(|n| n == "Contents"))?;
    let app = contents.parent().filter(|p| p.extension().is_some_and(|e| e == "app"))?;
    Some(app.to_path_buf())
}

pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

pub fn plist(spec: &AgentSpec) -> String {
    let mut args = vec![spec.exe.to_string_lossy().into_owned(), "scan".into(), "--background".into()];
    if let Some(threshold) = spec.threshold {
        args.extend(["--threshold".into(), threshold.to_string()]);
    }
    let args: String = args.iter().map(|a| format!("\n        <string>{}</string>", xml(a))).collect();
    let associated = spec
        .associated_bundle
        .map(|id| {
            format!(
                "\n    <key>AssociatedBundleIdentifiers</key>\n    <array>\n        <string>{}</string>\n    </array>",
                xml(id)
            )
        })
        .unwrap_or_default();
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
    <string>{err}</string>{associated}
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

/// Homebrew installs every version under a new directory, which `brew upgrade`
/// removes; a link outside it follows the upgrade. Path arithmetic only:
/// - cask: `<prefix>/Caskroom/<token>/<version>/<file>` → `<prefix>/bin/<file>`
/// - formula: `<prefix>/Cellar/<name>/<version>/bin/<file>` → `<prefix>/opt/<name>/bin/<file>`
pub fn homebrew_stable_path(exe: &Path) -> Option<PathBuf> {
    let file = exe.file_name()?;
    let parent = exe.parent()?;
    let named = |dir: &Path, name: &str| dir.file_name().is_some_and(|n| n == name);
    let caskroom = parent.parent()?.parent()?;
    if named(caskroom, "Caskroom") {
        return Some(caskroom.parent()?.join("bin").join(file));
    }
    let formula = parent.parent()?.parent()?;
    let cellar = formula.parent()?;
    if named(parent, "bin") && named(cellar, "Cellar") {
        return Some(cellar.parent()?.join("opt").join(formula.file_name()?).join("bin").join(file));
    }
    None
}

/// Both paths name the same file once symlinks are resolved.
pub fn same_binary(a: &Path, b: &Path) -> bool {
    matches!((a.canonical(), b.canonical()), (Ok(x), Ok(y)) if x == y)
}

/// The path the agent runs: Homebrew's stable link when it leads to this binary,
/// so the schedule survives `brew upgrade`; otherwise the binary itself.
pub fn scheduled_program(exe: &Path) -> PathBuf {
    homebrew_stable_path(exe).filter(|link| same_binary(link, exe)).unwrap_or_else(|| exe.to_path_buf())
}

/// How the program in an installed plist relates to the running binary.
#[derive(Debug, PartialEq, Eq)]
pub enum ProgramState {
    Missing,
    Other,
    Current,
}

pub fn program_state(program: &Path, current: Option<&Path>) -> ProgramState {
    if !program.exists() {
        ProgramState::Missing
    } else if current.is_some_and(|c| same_binary(program, c)) {
        ProgramState::Current
    } else {
        ProgramState::Other
    }
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
    let spec = AgentSpec { exe: scheduled_program(&spec.exe), ..spec.clone() };
    let path = ddc_core::store::write_atomic(&plist_path(home), plist(&spec).as_bytes())?;
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

/// A launchctl that only remembers what it loaded: no test ever runs the real
/// one (the owner's installed agent has the same label).
#[cfg(test)]
pub(crate) mod fake {
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::io;
    use std::path::Path;

    use super::{LABEL, Launchctl};

    /// Loaded targets; `true` makes every bootout fail.
    #[derive(Default)]
    pub(crate) struct FakeLaunchctl(pub RefCell<HashSet<String>>, pub bool);

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
}

// Unix only: the Homebrew layouts below are built with symlinks.
#[cfg(all(test, unix))]
mod tests {
    use std::cell::RefCell;

    use super::fake::FakeLaunchctl;
    use super::*;

    fn spec(dir: &Path) -> AgentSpec {
        AgentSpec {
            exe: PathBuf::from("/Users/me/.cargo/bin/deepdrivecleaner & <tools>"),
            interval: IntervalArg::Daily,
            threshold: Some(10 << 30),
            log_dir: dir.join("logs"),
            associated_bundle: None,
        }
    }

    // macOS only: plutil is a macOS tool.
    #[cfg(target_os = "macos")]
    #[test]
    fn generated_plist_passes_plutil() {
        let tmp = tempfile::tempdir().unwrap();
        let in_app = AgentSpec { threshold: None, associated_bundle: Some(APP_BUNDLE_ID), ..spec(tmp.path()) };
        for s in [spec(tmp.path()), AgentSpec { interval: IntervalArg::Weekly, ..spec(tmp.path()) }, in_app] {
            let path = tmp.path().join("agent.plist");
            std::fs::write(&path, plist(&s)).unwrap();
            let out = Command::new("/usr/bin/plutil").arg("-lint").arg(&path).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
        }
        let text = plist(&spec(tmp.path()));
        assert_eq!(installed_program(&text), Some(PathBuf::from("/Users/me/.cargo/bin/deepdrivecleaner & <tools>")));
    }

    /// The scheduled scan stays in the background: launchd gives it low I/O
    /// priority and a nice of 10 (story "Memory and CPU under load").
    // macOS only: plutil is a macOS tool.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_agent_runs_at_low_io_priority_and_nice_10() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agent.plist");
        std::fs::write(&path, plist(&spec(tmp.path()))).unwrap();
        let read = |key: &str| {
            let out = Command::new("/usr/bin/plutil").args(["-extract", key, "raw", "-o", "-"]).arg(&path).output();
            String::from_utf8(out.unwrap().stdout).unwrap().trim().to_owned()
        };
        assert_eq!(read("LowPriorityIO"), "true");
        assert_eq!(read("Nice"), "10");
    }

    #[test]
    fn an_agent_installed_from_the_app_is_listed_under_it() {
        let tmp = tempfile::tempdir().unwrap();
        let text = plist(&AgentSpec { threshold: None, associated_bundle: Some(APP_BUNDLE_ID), ..spec(tmp.path()) });
        assert!(text.contains(
            "<key>AssociatedBundleIdentifiers</key>\n    <array>\n        <string>com.smartandpoint.deepdrivecleaner</string>"
        ));
        assert!(!text.contains("--threshold"), "without the flag, settings.json's threshold applies");
        assert!(!plist(&spec(tmp.path())).contains("AssociatedBundleIdentifiers"));

        let app = |exe: &str| app_bundle_of(Path::new(exe));
        assert_eq!(
            app("/Applications/DeepDriveCleaner.app/Contents/Helpers/deepdrivecleaner"),
            Some(PathBuf::from("/Applications/DeepDriveCleaner.app"))
        );
        assert_eq!(app("/opt/homebrew/bin/deepdrivecleaner"), None);
        assert_eq!(app("/Applications/Other.app/Contents/MacOS/deepdrivecleaner"), None);
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

    /// `<root>/Cellar/deepdrivecleaner/<v>/bin/deepdrivecleaner`, a real file.
    fn keg(root: &Path, v: &str) -> PathBuf {
        let exe = root.join("Cellar/deepdrivecleaner").join(v).join("bin/deepdrivecleaner");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "binary").unwrap();
        exe
    }

    /// `<root>/opt/deepdrivecleaner -> ../Cellar/deepdrivecleaner/<v>`, as Homebrew links it.
    fn link_opt(root: &Path, v: &str) -> PathBuf {
        std::fs::create_dir_all(root.join("opt")).unwrap();
        std::os::unix::fs::symlink(format!("../Cellar/deepdrivecleaner/{v}"), root.join("opt/deepdrivecleaner"))
            .unwrap();
        root.join("opt/deepdrivecleaner/bin/deepdrivecleaner")
    }

    /// `<root>/Caskroom/deepdrivecleaner/<v>/deepdrivecleaner` linked from `<root>/bin`, as a cask installs it.
    fn cask(root: &Path, v: &str) -> (PathBuf, PathBuf) {
        let exe = root.join("Caskroom/deepdrivecleaner").join(v).join("deepdrivecleaner");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "binary").unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let link = root.join("bin/deepdrivecleaner");
        std::os::unix::fs::symlink(&exe, &link).unwrap();
        (exe, link)
    }

    #[test]
    fn homebrew_stable_path_maps_versioned_binaries_to_their_links() {
        let opt = |p: &str| homebrew_stable_path(Path::new(p));
        assert_eq!(
            opt("/opt/homebrew/Caskroom/deepdrivecleaner/0.1.0/deepdrivecleaner"),
            Some(PathBuf::from("/opt/homebrew/bin/deepdrivecleaner"))
        );
        assert_eq!(
            opt("/opt/homebrew/Cellar/deepdrivecleaner/0.1.0/bin/deepdrivecleaner"),
            Some(PathBuf::from("/opt/homebrew/opt/deepdrivecleaner/bin/deepdrivecleaner"))
        );
        assert_eq!(
            opt("/usr/local/Cellar/deepdrivecleaner/0.1.0_1/bin/ddc"),
            Some(PathBuf::from("/usr/local/opt/deepdrivecleaner/bin/ddc")),
            "another prefix, a revision suffix"
        );
        assert_eq!(opt("/Users/me/.cargo/bin/deepdrivecleaner"), None);
        assert_eq!(opt("/opt/homebrew/Cellar/deepdrivecleaner/bin/deepdrivecleaner"), None, "no version");
        assert_eq!(opt("/opt/homebrew/Cellar/deepdrivecleaner/0.1.0/libexec/deepdrivecleaner"), None);
    }

    #[test]
    fn a_cask_binary_is_scheduled_through_its_bin_link() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonical().unwrap();
        let (exe, link) = cask(&root, "0.1.0");

        install(&AgentSpec { exe: exe.clone(), ..spec(&root) }, &root, 501, &FakeLaunchctl::default()).unwrap();

        let text = std::fs::read_to_string(plist_path(&root)).unwrap();
        assert_eq!(installed_program(&text), Some(link.clone()));
        assert_eq!(program_state(&link, Some(&exe)), ProgramState::Current);
    }

    #[test]
    fn a_homebrew_keg_is_scheduled_through_its_opt_link() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonical().unwrap();
        let exe = keg(&root, "0.1.0");
        let opt = link_opt(&root, "0.1.0");

        install(&AgentSpec { exe, ..spec(&root) }, &root, 501, &FakeLaunchctl::default()).unwrap();

        let text = std::fs::read_to_string(plist_path(&root)).unwrap();
        assert_eq!(installed_program(&text), Some(opt));
    }

    #[test]
    fn other_locations_are_scheduled_by_their_own_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonical().unwrap();
        let no_link = keg(&root.join("a"), "0.1.0");
        let other_version = keg(&root.join("b"), "0.1.0");
        keg(&root.join("b"), "0.0.9");
        link_opt(&root.join("b"), "0.0.9");
        let dangling = keg(&root.join("c"), "0.1.0");
        link_opt(&root.join("c"), "0.2.0");
        let plain = root.join("d/bin/deepdrivecleaner");
        std::fs::create_dir_all(plain.parent().unwrap()).unwrap();
        std::fs::write(&plain, "binary").unwrap();

        let (old_cask, _) = cask(&root.join("f"), "0.0.9");
        let other_cask = root.join("f/Caskroom/deepdrivecleaner/0.1.0/deepdrivecleaner");
        std::fs::create_dir_all(other_cask.parent().unwrap()).unwrap();
        std::fs::write(&other_cask, "binary").unwrap();
        let (gone_cask, _) = cask(&root.join("g"), "0.1.0");
        let dangling_cask = root.join("g/Caskroom/deepdrivecleaner/0.2.0/deepdrivecleaner");
        std::fs::create_dir_all(dangling_cask.parent().unwrap()).unwrap();
        std::fs::write(&dangling_cask, "binary").unwrap();
        std::fs::rename(&gone_cask, root.join("g/moved")).unwrap();
        let unlinked_cask = root.join("e/Caskroom/deepdrivecleaner/0.1.0/deepdrivecleaner");
        std::fs::create_dir_all(unlinked_cask.parent().unwrap()).unwrap();
        std::fs::write(&unlinked_cask, "binary").unwrap();

        assert_eq!(scheduled_program(&old_cask), root.join("f/bin/deepdrivecleaner"), "the link leads to 0.0.9");
        for exe in [no_link, other_version, dangling, plain, unlinked_cask, other_cask, dangling_cask] {
            assert_eq!(scheduled_program(&exe), exe);
        }
    }

    #[test]
    fn status_recognizes_the_opt_path_as_this_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonical().unwrap();
        let exe = keg(&root.join("a"), "0.1.0");
        let opt = link_opt(&root.join("a"), "0.1.0");
        let newer = keg(&root.join("b"), "0.1.0");
        keg(&root.join("b"), "0.0.9");
        let stale = link_opt(&root.join("b"), "0.0.9");

        assert_eq!(program_state(&opt, Some(&exe)), ProgramState::Current);
        assert_eq!(program_state(&stale, Some(&newer)), ProgramState::Other);
        assert_eq!(program_state(&root.join("gone"), Some(&exe)), ProgramState::Missing);
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
