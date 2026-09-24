//! Read-only authoritative credential cache. Two fixed provider slots; no token
//! refresh/write path. Every cache hit revalidates the current descriptor, and
//! replacement/missing/unsafe files cannot return an old credential.
use hmux_usage::{
    credentials::{self, Credential},
    quota_state::Failure,
    Provider,
};
use rustix::fs::{self, Mode, OFlags};
use std::{
    fs::{File, Metadata},
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, Semaphore};
use tokio_util::sync::CancellationToken;

const LIMIT: u64 = 4 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);
static READ_SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
type Cached = Option<(Revision, Arc<Credential>)>;

#[derive(PartialEq, Eq)]
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
    fn from(meta: &Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime: meta.mtime(),
            mtime_ns: meta.mtime_nsec(),
            ctime: meta.ctime(),
            ctime_ns: meta.ctime_nsec(),
            mode: meta.mode(),
        }
    }
}
struct Inner {
    paths: [PathBuf; 2],
    cache: [Mutex<Cached>; 2],
}
#[derive(Clone)]
pub struct Store(Arc<Inner>);
impl Store {
    /// The Home owner supplies its configured absolute home, captured once.
    /// No environment lookup or provider storage read happens at construction.
    pub fn new(home: &Path) -> Result<Self, Failure> {
        if !home.is_absolute()
            || home
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(Failure::CredentialIo);
        }
        Ok(Self(Arc::new(Inner {
            paths: [
                home.join(".claude/.credentials.json"),
                home.join(".codex/auth.json"),
            ],
            cache: [Mutex::new(None), Mutex::new(None)],
        })))
    }

    /// At most two blocking readers process-wide, including abandoned/timed-out
    /// callers. Native file reads cannot be forcibly interrupted; their worker
    /// retains admission until it exits and can never initiate provider I/O.
    pub async fn load(
        &self,
        provider: Provider,
        force: bool,
        cancel: &CancellationToken,
    ) -> Result<Arc<Credential>, Failure> {
        if cancel.is_cancelled() {
            return Err(Failure::CredentialIo);
        }
        let permit = READ_SLOTS
            .get_or_init(|| Arc::new(Semaphore::new(2)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::CredentialIo)?;
        let inner = self.0.clone();
        let cancel = cancel.child_token();
        let worker_cancel = cancel.clone();
        let deadline = Instant::now() + TIMEOUT;
        let (tx, rx) = oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let index = index(provider);
            let result = inner.cache[index]
                .lock()
                .map_err(|_| Failure::CredentialIo)
                .and_then(|mut cache| {
                    let result = read_current(
                        &inner.paths[index],
                        provider,
                        force,
                        &mut cache,
                        &worker_cancel,
                        deadline,
                        || {},
                    );
                    if result.is_err() {
                        *cache = None;
                    }
                    result
                });
            let _ = tx.send(result);
        });
        // Child token also cancels the worker when the caller future is dropped.
        let _guard = cancel.clone().drop_guard();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Failure::CredentialIo),
            result = tokio::time::timeout(TIMEOUT, rx) => result.map_err(|_| Failure::CredentialIo)?.map_err(|_| Failure::CredentialIo)?,
        }
    }
}
fn index(provider: Provider) -> usize {
    match provider {
        Provider::Claude => 0,
        Provider::Codex => 1,
    }
}
fn check(cancel: &CancellationToken, deadline: Instant) -> Result<(), Failure> {
    if cancel.is_cancelled() || Instant::now() >= deadline {
        Err(Failure::CredentialIo)
    } else {
        Ok(())
    }
}
fn file_error(error: rustix::io::Errno) -> Failure {
    if error == rustix::io::Errno::NOENT {
        Failure::CredentialMissing
    } else {
        Failure::CredentialIo
    }
}
fn inspect(path: &Path) -> Result<(File, Revision), Failure> {
    // Walk by descriptors, rejecting symlinks at every path component. Final
    // NONBLOCK prevents special files (including FIFOs) from blocking open.
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut dir = fs::open("/", flags | OFlags::DIRECTORY, Mode::empty()).map_err(file_error)?;
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
        let name = parts.next().ok_or(Failure::CredentialIo)?;
        if parts.peek().is_none() {
            break File::from(
                fs::openat(&dir, name, flags | OFlags::NONBLOCK, Mode::empty())
                    .map_err(file_error)?,
            );
        }
        dir =
            fs::openat(&dir, name, flags | OFlags::DIRECTORY, Mode::empty()).map_err(file_error)?;
    };
    let meta = file.metadata().map_err(|_| Failure::CredentialIo)?;
    if !meta.is_file()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.nlink() != 1
        || meta.mode() & 0o022 != 0
        || meta.len() > LIMIT
    {
        return Err(Failure::CredentialIo);
    }
    Ok((file, Revision::from(&meta)))
}
fn read_current(
    path: &Path,
    provider: Provider,
    force: bool,
    cache: &mut Cached,
    cancel: &CancellationToken,
    deadline: Instant,
    mut after_read: impl FnMut(),
) -> Result<Arc<Credential>, Failure> {
    for _ in 0..2 {
        check(cancel, deadline)?;
        let (mut file, before) = inspect(path)?;
        if !force {
            if let Some((rev, value)) = cache {
                if *rev == before {
                    return Ok(value.clone());
                }
            }
        }
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            check(cancel, deadline)?;
            let n = file.read(&mut chunk).map_err(|_| Failure::CredentialIo)?;
            if n == 0 {
                break;
            }
            if n as u64 > LIMIT - bytes.len() as u64 {
                return Err(Failure::CredentialIo);
            }
            bytes.extend_from_slice(&chunk[..n]);
        }
        after_read();
        check(cancel, deadline)?;
        let held = Revision::from(&file.metadata().map_err(|_| Failure::CredentialIo)?);
        let (_, after) = inspect(path)?;
        if before != held || before != after || bytes.len() as u64 != before.len {
            continue;
        }
        let value = Arc::new(credentials::parse(provider, &bytes).map_err(
            |error| match error {
                credentials::CredentialError::MissingToken => Failure::CredentialMissing,
                _ => Failure::CredentialMalformed,
            },
        )?);
        *cache = Some((after, value.clone()));
        return Ok(value);
    }
    Err(Failure::CredentialIo)
}

#[cfg(test)]
#[path = "usage_credentials_tests.rs"]
mod tests;
