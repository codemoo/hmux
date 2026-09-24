//! Minimal native PTY owner for one disposable attached client. It never names,
//! signals or cleans up a tmux session. Only the direct child spawned here may
//! be killed. One process-wide admission slot stays owned through child reaping
//! and release of every master descriptor; keeping Tokio alive is required for
//! deterministic cleanup after a caller abort.
use pty_process::{Command, OwnedReadPty, OwnedWritePty, Pty, Size};
use std::{
    ffi::OsString,
    fmt, io,
    os::{unix::ffi::OsStrExt, unix::process::ExitStatusExt},
    path::Path,
    pin::Pin,
    process::ExitStatus,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const SLOTS: usize = 8;
const MAX_PROGRAM_BYTES: usize = 1024;
const MAX_ARGS: usize = 64;
const MAX_ARG_BYTES: usize = 16 * 1024;
static PTY_SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();

/// Fixed categories contain no executable, argument or PTY contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Busy,
    Open,
    Spawn,
    Resize,
}

/// Copied direct-child outcome; no output, command or private path is retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    Exited(i32),
    Signaled(i32),
    Other,
    WaitFailed,
}
impl Completion {
    fn from_status(status: ExitStatus) -> Self {
        if let Some(code) = status.code() {
            Self::Exited(code)
        } else if let Some(signal) = status.signal() {
            Self::Signaled(signal)
        } else {
            Self::Other
        }
    }
}

fn valid_size(cols: u16, rows: u16) -> bool {
    (2..=500).contains(&cols) && (2..=250).contains(&rows)
}
fn valid_command(executable: &Path, args: &[OsString]) -> bool {
    let program = executable.as_os_str().as_bytes();
    if !executable.is_absolute()
        || program.is_empty()
        || program.len() > MAX_PROGRAM_BYTES
        || program.contains(&0)
        || args.len() > MAX_ARGS
    {
        return false;
    }
    let mut total = 0usize;
    for arg in args {
        let bytes = arg.as_bytes();
        if bytes.contains(&0) {
            return false;
        }
        let Some(next) = total.checked_add(bytes.len()) else {
            return false;
        };
        if next > MAX_ARG_BYTES {
            return false;
        }
        total = next;
    }
    true
}

struct Lease {
    _permit: OwnedSemaphorePermit,
}
struct DescriptorOwner {
    stop: CancellationToken,
    _lease: Arc<Lease>,
}
impl Drop for DescriptorOwner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// Async master descriptor. Dropping its last owned half requests child shutdown.
pub struct Master {
    pty: Pty,
    owner: Arc<DescriptorOwner>,
}
impl Master {
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), Error> {
        if !valid_size(cols, rows) {
            return Err(Error::Invalid);
        }
        self.pty
            .resize(Size::new(rows, cols))
            .map_err(|_| Error::Resize)
    }
    /// Independent async halves. The write half retains resize access; child
    /// cancellation waits for both descriptor halves to be released.
    pub fn into_split(self) -> (ReadHalf, WriteHalf) {
        let Self { pty, owner } = self;
        let (read, write) = pty.into_split();
        (
            ReadHalf {
                pty: read,
                _owner: owner.clone(),
            },
            WriteHalf {
                pty: write,
                _owner: owner,
            },
        )
    }
}
impl AsyncRead for Master {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.pty).poll_read(cx, buf)
    }
}
impl AsyncWrite for Master {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.pty).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.pty).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.pty).poll_shutdown(cx)
    }
}

/// Owned read descriptor. It retains PTY admission until dropped.
pub struct ReadHalf {
    pty: OwnedReadPty,
    _owner: Arc<DescriptorOwner>,
}
impl AsyncRead for ReadHalf {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.pty).poll_read(cx, buf)
    }
}

/// Owned write descriptor with resize. It retains admission until dropped.
pub struct WriteHalf {
    pty: OwnedWritePty,
    _owner: Arc<DescriptorOwner>,
}
impl WriteHalf {
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), Error> {
        if !valid_size(cols, rows) {
            return Err(Error::Invalid);
        }
        self.pty
            .resize(Size::new(rows, cols))
            .map_err(|_| Error::Resize)
    }
}
impl AsyncWrite for WriteHalf {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.pty).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.pty).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.pty).poll_shutdown(cx)
    }
}

/// Direct child lifecycle. Dropping or aborting a waiter cancels the child;
/// its private task retains admission until it has actually waited/reaped.
pub struct ChildOwner {
    pid: u32,
    stop: CancellationToken,
    task: Option<JoinHandle<Completion>>,
}
impl ChildOwner {
    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub async fn wait(mut self) -> Completion {
        let Some(task) = self.task.take() else {
            return Completion::WaitFailed;
        };
        task.await.unwrap_or(Completion::WaitFailed)
    }
    pub async fn close(self) -> Completion {
        self.stop.cancel();
        self.wait().await
    }
}
impl Drop for ChildOwner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// One master plus its directly spawned child. No extra reader thread or
/// buffered I/O is introduced. Debug never prints the child command.
pub struct Session {
    master: Master,
    child: ChildOwner,
}
impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Session([redacted])")
    }
}
impl Session {
    pub fn pid(&self) -> u32 {
        self.child.pid()
    }
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), Error> {
        self.master.resize(cols, rows)
    }
    pub fn into_parts(self) -> (Master, ChildOwner) {
        (self.master, self.child)
    }
    /// Wait for natural exit while retaining the master descriptor. Dropping
    /// this future cancels the child and detaches the private reaper task.
    pub async fn wait(self) -> Completion {
        let Self { master, child } = self;
        let result = child.wait().await;
        drop(master);
        result
    }
    /// Close the master, cancel only this direct child, then join its reaper.
    pub async fn close(self) -> Completion {
        let Self { master, child } = self;
        drop(master);
        child.close().await
    }
}
impl AsyncRead for Session {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.master).poll_read(cx, buf)
    }
}
impl AsyncWrite for Session {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.master).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.master).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.master).poll_shutdown(cx)
    }
}

async fn supervise(
    mut child: tokio::process::Child,
    stop: CancellationToken,
    lease: Arc<Lease>,
) -> Completion {
    let status = tokio::select! {
        biased;
        _ = stop.cancelled() => {
            let _ = child.start_kill();
            child.wait().await
        },
        status = child.wait() => status,
    };
    let completion = status
        .map(Completion::from_status)
        .unwrap_or(Completion::WaitFailed);
    drop(lease);
    completion
}

/// Spawn one disposable attach client with a PTY and no shell-string API.
/// Synchronous PTY allocation and spawn happen only after validation and
/// immediate process-wide admission. No existing tmux/provider PID is touched.
pub fn spawn(executable: &Path, args: &[OsString], cols: u16, rows: u16) -> Result<Session, Error> {
    if !valid_size(cols, rows) || !valid_command(executable, args) {
        return Err(Error::Invalid);
    }
    let permit = PTY_SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(SLOTS)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::Busy)?;
    let (pty, child) = hmux_core::command::with_child_spawn(|| {
        let (pty, pts) = pty_process::open().map_err(|_| Error::Open)?;
        pty.resize(Size::new(rows, cols))
            .map_err(|_| Error::Resize)?;
        let child = Command::new(executable)
            .args(args)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("TERM")
            .env_remove("COLORTERM")
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .kill_on_drop(true)
            .spawn(pts)
            .map_err(|_| Error::Spawn)?;
        Ok::<_, Error>((pty, child))
    })?;
    let pid = child.id().expect("fresh direct child has PID");
    let stop = CancellationToken::new();
    let lease = Arc::new(Lease { _permit: permit });
    let task = tokio::spawn(supervise(child, stop.clone(), lease.clone()));
    Ok(Session {
        master: Master {
            pty,
            owner: Arc::new(DescriptorOwner {
                stop: stop.clone(),
                _lease: lease,
            }),
        },
        child: ChildOwner {
            pid,
            stop,
            task: Some(task),
        },
    })
}

#[cfg(test)]
#[path = "pty_tests.rs"]
mod tests;
