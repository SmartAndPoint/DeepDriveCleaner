//! `ddc clean --items` end to end with a temporary HOME and data directory,
//! and Ctrl-C / SIGTERM during a clean (story "CLI: clean an explicit item
//! list"), and picked user files in a dry run (story "User data: pick your
//! own files for the Trash"). No item is ever sent to the real Trash: the
//! lists hold only items that are deleted, run a fake tool, or are refused,
//! and a picked file is only ever dry-run.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime};

use ddc_core::exec::Outcome;
use ddc_core::exec::SkipReason;
use ddc_core::exec::report::CleanReport;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    data: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let fx = Self { home: root.join("home"), data: root.join("data"), root, _tmp: tmp };
        let old = SystemTime::now() - Duration::from_secs(90 * 86_400);
        fx.write("p/package.json", b"{}");
        fx.write("p/package-lock.json", b"{}");
        for f in ["p/package.json", "p/package-lock.json"] {
            File::options().write(true).open(fx.home.join(f)).unwrap().set_modified(old).unwrap();
        }
        fx.write("p/node_modules/dep/index.js", &[1u8; 30_000]);
        fx.write("q/__pycache__/m.cpython-312.pyc", &[2u8; 10_000]);
        fx.write("Pictures/a.jpg", &[3u8; 5_000]);
        fx
    }

    fn write(&self, rel: &str, content: &[u8]) {
        let p = self.home.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }

    /// `ddc` with this HOME and data directory; `PATH` holds only the fake tools.
    fn ddc(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
        cmd.env_clear()
            .env("HOME", &self.home)
            .env("PATH", self.root.join("bin"))
            .env("NO_COLOR", "1")
            .args(args)
            .arg("--data-dir")
            .arg(&self.data)
            .stdin(Stdio::null());
        cmd
    }

    /// The findings at these paths, as the app lists them after a scan.
    fn list(&self, rels: &[&str]) -> PathBuf {
        let out = self.ddc(&["scan", "--json"]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let items: Vec<serde_json::Value> = rels
            .iter()
            .map(|rel| {
                let path = self.path(rel);
                let f = report["findings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|f| f["path"] == path.to_str().unwrap())
                    .unwrap_or_else(|| panic!("no finding at {rel}"));
                let action = match f["removal"].as_str().unwrap() {
                    "none" => "delete",
                    other => other,
                };
                serde_json::json!({
                    "path": f["path"], "dev": f["dev"], "ino": f["ino"],
                    "rule": f["rule"], "class": f["class"], "action": action,
                })
            })
            .collect();
        let list = self.root.join("list.json");
        fs::write(&list, serde_json::json!({ "schema_version": 1, "items": items }).to_string()).unwrap();
        list
    }

    /// A fake `uv` whose cache is `~/.cache/uv`; `uv cache clean` records its
    /// pid and sleeps for 30 s, so only a forwarded signal ends it in time.
    /// `PATH` holds only the fakes, so `sleep` is named by full path. Only
    /// builtins run before the `exec`: a shell waiting for a child that exits
    /// normally ignores a SIGINT that arrived meanwhile.
    fn fake_uv(&self) -> PathBuf {
        self.write(".cache/uv/wheels/a.whl", &[4u8; 200_000]);
        let pid_file = self.root.join("uv.pid");
        let script = format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n  \"cache dir\") echo \"$HOME/.cache/uv\" ;;\n  \"cache clean\") echo $$ > '{}'; exec /bin/sleep 30 ;;\nesac\n",
            pid_file.display()
        );
        let bin = self.root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("uv"), script).unwrap();
        fs::set_permissions(bin.join("uv"), fs::Permissions::from_mode(0o755)).unwrap();
        pid_file
    }

    fn journal(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(self.data.join("logs/actions.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// path → (kind, inode) for everything under HOME.
    fn snapshot(&self) -> BTreeMap<PathBuf, (bool, u64)> {
        let mut out = BTreeMap::new();
        let mut stack = vec![self.home.clone()];
        while let Some(dir) = stack.pop() {
            for e in fs::read_dir(&dir).unwrap() {
                let path = e.unwrap().path();
                let md = fs::symlink_metadata(&path).unwrap();
                out.insert(path.clone(), (md.is_dir(), md.ino()));
                if md.is_dir() {
                    stack.push(path);
                }
            }
        }
        out
    }
}

fn report(stdout: &[u8]) -> CleanReport {
    serde_json::from_slice(stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(stdout)))
}

fn outcome<'a>(r: &'a CleanReport, path: &Path) -> &'a Outcome {
    &r.items.iter().find(|i| i.path == path).unwrap_or_else(|| panic!("{path:?} not reported")).outcome
}

/// The tool's pid, once its line is complete in `file`.
fn wait_for_pid(file: &Path, timeout: Duration) -> i32 {
    let started = Instant::now();
    loop {
        if let Some(pid) = fs::read_to_string(file).ok().filter(|s| s.ends_with('\n')) {
            return pid.trim().parse().unwrap_or_else(|_| panic!("not a pid: {pid:?}"));
        }
        assert!(started.elapsed() < timeout, "{file:?} never appeared");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_within(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// A process holding `path` open (as its stdin) until dropped. Spawned with
/// the file moved into it, so this harness never holds it open itself.
struct Holder(Child);

impl Holder {
    fn new(path: &Path) -> Self {
        Self(Command::new("/bin/sleep").arg("60").stdin(File::open(path).unwrap()).spawn().unwrap())
    }

    fn pid(&self) -> i32 {
        self.0.id() as i32
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn alive(pid: i32) -> bool {
    Command::new("/bin/kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().unwrap().success()
}

#[test]
fn listed_items_are_cleaned_and_a_swapped_or_user_data_one_is_refused() {
    let fx = Fx::new();
    let list = fx.list(&["q/__pycache__", "p/node_modules", "Pictures"]);
    // The project's dependencies are reinstalled after the review: same path, another directory.
    fs::rename(fx.path("p/node_modules"), fx.path("p/node_modules.old")).unwrap();
    fx.write("p/node_modules/dep/index.js", b"new");

    // No terminal: stdin is null and stdout a pipe (ADR §7: the list is the consent).
    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap(), "--json"]).output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("terminal"));
    let r = report(&out.stdout);
    assert_eq!(outcome(&r, &fx.path("q/__pycache__")), &Outcome::Deleted);
    assert!(!fx.path("q/__pycache__").exists());
    assert_eq!(outcome(&r, &fx.path("p/node_modules")), &Outcome::Skipped(SkipReason::ChangedSinceScan));
    assert!(fx.path("p/node_modules/dep/index.js").exists());
    assert_eq!(outcome(&r, &fx.path("Pictures")), &Outcome::Skipped(SkipReason::ActionNotAllowed));
    assert!(fx.path("Pictures/a.jpg").exists());
    assert_eq!(
        r.items.iter().map(|i| i.path.clone()).collect::<Vec<_>>(),
        [fx.path("q/__pycache__"), fx.path("p/node_modules"), fx.path("Pictures")]
    );
    assert_eq!(fx.journal().len(), 1, "only the cleaned item is journaled");
}

#[test]
fn a_listed_item_of_a_switched_off_type_is_still_cleaned() {
    let fx = Fx::new();
    let list = fx.list(&["q/__pycache__"]);
    // Switched off after the review: the list is the user's confirmation.
    fs::write(fx.data.join("settings.json"), r#"{"schema_version": 1, "disabled_rules": ["python.pycache"]}"#).unwrap();

    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap(), "--json"]).output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(outcome(&report(&out.stdout), &fx.path("q/__pycache__")), &Outcome::Deleted);
    assert!(!fx.path("q/__pycache__").exists());
}

#[test]
fn the_report_round_trips_names_the_volume_and_a_failed_item_still_exits_0() {
    let fx = Fx::new();
    let list = fx.list(&["q/__pycache__"]);
    // The parent cannot be written: the tombstone rename fails.
    fs::set_permissions(fx.path("q"), fs::Permissions::from_mode(0o555)).unwrap();

    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap(), "--json"]).output().unwrap();
    fs::set_permissions(fx.path("q"), fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let r = report(&out.stdout);
    assert_eq!(serde_json::to_value(&r).unwrap(), json, "every field is in the contract");
    assert_eq!(r.schema_version, ddc_core::exec::report::CLEAN_SCHEMA_VERSION);
    assert!(matches!(outcome(&r, &fx.path("q/__pycache__")), Outcome::Failed { .. }));
    assert_eq!(r.failed, 1);
    assert_eq!(r.volumes.len(), 1);
    assert!(r.volumes[0].free_before > 0 && r.volumes[0].free_after > 0);
    assert_eq!(r.journal, fx.data.join("logs/actions.jsonl"));
}

#[test]
fn the_text_mode_counts_refused_items_as_skipped_and_exits_1_on_a_failed_item() {
    let fx = Fx::new();
    let list = fx.list(&["q/__pycache__", "Pictures"]);
    fs::set_permissions(fx.path("q"), fs::Permissions::from_mode(0o555)).unwrap();

    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap()]).output().unwrap();
    fs::set_permissions(fx.path("q"), fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(out.status.code(), Some(1), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("skipped 1 · failed 1"), "{text}");
    let pos = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("{needle:?} missing in:\n{text}"));
    assert!(pos("  failed   ~/q/__pycache__") < pos("  skipped  ~/Pictures"), "in list order");
    assert!(pos("  skipped  ~/Pictures") < pos("Journal:"));
}

#[test]
fn items_with_permanent_or_a_malformed_list_are_refused_untouched() {
    let fx = Fx::new();
    let list = fx.list(&["q/__pycache__"]);
    // The scan made the data directory; the refused runs must not make it again.
    fs::rename(&fx.data, fx.root.join("scan-data")).unwrap();
    let before = fx.snapshot();
    let malformed = fx.root.join("bad.json");
    fs::write(&malformed, r#"{"schema_version":1,"items":[{"path":"q/__pycache__","dev":1,"ino":1,"rule":"python.pycache","class":"safe","action":"delete"}]}"#).unwrap();

    for args in [
        vec!["clean", "--items", list.to_str().unwrap(), "--permanent"],
        vec!["clean", "--items", list.to_str().unwrap(), "--yes"],
        vec!["clean", "--items", malformed.to_str().unwrap()],
        vec!["clean", "--items", fx.root.join("missing.json").to_str().unwrap()],
    ] {
        let out = fx.ddc(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    assert_eq!(fx.snapshot(), before, "the tree is untouched");
    assert!(!fx.data.exists(), "no lock, no journal, no data directory");
}

#[test]
fn the_dry_run_prints_a_line_per_item_and_changes_nothing() {
    let fx = Fx::new();
    let list = fx.list(&["q/__pycache__", "p/node_modules", "Pictures"]);
    let before = fx.snapshot();

    // The list on stdin, as the app pipes it.
    let mut child = fx
        .ddc(&["clean", "--items", "-", "--dry-run", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&fs::read(&list).unwrap()).unwrap();
    let out = child.wait_with_output().unwrap();

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let lines: Vec<serde_json::Value> =
        String::from_utf8(out.stdout).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let predictions: Vec<&str> = lines.iter().filter_map(|l| l["prediction"].as_str()).collect();
    assert_eq!(predictions, ["would-delete", "would-delete", "would-skip"]);
    assert_eq!(lines[2]["reason"], "action-not-allowed");
    assert_eq!(lines.last().unwrap()["type"], "summary");
    assert_eq!(fx.snapshot(), before);
    assert!(!fx.data.join("logs/actions.jsonl").exists());
}

#[test]
fn sigterm_or_sighup_during_a_tool_ends_the_tool_and_ddc_with_the_report() {
    // SIGHUP: a closed terminal no longer reaches a tool in its own process group.
    for (name, sig) in [("TERM", libc::SIGTERM), ("HUP", libc::SIGHUP)] {
        let fx = Fx::new();
        let pid_file = fx.fake_uv();
        let list = fx.list(&[".cache/uv", "q/__pycache__"]);
        let mut child = fx
            .ddc(&["clean", "--items", list.to_str().unwrap(), "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            stdout.read_to_end(&mut buf).unwrap();
            buf
        });
        let tool = wait_for_pid(&pid_file, Duration::from_secs(60));

        Command::new("/bin/kill").args([&format!("-{name}"), &child.id().to_string()]).status().unwrap();
        let status = wait_within(&mut child, Duration::from_secs(1)).expect("ddc ends within a second");

        assert_eq!(status.signal(), Some(sig), "ddc ends by the signal");
        assert!(!alive(tool), "SIG{name}: the tool's group got the signal");
        let mut stderr = String::new();
        child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
        assert!(stderr.contains("stopping after the current item"), "said at once: {stderr}");
        let r = report(&reader.join().unwrap());
        assert!(r.cancelled);
        assert!(
            matches!(outcome(&r, &fx.path(".cache/uv")), Outcome::Native { commands } if commands.iter().all(|c| !c.success))
        );
        assert_eq!(outcome(&r, &fx.path("q/__pycache__")), &Outcome::NotRun);
        assert!(fx.path("q/__pycache__").exists());
        let journal = fx.journal();
        assert_eq!(journal.len(), 1, "the interrupted item, not the one never started: {journal:#?}");
        assert_eq!(journal[0]["rule"], "cache.uv");
        assert_eq!(journal[0]["commands"][0]["success"], false);
    }
}

#[test]
fn ctrl_c_in_a_terminal_reaches_the_tool_and_stops_the_clean_after_it() {
    let fx = Fx::new();
    let pid_file = fx.fake_uv();
    // Bigger than the uv cache, so the clean runs it first.
    fx.write("r/__pycache__/big.cpython-312.pyc", &[5u8; 400_000]);
    let ddc = fx.ddc(&["clean", "--yes"]);
    // A pseudo-terminal: `script` runs ddc as its foreground process group.
    // Its stdin stays an open pipe (end of file would reach ddc as end of input).
    let mut script = Command::new("/usr/bin/script");
    script.env_clear();
    for (k, v) in ddc.get_envs() {
        script.env(k, v.unwrap());
    }
    let mut child = script
        .args(["-q", "/dev/null", ddc.get_program().to_str().unwrap()])
        .args(ddc.get_args())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let tool = wait_for_pid(&pid_file, Duration::from_secs(60));

    child.stdin.as_mut().unwrap().write_all(b"\x03").unwrap();
    // `script` exits with the child's signal number (2 for SIGINT): never asserted.
    wait_within(&mut child, Duration::from_secs(10)).expect("the clean stops");

    assert!(!alive(tool), "Ctrl-C reached the tool in its own process group");
    let journal = fx.journal();
    let rules: Vec<&str> = journal.iter().map(|e| e["rule"].as_str().unwrap()).collect();
    assert_eq!(rules, ["python.pycache", "cache.uv"], "the item before, the interrupted one, nothing after");
    assert_eq!(journal[1]["commands"][0]["success"], false);
    assert!(!fx.path("r/__pycache__").exists(), "the item before it was cleaned");
    assert!(fx.path("q/__pycache__").exists(), "the item after it was not started");
}

/// A picked entry of the user's own folders, as the app lists it.
fn picked(path: &Path, rule: &str, action: &str) -> serde_json::Value {
    let md = fs::symlink_metadata(path).unwrap();
    serde_json::json!({
        "path": path, "dev": md.dev(), "ino": md.ino(),
        "rule": rule, "class": "user-data", "action": action,
    })
}

#[test]
fn a_picked_download_would_go_to_the_trash_unless_it_is_recent_and_names_are_escaped() {
    let fx = Fx::new();
    let day_ago = SystemTime::now() - Duration::from_secs(86_400);
    let tricky = "cod\u{202e}fdp.exe";
    for name in ["old.pdf", "older.pdf", tricky] {
        fx.write(&format!("Downloads/{name}"), &[4u8; 8_000]);
        ddc_core::sys::fsops::set_times(&fx.path(&format!("Downloads/{name}")), day_ago, day_ago).unwrap();
    }
    fx.write("Downloads/new.pdf", &[5u8; 8_000]);
    let items = [
        picked(&fx.path("Downloads/old.pdf"), "user.downloads", "trash"),
        picked(&fx.path("Downloads/new.pdf"), "user.downloads", "trash"),
        picked(&fx.path("Downloads/older.pdf"), "user.downloads", "delete"),
        picked(&fx.path(&format!("Downloads/{tricky}")), "user.downloads", "trash"),
    ];
    let list = fx.root.join("picked.json");
    fs::write(&list, serde_json::json!({ "schema_version": 1, "items": items }).to_string()).unwrap();
    let before = fx.snapshot();

    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap(), "--dry-run", "--json"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let lines: Vec<serde_json::Value> =
        String::from_utf8(out.stdout).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let predictions: Vec<&str> = lines.iter().filter_map(|l| l["prediction"].as_str()).collect();
    assert_eq!(predictions, ["would-trash", "would-skip", "would-skip", "would-trash"]);
    assert_eq!(lines[1]["reason"], "entry-locked");
    assert_eq!(lines[1]["lock"], serde_json::json!({ "reason": "recent" }));
    assert_eq!(lines[2]["reason"], "action-not-allowed");

    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap(), "--dry-run"]).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("would move to Trash  ~/Downloads/cod\\u{202e}fdp.exe"), "{text}");
    assert!(text.contains("locked: added or changed in the last 10 minutes"), "{text}");
    assert!(!text.contains('\u{202e}'), "the override never reaches the terminal");
    assert_eq!(fx.snapshot(), before, "a dry run changes nothing");

    // Permanent on a picked file refuses the whole list when it is read,
    // before the dry run (a picked file is never cleaned for real here).
    let mut permanent = items[0].clone();
    permanent["permanent"] = true.into();
    fs::write(&list, serde_json::json!({ "schema_version": 1, "items": [permanent] }).to_string()).unwrap();
    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap(), "--dry-run"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("permanent"));
    assert_eq!(fx.snapshot(), before);
}

/// Story "Release 0.2.1": a file a running process holds open inside a
/// finding that would be deleted (safety ADR, §5, amendment for 0.2.1).
#[test]
fn a_file_held_open_by_a_running_process_is_skipped_until_it_closes() {
    let fx = Fx::new();
    let pycache = fx.path("q/__pycache__");
    let list = fx.list(&["q/__pycache__"]);
    let holder = Holder::new(&pycache.join("m.cpython-312.pyc"));

    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap(), "--dry-run", "--json"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let line: serde_json::Value =
        serde_json::from_str(String::from_utf8(out.stdout).unwrap().lines().next().unwrap()).unwrap();
    assert_eq!((line["prediction"].as_str(), line["reason"].as_str()), (Some("would-skip"), Some("in-use")), "{line}");
    assert_eq!((line["pid"].as_i64(), line["name"].as_str()), (Some(i64::from(holder.pid())), Some("sleep")), "{line}");

    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap(), "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let expected = SkipReason::InUse { pid: holder.pid(), name: "sleep".into() };
    assert_eq!(outcome(&report(&out.stdout), &pycache), &Outcome::Skipped(expected));
    assert!(pycache.exists());

    drop(holder);
    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap(), "--json"]).output().unwrap();
    assert_eq!(outcome(&report(&out.stdout), &pycache), &Outcome::Deleted);
    assert!(!pycache.exists());
}

/// The incident of 2026-10-20: `uv cache clean` waited out its timeout on a
/// cache lock that running `uv run` processes held. Now the tool never starts.
#[test]
fn a_tool_never_starts_on_a_cache_a_running_process_holds_open() {
    let fx = Fx::new();
    let pid_file = fx.fake_uv();
    fx.write(".cache/uv/.lock", b"");
    let uv = fx.path(".cache/uv");
    let list = fx.list(&[".cache/uv"]);
    let holder = Holder::new(&uv.join(".lock"));

    let started = Instant::now();
    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap(), "--json"]).output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let expected = SkipReason::InUse { pid: holder.pid(), name: "sleep".into() };
    assert_eq!(outcome(&report(&out.stdout), &uv), &Outcome::Skipped(expected));
    assert!(!pid_file.exists(), "uv cache clean never started");
    assert!(started.elapsed() < Duration::from_secs(20), "skipped, not waited out: {:?}", started.elapsed());
    assert!(uv.join("wheels/a.whl").exists());
}
