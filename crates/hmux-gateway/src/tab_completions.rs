//! Shared completion receipts, independent of browser push subscriptions.
//! One bounded event ledger and account-keyed read receipts; no new resident task.
use crate::{auth_store::SessionAccess, hub::CompletionEvent};
use hmux_core::PrivateDir;
use hmux_model::SessionIdentity;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashSet, VecDeque},
    ffi::OsStr,
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::Semaphore;

const MAX_ENTRIES: usize = 256;
const MAX_BYTES: usize = 128 << 10;
const CACHE_ACCOUNTS: usize = 16;
const IO_WORKERS: usize = 2;
const MAX_REVISION: i64 = 9_007_199_254_740_991;
const EVENTS_FILE: &str = "events.json";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub session: SessionIdentity,
    pub event_id: String,
    pub completed_at: String,
}
impl Event {
    fn valid(&self) -> bool {
        self.session.created_at > 0
            && hmux_model::validate_session_id(&self.session.id).is_ok()
            && self.event_id.len() == 64
            && self.event_id.bytes().all(|b| b.is_ascii_hexdigit())
            && self.completed_at.len() <= 64
            && chrono::DateTime::parse_from_rfc3339(&self.completed_at).is_ok()
    }
}
#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    revision: i64,
    events: Vec<Event>,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Reads {
    revision: i64,
    events: Vec<Event>,
}
fn valid_rows(revision: i64, rows: &[Event]) -> bool {
    let mut seen = HashSet::new();
    (0..MAX_REVISION).contains(&revision)
        && rows.len() <= MAX_ENTRIES
        && rows
            .iter()
            .all(|row| row.valid() && seen.insert(&row.session))
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    #[serde(flatten)]
    pub event: Event,
    pub unread: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub version: i64,
    pub sequence: i64,
    pub read_revision: i64,
    pub entries: Vec<Entry>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadRequest {
    pub session: SessionIdentity,
    pub event_id: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Conflict,
    Busy,
    Unavailable,
    Unauthorized,
}

fn authorized(access: &SessionAccess) -> Result<(), Error> {
    if *access.cancelled.borrow()
        || access.cancelled.has_changed().is_err()
        || access.expires_at <= chrono::Utc::now()
    {
        return Err(Error::Unauthorized);
    }
    Ok(())
}
fn account_key(access: &SessionAccess) -> String {
    format!(
        "{:x}.json",
        crate::auth::usage_preference_digest(&access.username, &access.profile)
    )
}
fn load<T: serde::de::DeserializeOwned + Default>(
    dir: &PrivateDir,
    name: &str,
) -> Result<T, Error> {
    match dir.read_private(OsStr::new(name), MAX_BYTES) {
        Ok(raw) => serde_json::from_slice(&raw).map_err(|_| Error::Unavailable),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(_) => Err(Error::Unavailable),
    }
}
struct State {
    dir: PrivateDir,
    lock: Option<hmux_core::FileLock>,
    storage_failed: bool,
    ledger: Ledger,
    reads: VecDeque<(String, Reads)>,
}
impl State {
    fn read_account(&self, key: &str) -> Result<Reads, Error> {
        let value: Reads = load(&self.dir, key)?;
        if !valid_rows(value.revision, &value.events) {
            return Err(Error::Unavailable);
        }
        Ok(value)
    }
    fn cache(&mut self, key: String, value: Reads) {
        self.reads.retain(|(k, _)| *k != key);
        if self.reads.len() == CACHE_ACCOUNTS {
            self.reads.pop_front();
        }
        self.reads.push_back((key, value));
    }
    fn account(&mut self, key: &str) -> Result<Reads, Error> {
        if let Some((_, value)) = self.reads.iter().find(|(k, _)| k == key) {
            return Ok(value.clone());
        }
        let value = self.read_account(key)?;
        self.cache(key.to_owned(), value.clone());
        Ok(value)
    }
    fn snapshot(&self, reads: &Reads, live: &HashSet<SessionIdentity>) -> Snapshot {
        Snapshot {
            version: 1,
            sequence: self.ledger.revision,
            read_revision: reads.revision,
            entries: self
                .ledger
                .events
                .iter()
                .filter(|e| live.contains(&e.session))
                .map(|event| Entry {
                    event: event.clone(),
                    unread: !reads
                        .events
                        .iter()
                        .any(|r| r.session == event.session && r.event_id == event.event_id),
                })
                .collect(),
        }
    }
    fn record(&mut self, event: Event) -> Result<(), Error> {
        if self.storage_failed {
            return Err(Error::Unavailable);
        }
        if !event.valid() {
            return Err(Error::Invalid);
        }
        if let Some(old) = self
            .ledger
            .events
            .iter()
            .find(|e| e.session == event.session)
        {
            if old.event_id == event.event_id
                || chrono::DateTime::parse_from_rfc3339(&old.completed_at).ok()
                    > chrono::DateTime::parse_from_rfc3339(&event.completed_at).ok()
            {
                return Ok(());
            }
        }
        let mut events = self.ledger.events.clone();
        events.retain(|e| e.session != event.session);
        if events.len() == MAX_ENTRIES {
            events.remove(0);
        }
        events.push(event);
        let next = Ledger {
            revision: self.ledger.revision + 1,
            events,
        };
        if !valid_rows(next.revision, &next.events) {
            return Err(Error::Unavailable);
        }
        let raw = serde_json::to_vec(&next).map_err(|_| Error::Unavailable)?;
        if self
            .dir
            .write_atomic_private(OsStr::new(EVENTS_FILE), &raw)
            .is_err()
        {
            // Rename may have succeeded before fsync failed. Reload before any
            // later transaction so an older cache cannot overwrite that event.
            self.storage_failed = true;
            let loaded: Ledger = load(&self.dir, EVENTS_FILE)?;
            if !valid_rows(loaded.revision, &loaded.events) {
                return Err(Error::Unavailable);
            }
            self.ledger = loaded;
            self.storage_failed = false;
            return Err(Error::Unavailable);
        }
        self.ledger = next;
        Ok(())
    }
    fn acknowledge(
        &mut self,
        access: &SessionAccess,
        request: &ReadRequest,
        live: &HashSet<SessionIdentity>,
        current: &impl Fn() -> bool,
    ) -> Result<Snapshot, Error> {
        if self.storage_failed {
            return Err(Error::Unavailable);
        }
        authorized(access)?;
        if !live.contains(&request.session) || !current() {
            return Err(Error::Conflict);
        }
        let Some(event) = self
            .ledger
            .events
            .iter()
            .find(|e| e.session == request.session && e.event_id == request.event_id)
            .cloned()
        else {
            return Err(Error::Conflict);
        };
        let key = account_key(access);
        // Re-read a write target even on cache hit; unsafe modes and post-rename
        // uncertainty must not be hidden by an account's cached receipt.
        let mut reads = self.read_account(&key)?;
        if !reads.events.contains(&event) {
            reads
                .events
                .retain(|e| live.contains(&e.session) && e.session != event.session);
            if reads.events.len() == MAX_ENTRIES {
                reads.events.remove(0);
            }
            reads.events.push(event);
            reads.revision += 1;
            if !valid_rows(reads.revision, &reads.events) {
                return Err(Error::Unavailable);
            }
            let raw = serde_json::to_vec(&reads).map_err(|_| Error::Unavailable)?;
            authorized(access)?;
            if !current() {
                return Err(Error::Conflict);
            }
            if self
                .dir
                .write_atomic_private(OsStr::new(&key), &raw)
                .is_err()
            {
                self.reads.retain(|(k, _)| k != &key);
                return Err(Error::Unavailable);
            }
        }
        self.cache(key, reads.clone());
        Ok(self.snapshot(&reads, live))
    }
}
struct Inner {
    state: Mutex<State>,
    admission: Arc<Semaphore>,
    closed: AtomicBool,
}
#[derive(Clone)]
pub struct Store(Arc<Inner>);
impl Store {
    pub async fn open(dir: PrivateDir) -> Result<Self, Error> {
        tokio::task::spawn_blocking(move || {
            let lock = dir
                .try_lock(OsStr::new("store.lock"))
                .map_err(|_| Error::Unavailable)?
                .ok_or(Error::Busy)?;
            let ledger: Ledger = load(&dir, EVENTS_FILE)?;
            if !valid_rows(ledger.revision, &ledger.events) {
                return Err(Error::Unavailable);
            }
            Ok(Self(Arc::new(Inner {
                state: Mutex::new(State {
                    dir,
                    lock: Some(lock),
                    storage_failed: false,
                    ledger,
                    reads: VecDeque::new(),
                }),
                admission: Arc::new(Semaphore::new(IO_WORKERS)),
                closed: AtomicBool::new(false),
            })))
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    async fn transact<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut State) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let permit = self
            .0
            .admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        self.execute(permit, work).await
    }
    async fn execute<T: Send + 'static>(
        &self,
        permit: tokio::sync::OwnedSemaphorePermit,
        work: impl FnOnce(&mut State) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        let inner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut state = inner.state.lock().map_err(|_| Error::Unavailable)?;
            work(&mut state)
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    pub async fn record(&self, event: &CompletionEvent) -> Result<(), Error> {
        let event = Event {
            session: SessionIdentity {
                id: event.session.id.clone(),
                created_at: event.session.created_at,
            },
            event_id: event.id.clone(),
            completed_at: event.completed_at.clone(),
        };
        // The single completion consumer waits for I/O admission; a pair of
        // browser reads cannot permanently lose a completion on a Busy result.
        let permit = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.0.admission.clone().acquire_owned(),
        )
        .await
        .map_err(|_| Error::Busy)?
        .map_err(|_| Error::Unavailable)?;
        self.execute(permit, move |state| state.record(event)).await
    }
    pub async fn get(
        &self,
        access: &SessionAccess,
        live: HashSet<SessionIdentity>,
    ) -> Result<Snapshot, Error> {
        authorized(access)?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        let key = account_key(access);
        if let Ok(state) = self.0.state.try_lock() {
            if state.storage_failed {
                return Err(Error::Unavailable);
            }
            if let Some((_, reads)) = state.reads.iter().find(|(k, _)| k == &key) {
                return Ok(state.snapshot(reads, &live));
            }
        }
        let access = access.clone();
        self.transact(move |state| {
            authorized(&access)?;
            if state.storage_failed {
                return Err(Error::Unavailable);
            }
            let reads = state.account(&key)?;
            Ok(state.snapshot(&reads, &live))
        })
        .await
    }
    pub async fn acknowledge(
        &self,
        access: &SessionAccess,
        request: ReadRequest,
        live: HashSet<SessionIdentity>,
        current: impl Fn() -> bool + Send + 'static,
    ) -> Result<Snapshot, Error> {
        if request.session.created_at < 1
            || hmux_model::validate_session_id(&request.session.id).is_err()
            || request.event_id.len() != 64
            || !request.event_id.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Invalid);
        }
        let access = access.clone();
        self.transact(move |state| state.acknowledge(&access, &request, &live, &current))
            .await
    }
    pub async fn shutdown(&self) {
        self.0.closed.store(true, Ordering::Release);
        let _drained = self
            .0
            .admission
            .clone()
            .acquire_many_owned(IO_WORKERS as u32)
            .await;
        if let Ok(mut state) = self.0.state.lock() {
            state.lock.take();
        }
    }
}

// Parse only identity fields, skipping paths/labels/transcript-like metadata.
pub(crate) fn live_sessions(raw: Option<&[u8]>) -> HashSet<SessionIdentity> {
    #[derive(Deserialize)]
    struct Catalog {
        sessions: Option<Vec<Identity>>,
    }
    #[derive(Deserialize)]
    struct Identity {
        id: String,
        created_at: i64,
    }
    raw.and_then(|raw| serde_json::from_slice::<Catalog>(raw).ok())
        .and_then(|c| c.sessions)
        .unwrap_or_default()
        .into_iter()
        .map(|s| SessionIdentity {
            id: s.id,
            created_at: s.created_at,
        })
        .collect()
}

#[cfg(test)]
#[path = "tab_completions_tests.rs"]
mod tests;
