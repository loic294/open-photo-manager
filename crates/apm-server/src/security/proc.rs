//! Running external tools (exiftool, ffmpeg, vips, heif-convert, ...).
//!
//! Every external process of the application goes through [`run`] (one-shot)
//! or [`ChildGuard`] (long-lived, e.g. `exiftool -stay_open`). They enforce:
//!
//! - **No shell.** The program is started with `Command::new` and an argument
//!   vector. Nothing is ever interpolated into a command line.
//! - **A clean environment.** The environment is cleared; the child gets only
//!   `PATH` (ours, or a default), `LANG`/`LC_ALL=C.UTF-8`, a `HOME` chosen by the
//!   caller, and the variables the caller lists explicitly.
//! - **Bounded time and memory.** A timeout and a cap per output stream; the
//!   output is never buffered without limit.
//! - **No stragglers.** The child leads its own process group. On timeout, on an
//!   output overflow or when the calling future is dropped, the whole group is
//!   killed, then the child is reaped. A tool's helper processes die with it.
//! - **No option injection.** [`arg_path`] turns any path into an argument that
//!   a tool cannot mistake for an option.
//!
//! ```no_run
//! use std::path::Path;
//! use std::time::Duration;
//!
//! use apm_server::security::proc::{ProcSpec, run};
//!
//! # async fn demo(file: &Path, scratch: &Path) -> Result<(), Box<dyn std::error::Error>> {
//! let output = run(ProcSpec::new("exiftool")
//!     .args(["-json", "-n", "--"]) // `--`: whatever follows is a file name
//!     .path_arg(file) // and `./` makes sure it is not an option anyway
//!     .home(scratch)
//!     .timeout(Duration::from_secs(20))
//!     .max_stdout(2 << 20))
//! .await?;
//! println!("{} bytes of JSON", output.stdout.len());
//! # Ok(())
//! # }
//! ```
//!
//! # Limits
//!
//! The process group is only killed while the child has not been reaped yet
//! (afterwards its id could belong to someone else). A tool that exits
//! normally but leaves a background process behind is therefore not cleaned
//! up, unless that process keeps the tool's output pipes open, in which case
//! [`run`] waits for the timeout and kills it then. None of the tools we run
//! daemonise.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process_group};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_STDOUT: usize = 16 << 20;
const DEFAULT_MAX_STDERR: usize = 1 << 20;
const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
const NO_HOME: &str = "/nonexistent";
/// How long to wait for a killed child to be reaped before giving up on it
/// (tokio's orphan reaper collects it later).
const REAP_TIMEOUT: Duration = Duration::from_secs(5);
/// How much of a failing tool's stderr appears in the error message.
const STDERR_TAIL_BYTES: usize = 512;

/// What to run and under which limits. Build it with [`ProcSpec::new`] and the
/// chained methods, or fill the public fields.
#[derive(Clone)]
pub struct ProcSpec {
    /// Program name (looked up in `PATH`) or absolute path.
    pub program: OsString,
    pub args: Vec<OsString>,
    /// Wall-clock limit for the whole run (default 30 s).
    pub timeout: Duration,
    /// Output above this many bytes kills the child (default 16 MiB).
    pub max_stdout_bytes: usize,
    /// Same for stderr (default 1 MiB).
    pub max_stderr_bytes: usize,
    /// Bytes written to the child's stdin; stdin is `/dev/null` when `None`.
    pub stdin: Option<Vec<u8>>,
    pub cwd: Option<PathBuf>,
    /// Value of `HOME` (many tools write caches or config there). Defaults to
    /// `cwd`, else to a directory that does not exist.
    pub home: Option<PathBuf>,
    /// Extra environment variables with explicit values.
    pub env: Vec<(OsString, OsString)>,
    /// Names of variables to copy from our environment (when set).
    pub env_inherit: Vec<OsString>,
}

impl ProcSpec {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            max_stdout_bytes: DEFAULT_MAX_STDOUT,
            max_stderr_bytes: DEFAULT_MAX_STDERR,
            stdin: None,
            cwd: None,
            home: None,
            env: Vec::new(),
            env_inherit: Vec::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Appends a file path as an argument that cannot be parsed as an option
    /// (see [`arg_path`]).
    pub fn path_arg(mut self, path: &Path) -> Self {
        self.args.push(arg_path(path));
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn max_stdout(mut self, bytes: usize) -> Self {
        self.max_stdout_bytes = bytes;
        self
    }

    pub fn max_stderr(mut self, bytes: usize) -> Self {
        self.max_stderr_bytes = bytes;
        self
    }

    pub fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn home(mut self, dir: impl Into<PathBuf>) -> Self {
        self.home = Some(dir.into());
        self
    }

    pub fn env(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }

    pub fn inherit_env(mut self, name: impl Into<OsString>) -> Self {
        self.env_inherit.push(name.into());
        self
    }

    fn program_name(&self) -> String {
        self.program.to_string_lossy().into_owned()
    }

    /// The configured command: cleared environment, own process group, killed
    /// when dropped. Stdio is left to the caller.
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command.env_clear();
        command.env(
            "PATH",
            std::env::var_os("PATH")
                .filter(|path| !path.is_empty())
                .unwrap_or_else(|| OsString::from(DEFAULT_PATH)),
        );
        command.env("LANG", "C.UTF-8");
        command.env("LC_ALL", "C.UTF-8");
        let home = self
            .home
            .as_deref()
            .or(self.cwd.as_deref())
            .unwrap_or_else(|| Path::new(NO_HOME));
        command.env("HOME", home);
        for name in &self.env_inherit {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        for (name, value) in &self.env {
            command.env(name, value);
        }
        if let Some(dir) = &self.cwd {
            command.current_dir(dir);
        }
        command.kill_on_drop(true);
        command.process_group(0);
        command
    }
}

impl fmt::Debug for ProcSpec {
    /// Never prints stdin or environment values (they may be secrets).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcSpec")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("timeout", &self.timeout)
            .field("max_stdout_bytes", &self.max_stdout_bytes)
            .field("max_stderr_bytes", &self.max_stderr_bytes)
            .field("stdin_bytes", &self.stdin.as_ref().map(Vec::len))
            .field("cwd", &self.cwd)
            .field("home", &self.home)
            .field(
                "env",
                &self.env.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .field("env_inherit", &self.env_inherit)
            .finish()
    }
}

/// Everything a successful run printed.
pub struct ProcOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl fmt::Debug for ProcOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcOutput")
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .finish()
    }
}

/// An output stream of a child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl fmt::Display for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        })
    }
}

/// Why [`run`] did not produce an output. Messages never contain the
/// arguments or environment, and only a short tail of stderr.
#[derive(thiserror::Error)]
pub enum ProcError {
    /// The program could not be started (not installed, not executable, bad
    /// arguments).
    #[error("cannot start {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("i/o error while running {program}: {source}")]
    Io {
        program: String,
        #[source]
        source: io::Error,
    },
    /// Did not finish in time; the process group was killed.
    #[error("{program} did not finish within {after:?} and was killed")]
    Timeout { program: String, after: Duration },
    /// Printed more than the cap; the process group was killed.
    #[error("{program} wrote more than {limit} bytes to {stream} and was killed")]
    OutputTooLarge {
        program: String,
        stream: Stream,
        limit: usize,
    },
    /// Exited with a non-zero status or was killed by a signal. The captured
    /// output is kept, since some tools report partial results this way.
    #[error("{program} {}: {}", describe_exit(.code, .signal), stderr_tail(.stderr))]
    Failed {
        program: String,
        code: Option<i32>,
        signal: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
}

impl ProcError {
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout { .. })
    }

    /// The program is not installed (or not found in `PATH`).
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Spawn { source, .. } if source.kind() == io::ErrorKind::NotFound)
    }

    /// The exit code of a [`ProcError::Failed`] that exited normally.
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            Self::Failed { code, .. } => *code,
            _ => None,
        }
    }
}

impl fmt::Debug for ProcError {
    /// Shows output sizes, never the output itself.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { program, source } => f
                .debug_struct("Spawn")
                .field("program", program)
                .field("source", source)
                .finish(),
            Self::Io { program, source } => f
                .debug_struct("Io")
                .field("program", program)
                .field("source", source)
                .finish(),
            Self::Timeout { program, after } => f
                .debug_struct("Timeout")
                .field("program", program)
                .field("after", after)
                .finish(),
            Self::OutputTooLarge {
                program,
                stream,
                limit,
            } => f
                .debug_struct("OutputTooLarge")
                .field("program", program)
                .field("stream", stream)
                .field("limit", limit)
                .finish(),
            Self::Failed {
                program,
                code,
                signal,
                stdout,
                stderr,
            } => f
                .debug_struct("Failed")
                .field("program", program)
                .field("code", code)
                .field("signal", signal)
                .field("stdout_bytes", &stdout.len())
                .field("stderr_bytes", &stderr.len())
                .finish(),
        }
    }
}

fn describe_exit(code: &Option<i32>, signal: &Option<i32>) -> String {
    match (code, signal) {
        (Some(code), _) => format!("exited with status {code}"),
        (None, Some(signal)) => format!("was killed by signal {signal}"),
        (None, None) => "failed".to_owned(),
    }
}

/// The last bytes of `stderr` as one line of text.
fn stderr_tail(stderr: &[u8]) -> String {
    let start = stderr.len().saturating_sub(STDERR_TAIL_BYTES);
    let text = String::from_utf8_lossy(&stderr[start..]);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        "(no error output)".to_owned()
    } else {
        lines.join(" | ")
    }
}

/// Turns `path` into an argument that no tool can parse as an option or as a
/// special syntax (`-`, `--help`, `@listfile`, `pipe:0`, `http://...`):
/// absolute paths and paths already starting with `./` or `../` pass through,
/// everything else gets a `./` prefix. An empty path stays empty (the tool
/// rejects it) instead of silently becoming the current directory.
///
/// Also put `--` before positional paths when the tool supports it.
pub fn arg_path(path: &Path) -> OsString {
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty()
        || bytes.starts_with(b"/")
        || bytes.starts_with(b"./")
        || bytes.starts_with(b"../")
    {
        return path.as_os_str().to_owned();
    }
    let mut out = OsString::with_capacity(bytes.len() + 2);
    out.push("./");
    out.push(path.as_os_str());
    out
}

/// Kills the child's whole process group when dropped, unless disarmed.
///
/// Arm it right after spawning and disarm it as soon as the child has been
/// reaped: the group id is the child's pid, which may be reused once the child
/// is gone, so a group must never be signalled after that.
struct GroupKill(Option<Pid>);

impl GroupKill {
    fn new(child_id: Option<u32>) -> Self {
        Self(
            child_id
                .and_then(|id| i32::try_from(id).ok())
                .and_then(Pid::from_raw),
        )
    }

    /// Sends SIGKILL to the group now (an already empty group is fine).
    fn kill(&self) {
        if let Some(pgid) = self.0 {
            let _ = kill_process_group(pgid, Signal::KILL);
        }
    }

    /// The child has been reaped: never signal the group again.
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for GroupKill {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Runs a program to completion and returns what it printed.
///
/// `Ok` means exit status 0. A non-zero status is [`ProcError::Failed`] with
/// the output attached. On any error the child (and its process group) is
/// dead and reaped when this returns. If the returned future is dropped
/// before completion the group is killed as well.
pub async fn run(spec: ProcSpec) -> Result<ProcOutput, ProcError> {
    let program = spec.program_name();
    let mut command = spec.command();
    command
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|source| ProcError::Spawn {
        program: program.clone(),
        source,
    })?;
    // Declared after `child` so it is dropped (and kills the group) first.
    let mut group = GroupKill::new(child.id());

    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(io_error(&program, io::Error::other("missing output pipe")));
    };
    let stdin = child.stdin.take();

    let io = async {
        let (stdout, stderr, ()) = tokio::try_join!(
            read_capped(stdout, spec.max_stdout_bytes, Stream::Stdout, &program),
            read_capped(stderr, spec.max_stderr_bytes, Stream::Stderr, &program),
            feed_stdin(stdin, spec.stdin.as_deref(), &program),
        )?;
        let status = child
            .wait()
            .await
            .map_err(|source| io_error(&program, source))?;
        Ok::<_, ProcError>((status, stdout, stderr))
    };
    let outcome = tokio::time::timeout(spec.timeout, io).await;

    match outcome {
        Ok(Ok((status, stdout, stderr))) => {
            group.disarm();
            if status.success() {
                Ok(ProcOutput { stdout, stderr })
            } else {
                Err(ProcError::Failed {
                    program,
                    code: status.code(),
                    signal: status.signal(),
                    stdout,
                    stderr,
                })
            }
        }
        Ok(Err(err)) => {
            kill_and_reap(&mut child, &mut group).await;
            Err(err)
        }
        Err(_elapsed) => {
            kill_and_reap(&mut child, &mut group).await;
            Err(ProcError::Timeout {
                program,
                after: spec.timeout,
            })
        }
    }
}

fn io_error(program: &str, source: io::Error) -> ProcError {
    ProcError::Io {
        program: program.to_owned(),
        source,
    }
}

/// Kills the group while the leader is still unreaped, then reaps the leader.
async fn kill_and_reap(child: &mut Child, group: &mut GroupKill) {
    group.kill();
    let _ = child.start_kill();
    if matches!(
        tokio::time::timeout(REAP_TIMEOUT, child.wait()).await,
        Ok(Ok(_))
    ) {
        group.disarm();
    }
}

/// Reads `reader` to EOF, failing as soon as more than `limit` bytes arrive.
async fn read_capped<R: AsyncRead + Unpin>(
    reader: R,
    limit: usize,
    stream: Stream,
    program: &str,
) -> Result<Vec<u8>, ProcError> {
    let mut buf = Vec::new();
    let cap = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    reader
        .take(cap)
        .read_to_end(&mut buf)
        .await
        .map_err(|source| io_error(program, source))?;
    if buf.len() > limit {
        return Err(ProcError::OutputTooLarge {
            program: program.to_owned(),
            stream,
            limit,
        });
    }
    Ok(buf)
}

/// Writes `data` to the child's stdin and closes it. A child that stops
/// reading (closed pipe) is not an error: its exit status tells the story.
async fn feed_stdin(
    stdin: Option<ChildStdin>,
    data: Option<&[u8]>,
    program: &str,
) -> Result<(), ProcError> {
    let (Some(mut stdin), Some(data)) = (stdin, data) else {
        return Ok(());
    };
    match stdin.write_all(data).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(err) => Err(io_error(program, err)),
    }
}

/// The last bytes a child wrote to stderr, shared with the draining task.
#[derive(Clone, Default)]
struct StderrTail(Arc<Mutex<Vec<u8>>>);

impl StderrTail {
    fn push(&self, chunk: &[u8], cap: usize) {
        let mut buf = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        buf.extend_from_slice(chunk);
        if buf.len() > cap {
            let excess = buf.len() - cap;
            buf.drain(..excess);
        }
    }

    fn text(&self) -> String {
        let buf = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&buf).into_owned()
    }
}

async fn drain_stderr(mut stderr: ChildStderr, tail: StderrTail, cap: usize) {
    let mut chunk = [0u8; 8192];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => tail.push(&chunk[..read], cap),
        }
    }
}

/// A long-lived child process (for example `exiftool -stay_open`) that cannot
/// outlive its owner.
///
/// The child runs with the same rules as [`run`] (no shell, cleared
/// environment, own process group), with stdin and stdout piped for the
/// caller and stderr drained in the background (only the last
/// `max_stderr_bytes` are kept, see [`ChildGuard::stderr_tail`]), so a chatty
/// tool never blocks on a full pipe. Dropping the guard kills the child's whole
/// process group; [`ChildGuard::shutdown`] is the polite way out.
///
/// The `timeout`, `max_stdout_bytes` and `stdin` fields of the spec do not apply
/// here: the owner drives the conversation through the pipes and is responsible
/// for bounding what it reads.
///
/// Typical pattern: `spawn`, `take_stdin` / `take_stdout` once, keep the guard
/// next to the pipes in one struct, talk to the tool, then `shutdown` (after
/// asking it to exit, e.g. `-stay_open False`). If the tool dies on its own,
/// `try_wait` reports it and the owner spawns a new guard.
pub struct ChildGuard {
    // Declared first: the group is killed before `child` is dropped.
    group: GroupKill,
    child: Child,
    program: String,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: StderrTail,
    drain: Option<JoinHandle<()>>,
}

impl ChildGuard {
    /// Starts the child. Must be called inside a Tokio runtime.
    pub fn spawn(spec: &ProcSpec) -> Result<Self, ProcError> {
        let program = spec.program_name();
        let mut command = spec.command();
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|source| ProcError::Spawn {
            program: program.clone(),
            source,
        })?;
        let group = GroupKill::new(child.id());
        let stderr = StderrTail::default();
        let drain = child.stderr.take().map(|pipe| {
            tokio::spawn(drain_stderr(
                pipe,
                stderr.clone(),
                spec.max_stderr_bytes.max(1),
            ))
        });
        Ok(Self {
            group,
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            child,
            program,
            stderr,
            drain,
        })
    }

    /// The process id while the child has not been reaped.
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// The program name, for log messages.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The child's stdin. Hand it out once; dropping it closes the pipe.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    /// The child's stdout. Hand it out once.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    /// The last bytes the child wrote to stderr, as lossy text.
    pub fn stderr_tail(&self) -> String {
        self.stderr.text()
    }

    /// `Some(status)` once the child has exited (it is reaped by this call).
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.group.disarm();
        }
        Ok(status)
    }

    /// Closes stdin, gives the child `grace` to exit by itself, then kills its
    /// process group. Returns the exit status, or `None` if the child could not
    /// be reaped in time (it is dead or dying; tokio collects it later).
    pub async fn shutdown(mut self, grace: Duration) -> Option<ExitStatus> {
        drop(self.stdin.take());
        if let Ok(Ok(status)) = tokio::time::timeout(grace, self.child.wait()).await {
            self.group.disarm();
            return Some(status);
        }
        self.group.kill();
        let _ = self.child.start_kill();
        match tokio::time::timeout(REAP_TIMEOUT, self.child.wait()).await {
            Ok(Ok(status)) => {
                self.group.disarm();
                Some(status)
            }
            _ => None,
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(task) = self.drain.take() {
            task.abort();
        }
    }
}

impl fmt::Debug for ChildGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChildGuard")
            .field("program", &self.program)
            .field("id", &self.child.id())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::Instant;

    use rustix::io::Errno;
    use rustix::process::test_kill_process;

    use super::*;
    use crate::security::test_support::scratch_dir;

    /// Whether `program` is installed; tests that need a tool skip otherwise.
    fn have(program: &str) -> bool {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
    }

    macro_rules! require {
        ($($program:literal),+) => {
            for program in [$($program),+] {
                if !have(program) {
                    eprintln!("skipping: {program} is not installed");
                    return;
                }
            }
        };
    }

    fn sh(script: &str) -> ProcSpec {
        ProcSpec::new("sh").args(["-c", script])
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    fn pid_is_gone(pid: i32) -> bool {
        Pid::from_raw(pid).is_some_and(|pid| test_kill_process(pid) == Err(Errno::SRCH))
    }

    /// Polls until `check` holds, for at most `secs` seconds.
    async fn eventually(secs: u64, mut check: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if check() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        check()
    }

    /// A shell snippet that appends to `$1` every 20 ms from a background
    /// subshell, then sleeps in the foreground.
    const TICKER: &str = "(while :; do echo t >> \"$1\"; sleep 0.02; done) & sleep 30";

    async fn assert_ticker_stopped(file: &Path) {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let first = std::fs::metadata(file).unwrap().len();
        assert!(first > 0, "the ticker never ran: the test proves nothing");
        tokio::time::sleep(Duration::from_millis(300)).await;
        let second = std::fs::metadata(file).unwrap().len();
        assert_eq!(first, second, "the background process is still running");
    }

    #[tokio::test]
    async fn success_captures_stdout_and_stderr() {
        require!("sh");
        let output = run(sh("echo out; echo err >&2")).await.unwrap();
        assert_eq!(text(&output.stdout), "out\n");
        assert_eq!(text(&output.stderr), "err\n");
    }

    #[tokio::test]
    async fn a_non_zero_exit_is_failed_and_keeps_the_output() {
        require!("sh");
        let err = run(sh("echo partial; echo oops >&2; exit 3"))
            .await
            .unwrap_err();
        assert_eq!(err.exit_code(), Some(3));
        let message = err.to_string();
        assert!(message.contains("status 3"), "{message}");
        assert!(message.contains("oops"), "{message}");
        match err {
            ProcError::Failed {
                code,
                signal,
                stdout,
                stderr,
                ..
            } => {
                assert_eq!((code, signal), (Some(3), None));
                assert_eq!(text(&stdout), "partial\n");
                assert_eq!(text(&stderr), "oops\n");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_signal_death_is_reported_as_a_signal() {
        require!("sh");
        let err = run(sh("kill -9 $$")).await.unwrap_err();
        match &err {
            ProcError::Failed { code, signal, .. } => {
                assert_eq!((*code, *signal), (None, Some(9)));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(err.to_string().contains("signal 9"), "{err}");
        assert_eq!(err.exit_code(), None);
    }

    #[tokio::test]
    async fn a_missing_program_is_a_spawn_error() {
        let err = run(ProcSpec::new("apm-definitely-not-installed"))
            .await
            .unwrap_err();
        assert!(matches!(err, ProcError::Spawn { .. }), "{err:?}");
        assert!(err.is_not_found());
        assert!(!err.is_timeout());
        assert!(err.to_string().contains("apm-definitely-not-installed"));
    }

    #[tokio::test]
    async fn a_timeout_kills_the_child_and_its_whole_process_group() {
        require!("sh", "sleep");
        let dir = scratch_dir();
        let file = dir.path().join("ticks");

        let started = Instant::now();
        let err = run(sh(TICKER)
            .arg("sh")
            .arg(&file)
            .timeout(Duration::from_millis(400)))
        .await
        .unwrap_err();
        assert!(err.is_timeout(), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_ticker_stopped(&file).await;
    }

    #[tokio::test]
    async fn a_straggler_holding_the_output_open_is_killed_at_the_timeout() {
        require!("sh", "sleep");
        let dir = scratch_dir();
        let file = dir.path().join("ticks");
        // The shell exits at once; the ticker keeps stdout open.
        let err = run(sh("(while :; do echo t >> \"$1\"; sleep 0.02; done) &")
            .arg("sh")
            .arg(&file)
            .timeout(Duration::from_millis(400)))
        .await
        .unwrap_err();
        assert!(err.is_timeout(), "{err:?}");
        assert_ticker_stopped(&file).await;
    }

    #[tokio::test]
    async fn dropping_the_future_kills_the_group() {
        require!("sh", "sleep");
        let dir = scratch_dir();
        let file = dir.path().join("ticks");
        let spec = sh(TICKER)
            .arg("sh")
            .arg(&file)
            .timeout(Duration::from_secs(60));

        let result = tokio::time::timeout(Duration::from_millis(400), run(spec)).await;
        assert!(result.is_err(), "the run should have been cancelled");
        assert_ticker_stopped(&file).await;
    }

    #[tokio::test]
    async fn no_zombie_is_left_behind() {
        require!("sh", "sleep");
        let dir = scratch_dir();
        let pid_file = dir.path().join("pid");

        // After a timeout.
        let err = run(sh("echo $$ > \"$1\"; sleep 30")
            .arg("sh")
            .arg(&pid_file)
            .timeout(Duration::from_millis(400)))
        .await
        .unwrap_err();
        assert!(err.is_timeout());
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(
            pid_is_gone(pid),
            "pid {pid} still exists (killed but not reaped?)"
        );

        // After a normal exit and after an output overflow.
        run(sh("echo $$ > \"$1\"").arg("sh").arg(&pid_file))
            .await
            .unwrap();
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(pid_is_gone(pid), "pid {pid} still exists");

        let err = run(sh("echo $$ > \"$1\"; yes")
            .arg("sh")
            .arg(&pid_file)
            .max_stdout(1000))
        .await
        .unwrap_err();
        assert!(matches!(err, ProcError::OutputTooLarge { .. }), "{err:?}");
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(pid_is_gone(pid), "pid {pid} still exists");
    }

    #[tokio::test]
    async fn the_output_cap_kills_the_child() {
        require!("yes");
        let started = Instant::now();
        let err = run(ProcSpec::new("yes").max_stdout(1000))
            .await
            .unwrap_err();
        match err {
            ProcError::OutputTooLarge { stream, limit, .. } => {
                assert_eq!((stream, limit), (Stream::Stdout, 1000));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn the_stderr_cap_applies_too() {
        require!("sh", "yes");
        let err = run(sh("yes >&2").max_stderr(100)).await.unwrap_err();
        match err {
            ProcError::OutputTooLarge { stream, limit, .. } => {
                assert_eq!((stream, limit), (Stream::Stderr, 100));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_cap_is_inclusive() {
        require!("sh");
        let at_limit = run(sh("printf 12345").max_stdout(5)).await.unwrap();
        assert_eq!(text(&at_limit.stdout), "12345");
        let over = run(sh("printf 123456").max_stdout(5)).await.unwrap_err();
        assert!(matches!(over, ProcError::OutputTooLarge { .. }), "{over:?}");
        let empty = run(sh("printf ''").max_stdout(0)).await.unwrap();
        assert!(empty.stdout.is_empty());
    }

    #[tokio::test]
    async fn arg_path_keeps_dash_files_from_becoming_options() {
        require!("cat");
        let dir = scratch_dir();
        std::fs::write(dir.path().join("-weird.txt"), "dash content").unwrap();

        let raw = run(ProcSpec::new("cat").cwd(dir.path()).arg("-weird.txt"))
            .await
            .unwrap_err();
        assert!(matches!(raw, ProcError::Failed { .. }), "{raw:?}");

        let safe = run(ProcSpec::new("cat")
            .cwd(dir.path())
            .path_arg(Path::new("-weird.txt")))
        .await
        .unwrap();
        assert_eq!(text(&safe.stdout), "dash content");
    }

    #[test]
    fn arg_path_rules() {
        let cases = [
            ("/abs/file.jpg", "/abs/file.jpg"),
            ("//double/slash", "//double/slash"),
            ("./already", "./already"),
            ("../parent", "../parent"),
            ("-rf", "./-rf"),
            ("--help", "./--help"),
            ("-", "./-"),
            ("plain.jpg", "./plain.jpg"),
            ("dir/-x", "./dir/-x"),
            (".hidden", "./.hidden"),
            ("@listfile", "./@listfile"),
            ("pipe:0", "./pipe:0"),
            ("http://host/x", "./http://host/x"),
            ("..", "./.."),
            (".", "./."),
            ("", ""),
        ];
        for (input, expected) in cases {
            assert_eq!(
                arg_path(Path::new(input)),
                OsString::from(expected),
                "{input}"
            );
        }
    }

    #[tokio::test]
    async fn the_environment_is_cleared_to_an_allow_list() {
        require!("env");
        if std::env::var_os("CARGO_MANIFEST_DIR").is_none() {
            eprintln!("skipping: no sentinel variable in the environment");
            return;
        }
        let dir = scratch_dir();
        let output = run(ProcSpec::new("env").home(dir.path())).await.unwrap();
        let listing = text(&output.stdout);
        let vars: BTreeSet<&str> = listing
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name))
            .collect();
        assert!(!vars.contains("CARGO_MANIFEST_DIR"), "{vars:?}");
        for expected in ["PATH", "HOME", "LANG", "LC_ALL"] {
            assert!(vars.contains(expected), "{expected} missing: {vars:?}");
        }
        // macOS may add this one itself.
        let unexpected: Vec<_> = vars
            .iter()
            .filter(|name| {
                !["PATH", "HOME", "LANG", "LC_ALL", "__CF_USER_TEXT_ENCODING"].contains(name)
            })
            .collect();
        assert!(unexpected.is_empty(), "{unexpected:?}");
        assert!(listing.contains("LANG=C.UTF-8"), "{listing}");
        assert!(listing.contains("LC_ALL=C.UTF-8"), "{listing}");
        assert!(
            listing.contains(&format!("HOME={}", dir.path().display())),
            "{listing}"
        );
    }

    #[tokio::test]
    async fn home_defaults_to_the_cwd_and_otherwise_to_nowhere() {
        require!("env");
        let dir = scratch_dir();
        let with_cwd = run(ProcSpec::new("env").cwd(dir.path())).await.unwrap();
        assert!(
            text(&with_cwd.stdout).contains(&format!("HOME={}", dir.path().display())),
            "{}",
            text(&with_cwd.stdout)
        );
        let bare = run(ProcSpec::new("env")).await.unwrap();
        assert!(text(&bare.stdout).contains("HOME=/nonexistent"));
    }

    #[tokio::test]
    async fn listed_variables_are_passed_and_inherited_ones_copied() {
        require!("env");
        let Some(manifest) = std::env::var_os("CARGO_MANIFEST_DIR") else {
            eprintln!("skipping: no sentinel variable in the environment");
            return;
        };
        let output = run(ProcSpec::new("env")
            .inherit_env("CARGO_MANIFEST_DIR")
            .inherit_env("APM_SURELY_UNSET_VARIABLE")
            .env("APM_EXPLICIT", "value 1")
            .env("LANG", "xx_XX.override"))
        .await
        .unwrap();
        let listing = text(&output.stdout);
        assert!(
            listing.contains(&format!(
                "CARGO_MANIFEST_DIR={}",
                manifest.to_string_lossy()
            )),
            "{listing}"
        );
        assert!(listing.contains("APM_EXPLICIT=value 1"), "{listing}");
        assert!(!listing.contains("APM_SURELY_UNSET_VARIABLE"), "{listing}");
        assert!(
            listing.contains("LANG=xx_XX.override"),
            "explicit wins: {listing}"
        );
    }

    #[tokio::test]
    async fn stdin_is_null_unless_provided() {
        require!("cat");
        let started = Instant::now();
        let output = run(ProcSpec::new("cat")).await.unwrap();
        assert!(output.stdout.is_empty());
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn stdin_is_passed_through() {
        require!("cat");
        let output = run(ProcSpec::new("cat").stdin("hello stdin"))
            .await
            .unwrap();
        assert_eq!(text(&output.stdout), "hello stdin");
    }

    #[tokio::test]
    async fn large_stdin_and_stdout_do_not_deadlock() {
        require!("cat");
        let data = vec![b'x'; 4 << 20];
        let output = run(ProcSpec::new("cat")
            .stdin(data.clone())
            .max_stdout(8 << 20)
            .timeout(Duration::from_secs(30)))
        .await
        .unwrap();
        assert_eq!(output.stdout, data);
    }

    #[tokio::test]
    async fn a_child_that_ignores_stdin_is_not_an_error() {
        require!("sh");
        let output = run(sh("exit 0").stdin(vec![b'x'; 4 << 20])).await.unwrap();
        assert!(output.stdout.is_empty());
    }

    #[tokio::test]
    async fn cwd_is_applied() {
        require!("pwd");
        let dir = scratch_dir();
        let output = run(ProcSpec::new("pwd").cwd(dir.path())).await.unwrap();
        assert_eq!(text(&output.stdout).trim(), dir.path().to_str().unwrap());
    }

    #[test]
    fn debug_output_hides_stdin_and_environment_values() {
        let spec = ProcSpec::new("tool")
            .arg("--flag")
            .stdin("super-secret-stdin")
            .env("API_KEY", "super-secret-value");
        let shown = format!("{spec:?}");
        assert!(shown.contains("tool"), "{shown}");
        assert!(shown.contains("API_KEY"), "{shown}");
        assert!(!shown.contains("super-secret"), "{shown}");

        let err = ProcError::Failed {
            program: "tool".into(),
            code: Some(1),
            signal: None,
            stdout: b"secret stdout".to_vec(),
            stderr: b"secret stderr".to_vec(),
        };
        assert!(!format!("{err:?}").contains("secret"), "{err:?}");
        let out = ProcOutput {
            stdout: b"secret".to_vec(),
            stderr: Vec::new(),
        };
        assert!(!format!("{out:?}").contains("secret"));
    }

    #[test]
    fn failure_messages_carry_only_a_short_stderr_tail() {
        let long = format!("{}\nthe final line", "noise ".repeat(1000));
        let err = ProcError::Failed {
            program: "tool".into(),
            code: Some(2),
            signal: None,
            stdout: Vec::new(),
            stderr: long.into_bytes(),
        };
        let message = err.to_string();
        assert!(message.ends_with("the final line"), "{message}");
        assert!(message.len() < 700, "{}", message.len());

        let quiet = ProcError::Failed {
            program: "tool".into(),
            code: Some(2),
            signal: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert!(quiet.to_string().contains("no error output"));
    }

    #[tokio::test]
    async fn child_guard_talks_to_a_long_lived_child_and_shuts_it_down() {
        require!("cat");
        let mut guard = ChildGuard::spawn(&ProcSpec::new("cat")).unwrap();
        assert!(guard.id().is_some());
        assert_eq!(guard.program(), "cat");
        let mut stdin = guard.take_stdin().unwrap();
        let mut stdout = guard.take_stdout().unwrap();
        assert!(guard.take_stdin().is_none());

        stdin.write_all(b"ping\n").await.unwrap();
        let mut line = [0u8; 5];
        stdout.read_exact(&mut line).await.unwrap();
        assert_eq!(&line, b"ping\n");
        assert!(guard.try_wait().unwrap().is_none());

        drop(stdin);
        let status = guard.shutdown(Duration::from_secs(5)).await.unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn child_guard_shutdown_kills_a_child_that_ignores_the_request() {
        require!("sh", "sleep");
        let dir = scratch_dir();
        let file = dir.path().join("ticks");
        let guard = ChildGuard::spawn(
            &sh(&format!("trap '' TERM HUP; {TICKER}"))
                .arg("sh")
                .arg(&file),
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        let started = Instant::now();
        let status = guard.shutdown(Duration::from_millis(200)).await.unwrap();
        assert!(!status.success());
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_ticker_stopped(&file).await;
    }

    #[tokio::test]
    async fn dropping_a_child_guard_kills_the_group() {
        require!("sh", "sleep");
        let dir = scratch_dir();
        let file = dir.path().join("ticks");
        let guard = ChildGuard::spawn(&sh(TICKER).arg("sh").arg(&file)).unwrap();
        let pid = i32::try_from(guard.id().unwrap()).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        drop(guard);
        assert_ticker_stopped(&file).await;
        assert!(
            eventually(5, || pid_is_gone(pid)).await,
            "pid {pid} was not reaped"
        );
    }

    #[tokio::test]
    async fn child_guard_reports_an_exited_child_and_keeps_the_stderr_tail() {
        require!("sh");
        let spec = sh("echo first >&2; echo last >&2; exit 4").max_stderr(8);
        let mut guard = ChildGuard::spawn(&spec).unwrap();
        let status = loop {
            if let Some(status) = guard.try_wait().unwrap() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(status.code(), Some(4));
        assert!(guard.id().is_none());
        // Only the last 8 bytes of "first\nlast\n" are kept.
        assert!(
            eventually(5, || guard.stderr_tail() == "st\nlast\n").await,
            "{:?}",
            guard.stderr_tail()
        );
        let status = guard.shutdown(Duration::from_millis(50)).await;
        assert_eq!(status.and_then(|status| status.code()), Some(4));
    }

    #[tokio::test]
    async fn child_guard_spawn_reports_a_missing_program() {
        let err = ChildGuard::spawn(&ProcSpec::new("apm-definitely-not-installed")).unwrap_err();
        assert!(err.is_not_found(), "{err:?}");
    }
}
