//! The calls of `cases.json`, each on a fresh synthetic home (story "CLI
//! invocation cases: a table of ddc calls and the suggested commands they
//! print", DDC-005). A case states what its call must do; then every `ddc …`
//! its output suggests runs too, from the same folder, and a suggested clean
//! must cover the case's roots. Only the calls the allowlist names ever run:
//! never a real clean, never launchd, never a path outside the temporary root.
//! A command the platform refuses is expected to exit 69 with the core's own
//! message, read from `SUPPORTED` and `CAPABILITIES`, not skipped.

mod contract;

use ddc_core::platform::Canonical as _;
use std::collections::BTreeSet;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

use ddc_core::platform::{self, Capability};
use ddc_loadgen::isolate::ClearEnv as _;
use regex::Regex;
use serde_json::Value;

const CASES: &str = include_str!("cases.json");

/// The file's shape; an unknown key is an error, so a misspelt expectation
/// never passes silently.
mod format {
    use std::collections::BTreeMap;

    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct File {
        #[allow(dead_code)] // read by people
        pub about: String,
        pub fixture: Fixture,
        pub data_dir_commands: Vec<String>,
        pub cases: Vec<Case>,
        pub not_run: Vec<NotRun>,
        #[allow(dead_code)] // read by src/cli.rs's coverage test
        pub uncovered: Vec<Uncovered>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Fixture {
        #[serde(default)]
        pub dirs: Vec<String>,
        #[serde(default)]
        pub files: Vec<FixtureFile>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct FixtureFile {
        pub path: String,
        pub bytes: Option<usize>,
        pub text: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Case {
        pub name: String,
        pub group: Group,
        pub cwd: Option<String>,
        pub args: Vec<String>,
        /// `settings.json` in the data folder, whole.
        pub settings: Option<Value>,
        pub expect: Expect,
    }

    #[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
    #[serde(rename_all = "lowercase")]
    pub enum Group {
        Scan,
        Roots,
        Clean,
        Background,
        Commands,
    }

    /// What a call must do. `linux` and `windows` replace the keys they name
    /// on that platform.
    #[derive(Deserialize, Clone, Default)]
    #[serde(deny_unknown_fields)]
    pub struct Expect {
        pub exit: Option<i32>,
        /// `scan_roots` of `last-scan.json`, as a set.
        pub roots: Option<Vec<String>>,
        /// The rule ids of `last-scan.json`'s findings, as a set.
        pub rules: Option<Vec<String>>,
        pub stdout: Option<Text>,
        pub stderr: Option<Text>,
        pub files: Option<BTreeMap<String, Text>>,
        /// stdout valid against `docs/<schema>.schema.json`.
        pub schema: Option<String>,
        /// Every JSON line of stderr valid against `docs/<schema>.schema.json`.
        pub stderr_schema: Option<String>,
        pub linux: Option<Box<Expect>>,
        pub windows: Option<Box<Expect>>,
    }

    #[derive(Deserialize, Clone, Default)]
    #[serde(deny_unknown_fields)]
    pub struct Text {
        #[serde(default)]
        pub contains: Vec<String>,
        #[serde(default)]
        pub not_contains: Vec<String>,
        #[serde(default)]
        pub matches: Vec<String>,
        #[serde(default)]
        pub not_matches: Vec<String>,
        pub empty: Option<bool>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct NotRun {
        pub pattern: String,
        #[allow(dead_code)] // read by people
        pub reason: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Uncovered {
        #[allow(dead_code)] // read by src/cli.rs's coverage test
        pub name: String,
        #[allow(dead_code)] // read by people
        pub reason: String,
    }

    impl Expect {
        /// This platform's expectation: the overlay's keys over the base's.
        pub fn here(&self) -> Expect {
            let overlay = match ddc_core::platform::PLATFORM {
                ddc_core::platform::Platform::Linux => self.linux.as_deref(),
                ddc_core::platform::Platform::Windows => self.windows.as_deref(),
                _ => None,
            };
            let mut e = self.clone();
            if let Some(o) = overlay.cloned() {
                e.exit = o.exit.or(e.exit);
                e.roots = o.roots.or(e.roots);
                e.rules = o.rules.or(e.rules);
                e.stdout = o.stdout.or(e.stdout);
                e.stderr = o.stderr.or(e.stderr);
                e.files = o.files.or(e.files);
                e.schema = o.schema.or(e.schema);
                e.stderr_schema = o.stderr_schema.or(e.stderr_schema);
            }
            e
        }
    }
}

use format::{Case, Expect, File, Group, Text};

fn file() -> File {
    serde_json::from_str(CASES).unwrap_or_else(|e| panic!("cases.json: {e}"))
}

/// The commands, by the words that name them.
const COMMANDS: &[&str] = &[
    "scan",
    "clean",
    "rules",
    "disk",
    "disk agents",
    "disk free",
    "disk brew-packages",
    "keep remove",
    "schedule install",
    "schedule uninstall",
    "schedule status",
    "notify ack",
    "notify snooze",
    "du",
    "diag",
];

/// The command `argv` calls, and how many words name it; `""` for a bare
/// `--help` or `--version`.
fn command_path(argv: &[String]) -> (&'static str, usize) {
    let two = argv.get(..2).map(|w| w.join(" "));
    if let Some(c) = COMMANDS.iter().find(|c| Some(**c) == two.as_deref()) {
        return (c, 2);
    }
    match argv.first().and_then(|w| COMMANDS.iter().find(|c| **c == w.as_str())) {
        Some(c) => (c, 1),
        None => ("", 0),
    }
}

/// Where the case runs: a canonical temporary root with the home and the
/// data folder in it.
mod place {
    use ddc_core::platform::Canonical as _;
    use std::fs;
    use std::path::{Path, PathBuf};

    use serde_json::Value;

    use super::format::{Case, Fixture};

    pub struct Sandbox {
        _tmp: tempfile::TempDir,
        pub root: PathBuf,
        pub home: PathBuf,
        pub data: PathBuf,
    }

    impl Sandbox {
        pub fn new(fixture: &Fixture, case: Option<&Case>) -> Sandbox {
            let tmp = tempfile::tempdir().unwrap();
            // `/var/folders` is `/private/var/folders` on macOS: the root is
            // canonical before any placeholder is expanded.
            let root = tmp.path().canonical().unwrap();
            let sb = Sandbox { home: root.join("home"), data: root.join("data"), root, _tmp: tmp };
            for d in ["home", "data"].iter().copied().chain(fixture.dirs.iter().map(String::as_str)) {
                fs::create_dir_all(sb.root.join(sb.expand(d))).unwrap();
            }
            for f in &fixture.files {
                let p = sb.root.join(sb.expand(&f.path));
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                let bytes = match (&f.text, f.bytes) {
                    (Some(t), None) => t.clone().into_bytes(),
                    (None, Some(n)) => vec![7u8; n],
                    _ => panic!("{}: exactly one of text and bytes", f.path),
                };
                fs::write(p, bytes).unwrap();
            }
            if let Some(settings) = case.and_then(|c| c.settings.as_ref()) {
                let text = serde_json::to_string(&sb.expand_json(settings)).unwrap();
                fs::write(sb.data.join("settings.json"), text).unwrap();
            }
            sb
        }

        /// `s` with its placeholders replaced.
        pub fn expand(&self, s: &str) -> String {
            let mut out = s
                .replace("{root}", &self.root.display().to_string())
                .replace("{home}", &self.home.display().to_string())
                .replace("{data}", &self.data.display().to_string());
            let mut from = 0;
            while let Some(start) = out[from..].find('{').map(|i| from + i) {
                let Some(len) = out[start..].find('}') else { break };
                let inner = out[start + 1..start + len].to_owned();
                let value = match inner.split_once(':') {
                    Some(("rule", id)) => rule_path(id),
                    Some(("dev", rel)) => self.file_id(rel).0.to_string(),
                    Some(("ino", rel)) => self.file_id(rel).1.to_string(),
                    // Not a placeholder: left as it is.
                    _ => {
                        from = start + 1;
                        continue;
                    }
                };
                out.replace_range(start..=start + len, &value);
                from = start + value.len();
            }
            out
        }

        fn expand_json(&self, v: &Value) -> Value {
            match v {
                Value::String(s) => Value::String(self.expand(s)),
                Value::Array(a) => Value::Array(a.iter().map(|x| self.expand_json(x)).collect()),
                Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), self.expand_json(x))).collect()),
                other => other.clone(),
            }
        }

        /// A platform that cannot read the identity refuses the command
        /// before it would read it.
        fn file_id(&self, rel: &str) -> (u64, u64) {
            ddc_core::platform::meta::lstat(&self.root.join(rel)).map(|st| st.id()).unwrap_or((0, 0))
        }

        pub fn inside(&self, p: &Path) -> bool {
            super::normal(p).starts_with(&self.root)
        }
    }

    /// The first home-relative folder this platform's catalog looks in for
    /// rule `id`.
    fn rule_path(id: &str) -> String {
        use ddc_core::rules::Matcher;
        let rule = ddc_loadgen::fixtures::catalog()
            .iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("no rule {id} in this platform's catalog"));
        match rule.matcher {
            Matcher::KnownPaths(p) | Matcher::KnownRegion(p) | Matcher::KnownChildren(p) | Matcher::NoTraverse(p) => {
                p[0].to_owned()
            }
            _ => panic!("{id} names no folder"),
        }
    }
}

use place::Sandbox;

/// `p` canonical: its longest existing ancestor resolved by the file system,
/// links included, the rest with `.` and `..` resolved by hand.
fn normal(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonical() {
        return c;
    }
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => {
                out.push(other);
                if let Ok(c) = out.canonical() {
                    out = c;
                }
            }
        }
    }
    out
}

/// A suggestion's words, split as a POSIX shell splits them. A word that
/// starts with an unquoted `~` alone or `~/` is in the home.
mod split {
    use std::path::Path;

    pub fn words(span: &str, home: &Path) -> Result<Vec<String>, String> {
        let mut words = Vec::new();
        let mut word: Option<(String, bool)> = None;
        let mut chars = span.chars();
        while let Some(c) = chars.next() {
            match c {
                ' ' | '\t' | '\n' => {
                    if let Some(w) = word.take() {
                        words.push(w);
                    }
                }
                '\'' => {
                    let w = word.get_or_insert((String::new(), false));
                    loop {
                        match chars.next() {
                            Some('\'') => break,
                            Some(q) => w.0.push(q),
                            None => return Err(format!("an unclosed ' in {span}")),
                        }
                    }
                }
                '"' => {
                    let w = word.get_or_insert((String::new(), false));
                    loop {
                        match chars.next() {
                            Some('"') => break,
                            Some('\\') => match chars.next() {
                                Some(e @ ('"' | '\\' | '$' | '`')) => w.0.push(e),
                                Some(e) => {
                                    w.0.push('\\');
                                    w.0.push(e);
                                }
                                None => return Err(format!("an unclosed \" in {span}")),
                            },
                            Some(q) => w.0.push(q),
                            None => return Err(format!("an unclosed \" in {span}")),
                        }
                    }
                }
                '\\' => match chars.next() {
                    Some(e) => word.get_or_insert((String::new(), false)).0.push(e),
                    None => return Err(format!("a trailing \\ in {span}")),
                },
                '~' if word.is_none() => word = Some(("~".to_owned(), true)),
                other => word.get_or_insert((String::new(), false)).0.push(other),
            }
        }
        words.extend(word);
        let home = home.display().to_string();
        Ok(words
            .into_iter()
            .map(|(w, tilde)| match w.strip_prefix('~') {
                Some(rest) if tilde && (rest.is_empty() || rest.starts_with('/')) => format!("{home}{rest}"),
                _ => w,
            })
            .collect())
    }
}

/// What the runner may run. Anything not named here is refused by name.
mod allow {
    use std::path::Path;

    use super::{Sandbox, command_path};

    /// Options whose value is not a path.
    const VALUES: &[&str] = &[
        "--min-age",
        "--threads",
        "--progress",
        "--notify",
        "--threshold",
        "--platform",
        "--class",
        "--category",
        "--min-size",
        "--temp-entry-budget",
        "--folder-dev",
        "--folder-ino",
        "--bytes",
        "--until",
        "--reader",
        "--interval",
        "--local-snapshots",
    ];
    /// Options whose value is a path.
    const PATHS: &[&str] = &["--exclude", "--skip", "--data-dir", "--system-root", "--items", "--actions"];

    struct Parsed<'a> {
        flags: Vec<&'a str>,
        values: Vec<(&'a str, &'a str)>,
        paths: Vec<&'a str>,
    }

    fn parse(rest: &[String]) -> Parsed<'_> {
        let mut p = Parsed { flags: Vec::new(), values: Vec::new(), paths: Vec::new() };
        let mut i = 0;
        while i < rest.len() {
            let w = rest[i].as_str();
            let (name, inline) = match w.split_once('=') {
                Some((n, v)) if w.starts_with("--") => (n, Some(v)),
                _ => (w, None),
            };
            let takes_value = VALUES.contains(&name) || PATHS.contains(&name);
            if takes_value {
                let value = match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        rest.get(i).map_or("", String::as_str)
                    }
                };
                p.values.push((name, value));
                if PATHS.contains(&name) {
                    p.paths.push(value);
                }
            } else if name.starts_with("--") {
                p.flags.push(name);
            } else {
                // Anything else is a path: a word the allowlist cannot read fails closed.
                p.paths.push(w);
            }
            i += 1;
        }
        p
    }

    /// `Ok` when `argv`, run in `cwd`, is on the allowlist.
    pub fn check(argv: &[String], cwd: &Path, sb: &Sandbox) -> Result<(), String> {
        if matches!(argv, [one] if one == "--help" || one == "--version") {
            return Ok(());
        }
        let (path, words) = command_path(argv);
        let p = parse(&argv[words..]);
        let has = |f: &str| p.flags.contains(&f) || p.values.iter().any(|(n, _)| *n == f);
        let value = |f: &str| p.values.iter().find(|(n, _)| *n == f).map(|(_, v)| *v);
        let inside = |v: Option<&str>| v.is_some_and(|v| sb.inside(&cwd.join(v)));
        match path {
            "scan" if has("--background") && !has("--no-notify") && value("--notify") != Some("json") => {
                return Err("scan --background without --no-notify or --notify json can show a notification".into());
            }
            "scan" | "rules" | "du" | "diag" | "schedule status" => {}
            "clean" if !has("--dry-run") => return Err("clean without --dry-run".into()),
            "clean" => {
                if let Some(f) = ["--yes", "--items", "--permanent"].into_iter().find(|f| has(f)) {
                    return Err(format!("clean {f}"));
                }
            }
            "keep remove" | "notify ack" | "notify snooze" if !inside(value("--data-dir")) => {
                return Err(format!("{path} without --data-dir inside the temporary root"));
            }
            "keep remove" | "notify ack" | "notify snooze" => {}
            "disk" | "disk agents" | "disk brew-packages" if !inside(value("--system-root")) => {
                return Err(format!("{path} without --system-root inside the temporary root"));
            }
            "disk" | "disk agents" | "disk brew-packages" => {}
            "" => return Err(format!("{} names no command", argv.join(" "))),
            other => return Err(format!("{other} is never run")),
        }
        if !sb.inside(cwd) {
            return Err(format!("the folder {} is outside the temporary root", cwd.display()));
        }
        match p.paths.iter().find(|a| !sb.inside(&cwd.join(a))) {
            Some(outside) => Err(format!("{outside} is outside the temporary root")),
            None => Ok(()),
        }
    }
}

/// The suggestions of a case's output, run and compared.
mod follow {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use regex::Regex;

    use super::{Sandbox, allow, command_path, split};

    /// The backtick spans that start with `ddc `, once each, in order. Paired
    /// per line, so a stray backtick in a path hides nothing on later lines.
    pub fn spans(texts: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for line in texts.iter().flat_map(|t| t.lines()) {
            for (i, s) in line.split('`').enumerate() {
                if i % 2 == 1 && s.starts_with("ddc ") && !out.iter().any(|o| o == s) {
                    out.push(s.to_owned());
                }
            }
        }
        out
    }

    /// What one run of a suggestion did: exit 0 and the roots it scanned, if any.
    pub type Run<'a> = dyn FnMut(&[String]) -> Result<Option<BTreeSet<PathBuf>>, String> + 'a;

    pub struct Ctx<'a> {
        pub sb: &'a Sandbox,
        pub cwd: &'a Path,
        pub not_run: &'a [Regex],
        pub data_dir_commands: &'a [String],
    }

    /// The failures of following `texts`' suggestions, after a case that
    /// scanned `roots`.
    pub fn follow(texts: &[&str], roots: Option<&BTreeSet<PathBuf>>, ctx: &Ctx, run: &mut Run) -> Vec<String> {
        let mut failures = Vec::new();
        for span in spans(texts) {
            if ctx.not_run.iter().any(|r| r.is_match(&span)) {
                continue;
            }
            if span.contains(['<', '>', '|']) {
                failures.push(format!("`{span}` is a template no not_run entry names"));
                continue;
            }
            let mut argv = match split::words(&span, &ctx.sb.home) {
                Ok(w) => w[1..].to_vec(),
                Err(e) => {
                    failures.push(e);
                    continue;
                }
            };
            let (path, _) = command_path(&argv);
            if path == "clean" && !argv.iter().any(|a| a == "--dry-run") {
                argv.push("--dry-run".into());
            }
            if ctx.data_dir_commands.iter().any(|c| c == path) && !argv.iter().any(|a| a.starts_with("--data-dir")) {
                argv.extend(["--data-dir".into(), ctx.sb.data.display().to_string()]);
            }
            if let Err(why) = allow::check(&argv, ctx.cwd, ctx.sb) {
                failures.push(format!("`{span}` is refused: {why}"));
                continue;
            }
            match (run(&argv), path) {
                (Err(e), _) => failures.push(format!("`{span}`: {e}")),
                (Ok(scanned), "clean" | "scan") => match (roots, scanned) {
                    (Some(r), Some(s)) if *r == s => {}
                    (r, s) => failures.push(format!("`{span}` covers {s:?}, the case scanned {r:?}")),
                },
                (Ok(_), _) => {}
            }
        }
        failures
    }
}

fn spawn(sb: &Sandbox, cwd: &Path, argv: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .clear_env()
        .home_env(&sb.home)
        .env("USERPROFILE", &sb.home)
        .env("APPDATA", sb.home.join("AppData/Roaming"))
        .env("LOCALAPPDATA", sb.home.join("AppData/Local"))
        .env("DDC_DATA_DIR", &sb.data)
        .env("PATH", "")
        .env("NO_COLOR", "1")
        .current_dir(cwd)
        .args(argv)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// The roots `last-scan.json` names, canonical.
fn scanned_roots(sb: &Sandbox) -> Option<BTreeSet<PathBuf>> {
    let report: Value = serde_json::from_slice(&fs::read(sb.data.join("last-scan.json")).ok()?).ok()?;
    let roots = report["scan_roots"].as_array()?;
    Some(roots.iter().filter_map(Value::as_str).map(|r| normal(Path::new(r))).collect())
}

fn scanned_rules(sb: &Sandbox) -> BTreeSet<String> {
    let report: Value = serde_json::from_slice(&fs::read(sb.data.join("last-scan.json")).unwrap()).unwrap();
    report["findings"].as_array().unwrap().iter().filter_map(|f| f["rule"].as_str().map(str::to_owned)).collect()
}

/// The `ddc: …` message this build refuses `path` with, the core's own words
/// (`gate.rs`); `None` when it runs.
fn refusal(path: &str) -> Option<String> {
    if matches!(path, "rules" | "") {
        return None;
    }
    if let Err(u) = platform::require_supported() {
        return Some(format!("{u}: only --version, --help and rules run"));
    }
    let declared = platform::CAPABILITIES;
    let needs = match path {
        "schedule install" | "schedule uninstall" | "schedule status" => Capability::Schedule,
        "disk" => Capability::WholeDisk,
        "disk agents" => Capability::CodingAgents,
        "disk brew-packages" => Capability::Homebrew,
        "disk free" if !declared.contains(&Capability::WholeDisk) && !declared.contains(&Capability::Homebrew) => {
            Capability::WholeDisk
        }
        _ => return None,
    };
    platform::require(needs, declared, None).err().map(|e| e.to_string())
}

fn text_failures(what: &str, text: &str, t: &Text) -> Vec<String> {
    let mut f = Vec::new();
    for s in &t.contains {
        if !text.contains(s.as_str()) {
            f.push(format!("{what} lacks {s:?}"));
        }
    }
    for s in &t.not_contains {
        if text.contains(s.as_str()) {
            f.push(format!("{what} has {s:?}"));
        }
    }
    for r in &t.matches {
        if !Regex::new(r).unwrap().is_match(text) {
            f.push(format!("{what} does not match /{r}/"));
        }
    }
    for r in &t.not_matches {
        if Regex::new(r).unwrap().is_match(text) {
            f.push(format!("{what} matches /{r}/"));
        }
    }
    if let Some(empty) = t.empty
        && empty != text.is_empty()
    {
        f.push(format!("{what} is {}empty", if empty { "not " } else { "" }));
    }
    f
}

fn schema(name: &str, json: &[u8]) {
    use ddc_core::{agents, brew, disk, keep, model, notify, progress, refusal, rules};
    match name {
        "scan" => drop(contract::holds::<model::ScanReport>(name, json)),
        "notify" => drop(contract::holds::<notify::NotifyReport>(name, json)),
        "rules" => drop(contract::holds::<rules::listing::RuleList>(name, json)),
        "disk" => drop(contract::holds::<disk::DiskReport>(name, json)),
        "agents" => drop(contract::holds::<agents::AgentsReport>(name, json)),
        "keep" => drop(contract::holds::<keep::KeepReport>(name, json)),
        "brew-packages" => drop(contract::holds::<brew::BrewPackages>(name, json)),
        "error" => drop(contract::holds::<refusal::ErrorReport>(name, json)),
        "progress" => drop(contract::holds::<progress::ProgressLine>(name, json)),
        other => panic!("no schema {other}"),
    }
}

/// The case's failures; empty when it passed.
fn run_case(file: &File, not_run: &[Regex], case: &Case) -> Vec<String> {
    let sb = Sandbox::new(&file.fixture, Some(case));
    let cwd = case.cwd.as_deref().map_or_else(|| sb.root.clone(), |c| PathBuf::from(sb.expand(c)));
    let mut argv: Vec<String> = case.args.iter().map(|a| sb.expand(a)).collect();
    let (path, _) = command_path(&argv);
    if file.data_dir_commands.iter().any(|c| c == path) {
        argv.extend(["--data-dir".into(), sb.data.display().to_string()]);
    }
    if let Err(why) = allow::check(&argv, &cwd, &sb) {
        return vec![format!("the case is refused: {why}")];
    }
    let out = spawn(&sb, &cwd, &argv);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let expect: Expect = case.expect.here();
    let mut f = Vec::new();

    if let Some(message) = refusal(path) {
        if out.status.code() != Some(69) {
            f.push(format!("exit {:?}, not the refusal's 69", out.status.code()));
        }
        if !stderr.contains(&format!("ddc: {message}")) {
            f.push(format!("stderr lacks the refusal {message:?}"));
        }
        if !out.stdout.is_empty() {
            schema("error", &out.stdout);
        }
        return f;
    }

    if out.status.code() != expect.exit {
        f.push(format!("exit {:?}, expected {:?}", out.status.code(), expect.exit));
    }
    if let Some(t) = &expect.stdout {
        f.extend(text_failures("stdout", &stdout, t));
    }
    if let Some(t) = &expect.stderr {
        f.extend(text_failures("stderr", &stderr, t));
    }
    if let Some(name) = &expect.schema {
        schema(name, &out.stdout);
    }
    if let Some(name) = &expect.stderr_schema {
        let lines: Vec<&str> = stderr.lines().filter(|l| l.starts_with('{')).collect();
        if lines.is_empty() {
            f.push(format!("stderr has no {name} line"));
        }
        for l in lines {
            schema(name, l.as_bytes());
        }
    }
    let mut texts = vec![stdout.clone(), stderr.clone()];
    for (path, t) in expect.files.iter().flatten() {
        let p = sb.expand(path);
        match fs::read_to_string(&p) {
            Ok(text) => {
                f.extend(text_failures(&p, &text, t));
                texts.push(text);
            }
            Err(e) => f.push(format!("{p}: {e}")),
        }
    }
    let roots = scanned_roots(&sb);
    if let Some(want) = &expect.roots {
        let want: BTreeSet<PathBuf> = want.iter().map(|r| normal(Path::new(&sb.expand(r)))).collect();
        if roots.as_ref() != Some(&want) {
            f.push(format!("scanned {roots:?}, expected {want:?}"));
        }
    }
    if let Some(want) = &expect.rules {
        let want: BTreeSet<String> = want.iter().cloned().collect();
        let got = scanned_rules(&sb);
        if got != want {
            f.push(format!("found rules {got:?}, expected {want:?}"));
        }
    }

    // Then the advice, once the case's own output is checked: a dry run
    // rewrites last-scan.json.
    let ctx = follow::Ctx { sb: &sb, cwd: &cwd, not_run, data_dir_commands: &file.data_dir_commands };
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    f.extend(follow::follow(&texts, roots.as_ref(), &ctx, &mut |argv: &[String]| {
        let out = spawn(&sb, &cwd, argv);
        if !out.status.success() {
            return Err(format!("exit {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stderr)));
        }
        Ok(scanned_roots(&sb))
    }));
    f
}

/// Runs every case of `group` and fails once, naming each failing case
/// with its call and output.
fn group(g: Group) {
    let file = file();
    let not_run: Vec<Regex> = file.not_run.iter().map(|n| Regex::new(&n.pattern).unwrap()).collect();
    let mut failures = Vec::new();
    for case in file.cases.iter().filter(|c| c.group == g) {
        let found = catch_unwind(AssertUnwindSafe(|| run_case(&file, &not_run, case)))
            .unwrap_or_else(|p| vec![format!("panicked: {}", panic_text(&p))]);
        if !found.is_empty() {
            failures.push(format!("{}\n  `ddc {}`\n  - {}", case.name, case.args.join(" "), found.join("\n  - ")));
        }
    }
    assert!(failures.is_empty(), "{} failing case(s):\n{}", failures.len(), failures.join("\n\n"));
}

fn panic_text(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<String>()
        .cloned()
        .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default()
}

#[test]
fn scan_cases() {
    group(Group::Scan);
}

#[test]
fn roots_cases() {
    group(Group::Roots);
}

#[test]
fn clean_cases() {
    group(Group::Clean);
}

#[test]
fn background_cases() {
    group(Group::Background);
}

#[test]
fn command_cases() {
    group(Group::Commands);
}

#[test]
fn the_case_file_parses_and_its_names_are_unique() {
    let file = file();
    let mut names = BTreeSet::new();
    for c in &file.cases {
        assert!(names.insert(c.name.as_str()), "two cases are named {:?}", c.name);
        assert!(c.expect.exit.is_some(), "{}: no exit", c.name);
        for o in [&c.expect.linux, &c.expect.windows].into_iter().flatten() {
            assert!(o.linux.is_none() && o.windows.is_none(), "{}: an overlay in an overlay", c.name);
        }
        for t in [&c.expect.stdout, &c.expect.stderr]
            .into_iter()
            .flatten()
            .chain(c.expect.files.iter().flat_map(|m| m.values()))
        {
            for r in t.matches.iter().chain(&t.not_matches) {
                Regex::new(r).unwrap_or_else(|e| panic!("{}: {e}", c.name));
            }
        }
    }
    for n in &file.not_run {
        Regex::new(&n.pattern).unwrap();
    }
}

#[test]
fn the_splitter_reads_the_renderers_quoting() {
    let home = Path::new("/h");
    let split = |s: &str| split::words(s, home).unwrap();
    assert_eq!(split("ddc clean"), ["ddc", "clean"]);
    assert_eq!(split("ddc clean ~/Projects"), ["ddc", "clean", "/h/Projects"]);
    assert_eq!(split("ddc clean ~"), ["ddc", "clean", "/h"]);
    assert_eq!(split(r"ddc clean ~/'My Code' '/Volumes/it'\''s'"), ["ddc", "clean", "/h/My Code", "/Volumes/it's"]);
    assert_eq!(split(r"ddc clean ~/'a\u{1b}[2J'"), ["ddc", "clean", r"/h/a\u{1b}[2J"]);
    assert_eq!(
        split("ddc clean '~/x' ~user"),
        ["ddc", "clean", "~/x", "~user"],
        "quoted or another user's: not this home"
    );
    assert!(split::words("ddc clean 'open", home).is_err());
}

#[test]
fn the_allowlist_refuses_each_forbidden_call() {
    let file = file();
    let sb = Sandbox::new(&file.fixture, None);
    let data = sb.data.display().to_string();
    let home = sb.home.display().to_string();
    let sys = sb.root.join("sys").display().to_string();
    let check = |args: &[&str]| {
        let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        allow::check(&argv, &sb.home, &sb)
    };
    // Each refused call with a part of the reason it must give.
    for (refused, why) in [
        (&["scan", "--background"][..], "--no-notify"),
        (&["scan", "--background", "--threshold", "1K"], "--no-notify"),
        (&["clean"], "without --dry-run"),
        (&["clean", ".", "--data-dir", &data], "without --dry-run"),
        (&["clean", "--dry-run", "--yes"], "clean --yes"),
        (&["clean", "--dry-run", "--items", "list"], "clean --items"),
        (&["clean", "--dry-run", "--permanent"], "clean --permanent"),
        (&["schedule", "install"], "schedule install is never run"),
        (&["schedule", "uninstall"], "schedule uninstall is never run"),
        (&["disk", "free", "--dry-run", "--system-root", &sys], "disk free is never run"),
        (&["disk", "--json"], "without --system-root"),
        (&["disk", "agents", "--json"], "without --system-root"),
        (&["disk", "brew-packages", "--system-root", "/"], "without --system-root"),
        (&["keep", "remove", "x"], "without --data-dir"),
        (&["notify", "ack", "--bytes", "1", "--data-dir", "/tmp"], "without --data-dir"),
        (&["scan", "/"], "/ is outside"),
        (&["scan", "--skip", "/", &home], "/ is outside"),
        (&["clean", "--dry-run", "--exclude=/etc", &home], "/etc is outside"),
        (&["du", "../.."], "../.. is outside"),
        (&["scan", "--unknown-flag", "/"], "/ is outside"),
        (&["keep"], "names no command"),
        (&["help"], "names no command"),
    ] {
        match check(refused) {
            Err(e) => assert!(e.contains(why), "{refused:?}: {e:?} does not say {why:?}"),
            Ok(()) => panic!("{refused:?} is allowed"),
        }
    }
    for allowed in [
        &["scan", ".", "--data-dir", &data][..],
        &["scan", "--background", "--no-notify"],
        &["scan", "--background", "--notify", "json"],
        &["clean", "--dry-run", &home],
        &["rules", "--json"],
        &["du", "."],
        &["diag"],
        &["schedule", "status"],
        &["disk", "--system-root", &sys],
        &["keep", "remove", ".", "--data-dir", &data],
        &["--help"],
    ] {
        assert!(check(allowed).is_ok(), "{allowed:?} is refused: {:?}", check(allowed));
    }
}

/// The rule that would have caught 0.14.1's summary: after a scan of
/// `~/Projects`, a plain `ddc clean` reviews the home and fails; the one
/// that names `~/Projects` passes.
#[test]
fn plain_ddc_clean_after_a_scan_of_own_roots_fails_the_comparison() {
    let file = file();
    let sb = Sandbox::new(&file.fixture, None);
    let projects = sb.home.join("Projects");
    let roots: BTreeSet<PathBuf> = [projects.clone()].into();
    let ctx = follow::Ctx { sb: &sb, cwd: &sb.home, not_run: &[], data_dir_commands: &file.data_dir_commands };
    // A clean scans the paths it is given, else the home.
    let mut fake = |argv: &[String]| {
        let given: BTreeSet<PathBuf> =
            argv[1..].iter().take_while(|a| !a.starts_with("--")).map(|a| normal(Path::new(a))).collect();
        Ok(Some(if given.is_empty() { [sb.home.clone()].into() } else { given }))
    };
    let plain = follow::follow(&["run `ddc clean` to review and confirm"], Some(&roots), &ctx, &mut fake);
    assert_eq!(plain.len(), 1, "{plain:?}");
    let named = follow::follow(&["run `ddc clean ~/Projects` to review and confirm"], Some(&roots), &ctx, &mut fake);
    assert!(named.is_empty(), "{named:?}");
    let template = follow::follow(&["`ddc keep remove <folder>`"], Some(&roots), &ctx, &mut fake);
    assert_eq!(template.len(), 1, "a template no not_run names: {template:?}");
}

#[test]
fn an_overlay_replaces_only_the_keys_it_names_on_its_platform() {
    let e: Expect = serde_json::from_str(
        r#"{"exit": 0, "roots": ["{home}"], "stdout": {"contains": ["a"]},
            "linux": {"exit": 69}, "windows": {"stdout": {"contains": ["w"]}}}"#,
    )
    .unwrap();
    let here = e.here();
    let (exit, contains) = match platform::PLATFORM {
        platform::Platform::Linux => (Some(69), "a"),
        platform::Platform::Windows => (Some(0), "w"),
        _ => (Some(0), "a"),
    };
    assert_eq!(here.exit, exit);
    assert_eq!(here.stdout.unwrap().contains, [contains]);
    assert_eq!(here.roots.as_deref(), Some(&["{home}".to_owned()][..]), "a key no overlay names stays");
}
