//! Account-scoped browser diagnostics: one bounded ring and one save task.
//! Call `shutdown` before stopping Tokio; dropping the last owner requests the
//! same final flush, but the runtime must remain alive for it to complete.
use crate::auth_store::SessionAccess;
use chrono::{DateTime, Utc};
use hmux_core::{PrivateDir, WriteError};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    ffi::{OsStr, OsString},
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, SystemTime},
};
use tokio::sync::{watch, Semaphore};
use tokio_util::sync::CancellationToken;

#[path = "diagnostic_model.rs"]
mod model;
pub use model::Batch;
use model::{Export, Record, Source, ACCOUNT_LIMIT, DISK_BYTES, LIMIT, TTL_MS};
static STARTUP: OnceLock<Arc<Semaphore>> = OnceLock::new();
const RATE_LIMIT: usize = 1024;
const INTERVAL: Duration = Duration::from_secs(10);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    RateLimited,
    Busy,
    Unavailable,
    Unauthorized,
}
#[derive(Serialize)]
pub struct Report {
    version: u8,
    #[serde(serialize_with = "model::go_time")]
    generated_at: DateTime<Utc>,
    retention_days: u8,
    events: Export,
    counts: BTreeMap<String, usize>,
    storage_ok: bool,
    pending_save: bool,
}
struct Rate {
    start: DateTime<Utc>,
    batches: u8,
}
struct State {
    records: VecDeque<Record>,
    rates: HashMap<[u8; 32], Rate>,
    revision: u64,
    saved: u64,
    disabled: bool,
    storage_error: bool,
}
impl State {
    fn new(records: Vec<Record>, disabled: bool) -> Self {
        Self {
            records: records.into(),
            rates: HashMap::new(),
            revision: 0,
            saved: 0,
            disabled,
            storage_error: disabled,
        }
    }
    fn bump(&mut self) -> Result<(), Error> {
        if let Some(next) = self.revision.checked_add(1) {
            self.revision = next;
            Ok(())
        } else {
            self.disabled = true;
            self.storage_error = true;
            Err(Error::Unavailable)
        }
    }
    fn prune(&mut self, now: DateTime<Utc>) -> Result<(), Error> {
        let cutoff = now - chrono::Duration::milliseconds(TTL_MS);
        if self.records.iter().any(|r| r.received <= cutoff) {
            self.bump()?;
            self.records.retain(|r| r.received > cutoff);
        }
        self.rates
            .retain(|_, rate| now.signed_duration_since(rate.start) < chrono::Duration::minutes(1));
        Ok(())
    }
    fn append(
        &mut self,
        access: &SessionAccess,
        browser: &str,
        batch: Batch,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        if self.disabled {
            return Err(Error::Unavailable);
        }
        self.prune(now)?;
        let key: [u8; 32] = Sha256::digest(access.id.as_bytes()).into();
        if self.rates.get(&key).is_some_and(|r| r.batches >= 6)
            || self.rates.len() >= RATE_LIMIT && !self.rates.contains_key(&key)
        {
            return Err(Error::RateLimited);
        }
        // Reserve the revision before mutating, including duplicate-only batches.
        self.bump()?;
        self.rates
            .entry(key)
            .or_insert(Rate {
                start: now,
                batches: 0,
            })
            .batches += 1;
        let source = Source {
            account: access.username.clone(),
            profile: access.profile.clone(),
            browser: browser.into(),
            ..Source::default()
        };
        let mut accepted: Vec<Record> = Vec::new();
        for mut record in batch.records(source, now) {
            if self.records.iter().chain(accepted.iter()).any(|r| {
                r.same_owner(&record.source.account, &record.source.profile)
                    && r.source.client == record.source.client
                    && r.event.sequence() == record.event.sequence()
            }) {
                continue;
            }
            if let Some(previous) = self
                .records
                .iter()
                .rev()
                .find(|r| r.source == record.source)
            {
                record.source = previous.source.clone();
            }
            accepted.push(record);
        }
        // Deduplicate the whole batch against the original ring before evicting;
        // otherwise a later replay could be mistaken for a new event after eviction.
        for record in accepted {
            // Drop old rows before appending so the ring itself never exceeds cap.
            if self
                .records
                .iter()
                .filter(|r| r.same_owner(&access.username, &access.profile))
                .count()
                >= ACCOUNT_LIMIT
            {
                if let Some(index) = self
                    .records
                    .iter()
                    .position(|r| r.same_owner(&access.username, &access.profile))
                {
                    self.records.remove(index);
                }
            }
            if self.records.len() == LIMIT {
                self.records.pop_front();
            }
            self.records.push_back(record);
        }
        Ok(())
    }
    fn report(
        &mut self,
        account: &str,
        profile: &str,
        now: DateTime<Utc>,
    ) -> Result<Report, Error> {
        self.prune(now)?;
        let rows: Vec<_> = self
            .records
            .iter()
            .filter(|r| r.same_owner(account, profile))
            .cloned()
            .collect();
        let mut counts = BTreeMap::new();
        for r in &rows {
            if let Some(key) = r.event.failure_key() {
                *counts.entry(key).or_insert(0) += 1;
            }
        }
        Ok(Report {
            version: 1,
            generated_at: now,
            retention_days: 7,
            events: Export(rows),
            counts,
            storage_ok: !self.storage_error,
            pending_save: self.saved != self.revision,
        })
    }
}
struct Disk {
    dir: PrivateDir,
    name: OsString,
    expected: Option<[u8; 32]>,
}
impl Disk {
    fn read(&self) -> io::Result<Option<Vec<u8>>> {
        match self.dir.read_private(&self.name, DISK_BYTES) {
            Ok(raw) => Ok(Some(raw)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn persist(&mut self, records: Vec<Record>) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Snapshot {
            version: u8,
            records: Vec<Record>,
        }
        let mut writer = Limited(Vec::new());
        serde_json::to_writer(
            &mut writer,
            &Snapshot {
                version: 1,
                records,
            },
        )
        .map_err(|_| Error::Unavailable)?;
        let raw = writer.0;
        let current = self.read().map_err(|_| Error::Unavailable)?;
        let digest = |v: &[u8]| <[u8; 32]>::from(Sha256::digest(v));
        if current.as_deref().map(digest) != self.expected {
            return Err(Error::Unavailable);
        }
        drop(current);
        let next = Some(digest(&raw));
        match self.dir.write_atomic_private(&self.name, &raw) {
            Ok(()) => {
                self.expected = next;
                Ok(())
            }
            Err(WriteError::AfterCommit(_)) => {
                self.expected = next;
                Err(Error::Unavailable)
            }
            Err(_) => Err(Error::Unavailable),
        }
    }
}
struct Limited(Vec<u8>);
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > DISK_BYTES - self.0.len() {
            return Err(io::Error::other("diagnostics too large"));
        }
        let need = self.0.len() + bytes.len();
        if need > self.0.capacity() {
            let capacity = need.next_power_of_two().min(DISK_BYTES);
            self.0
                .try_reserve_exact(capacity - self.0.len())
                .map_err(|_| io::Error::other("diagnostic allocation failed"))?;
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Inner {
    state: Mutex<State>,
    disk: Mutex<Disk>,
    closed: AtomicBool,
    done: watch::Sender<bool>,
}
struct Owner {
    inner: Arc<Inner>,
    stop: CancellationToken,
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.inner.closed.store(true, Ordering::Release);
        self.stop.cancel();
    }
}
#[derive(Clone)]
pub struct Store {
    owner: Arc<Owner>,
}
fn now() -> DateTime<Utc> {
    SystemTime::now().into()
}
fn authorized(access: &SessionAccess, now: DateTime<Utc>) -> Result<(), Error> {
    if *access.cancelled.borrow()
        || access.cancelled.has_changed().is_err()
        || access.expires_at <= now
    {
        Err(Error::Unauthorized)
    } else {
        Ok(())
    }
}
impl Store {
    /// The directory is opened by the caller under the private-state policy.
    /// A corrupt/unsafe existing file disables this store but remains untouched;
    /// reports still expose `storage_ok:false` and no unvalidated records.
    pub async fn open(dir: PrivateDir, name: OsString) -> Result<Self, Error> {
        if name.is_empty() || std::path::Path::new(&name).file_name() != Some(OsStr::new(&name)) {
            return Err(Error::Invalid);
        }
        let permit = STARTUP
            .get_or_init(|| Arc::new(Semaphore::new(2)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let (disk, mut state) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut disk = Disk {
                dir,
                name,
                expected: None,
            };
            let state = match disk.read() {
                Ok(None) => State::new(Vec::new(), false),
                Ok(Some(raw)) => match model::decode_disk(&raw, now()) {
                    Ok(records) => {
                        disk.expected = Some(Sha256::digest(&raw).into());
                        State::new(records, false)
                    }
                    Err(_) => State::new(Vec::new(), true),
                },
                Err(_) => State::new(Vec::new(), true),
            };
            (disk, state)
        })
        .await
        .map_err(|_| Error::Unavailable)?;
        state.prune(now())?;
        let inner = Arc::new(Inner {
            state: Mutex::new(state),
            disk: Mutex::new(disk),
            closed: AtomicBool::new(false),
            done: watch::channel(false).0,
        });
        let stop = CancellationToken::new();
        let owner = Arc::new(Owner {
            inner: inner.clone(),
            stop: stop.clone(),
        });
        tokio::spawn(run(inner, stop));
        Ok(Self { owner })
    }
    pub fn append(&self, access: &SessionAccess, browser: &str, batch: Batch) -> Result<(), Error> {
        let now = now();
        if !model::valid_owner(&access.username, &access.profile)
            || access.id.is_empty()
            || access.id.len() > 128
            || !model::valid_browser(browser)
            || !batch.valid(now)
        {
            return Err(Error::Invalid);
        }
        let mut state = self
            .owner
            .inner
            .state
            .lock()
            .map_err(|_| Error::Unavailable)?;
        authorized(access, self::now())?;
        if self.owner.inner.closed.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        state.append(access, browser, batch, now)
    }
    pub fn report(&self, access: &SessionAccess) -> Result<Report, Error> {
        let mut state = self
            .owner
            .inner
            .state
            .lock()
            .map_err(|_| Error::Unavailable)?;
        authorized(access, now())?;
        state.report(&access.username, &access.profile, now())
    }
    pub async fn shutdown(&self) {
        let mut done = self.owner.inner.done.subscribe();
        self.owner.inner.closed.store(true, Ordering::Release);
        self.owner.stop.cancel();
        while !*done.borrow_and_update() {
            if done.changed().await.is_err() {
                break;
            }
        }
    }
}
struct Finished(Arc<Inner>);
impl Drop for Finished {
    fn drop(&mut self) {
        self.0.closed.store(true, Ordering::Release);
        self.0.done.send_replace(true);
    }
}
async fn run(inner: Arc<Inner>, stop: CancellationToken) {
    let _finished = Finished(inner.clone());
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + INTERVAL, INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let closing = tokio::select! {biased;_=stop.cancelled()=>true,_=interval.tick()=>false};
        flush(inner.clone()).await;
        if closing {
            return;
        }
    }
}
async fn flush(inner: Arc<Inner>) {
    let snapshot = {
        let Ok(mut state) = inner.state.lock() else {
            return;
        };
        if state.prune(now()).is_err() || state.disabled || state.saved == state.revision {
            return;
        }
        (state.revision, state.records.iter().cloned().collect())
    };
    let revision = snapshot.0;
    let worker = inner.clone();
    // One sequential save owner; shutdown waits through the real blocking work.
    let result = tokio::task::spawn_blocking(move || {
        worker
            .disk
            .lock()
            .map_err(|_| Error::Unavailable)?
            .persist(snapshot.1)
    })
    .await;
    if let Ok(mut state) = inner.state.lock() {
        match result {
            Ok(Ok(())) => {
                state.saved = revision;
                state.storage_error = false;
            }
            Ok(Err(_)) => state.storage_error = true,
            Err(_) => {
                state.storage_error = true;
                state.disabled = true;
            }
        }
    }
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
