//! Fakes for the executor's ports: tools, the Trash and the process table.
#![allow(dead_code)]

use std::cell::RefCell;
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
    fn run(&self, exe: &Path, args: &[String], _: Duration) -> io::Result<CmdOutput> {
        let name = exe.file_name().unwrap().to_str().unwrap();
        self.calls.borrow_mut().push(format!("{name} {}", args.join(" ")));
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
