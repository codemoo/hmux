//! Account-scoped Go-compatible usage settings. One store per gateway owner.
//! Bounded cache and blocking admission; no daemon or per-account worker.
use crate::auth_store::SessionAccess;
use hmux_core::PrivateDir;
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use std::{
    collections::VecDeque,
    ffi::OsStr,
    fmt, io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::Semaphore;

const CACHE_ACCOUNTS: usize = 9;
const IO_WORKERS: usize = 2;
const MAX_BYTES: usize = 4096;
const MAX_REVISION: i64 = 1 << 53;

fn null_default<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(
    d: D,
) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderPreference {
    #[serde(deserialize_with = "null_default")]
    pub enabled: bool,
    #[serde(deserialize_with = "null_default")]
    pub source: String,
}
fn provider<'de, D: Deserializer<'de>>(d: D) -> Result<ProviderPreference, D::Error> {
    let Some(raw) = Option::<Box<RawValue>>::deserialize(d)? else {
        return Ok(ProviderPreference::default());
    };
    if !raw.get().trim_start().starts_with('{') {
        return Err(serde::de::Error::custom("provider object required"));
    }
    serde_json::from_str(raw.get())
        .map_err(|_| serde::de::Error::custom("invalid provider preference"))
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    #[serde(default, deserialize_with = "null_default")]
    pub version: i64,
    #[serde(default, deserialize_with = "null_default")]
    pub revision: i64,
    #[serde(default, deserialize_with = "provider")]
    pub claude: ProviderPreference,
    #[serde(default, deserialize_with = "provider")]
    pub codex: ProviderPreference,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: 1,
            revision: 0,
            claude: ProviderPreference {
                enabled: true,
                source: "cswap".into(),
            },
            codex: ProviderPreference {
                enabled: true,
                source: "codex-lb".into(),
            },
        }
    }
}
impl Preferences {
    pub fn valid(&self) -> bool {
        self.version == 1
            && (0..MAX_REVISION).contains(&self.revision)
            && matches!(self.claude.source.as_str(), "cli" | "cswap")
            && matches!(self.codex.source.as_str(), "cli" | "codex-lb")
    }
    pub fn decode(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > MAX_BYTES || raw.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
            return Err(Error::Invalid);
        }
        let value: Self = serde_json::from_slice(raw).map_err(|_| Error::Invalid)?;
        if !value.valid() {
            return Err(Error::Invalid);
        }
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Conflict,
    Busy,
    Unavailable,
    Unauthorized,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid usage preferences",
            Self::Conflict => "usage preferences changed",
            Self::Busy => "usage preferences busy",
            Self::Unavailable => "usage preferences unavailable",
            Self::Unauthorized => "session expired",
        })
    }
}
impl std::error::Error for Error {}

fn key(access: &SessionAccess) -> [u8; 32] {
    crate::auth::usage_preference_digest(&access.username, &access.profile).into()
}
fn authorized(access: &SessionAccess) -> Result<(), Error> {
    if *access.cancelled.borrow()
        || access.cancelled.has_changed().is_err()
        || access.expires_at <= chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
    {
        return Err(Error::Unauthorized);
    }
    Ok(())
}
fn filename(key: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut name = String::with_capacity(69);
    for byte in key {
        write!(name, "{byte:02x}").expect("String formatter");
    }
    name.push_str(".json");
    name
}
struct State {
    dir: PrivateDir,
    cache: VecDeque<([u8; 32], Preferences)>,
}
impl State {
    fn read(&self, key: &[u8; 32]) -> Result<Preferences, Error> {
        match self.dir.read_private(OsStr::new(&filename(key)), MAX_BYTES) {
            Ok(raw) => Preferences::decode(&raw).map_err(|_| Error::Unavailable),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Preferences::default()),
            Err(_) => Err(Error::Unavailable),
        }
    }
    fn cache(&mut self, key: [u8; 32], value: Preferences) {
        self.cache.retain(|(k, _)| *k != key);
        if self.cache.len() == CACHE_ACCOUNTS {
            self.cache.pop_front();
        }
        self.cache.push_back((key, value));
    }
    fn get(&mut self, key: [u8; 32]) -> Result<Preferences, Error> {
        if let Some((_, value)) = self.cache.iter().find(|(k, _)| *k == key) {
            return Ok(value.clone());
        }
        let value = self.read(&key)?;
        self.cache(key, value.clone());
        Ok(value)
    }
    fn set(
        &mut self,
        key: [u8; 32],
        mut next: Preferences,
        access: &SessionAccess,
    ) -> Result<Preferences, Error> {
        // A worker can wait for another disk transaction after HTTP authorization.
        authorized(access)?;
        if !next.valid() {
            return Err(Error::Invalid);
        }
        let old = self.get(key)?;
        // Recheck the actual target before replacing it: private modes, links,
        // external edits and post-rename durability errors cannot use stale cache.
        let current = self.read(&key)?;
        if old != current || current.revision != next.revision {
            self.cache(key, current);
            return Err(Error::Conflict);
        }
        next.revision += 1;
        if !next.valid() {
            return Err(Error::Unavailable);
        }
        let raw = serde_json::to_vec(&next).map_err(|_| Error::Unavailable)?;
        // Recheck after disk reads, just before the atomic transaction begins.
        // Revocation after this point cannot roll back a rename already started.
        authorized(access)?;
        if self
            .dir
            .write_atomic_private(OsStr::new(&filename(&key)), &raw)
            .is_err()
        {
            self.cache.retain(|(k, _)| *k != key);
            return Err(Error::Unavailable);
        }
        self.cache(key, next.clone());
        Ok(next)
    }
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}
struct Inner {
    state: Mutex<State>,
    admission: Arc<Semaphore>,
    closed: AtomicBool,
}
impl Store {
    /// Open/create the private directory during bounded startup, before passing
    /// its fd here. Go and Rust gateways must not concurrently own these files.
    pub fn new(dir: PrivateDir) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    dir,
                    cache: VecDeque::new(),
                }),
                admission: Arc::new(Semaphore::new(IO_WORKERS)),
                closed: AtomicBool::new(false),
            }),
        }
    }
    async fn transact(
        &self,
        work: impl FnOnce(&mut State) -> Result<Preferences, Error> + Send + 'static,
    ) -> Result<Preferences, Error> {
        let permit = self
            .inner
            .admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            // Keep the permit until the actual transaction completes even if its
            // HTTP caller disappears. A begun atomic save may still commit.
            let _permit = permit;
            let mut guard = inner.state.lock().map_err(|_| Error::Unavailable)?;
            work(&mut guard)
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    pub async fn get(&self, access: &SessionAccess) -> Result<Preferences, Error> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        let key = key(access);
        // Cached reads do not need a blocking worker, and never block an async
        // executor on a disk transaction holding the mutex.
        if let Ok(state) = self.inner.state.try_lock() {
            if let Some((_, value)) = state.cache.iter().find(|(k, _)| *k == key) {
                return Ok(value.clone());
            }
        }
        self.transact(move |state| state.get(key)).await
    }
    pub async fn set(
        &self,
        access: &SessionAccess,
        next: Preferences,
    ) -> Result<Preferences, Error> {
        if !next.valid() {
            return Err(Error::Invalid);
        }
        let key = key(access);
        let access = access.clone();
        self.transact(move |state| state.set(key, next, &access))
            .await
    }
    pub async fn shutdown(&self) {
        self.inner.closed.store(true, Ordering::Release);
        // Capture both slots after closing admission logically. A caller which
        // raced slot acquisition rechecks `closed` before spawning its worker.
        let _drained = self
            .inner
            .admission
            .clone()
            .acquire_many_owned(IO_WORKERS as u32)
            .await;
    }
}

#[cfg(test)]
#[path = "usage_preferences_tests.rs"]
mod tests;
