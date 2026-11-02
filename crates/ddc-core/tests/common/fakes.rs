//! Fakes for the executor's ports: tools, the Trash and the process table.
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use ddc_core::env::Env;
use ddc_core::exec::{
    CmdOutput, CommandRunner, OpenVnode, ProcInfo, ProcStart, ProcessTable, SystemCommands, TrashError, TrashPlace,
    TrashTarget, Trasher,
};

/// Told each call of [`FakeCommands`] before it answers.
pub type CallHook = Box<dyn Fn(&str)>;

#[derive(Default)]
pub struct FakeCommands {
    pub tools: HashMap<&'static str, (bool, String)>,
    /// Answers for one exact call (`"simctl runtime list -j"`), before `tools`.
    pub answers: HashMap<String, (bool, String)>,
    pub calls: RefCell<Vec<String>>,
    /// The environment variables of each call, in the order of `calls`.
    pub envs: RefCell<Vec<Vec<String>>>,
    /// Runs on every call (to simulate the world changing during a command).
    pub on_run: Option<Box<dyn Fn()>>,
    /// The executable of each call, in the order of `calls`.
    pub exes: RefCell<Vec<PathBuf>>,
    /// Answers for one exact call, taken in order; the last one repeats.
    pub sequences: RefCell<HashMap<String, VecDeque<(bool, String)>>>,
    /// stderr for one exact call.
    pub stderr: HashMap<String, String>,
    /// One exact call fails to run (a timeout, say).
    pub errors: HashMap<String, io::ErrorKind>,
    /// Runs on every call with the call, before it answers.
    pub on_call: Option<CallHook>,
    /// The timeout of each call, in the order of `calls`.
    pub timeouts: RefCell<Vec<Duration>>,
    /// Programs that exist at exactly these absolute paths (each conda
    /// install's `bin/conda`): `at` finds them by their path, not their name.
    pub present: HashSet<PathBuf>,
    /// Answers for one exact call of one absolute path
    /// (`"/x/bin/conda config --show pkgs_dirs --json"`), before `answers`.
    pub by_path: HashMap<String, (bool, String)>,
    /// Each call with its absolute path, in the order of `calls`.
    pub path_calls: RefCell<Vec<String>>,
    /// The prefix each [`CommandRunner::run_cleared`] call cleared, in order.
    pub cleared: RefCell<Vec<String>>,
}

impl FakeCommands {
    /// A tool that exits with `success` and prints `stdout` for every call.
    pub fn with(mut self, program: &'static str, success: bool, stdout: &str) -> Self {
        self.tools.insert(program, (success, stdout.to_owned()));
        self
    }

    /// `program` answers `call` (its arguments joined by spaces) this way.
    pub fn answer(mut self, program: &'static str, call: &str, success: bool, stdout: &str) -> Self {
        self.tools.entry(program).or_insert((false, String::new()));
        self.answers.insert(format!("{program} {call}"), (success, stdout.to_owned()));
        self
    }

    /// `program` answers `call` with each of `answers` in turn, then the last again.
    pub fn answers_in_turn(mut self, program: &'static str, call: &str, answers: &[(bool, &str)]) -> Self {
        self.tools.entry(program).or_insert((false, String::new()));
        let queue = answers.iter().map(|(ok, out)| (*ok, (*out).to_owned())).collect();
        self.sequences.get_mut().insert(format!("{program} {call}"), queue);
        self
    }

    /// `call` of `program` also prints `text` on stderr.
    pub fn stderr_on(mut self, program: &'static str, call: &str, text: &str) -> Self {
        self.stderr.insert(format!("{program} {call}"), text.to_owned());
        self
    }

    /// `call` of `program` cannot run: `run` returns an error of `kind`.
    pub fn error_on(mut self, program: &'static str, call: &str, kind: io::ErrorKind) -> Self {
        self.tools.entry(program).or_insert((false, String::new()));
        self.errors.insert(format!("{program} {call}"), kind);
        self
    }

    /// A program at `exe` that answers `call` this way; any other call of
    /// it fails with no output, unless `answers` has it.
    pub fn at_path(mut self, exe: &Path, call: &str, success: bool, stdout: &str) -> Self {
        self.present.insert(exe.to_path_buf());
        self.by_path.insert(format!("{} {call}", exe.display()), (success, stdout.to_owned()));
        self
    }

    pub fn ran(&self, call: &str) -> bool {
        self.calls.borrow().iter().any(|c| c == call)
    }

    /// The calls of the program at `exe`, its arguments joined by spaces.
    pub fn calls_of(&self, exe: &Path) -> Vec<String> {
        let prefix = format!("{} ", exe.display());
        self.path_calls.borrow().iter().filter_map(|c| c.strip_prefix(&prefix).map(str::to_owned)).collect()
    }
}

impl CommandRunner for FakeCommands {
    fn find(&self, program: &str) -> Option<PathBuf> {
        self.tools.contains_key(program).then(|| PathBuf::from("/fake/bin").join(program))
    }
    /// A system tool at an absolute path, known by its file name.
    fn at(&self, exe: &Path) -> Option<PathBuf> {
        if self.present.contains(exe) {
            return Some(exe.to_path_buf());
        }
        let name = exe.file_name()?.to_str()?;
        self.tools.contains_key(name).then(|| exe.to_path_buf())
    }
    fn run(&self, exe: &Path, args: &[String], env: &[(&str, &str)], timeout: Duration) -> io::Result<CmdOutput> {
        let name = exe.file_name().unwrap().to_str().unwrap();
        let call = format!("{name} {}", args.join(" "));
        let path_call = format!("{} {}", exe.display(), args.join(" "));
        self.calls.borrow_mut().push(call.clone());
        self.path_calls.borrow_mut().push(path_call.clone());
        self.exes.borrow_mut().push(exe.to_path_buf());
        self.envs.borrow_mut().push(env.iter().map(|(k, v)| format!("{k}={v}")).collect());
        self.timeouts.borrow_mut().push(timeout);
        if let Some(hook) = &self.on_run {
            hook();
        }
        if let Some(hook) = &self.on_call {
            hook(&call);
        }
        if let Some(kind) = self.errors.get(&call) {
            return Err(io::Error::from(*kind));
        }
        let queued = self
            .sequences
            .borrow_mut()
            .get_mut(&call)
            .and_then(|q| if q.len() > 1 { q.pop_front() } else { q.front().cloned() });
        let (success, stdout) = queued.unwrap_or_else(|| {
            if let Some(answer) = self.by_path.get(&path_call) {
                return answer.clone();
            }
            if self.present.contains(exe) {
                return self.answers.get(&call).cloned().unwrap_or((false, String::new()));
            }
            self.answers.get(&call).unwrap_or(&self.tools[name]).clone()
        });
        let stderr = self.stderr.get(&call).cloned().unwrap_or_default();
        Ok(CmdOutput { success, code: Some(i32::from(!success)), stdout, stderr })
    }
    /// As [`run`](CommandRunner::run); `cleared` is recorded in `cleared`.
    fn run_cleared(
        &self,
        exe: &Path,
        args: &[String],
        cleared: &str,
        env: &[(&str, &str)],
        timeout: Duration,
    ) -> io::Result<CmdOutput> {
        self.cleared.borrow_mut().push(cleared.to_owned());
        self.run(exe, args, env, timeout)
    }
}

/// One call [`SharedCommands`] saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// The prefix of the inherited variables removed; `None` for a plain `run`.
    pub cleared: Option<String>,
    pub timeout: Duration,
}

/// A runner a scan's parallel probes can share (`Send + Sync`, unlike
/// [`FakeCommands`]): it records every call behind a mutex, and runs the
/// real tools through `inner`, or answers from `answers` for the programs
/// at the paths of `present`.
#[derive(Default)]
pub struct SharedCommands {
    pub inner: Option<SystemCommands>,
    /// Without `inner`: the only programs that exist, by absolute path.
    pub present: HashSet<PathBuf>,
    /// Without `inner`: `"<exe> <args joined by spaces>"` → exit 0 or not, stdout.
    /// Any other call fails to start.
    pub answers: HashMap<String, (bool, String)>,
    pub calls: Mutex<Vec<Recorded>>,
}

impl SharedCommands {
    /// The system's tools, as a scan in `env` finds them, recorded.
    pub fn system(env: &Env) -> Self {
        Self { inner: Some(SystemCommands::new(env)), ..Self::default() }
    }

    /// Only the programs at `present`, each answering as `answers` says.
    pub fn answering(present: &[&str], answers: &[(&str, bool, &str)]) -> Self {
        Self {
            present: present.iter().map(PathBuf::from).collect(),
            answers: answers.iter().map(|(call, ok, out)| ((*call).to_owned(), (*ok, (*out).to_owned()))).collect(),
            ..Self::default()
        }
    }

    pub fn calls(&self) -> Vec<Recorded> {
        self.calls.lock().unwrap().clone()
    }

    /// The calls of a program named `git`.
    pub fn git_calls(&self) -> Vec<Recorded> {
        self.calls().into_iter().filter(|c| c.exe.file_name().is_some_and(|n| n == "git")).collect()
    }

    fn record(&self, exe: &Path, args: &[String], cleared: Option<&str>, env: &[(&str, &str)], timeout: Duration) {
        self.calls.lock().unwrap().push(Recorded {
            exe: exe.to_path_buf(),
            args: args.to_vec(),
            env: env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
            cleared: cleared.map(str::to_owned),
            timeout,
        });
    }

    fn answer(&self, exe: &Path, args: &[String]) -> io::Result<CmdOutput> {
        let call = format!("{} {}", exe.display(), args.join(" "));
        match self.answers.get(&call) {
            Some((success, stdout)) => Ok(CmdOutput {
                success: *success,
                code: Some(i32::from(!success)),
                stdout: stdout.clone(),
                stderr: String::new(),
            }),
            None => Err(io::Error::new(io::ErrorKind::NotFound, call)),
        }
    }
}

impl CommandRunner for SharedCommands {
    fn find(&self, program: &str) -> Option<PathBuf> {
        self.inner.as_ref().and_then(|i| i.find(program))
    }
    fn at(&self, exe: &Path) -> Option<PathBuf> {
        match &self.inner {
            Some(inner) => inner.at(exe),
            None => self.present.contains(exe).then(|| exe.to_path_buf()),
        }
    }
    fn run(&self, exe: &Path, args: &[String], env: &[(&str, &str)], timeout: Duration) -> io::Result<CmdOutput> {
        self.record(exe, args, None, env, timeout);
        match &self.inner {
            Some(inner) => inner.run(exe, args, env, timeout),
            None => self.answer(exe, args),
        }
    }
    fn run_cleared(
        &self,
        exe: &Path,
        args: &[String],
        cleared: &str,
        env: &[(&str, &str)],
        timeout: Duration,
    ) -> io::Result<CmdOutput> {
        self.record(exe, args, Some(cleared), env, timeout);
        match &self.inner {
            Some(inner) => inner.run_cleared(exe, args, cleared, env, timeout),
            None => self.answer(exe, args),
        }
    }
}

/// Moves into a directory of the test tree instead of the real Trash.
pub struct FakeTrash(pub PathBuf);

impl Trasher for FakeTrash {
    fn trash(&self, target: &TrashTarget<'_>) -> Result<Option<TrashPlace>, TrashError> {
        fs::create_dir_all(&self.0)?;
        fs::rename(target.path, self.0.join(target.name))?;
        Ok(None)
    }
}

/// A fixed process table. `starts` and `vnodes` answer by pid; a pid
/// without an entry is gone (no start) or has nothing open.
#[derive(Default)]
pub struct FakeProcs {
    pub procs: Vec<ProcInfo>,
    pub starts: HashMap<i32, ProcStart>,
    pub vnodes: HashMap<i32, Vec<OpenVnode>>,
}

impl FakeProcs {
    pub fn new(procs: Vec<ProcInfo>) -> Self {
        Self { procs, ..Self::default() }
    }

    /// The same table, where `pid` started as `start` says.
    pub fn with_start(mut self, pid: i32, start: ProcStart) -> Self {
        self.starts.insert(pid, start);
        self
    }

    /// The same table, where `pid` holds `vnodes` open.
    pub fn with_vnodes(mut self, pid: i32, vnodes: Vec<OpenVnode>) -> Self {
        self.vnodes.insert(pid, vnodes);
        self
    }
}

impl ProcessTable for FakeProcs {
    fn processes(&self) -> Vec<ProcInfo> {
        self.procs.clone()
    }
    fn open_files(&self, pid: i32) -> Vec<PathBuf> {
        self.procs.iter().find(|p| p.pid == pid).map(|p| p.open_files.clone()).unwrap_or_default()
    }
    fn process_start(&self, pid: i32) -> Option<ProcStart> {
        self.starts.get(&pid).copied()
    }
    fn open_vnodes(&self, pid: i32) -> Vec<OpenVnode> {
        self.vnodes.get(&pid).cloned().unwrap_or_default()
    }
}

/// A table that changes after its first read: the first `processes()` call
/// answers from `first`, every later read (the whole table or one process)
/// from `then`. The guard's snapshot, then its re-reads.
pub struct ChangingProcs {
    pub first: Vec<ProcInfo>,
    pub then: Vec<ProcInfo>,
    pub reads: Cell<usize>,
    /// By pid, as of the re-reads: one process's facts are always read after the table.
    pub starts: HashMap<i32, ProcStart>,
    pub vnodes: HashMap<i32, Vec<OpenVnode>>,
}

impl ChangingProcs {
    pub fn new(first: Vec<ProcInfo>, then: Vec<ProcInfo>) -> Self {
        Self { first, then, reads: Cell::new(0), starts: HashMap::new(), vnodes: HashMap::new() }
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
    fn process_start(&self, pid: i32) -> Option<ProcStart> {
        self.reads.set(self.reads.get() + 1);
        self.starts.get(&pid).copied()
    }
    fn open_vnodes(&self, pid: i32) -> Vec<OpenVnode> {
        self.reads.set(self.reads.get() + 1);
        self.vnodes.get(&pid).cloned().unwrap_or_default()
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
