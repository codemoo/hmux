//! Private shared workspace transactions for Home and account-scoped gateway use.
//! One owner bounds all of its scopes; no per-profile worker, cache or daemon.
use crate::PrivateDir;
use hmux_model::workspace::{self, Change, SessionLineage, Snapshot};
use std::{
    ffi::OsStr,
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

const WORKERS: usize = 2;
const LOCK_WAIT: Duration = Duration::from_secs(3);
const LOCK_POLL: Duration = Duration::from_millis(25);
const FILE: &str = "workspace.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Busy,
    Cancelled,
    Invalid,
    Unavailable,
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}
struct Inner {
    root: PrivateDir,
    admission: Arc<Semaphore>,
    closed: AtomicBool,
}
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn scope_valid(scope: Option<&str>) -> bool {
    // Account profile is the existing lowercase SHA-256 username digest.
    scope.is_none_or(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn read(dir: &PrivateDir) -> Result<Option<Vec<u8>>, Error> {
    match dir.read_private(OsStr::new(FILE), workspace::MAX_BYTES) {
        Ok(raw) => Ok(Some(raw)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Error::Unavailable),
    }
}
impl Store {
    /// `root` is the Home state directory, or gateway's private web-profiles root.
    /// `None` scope is Home's workspace; account routes must pass their auth profile.
    pub fn new(root: PrivateDir) -> Self {
        Self {
            inner: Arc::new(Inner {
                root,
                admission: Arc::new(Semaphore::new(WORKERS)),
                closed: AtomicBool::new(false),
            }),
        }
    }
    /// Fetch under bounded blocking admission, before taking any workspace lock.
    /// This preserves recovery lock ordering. `allowed` must be a nonblocking
    /// authorization/deadline check; it runs while waiting and before persistence.
    /// Dropping the future cancels queued/waiting work. An atomic write already
    /// begun may commit; admission remains held until the actual worker returns.
    pub async fn sync(
        &self,
        scope: Option<String>,
        change: Option<Change>,
        fetch: impl FnOnce() -> Result<Vec<SessionLineage>, Error> + Send + 'static,
        allowed: impl Fn() -> bool + Send + 'static,
    ) -> Result<Snapshot, Error> {
        if !scope_valid(scope.as_deref()) {
            return Err(Error::Invalid);
        }
        let permit = self
            .inner
            .admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let _caller = CancelOnDrop(cancelled.clone());
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let check = || {
                if cancelled.load(Ordering::Acquire)
                    || inner.closed.load(Ordering::Acquire)
                    || !allowed()
                {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            };
            check()?;
            let sessions = fetch()?;
            check()?;
            let account = scope
                .as_ref()
                .map(|s| inner.root.create_private_child(OsStr::new(s)))
                .transpose()
                .map_err(|_| Error::Unavailable)?;
            let root = account.as_ref().unwrap_or(&inner.root);
            let dir = root
                .create_private_child(OsStr::new("shared-workspace"))
                .map_err(|_| Error::Unavailable)?;
            let started = Instant::now();
            let _lock = loop {
                check()?;
                if let Some(lock) = dir
                    .try_lock(OsStr::new("lock"))
                    .map_err(|_| Error::Unavailable)?
                {
                    break lock;
                }
                let remaining = LOCK_WAIT.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Err(Error::Busy);
                }
                std::thread::sleep(remaining.min(LOCK_POLL));
            };
            check()?;
            let original = read(&dir)?;
            let current = match &original {
                Some(raw) => Snapshot::decode(raw).map_err(|_| Error::Invalid)?,
                None => Snapshot::empty(),
            };
            let update = workspace::reconcile(current, change.as_ref(), &sessions)
                .map_err(|_| Error::Invalid)?;
            if update.changed {
                let raw = serde_json::to_vec(&update.state).map_err(|_| Error::Invalid)?;
                if raw.len() > workspace::MAX_BYTES {
                    return Err(Error::Invalid);
                }
                // Reject an external edit or unsafe target, even if it ignored
                // our advisory lock. Never overwrite an uncertain original.
                if read(&dir)? != original {
                    return Err(Error::Unavailable);
                }
                check()?;
                dir.write_atomic_private(OsStr::new(FILE), &raw)
                    .map_err(|_| Error::Unavailable)?;
            }
            check()?;
            Ok(update.reply())
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    pub async fn shutdown(&self) {
        self.inner.closed.store(true, Ordering::Release);
        let _drained = self
            .inner
            .admission
            .clone()
            .acquire_many_owned(WORKERS as u32)
            .await;
    }
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
