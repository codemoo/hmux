//! Bounded execution of owned, one-shot child processes.
//!
//! A runner admits at most its configured number of jobs without a waiting queue.
//! Its task owns each child and permit even when the caller future is dropped. On
//! timeout, caller drop, or output overflow, it kills and reaps the **direct child**
//! before releasing the permit. It never signals a process group: a collector may
//! start a provider or tmux process that HMux must not terminate implicitly.
//! Descendants that inherit stdout/stderr can delay EOF after the direct child
//! exits; the deadline also bounds that drain. Those descendants are not reaped
//! or killed here, so callers must use short-lived, controlled one-shot commands.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Semaphore};

const STDERR_LIMIT: usize = 64 * 1024;
static CHILD_SPAWN: Mutex<()> = Mutex::new(());

/// Serialize HMux's synchronous descriptor setup and child-spawn boundary.
/// Some portable PTY APIs set CLOEXEC after opening the master. All HMux child
/// spawners must share this gate so another owned child cannot inherit that
/// transient descriptor. Never await, recurse, or wait for a child inside it.
pub fn with_child_spawn<T>(spawn: impl FnOnce() -> T) -> T {
    let _guard = CHILD_SPAWN
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    spawn()
}

/// An argument-array command. Debug intentionally omits every caller value.
pub struct CommandSpec {
    program: OsString,
    args: Vec<OsString>,
    current_dir: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
    clear_env: bool,
    stdout_limit: usize,
    timeout: Duration,
    partial_exit_code: Option<i32>,
}

impl CommandSpec {
    pub fn new(program: impl Into<OsString>, stdout_limit: usize, timeout: Duration) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            current_dir: None,
            env: Vec::new(),
            clear_env: false,
            stdout_limit,
            timeout,
            partial_exit_code: None,
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

    pub fn current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(dir.into());
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Opt-in clean environment for fixed OS collectors. Provider/session
    /// launch specifications continue inheriting the host environment by default.
    pub fn env_clear(mut self) -> Self {
        self.clear_env = true;
        self
    }

    /// Accept bounded stdout from exactly this nonzero exit code.
    pub fn partial_exit(mut self, code: i32) -> Self {
        self.partial_exit_code = Some(code);
        self
    }
}

impl fmt::Debug for CommandSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandSpec")
            .field("stdout_limit", &self.stdout_limit)
            .field("timeout", &self.timeout)
            .field("partial_exit_code", &self.partial_exit_code)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunErrorKind {
    InvalidSpec,
    Busy,
    Spawn,
    Io,
    Exit,
    StdoutLimit,
    StderrLimit,
    TimedOut,
    Cancelled,
}

/// Stderr is available only through the explicit accessor. Formatting never
/// includes stderr, the executable path, arguments, environment, or working dir.
pub struct RunError {
    kind: RunErrorKind,
    exit_code: Option<i32>,
    stderr: Vec<u8>,
}

impl RunError {
    fn new(kind: RunErrorKind) -> Self {
        Self {
            kind,
            exit_code: None,
            stderr: Vec::new(),
        }
    }

    pub fn kind(&self) -> RunErrorKind {
        self.kind
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    pub fn stderr(&self) -> Option<&[u8]> {
        (self.kind == RunErrorKind::Exit).then_some(&self.stderr)
    }
}

impl fmt::Debug for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunError")
            .field("kind", &self.kind)
            .field("exit_code", &self.exit_code)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            RunErrorKind::InvalidSpec => f.write_str("invalid command specification"),
            RunErrorKind::Busy => f.write_str("command runner is busy"),
            RunErrorKind::Spawn => f.write_str("could not start command"),
            RunErrorKind::Io => f.write_str("command I/O failed"),
            RunErrorKind::Exit => write!(f, "command exited unsuccessfully ({:?})", self.exit_code),
            RunErrorKind::StdoutLimit => f.write_str("command stdout limit exceeded"),
            RunErrorKind::StderrLimit => f.write_str("command stderr limit exceeded"),
            RunErrorKind::TimedOut => f.write_str("command timed out"),
            RunErrorKind::Cancelled => f.write_str("command result unavailable"),
        }
    }
}

impl std::error::Error for RunError {}

pub struct RunOutput {
    pub stdout: Vec<u8>,
    pub partial: bool,
}

/// Shared finite admission for all collectors using this runner instance.
#[derive(Clone)]
pub struct CommandRunner {
    slots: Arc<Semaphore>,
}

impl CommandRunner {
    pub fn new(max_concurrent: usize) -> Result<Self, RunError> {
        if max_concurrent == 0 || max_concurrent > Semaphore::MAX_PERMITS {
            return Err(RunError::new(RunErrorKind::InvalidSpec));
        }
        Ok(Self {
            slots: Arc::new(Semaphore::new(max_concurrent)),
        })
    }

    pub fn available_slots(&self) -> usize {
        self.slots.available_permits()
    }

    pub async fn run(&self, spec: CommandSpec) -> Result<RunOutput, RunError> {
        self.run_inner(spec, None).await
    }

    /// A sent or dropped cancellation sender requests termination. The result is
    /// returned only after the direct child is reaped.
    pub async fn run_cancelable(
        &self,
        spec: CommandSpec,
        cancellation: oneshot::Receiver<()>,
    ) -> Result<RunOutput, RunError> {
        self.run_inner(spec, Some(cancellation)).await
    }

    async fn run_inner(
        &self,
        spec: CommandSpec,
        cancellation: Option<oneshot::Receiver<()>>,
    ) -> Result<RunOutput, RunError> {
        if spec.stdout_limit == 0
            || spec.timeout.is_zero()
            || spec.partial_exit_code.is_some_and(|code| code <= 0)
            || spec.program.as_os_str().is_empty()
        {
            return Err(RunError::new(RunErrorKind::InvalidSpec));
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| RunError::new(RunErrorKind::Busy))?;
        let (mut sender, receiver) = oneshot::channel();
        tokio::spawn(async move {
            if sender.is_closed() {
                return;
            }
            let result = run_owned(spec, cancellation, &mut sender).await;
            drop(permit);
            let _ = sender.send(result);
        });
        receiver
            .await
            .unwrap_or_else(|_| Err(RunError::new(RunErrorKind::Cancelled)))
    }
}

async fn run_owned(
    spec: CommandSpec,
    mut cancellation: Option<oneshot::Receiver<()>>,
    sender: &mut oneshot::Sender<Result<RunOutput, RunError>>,
) -> Result<RunOutput, RunError> {
    if cancellation.as_mut().is_some_and(|receiver| {
        !matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        )
    }) {
        return Err(RunError::new(RunErrorKind::Cancelled));
    }
    let mut command = Command::new(&spec.program);
    if spec.clear_env {
        command.env_clear();
    }
    command.args(&spec.args);
    if let Some(dir) = &spec.current_dir {
        command.current_dir(dir);
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child =
        with_child_spawn(|| command.spawn()).map_err(|_| RunError::new(RunErrorKind::Spawn))?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err(RunError::new(RunErrorKind::Io));
    };
    let mut deadline = Box::pin(tokio::time::sleep(spec.timeout));
    let cancelled = async {
        match cancellation {
            Some(receiver) => {
                let _ = receiver.await;
            }
            None => std::future::pending().await,
        }
    };
    let result = tokio::select! {
        biased;
        result = collect(&mut child, stdout, stderr, spec.stdout_limit) => result,
        () = &mut deadline => Err(RunError::new(RunErrorKind::TimedOut)),
        () = cancelled => Err(RunError::new(RunErrorKind::Cancelled)),
        () = sender.closed() => Err(RunError::new(RunErrorKind::Cancelled)),
    };
    // A completed collect already reaped the child. A second wait is harmless;
    // on any error it ensures a still-running child is killed and reaped first.
    if result.is_err() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    let (status, stdout, stderr) = result?;
    if status.success() {
        return Ok(RunOutput {
            stdout,
            partial: false,
        });
    }
    if spec
        .partial_exit_code
        .is_some_and(|code| status.code() == Some(code))
    {
        return Ok(RunOutput {
            stdout,
            partial: true,
        });
    }
    Err(RunError {
        kind: RunErrorKind::Exit,
        exit_code: status.code(),
        stderr,
    })
}

async fn collect(
    child: &mut Child,
    stdout: impl AsyncRead + Unpin,
    stderr: impl AsyncRead + Unpin,
    stdout_limit: usize,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), RunError> {
    tokio::try_join!(
        async {
            child
                .wait()
                .await
                .map_err(|_| RunError::new(RunErrorKind::Io))
        },
        read_limited(stdout, stdout_limit, RunErrorKind::StdoutLimit),
        read_limited(stderr, STDERR_LIMIT, RunErrorKind::StderrLimit),
    )
}

async fn read_limited(
    mut pipe: impl AsyncRead + Unpin,
    limit: usize,
    overflow: RunErrorKind,
) -> Result<Vec<u8>, RunError> {
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut chunk = [0_u8; 8192];
    loop {
        let count = pipe
            .read(&mut chunk)
            .await
            .map_err(|_| RunError::new(RunErrorKind::Io))?;
        if count == 0 {
            return Ok(bytes);
        }
        if count > limit - bytes.len() {
            return Err(RunError::new(overflow));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}
