//! Fakes for the executor's ports: tools, the Trash and the process table.
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ddc_core::exec::{CmdOutput, CommandRunner, ProcessTable, Trasher};
use ddc_core::sys::procs::ProcInfo;

#[derive(Default)]
pub struct FakeCommands {
    pub tools: HashMap<&'static str, (bool, String)>,
    pub calls: RefCell<Vec<String>>,
    /// The environment variables of each call, in the order of `calls`.
    pub envs: RefCell<Vec<Vec<String>>>,
    /// Runs on every call (to simulate the world changing during a command).
    pub on_run: Option<Box<dyn Fn()>>,
}

impl FakeCommands {
    /// A tool that exits with `success` and prints `stdout` for every call.
    pub fn with(mut self, program: &'static str, success: bool, stdout: &str) -> Self {
        self.tools.insert(program, (success, stdout.to_owned()));
        self
    }
}

impl CommandRunner for FakeCommands {
    fn find(&self, program: &str) -> Option<PathBuf> {
        self.tools.contains_key(program).then(|| PathBuf::from("/fake/bin").join(program))
    }
    fn run(&self, exe: &Path, args: &[String], env: &[(&str, &str)], _: Duration) -> io::Result<CmdOutput> {
        let name = exe.file_name().unwrap().to_str().unwrap();
        self.calls.borrow_mut().push(format!("{name} {}", args.join(" ")));
        self.envs.borrow_mut().push(env.iter().map(|(k, v)| format!("{k}={v}")).collect());
        if let Some(hook) = &self.on_run {
            hook();
        }
        let (success, stdout) = self.tools[name].clone();
        Ok(CmdOutput { success, code: Some(i32::from(!success)), stdout, stderr: String::new() })
    }
}

/// Moves into a directory of the test tree instead of the real Trash.
pub struct FakeTrash(pub PathBuf);

impl Trasher for FakeTrash {
    fn trash(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(&self.0)?;
        fs::rename(path, self.0.join(path.file_name().unwrap()))
    }
}

pub struct FakeProcs(pub Vec<ProcInfo>);

impl ProcessTable for FakeProcs {
    fn processes(&self) -> Vec<ProcInfo> {
        self.0.clone()
    }
    fn open_files(&self, pid: i32) -> Vec<PathBuf> {
        self.0.iter().find(|p| p.pid == pid).map(|p| p.open_files.clone()).unwrap_or_default()
    }
}

/// A table that changes after its first read: the first `processes()` call
/// answers from `first`, every later read (the whole table or one process)
/// from `then`. The guard's snapshot, then its re-reads.
pub struct ChangingProcs {
    pub first: Vec<ProcInfo>,
    pub then: Vec<ProcInfo>,
    pub reads: Cell<usize>,
}

impl ChangingProcs {
    pub fn new(first: Vec<ProcInfo>, then: Vec<ProcInfo>) -> Self {
        Self { first, then, reads: Cell::new(0) }
    }
}

impl ProcessTable for ChangingProcs {
    fn processes(&self) -> Vec<ProcInfo> {
        let n = self.reads.get();
        self.reads.set(n + 1);
        if n == 0 { self.first.clone() } else { self.then.clone() }
    }
    fn open_files(&self, pid: i32) -> Vec<PathBuf> {
        self.reads.set(self.reads.get() + 1);
        self.then.iter().find(|p| p.pid == pid).map(|p| p.open_files.clone()).unwrap_or_default()
    }
}

/// A process with files open, as the guard sees it.
pub fn holder(pid: i32, name: &str, open: &[&Path]) -> ProcInfo {
    ProcInfo {
        pid,
        name: name.to_owned(),
        open_files: open.iter().map(|p| p.to_path_buf()).collect(),
        ..ProcInfo::default()
    }
}

/// A process running from an app bundle, as the guard sees it.
pub fn app_process(pid: i32, exe: &str) -> ProcInfo {
    let exe = PathBuf::from(exe);
    ProcInfo {
        pid,
        name: exe.file_name().unwrap().to_string_lossy().into_owned(),
        exe: Some(exe),
        ..ProcInfo::default()
    }
}
