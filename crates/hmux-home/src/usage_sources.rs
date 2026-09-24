//! Read-only owners for optional local account usage sources. Neither owner
//! starts a service, changes the active account, or retains raw source bytes.
use chrono::{DateTime, Utc};
use hmux_core::command::{CommandRunner, CommandSpec};
use hmux_usage::{codex_accounts, cswap};
use rustix::fs::{self, Mode, OFlags};
use std::{
    ffi::OsString,
    fs::{File, Metadata},
    io::Read,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::{oneshot, Semaphore};
use tokio_util::sync::CancellationToken;

const LIMIT: usize = 8 * 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const CHECK_EVERY: Duration = Duration::from_secs(2);
const REFRESH_EVERY: chrono::Duration = chrono::Duration::seconds(60);
static RESOLVE_SLOT: OnceLock<Arc<Semaphore>> = OnceLock::new();
static READ_SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
static COMMANDS: OnceLock<CommandRunner> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidPath,
    Missing,
    UnsafeFile,
    Busy,
    Cancelled,
    Io,
    Parse,
    Stale,
    Command,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidPath => "invalid source path",
            Self::Missing => "usage source unavailable",
            Self::UnsafeFile => "unsafe usage source",
            Self::Busy => "usage source busy",
            Self::Cancelled => "usage source cancelled",
            Self::Io => "usage source read failed",
            Self::Parse => "usage source invalid",
            Self::Stale => "usage source stale",
            Self::Command => "usage command failed",
        })
    }
}
impl std::error::Error for Error {}

fn valid_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().as_bytes().len() <= 4096
        && !path.as_os_str().as_bytes().contains(&0)
        && !path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
}

/// Captured PATH and Home make resolution independent of later environment
/// changes. Construction never launches a command or reads provider data.
pub struct Cswap {
    home: PathBuf,
    path: OsString,
    last_attempt: Option<DateTime<Utc>>,
    last_good: cswap::LastGood,
}
impl Cswap {
    pub fn new(home: PathBuf, path: OsString) -> Result<Self, Error> {
        if !valid_absolute(&home)
            || path.as_bytes().len() > 64 * 1024
            || path.as_bytes().contains(&0)
        {
            return Err(Error::InvalidPath);
        }
        Ok(Self {
            home,
            path,
            last_attempt: None,
            last_good: cswap::LastGood::default(),
        })
    }

    /// Account/setup changes invalidate both cadence and previous account data.
    pub fn invalidate(&mut self) {
        self.last_attempt = None;
        self.last_good = cswap::LastGood::default();
    }

    pub async fn refresh(
        &mut self,
        now: DateTime<Utc>,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if self
            .last_attempt
            .is_some_and(|last| now >= last && now - last < REFRESH_EVERY)
        {
            return Ok(());
        }
        self.last_attempt = Some(now);
        let executable = self.resolve(cancel).await?;
        let spec = CommandSpec::new(
            executable.into_os_string(),
            cswap::MAX_COMMAND_BYTES,
            COMMAND_TIMEOUT,
        )
        .arg("list")
        .arg("--json")
        .env("HOME", self.home.as_os_str())
        .env("PATH", &self.path);
        let (sender, receiver) = oneshot::channel();
        let work = COMMANDS
            .get_or_init(|| CommandRunner::new(1).expect("nonzero command limit"))
            .run_cancelable(spec, receiver);
        tokio::pin!(work);
        let bytes = tokio::select! {
            biased;
            _ = cancel.cancelled() => { drop(sender); let _ = work.await; return Err(Error::Cancelled); }
            result = &mut work => result.map_err(|_| Error::Command)?.stdout,
        };
        let parsed = cswap::parse_command(&bytes, now).map_err(|_| Error::Parse)?;
        self.last_good.replace(parsed, now);
        Ok(())
    }

    pub fn current(&self, now: DateTime<Utc>) -> Option<cswap::Parsed> {
        self.last_good.current(now)
    }

    async fn resolve(&self, cancel: &CancellationToken) -> Result<PathBuf, Error> {
        let permit = RESOLVE_SLOT
            .get_or_init(|| Arc::new(Semaphore::new(1)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let home = self.home.clone();
        let path = self.path.clone();
        let child = cancel.child_token();
        let _guard = child.clone().drop_guard();
        let deadline = Instant::now() + READ_TIMEOUT;
        let (tx, rx) = oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let result = (|| {
                // Relative PATH entries must never resolve through the cwd.
                for dir in std::env::split_paths(&path).take(128) {
                    check(&child, deadline)?;
                    if !valid_absolute(&dir) {
                        continue;
                    }
                    let candidate = dir.join("cswap");
                    if is_executable(&candidate) {
                        return Ok(candidate);
                    }
                }
                check(&child, deadline)?;
                let fallback = home.join(".local/bin/cswap");
                if is_executable(&fallback) {
                    Ok(fallback)
                } else {
                    Err(Error::Missing)
                }
            })();
            let _ = tx.send(result);
        });
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(READ_TIMEOUT, rx) => result.map_err(|_| Error::Io)?.map_err(|_| Error::Io)?,
        }
    }
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[derive(Clone, PartialEq, Eq)]
struct Revision {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
    mode: u32,
}
impl Revision {
    fn from(m: &Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            len: m.len(),
            mtime: m.mtime(),
            mtime_ns: m.mtime_nsec(),
            ctime: m.ctime(),
            ctime_ns: m.ctime_nsec(),
            mode: m.mode(),
        }
    }
}

struct AccountCache {
    last_check: Option<Instant>,
    good: Option<(Revision, codex_accounts::AccountExport)>,
}
struct AccountInner {
    path: PathBuf,
    cache: Mutex<AccountCache>,
}
#[derive(Clone)]
pub struct Accounts(Arc<AccountInner>);
impl Accounts {
    pub fn new(path: PathBuf) -> Result<Self, Error> {
        if !valid_absolute(&path) || path.file_name().is_none() {
            return Err(Error::InvalidPath);
        }
        Ok(Self(Arc::new(AccountInner {
            path,
            cache: Mutex::new(AccountCache {
                last_check: None,
                good: None,
            }),
        })))
    }

    /// A process-wide two-reader admission bounds blocking work, including
    /// workers whose caller has timed out or disappeared.
    pub async fn read(
        &self,
        now: DateTime<Utc>,
        cancel: &CancellationToken,
    ) -> Result<codex_accounts::AccountExport, Error> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let permit = READ_SLOTS
            .get_or_init(|| Arc::new(Semaphore::new(2)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let inner = self.0.clone();
        let worker_cancel = cancel.child_token();
        let guard = worker_cancel.clone().drop_guard();
        let deadline = Instant::now() + READ_TIMEOUT;
        let (sender, receiver) = oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let result = read_account_inner(&inner, now, &worker_cancel, deadline);
            let _ = sender.send(result);
        });
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(READ_TIMEOUT, receiver) => result.map_err(|_| Error::Io)?.map_err(|_| Error::Io)?,
        };
        drop(guard);
        result
    }
}

fn check(cancel: &CancellationToken, deadline: Instant) -> Result<(), Error> {
    if cancel.is_cancelled() || Instant::now() >= deadline {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
fn inspect(path: &Path) -> Result<(File, Revision, DateTime<Utc>), Error> {
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut dir = fs::open("/", flags | OFlags::DIRECTORY, Mode::empty()).map_err(|_| Error::Io)?;
    let mut parts = path
        .components()
        .filter_map(|c| {
            if let Component::Normal(v) = c {
                Some(v)
            } else {
                None
            }
        })
        .peekable();
    let file = loop {
        let part = parts.next().ok_or(Error::InvalidPath)?;
        if parts.peek().is_none() {
            break File::from(
                fs::openat(&dir, part, flags | OFlags::NONBLOCK, Mode::empty()).map_err(|e| {
                    if e == rustix::io::Errno::NOENT {
                        Error::Missing
                    } else {
                        Error::UnsafeFile
                    }
                })?,
            );
        }
        dir = fs::openat(&dir, part, flags | OFlags::DIRECTORY, Mode::empty()).map_err(|e| {
            if e == rustix::io::Errno::NOENT {
                Error::Missing
            } else {
                Error::UnsafeFile
            }
        })?;
    };
    let m = file.metadata().map_err(|_| Error::Io)?;
    if !m.is_file()
        || m.uid() != rustix::process::geteuid().as_raw()
        || m.nlink() != 1
        || m.mode() & 0o022 != 0
        || m.len() > LIMIT as u64
    {
        return Err(Error::UnsafeFile);
    }
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .and_then(|d| DateTime::<Utc>::from_timestamp(d.as_secs() as i64, d.subsec_nanos()))
        .ok_or(Error::Io)?;
    Ok((file, Revision::from(&m), mtime))
}
fn read_account_inner(
    inner: &AccountInner,
    now: DateTime<Utc>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<codex_accounts::AccountExport, Error> {
    let mut cache = inner.cache.lock().map_err(|_| Error::Io)?;
    check(cancel, deadline)?;
    if cache
        .last_check
        .is_some_and(|last| last.elapsed() < CHECK_EVERY)
    {
        return cached(&cache, now).ok_or(Error::Stale);
    }
    cache.last_check = Some(Instant::now());
    let attempt = (|| {
        for _ in 0..2 {
            check(cancel, deadline)?;
            let (mut file, before, mtime) = inspect(&inner.path)?;
            if let Some((revision, export)) = &cache.good {
                if *revision == before {
                    return Ok(export.clone());
                }
            }
            let mut data = Vec::with_capacity((before.len as usize).min(8192));
            let mut chunk = [0u8; 8192];
            loop {
                check(cancel, deadline)?;
                let n = file.read(&mut chunk).map_err(|_| Error::Io)?;
                if n == 0 {
                    break;
                }
                if n > LIMIT - data.len() {
                    return Err(Error::UnsafeFile);
                }
                data.extend_from_slice(&chunk[..n]);
            }
            check(cancel, deadline)?;
            let held = Revision::from(&file.metadata().map_err(|_| Error::Io)?);
            let (_, after, _) = inspect(&inner.path)?;
            if before != held || before != after || data.len() as u64 != before.len {
                continue;
            }
            let export = codex_accounts::parse_account_export(&data, now, mtime)
                .map_err(|_| Error::Parse)?;
            cache.good = Some((after, export.clone()));
            return Ok(export);
        }
        Err(Error::Io)
    })();
    match attempt {
        Ok(export) if codex_accounts::is_fresh(export.source_time, now) => Ok(export),
        Ok(_) => Err(Error::Stale),
        Err(Error::Cancelled) => Err(Error::Cancelled),
        Err(error) => cached(&cache, now).ok_or_else(|| {
            if cache.good.is_some() {
                Error::Stale
            } else {
                error
            }
        }),
    }
}
fn cached(cache: &AccountCache, now: DateTime<Utc>) -> Option<codex_accounts::AccountExport> {
    cache
        .good
        .as_ref()
        .map(|(_, export)| export)
        .filter(|export| codex_accounts::is_fresh(export.source_time, now))
        .cloned()
}

#[cfg(test)]
#[path = "usage_sources_tests.rs"]
mod tests;
