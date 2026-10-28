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
use ddc_core::progress::ProgressLine;

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
        self.list_of(&serde_json::from_slice(&out.stdout).unwrap(), rels)
    }

    /// The findings at these paths in `report`, as the app lists them.
    fn list_of(&self, report: &serde_json::Value, rels: &[&str]) -> PathBuf {
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
        let before = fx.last_scan();
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
        // Story "Faster clean: re-check only what the selection needs", AC 6:
        // saved after a cancel too, with what was done; the not-run item stays.
        assert!(r.last_scan_saved);
        let after = fx.last_scan();
        assert_eq!(after["generated_at"], before["generated_at"]);
        assert!(found(&after, &fx.path("q/__pycache__")).is_some());
        assert!(found(&after, &fx.path(".cache/uv")).is_some(), "the tool removed nothing");
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

// Story "CLI: progress events": `--progress json` on stderr.

/// The event lines of `stderr`, each checked to read back as written; the
/// other lines are the CLI's own (`ddc: …`).
fn events(stderr: &str) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for line in stderr.lines().filter(|l| !l.trim().is_empty()) {
        if !line.starts_with('{') {
            assert!(line.starts_with("ddc:"), "not an event and not the CLI's own: {line:?}");
            continue;
        }
        let event: ProgressLine = serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}"));
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value, serde_json::from_str::<serde_json::Value>(line).unwrap(), "reads back as written");
        out.push(value);
    }
    out
}

fn kind(e: &serde_json::Value) -> String {
    match e["event"].as_str().unwrap() {
        "phase" => format!("phase {}", e["phase"].as_str().unwrap()),
        "walk" => format!("walk {}", e["step"].as_str().unwrap()),
        other => other.to_owned(),
    }
}

#[test]
fn a_clean_emits_its_phases_walks_and_items_in_order() {
    let fx = Fx::new();
    let list = fx.list(&["q/__pycache__", "p/node_modules"]);

    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap(), "--json", "--progress", "json"]).output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let events = events(&String::from_utf8_lossy(&out.stderr));
    let kinds: Vec<String> = events.iter().map(kind).collect();
    let at = |k: &str| kinds.iter().position(|x| x == k).unwrap_or_else(|| panic!("no {k}: {kinds:?}"));
    let last = |k: &str| kinds.iter().rposition(|x| x == k).unwrap();
    assert_eq!(kinds[0], "phase preparing");
    let cleaning = at("phase cleaning");
    assert!(at("walk recheck") < at("walk markers") && last("walk recheck") < at("walk markers"));
    assert!(last("walk markers") < cleaning);
    assert!(kinds[cleaning..].iter().all(|k| !k.starts_with("walk")), "no walk tick after cleaning: {kinds:?}");
    assert!(
        kinds[..cleaning].iter().all(|k| k != "items" && k != "item"),
        "no item counter before cleaning: {kinds:?}"
    );

    // Pruned to the list: the listed findings' files, besides what lies above them.
    let recheck = &events[last("walk recheck")];
    assert_eq!(recheck["files_expected"], 2, "the listed findings' files");
    assert!(recheck["files"].as_u64() >= recheck["files_expected"].as_u64(), "{recheck}");
    let markers = &events[last("walk markers")];
    assert_eq!(markers["files"], markers["files_expected"]);

    let items: Vec<&serde_json::Value> = events.iter().filter(|e| e["event"] == "item").collect();
    assert_eq!(items.iter().map(|e| e["i"].as_u64().unwrap()).collect::<Vec<_>>(), [1, 2]);
    assert!(items.iter().all(|e| e["n"] == 2
        && e["outcome"] == "deleted"
        && e["action"] == "delete"
        && e["bytes"].as_u64().unwrap() > 0));
    let fin = events.last().unwrap();
    assert_eq!(kind(fin), "items");
    assert_eq!((fin["done"].as_u64(), fin["n"].as_u64()), (Some(2), Some(2)));
    assert_eq!(fin["bytes_done"], fin["bytes_total"]);
}

#[test]
fn cleaning_is_on_stderr_before_the_first_action() {
    let fx = Fx::new();
    let pid_file = fx.fake_uv();
    let list = fx.list(&[".cache/uv", "q/__pycache__"]);
    let mut child = fx
        .ddc(&["clean", "--items", list.to_str().unwrap(), "--json", "--progress", "json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut buf = Vec::new();
        stdout.read_to_end(&mut buf).unwrap();
    });
    // Read as it comes, as the app does: a full pipe would block the clean.
    let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut stderr = child.stderr.take().unwrap();
    let sink = std::sync::Arc::clone(&seen);
    let err = std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut chunk) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().push_str(&String::from_utf8_lossy(&chunk[..n]));
        }
    });
    wait_for_pid(&pid_file, Duration::from_secs(60));

    // The tool runs: the first item's action has begun, none has finished.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !seen.lock().unwrap().contains(r#""phase":"cleaning""#) {
        assert!(Instant::now() < deadline, "no cleaning event while the tool runs: {}", seen.lock().unwrap());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(fx.journal().is_empty(), "nothing journaled yet");

    Command::new("/bin/kill").args(["-TERM", &child.id().to_string()]).status().unwrap();
    wait_within(&mut child, Duration::from_secs(5)).expect("ddc ends");
    out.join().unwrap();
    err.join().unwrap();
    let kinds: Vec<String> = events(&seen.lock().unwrap()).iter().map(kind).collect();
    assert!(kinds.contains(&"item".to_owned()), "the interrupted item still reports: {kinds:?}");
}

#[test]
fn without_the_flag_no_event_reaches_stderr() {
    let fx = Fx::new();
    let list = fx.list(&["q/__pycache__"]);
    for args in [vec!["scan", "--json"], vec!["clean", "--items", list.to_str().unwrap(), "--json"]] {
        let out = fx.ddc(&args).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.lines().all(|l| !l.starts_with('{')), "{args:?}: {stderr}");
    }
}

// ---- Story "Faster clean: re-check only what the selection needs" ----------

impl Fx {
    fn last_scan(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.data.join("last-scan.json")).unwrap()).unwrap()
    }

    /// Trees no list touches.
    fn unrelated(&self) {
        for i in 0..8 {
            self.write(&format!("big/t{i}/deep/f.bin"), b"x");
        }
    }

    fn clean_json(&self, list: &Path) -> (CleanReport, Vec<serde_json::Value>, String) {
        let out =
            self.ddc(&["clean", "--items", list.to_str().unwrap(), "--json", "--progress", "json"]).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
        (report(&out.stdout), events(&stderr), stderr)
    }
}

/// The final `walk recheck` tick.
fn recheck(events: &[serde_json::Value]) -> &serde_json::Value {
    events.iter().rev().find(|e| e["event"] == "walk" && e["step"] == "recheck").expect("a recheck tick")
}

fn found<'a>(report: &'a serde_json::Value, path: &Path) -> Option<&'a serde_json::Value> {
    report["findings"].as_array().unwrap().iter().find(|f| f["path"] == path.to_str().unwrap())
}

fn total(report: &serde_json::Value, key: &str) -> u64 {
    report["totals"][key].as_u64().unwrap()
}

/// AC 1 and AC 6: the walk is pruned to the list, and the report on disk is
/// the one the clean read, without what it deleted; no scan is needed after.
#[test]
fn a_pruned_clean_edits_the_report_it_read() {
    let fx = Fx::new();
    fx.unrelated();
    let list = fx.list(&["q/__pycache__", "p/node_modules"]);
    let before = fx.last_scan();
    let gone: Vec<serde_json::Value> =
        ["q/__pycache__", "p/node_modules"].iter().map(|r| found(&before, &fx.path(r)).unwrap().clone()).collect();
    let sum = |key: &str| gone.iter().map(|f| f[key].as_u64().unwrap()).sum::<u64>();

    let (clean, events, _) = fx.clean_json(&list);

    assert!(clean.last_scan_saved);
    assert!(clean.items.iter().all(|i| i.outcome == Outcome::Deleted), "{:?}", clean.items);
    assert!(recheck(&events)["files"].as_u64().unwrap() < total(&before, "files"), "the unrelated trees are not read");
    let after = fx.last_scan();
    assert_eq!(after["generated_at"], before["generated_at"], "no fresh scan of the whole home is claimed");
    assert_eq!(after["duration_ms"], before["duration_ms"]);
    assert!(gone.iter().all(|f| found(&after, Path::new(f["path"].as_str().unwrap())).is_none()));
    assert_eq!(total(&after, "scanned_bytes"), total(&before, "scanned_bytes") - sum("bytes"));
    assert_eq!(total(&after, "files"), total(&before, "files") - sum("files"));
    assert_eq!(total(&after, "dirs"), total(&before, "dirs") - sum("dirs"));
}

/// Story "Review window: after a clean, only skips that can clear stay
/// selected": an item the clean skipped as changed takes the re-check's
/// identity, so the app's next list from the saved report cleans it; a
/// missing one leaves the report.
#[test]
fn a_skipped_item_is_saved_as_the_recheck_found_it() {
    let fx = Fx::new();
    fx.unrelated();
    fx.write("r/__pycache__/m.cpython-312.pyc", &[2u8; 10_000]);
    let list = fx.list(&["q/__pycache__", "p/node_modules", "r/__pycache__"]);
    let old = found(&fx.last_scan(), &fx.path("p/node_modules")).unwrap()["ino"].clone();
    fs::rename(fx.path("p/node_modules"), fx.path("p/node_modules.old")).unwrap();
    fx.write("p/node_modules/dep/index.js", &[6u8; 20_000]);
    fs::rename(fx.path("r/__pycache__"), fx.path("r/pycache.old")).unwrap();

    let (clean, _, _) = fx.clean_json(&list);

    assert_eq!(outcome(&clean, &fx.path("q/__pycache__")), &Outcome::Deleted);
    assert_eq!(outcome(&clean, &fx.path("p/node_modules")), &Outcome::Skipped(SkipReason::ChangedSinceScan));
    let missing = outcome(&clean, &fx.path("r/__pycache__"));
    assert!(matches!(missing, Outcome::Skipped(SkipReason::Missing | SkipReason::ChangedSinceScan)), "{missing:?}");
    let saved = fx.last_scan();
    let swapped = found(&saved, &fx.path("p/node_modules")).expect("the swapped folder stays in the report");
    assert_ne!(swapped["ino"], old, "with the identity it has now");
    assert!(found(&saved, &fx.path("r/__pycache__")).is_none(), "a missing item leaves the report");

    let (again, _, _) = fx.clean_json(&fx.list_of(&saved, &["p/node_modules"]));

    assert_eq!(outcome(&again, &fx.path("p/node_modules")), &Outcome::Deleted, "{:?}", again.items);
    assert!(!fx.path("p/node_modules").exists());
}

/// AC 6: a tool-cleaned item is measured again after its tool ran.
#[test]
fn a_tool_cleaned_item_is_saved_at_its_new_size() {
    let fx = Fx::new();
    fx.write(".cache/uv/wheels/a.whl", &[4u8; 200_000]);
    fx.write(".cache/uv/keep.txt", &[5u8; 1_000]);
    let bin = fx.root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = "#!/bin/sh\ncase \"$1 $2\" in\n  \"cache dir\") echo \"$HOME/.cache/uv\" ;;\n  \"cache clean\") /bin/rm -rf \"$HOME/.cache/uv/wheels\" ;;\nesac\n";
    fs::write(bin.join("uv"), script).unwrap();
    fs::set_permissions(bin.join("uv"), fs::Permissions::from_mode(0o755)).unwrap();
    let list = fx.list(&[".cache/uv"]);
    let before = found(&fx.last_scan(), &fx.path(".cache/uv")).unwrap().clone();

    let (clean, _, _) = fx.clean_json(&list);

    assert!(matches!(outcome(&clean, &fx.path(".cache/uv")), Outcome::Native { .. }), "{:?}", clean.items);
    assert!(clean.last_scan_saved);
    let after = found(&fx.last_scan(), &fx.path(".cache/uv")).unwrap().clone();
    assert_eq!((after["files"].as_u64(), after["dirs"].as_u64()), (Some(1), Some(1)), "{after}");
    assert!(after["bytes"].as_u64() < before["bytes"].as_u64());
}

/// AC 3 (gate decision 10a): with no report of the clean's roots and home
/// to edit, the clean walks the whole home and saves that walk's report.
#[test]
fn without_a_report_of_these_roots_the_clean_walks_the_whole_home() {
    for case in ["none", "unreadable", "other roots"] {
        let fx = Fx::new();
        fx.unrelated();
        let list = fx.list(&["q/__pycache__"]);
        let whole = total(&fx.last_scan(), "files");
        let saved = fx.data.join("last-scan.json");
        match case {
            "none" => fs::rename(&saved, fx.root.join("held.json")).unwrap(),
            "unreadable" => fs::write(&saved, b"{ \"schema_version\": 1, ").unwrap(),
            _ => {
                let out = fx.ddc(&["scan", fx.path("p").to_str().unwrap()]).output().unwrap();
                assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
                assert_eq!(fx.last_scan()["scan_roots"], serde_json::json!([fx.path("p")]));
            }
        }

        let (clean, events, _) = fx.clean_json(&list);

        assert_eq!(outcome(&clean, &fx.path("q/__pycache__")), &Outcome::Deleted, "{case}");
        assert_eq!(recheck(&events)["files"], whole, "{case}: the whole home was walked");
        assert!(recheck(&events).get("files_expected").is_none(), "{case}: no estimate from another report");
        assert!(clean.last_scan_saved, "{case}");
        let after = fx.last_scan();
        assert_eq!(after["scan_roots"], serde_json::json!([fx.home]), "{case}");
        assert!(found(&after, &fx.path("q/__pycache__")).is_none(), "{case}");
        assert_eq!(total(&after, "files"), whole - 1, "{case}");
    }
}

/// Gate decision 10: an ancestor the pruned walk cannot read sends it to the
/// full walk, which cannot read it either; the item is refused.
#[test]
fn an_unreadable_ancestor_takes_the_full_walk() {
    let fx = Fx::new();
    fx.unrelated();
    let list = fx.list(&["q/__pycache__", "p/node_modules"]);
    let whole = total(&fx.last_scan(), "files");
    fs::set_permissions(fx.path("q"), fs::Permissions::from_mode(0o000)).unwrap();

    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap(), "--json", "--progress", "json"]).output().unwrap();
    fs::set_permissions(fx.path("q"), fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let clean = report(&out.stdout);
    assert_eq!(outcome(&clean, &fx.path("q/__pycache__")), &Outcome::Skipped(SkipReason::ChangedSinceScan));
    assert_eq!(outcome(&clean, &fx.path("p/node_modules")), &Outcome::Deleted);
    let events = events(&String::from_utf8_lossy(&out.stderr));
    assert_eq!(recheck(&events)["files"], whole - 1, "the whole home but what q holds");
    assert!(fx.path("q/__pycache__").exists());
}

/// Gate decision 9: a report that cannot be saved is a warning; the clean
/// report says so, and the app rescans.
#[test]
fn a_report_that_cannot_be_saved_is_a_warning_and_the_clean_says_so() {
    let fx = Fx::new();
    let list = fx.list(&["q/__pycache__"]);
    fs::create_dir_all(fx.data.join("logs")).unwrap();
    fs::set_permissions(&fx.data, fs::Permissions::from_mode(0o555)).unwrap();

    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap(), "--json"]).output().unwrap();
    fs::set_permissions(&fx.data, fs::Permissions::from_mode(0o755)).unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.contains("ddc: warning: cannot save the scan report:"), "{stderr}");
    let clean = report(&out.stdout);
    assert!(!clean.last_scan_saved);
    assert_eq!(outcome(&clean, &fx.path("q/__pycache__")), &Outcome::Deleted);
}

/// "Skip entirely" (safety ADR, amendment of 2026-10-28): an item at, under or
/// above a skip is kept, in the dry run and in the clean alike; the walk never
/// reads the skipped folder, so without that the clean would say "changed".
#[test]
fn an_item_at_under_or_above_a_skip_is_kept_in_the_dry_run_and_the_clean() {
    let fx = Fx::new();
    fx.write("q/__pycache__/sub/n.cpython-312.pyc", &[2u8; 1_000]);
    fx.write("r/__pycache__/o.cpython-312.pyc", &[2u8; 1_000]);
    fx.write("s/__pycache__/o.cpython-312.pyc", &[2u8; 1_000]);
    let list = fx.list(&["q/__pycache__", "r/__pycache__", "s/__pycache__"]);
    fs::write(
        fx.data.join("settings.json"),
        serde_json::json!({
            "schema_version": 1,
            "skips": [fx.path("q/__pycache__/sub"), fx.path("r/__pycache__"), fx.path("s")],
        })
        .to_string(),
    )
    .unwrap();
    let before = fx.snapshot();

    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap(), "--dry-run", "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let lines: Vec<serde_json::Value> =
        String::from_utf8(out.stdout).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let kept = lines.iter().filter(|l| l["prediction"] == "would-skip" && l["reason"] == "kept").count();
    assert_eq!(kept, 3, "above, at and under a skip: {lines:?}");

    let out = fx.ddc(&["clean", "--items", list.to_str().unwrap(), "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let r = report(&out.stdout);
    for rel in ["q/__pycache__", "r/__pycache__", "s/__pycache__"] {
        assert_eq!(outcome(&r, &fx.path(rel)), &Outcome::Skipped(SkipReason::Kept), "{rel}");
    }
    assert_eq!(fx.snapshot(), before, "nothing removed");
}
