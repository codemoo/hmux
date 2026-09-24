//! Private Go v1 push state. This library owns storage only: no routes or sends.
//! One gateway owns the sibling `.lock` inode for the store lifetime. The
//! digest-before-rename check detects unexpected edits by other actors, but is
//! not a compare-and-swap against concurrent external writers.
use crate::auth_store::SessionAccess;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use hmux_core::{FileLock, PrivateDir};
use p256::{
    elliptic_curve::{sec1::ToEncodedPoint, zeroize::Zeroize},
    PublicKey, SecretKey,
};
use serde::{
    de::{self, MapAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fmt, io,
    io::Write,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, SystemTime},
};
use tokio::sync::Semaphore;

const DISK_BYTES: usize = 2 << 20;
const MAX_SUBSCRIPTIONS: usize = 256;
const IO_SLOTS: u32 = 2;
static OPEN_SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Unauthorized,
    Busy,
    Unavailable,
}

/// Validated browser push subscription. The `auth` key is secret browser key
/// material; snapshots are for tightly scoped delivery code, never public APIs.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Subscription {
    pub endpoint: String,
    pub keys: Keys,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Keys {
    pub auth: String,
    pub p256dh: String,
}

impl<'de> Deserialize<'de> for Keys {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct KeysVisitor;
        impl<'de> Visitor<'de> for KeysVisitor {
            type Value = Keys;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a push keys object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Keys, M::Error> {
                let (mut auth, mut p256dh) = (None, None);
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match key.as_ref() {
                        "auth" if auth.is_none() => {
                            auth = Some(map.next_value::<Option<String>>()?.unwrap_or_default())
                        }
                        "p256dh" if p256dh.is_none() => {
                            p256dh = Some(map.next_value::<Option<String>>()?.unwrap_or_default())
                        }
                        "auth" | "p256dh" => return Err(de::Error::custom("duplicate push key")),
                        _ => return Err(de::Error::custom("unknown push key")),
                    }
                }
                Ok(Keys {
                    auth: auth.unwrap_or_default(),
                    p256dh: p256dh.unwrap_or_default(),
                })
            }
        }
        d.deserialize_map(KeysVisitor)
    }
}

impl<'de> Deserialize<'de> for Subscription {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct SubVisitor;
        impl<'de> Visitor<'de> for SubVisitor {
            type Value = Subscription;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a push subscription object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Subscription, M::Error> {
                let (mut endpoint, mut keys) = (None, None);
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match key.as_ref() {
                        "endpoint" if endpoint.is_none() => {
                            endpoint = Some(map.next_value::<Option<String>>()?.unwrap_or_default())
                        }
                        "keys" if keys.is_none() => keys = Some(map.next_value::<Keys>()?),
                        "endpoint" | "keys" => {
                            return Err(de::Error::custom("duplicate push field"))
                        }
                        _ => return Err(de::Error::custom("unknown push field")),
                    }
                }
                Ok(Subscription {
                    endpoint: endpoint.unwrap_or_default(),
                    keys: keys.ok_or_else(|| de::Error::custom("missing push keys"))?,
                })
            }
        }
        d.deserialize_map(SubVisitor)
    }
}

#[derive(Clone, Serialize)]
pub struct PublicConfig {
    pub public_key: String,
    pub login_id: String,
    pub enabled: bool,
    pub endpoint: String,
}

#[derive(Default)]
struct Subscriptions(BTreeMap<String, Subscription>);

impl<'de> Deserialize<'de> for Subscriptions {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SubsVisitor;
        impl<'de> Visitor<'de> for SubsVisitor {
            type Value = Subscriptions;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a bounded push subscription object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut out = BTreeMap::new();
                // Check the cap and decoded key syntax before retaining the
                // 257th entry or decoding its value. JSON escapes are allowed.
                while let Some(id) = map.next_key::<Cow<'de, str>>()? {
                    if out.len() == MAX_SUBSCRIPTIONS
                        || !valid_id(&id)
                        || out.contains_key(id.as_ref())
                    {
                        return Err(de::Error::custom("invalid push subscription map"));
                    }
                    let sub = map.next_value::<Subscription>()?;
                    validate_subscription(&sub)
                        .map_err(|_| de::Error::custom("invalid push subscription"))?;
                    out.insert(id.into_owned(), sub);
                }
                Ok(Subscriptions(out))
            }
        }
        deserializer.deserialize_map(SubsVisitor)
    }
}

struct DiskState {
    version: Option<i64>,
    public_key: Option<String>,
    private_key: Option<String>,
    subscriptions: Subscriptions,
}

impl<'de> Deserialize<'de> for DiskState {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct StateVisitor;
        impl<'de> Visitor<'de> for StateVisitor {
            type Value = DiskState;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a push state object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<DiskState, M::Error> {
                let (mut version, mut public_key, mut private_key, mut subscriptions) =
                    (None, None, None, None);
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match key.as_ref() {
                        "version" if version.is_none() => {
                            version = Some(map.next_value::<Option<i64>>()?)
                        }
                        "public_key" if public_key.is_none() => {
                            public_key = Some(map.next_value::<Option<String>>()?)
                        }
                        "private_key" if private_key.is_none() => {
                            private_key = Some(map.next_value::<Option<String>>()?)
                        }
                        "subscriptions" if subscriptions.is_none() => {
                            subscriptions = Some(map.next_value::<Option<Subscriptions>>()?)
                        }
                        "version" | "public_key" | "private_key" | "subscriptions" => {
                            return Err(de::Error::custom("duplicate push state field"))
                        }
                        _ => return Err(de::Error::custom("unknown push state field")),
                    }
                }
                Ok(DiskState {
                    version: version.flatten(),
                    public_key: public_key.flatten(),
                    private_key: private_key.flatten(),
                    subscriptions: subscriptions.flatten().unwrap_or_default(),
                })
            }
        }
        d.deserialize_map(StateVisitor)
    }
}

struct State {
    dir: PrivateDir,
    name: OsString,
    _lock: Option<FileLock>,
    expected: Option<[u8; 32]>,
    public_key: String,
    private_key: String,
    subscriptions: BTreeMap<String, Subscription>,
    failed: bool,
}
impl Drop for State {
    fn drop(&mut self) {
        self.private_key.zeroize();
    }
}

impl State {
    fn read(&self) -> io::Result<Option<Vec<u8>>> {
        match self.dir.read_private(&self.name, DISK_BYTES) {
            Ok(raw) => Ok(Some(raw)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
    fn save(
        &mut self,
        next: BTreeMap<String, Subscription>,
        check: impl Fn() -> Result<(), Error>,
    ) -> Result<(), Error> {
        if self.failed {
            return Err(Error::Unavailable);
        }
        #[derive(Serialize)]
        struct Output<'a> {
            version: u8,
            public_key: &'a str,
            private_key: &'a str,
            subscriptions: &'a BTreeMap<String, Subscription>,
        }
        let mut writer = Limited(Vec::new());
        serde_json::to_writer(
            &mut writer,
            &Output {
                version: 1,
                public_key: &self.public_key,
                private_key: &self.private_key,
                subscriptions: &next,
            },
        )
        .map_err(|_| Error::Unavailable)?;
        let raw = writer.0;
        let current = self.read().map_err(|_| {
            self.failed = true;
            Error::Unavailable
        })?;
        if current.as_deref().map(digest) != self.expected {
            self.failed = true;
            return Err(Error::Unavailable);
        }
        check()?;
        let new_digest = digest(&raw);
        // A rename may have committed even when directory sync fails. After any
        // write error, refuse further transactions until a fresh open validates
        // the actual file; never attempt to roll back a possibly committed write.
        if self.dir.write_atomic_private(&self.name, &raw).is_err() {
            self.failed = true;
            return Err(Error::Unavailable);
        }
        self.expected = Some(new_digest);
        self.subscriptions = next;
        Ok(())
    }
}

struct Limited(Vec<u8>);
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > DISK_BYTES - self.0.len() {
            return Err(io::Error::other("push state too large"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn digest(raw: &[u8]) -> [u8; 32] {
    Sha256::digest(raw).into()
}
fn now() -> DateTime<Utc> {
    SystemTime::now().into()
}

fn decode_canonical(value: &str, bytes: usize) -> Result<Vec<u8>, Error> {
    if value.len() > (bytes * 4).div_ceil(3) {
        return Err(Error::Invalid);
    }
    let decoded = URL_SAFE_NO_PAD.decode(value).map_err(|_| Error::Invalid)?;
    if decoded.len() != bytes || URL_SAFE_NO_PAD.encode(&decoded) != value {
        return Err(Error::Invalid);
    }
    Ok(decoded)
}

pub fn valid_id(id: &str) -> bool {
    decode_canonical(id, 32).is_ok()
}

pub fn validate_subscription(sub: &Subscription) -> Result<(), Error> {
    validate_endpoint(&sub.endpoint)?;
    decode_canonical(&sub.keys.auth, 16)?;
    let public = decode_canonical(&sub.keys.p256dh, 65)?;
    if public.first() != Some(&4) || PublicKey::from_sec1_bytes(&public).is_err() {
        return Err(Error::Invalid);
    }
    Ok(())
}

pub(crate) fn validate_endpoint(endpoint: &str) -> Result<http::Uri, Error> {
    if endpoint.len() > 2048
        || !endpoint.starts_with("https://")
        || !endpoint.is_ascii()
        || endpoint
            .bytes()
            .any(|b| b <= 0x20 || b == 0x7f || b == b'\\' || b == b'#')
    {
        return Err(Error::Invalid);
    }
    let bytes = endpoint.as_bytes();
    for (i, &byte) in bytes.iter().enumerate() {
        if byte == b'%'
            && (i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit())
        {
            return Err(Error::Invalid);
        }
    }
    let authority = endpoint[8..].split(['/', '?']).next().unwrap_or("");
    if authority.is_empty() || authority.contains(['@', ':', '%']) {
        return Err(Error::Invalid);
    }
    let uri: http::Uri = endpoint.parse().map_err(|_| Error::Invalid)?;
    if uri.scheme_str() != Some("https")
        || uri.authority().map(|a| a.as_str()) != Some(authority)
        || uri.port().is_some()
    {
        return Err(Error::Invalid);
    }
    let host = authority.to_ascii_lowercase();
    let allowed = host == "fcm.googleapis.com"
        || host == "web.push.apple.com"
        || host == "updates.push.services.mozilla.com"
        || host.ends_with(".push.services.mozilla.com")
        || host.ends_with(".notify.windows.com");
    if !allowed {
        return Err(Error::Invalid);
    }
    Ok(uri)
}

fn validate_keys(public: &str, private: &str) -> Result<(), Error> {
    let mut scalar = decode_canonical(private, 32)?;
    let key = SecretKey::from_slice(&scalar).map_err(|_| Error::Invalid);
    scalar.zeroize();
    let key = key?;
    let expected = key.public_key().to_encoded_point(false);
    let encoded = decode_canonical(public, 65)?;
    if encoded.first() != Some(&4) || expected.as_bytes() != encoded {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn generate_keys() -> Result<(String, String), Error> {
    for _ in 0..16 {
        let mut scalar = [0u8; 32];
        getrandom::fill(&mut scalar).map_err(|_| Error::Unavailable)?;
        if let Ok(key) = SecretKey::from_slice(&scalar) {
            let public =
                URL_SAFE_NO_PAD.encode(key.public_key().to_encoded_point(false).as_bytes());
            let private = URL_SAFE_NO_PAD.encode(scalar);
            scalar.zeroize();
            return Ok((public, private));
        }
        scalar.zeroize();
    }
    Err(Error::Unavailable)
}

fn authorized(access: &SessionAccess) -> Result<(), Error> {
    if !valid_id(&access.id)
        || *access.cancelled.borrow()
        || access.cancelled.has_changed().is_err()
        || access.expires_at <= now()
    {
        return Err(Error::Unauthorized);
    }
    Ok(())
}

struct Inner {
    state: Mutex<State>,
    slots: Arc<Semaphore>,
    closed: AtomicBool,
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Cloneable handle; shutdown closes all handles and joins admitted blocking work.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

impl Store {
    /// `credentials_name` is the basename of the Go credentials file; storage
    /// uses `<credentials_name>.push.json` and its persistent `.lock` sibling.
    pub async fn open(dir: PrivateDir, credentials_name: &OsStr) -> Result<Self, Error> {
        if credentials_name.is_empty()
            || Path::new(credentials_name).file_name() != Some(credentials_name)
            || credentials_name == OsStr::new(".")
            || credentials_name == OsStr::new("..")
        {
            return Err(Error::Invalid);
        }
        let mut name = credentials_name.to_os_string();
        name.push(".push.json");
        let mut lock_name = name.clone();
        lock_name.push(".lock");
        let permit = OPEN_SLOTS
            .get_or_init(|| Arc::new(Semaphore::new(IO_SLOTS as usize)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let state = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let lock = dir
                .lock_for(&lock_name, Duration::from_millis(100))
                .map_err(|_| Error::Unavailable)?;
            let mut state = State {
                dir,
                name,
                _lock: Some(lock),
                expected: None,
                public_key: String::new(),
                private_key: String::new(),
                subscriptions: BTreeMap::new(),
                failed: false,
            };
            match state.read().map_err(|_| Error::Unavailable)? {
                Some(raw) => {
                    let parsed: DiskState =
                        serde_json::from_slice(&raw).map_err(|_| Error::Unavailable)?;
                    if parsed.version != Some(1) {
                        return Err(Error::Unavailable);
                    }
                    let public = parsed.public_key.ok_or(Error::Unavailable)?;
                    let private = parsed.private_key.ok_or(Error::Unavailable)?;
                    validate_keys(&public, &private).map_err(|_| Error::Unavailable)?;
                    state.expected = Some(digest(&raw));
                    state.public_key = public;
                    state.private_key = private;
                    state.subscriptions = parsed.subscriptions.0;
                }
                None => {
                    (state.public_key, state.private_key) = generate_keys()?;
                    state.save(BTreeMap::new(), || Ok(()))?;
                }
            }
            Ok(state)
        })
        .await
        .map_err(|_| Error::Unavailable)??;
        Ok(Self {
            inner: Arc::new(Inner {
                state: Mutex::new(state),
                slots: Arc::new(Semaphore::new(IO_SLOTS as usize)),
                closed: AtomicBool::new(false),
            }),
        })
    }

    async fn transact<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut State, &AtomicBool) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        let permit = self
            .inner
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        let inner = self.inner.clone();
        let cancellation = Arc::new(AtomicBool::new(false));
        let caller = CancelOnDrop(cancellation.clone());
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit; // survives caller cancellation until worker completion
            let mut state = inner.state.lock().map_err(|_| Error::Unavailable)?;
            if inner.closed.load(Ordering::Acquire)
                || cancellation.load(Ordering::Acquire)
                || state.failed
            {
                return Err(Error::Unavailable);
            }
            work(&mut state, &cancellation)
        })
        .await
        .map_err(|_| Error::Unavailable)?;
        drop(caller);
        result
    }

    pub async fn public_config(&self, access: &SessionAccess) -> Result<PublicConfig, Error> {
        authorized(access)?;
        let access = access.clone();
        self.transact(move |state, _| {
            authorized(&access)?;
            let sub = state.subscriptions.get(&access.id);
            Ok(PublicConfig {
                public_key: state.public_key.clone(),
                login_id: access.id,
                enabled: sub.is_some(),
                endpoint: sub.map_or("", |s| s.endpoint.as_str()).into(),
            })
        })
        .await
    }

    /// Bounded delivery snapshot. Never includes the VAPID private scalar.
    pub async fn snapshot(&self) -> Result<Vec<(String, Subscription)>, Error> {
        self.transact(|state, _| {
            Ok(state
                .subscriptions
                .iter()
                .map(|(id, sub)| (id.clone(), sub.clone()))
                .collect())
        })
        .await
    }

    pub(crate) async fn subscription(&self, login_id: &str) -> Result<Option<Subscription>, Error> {
        if !valid_id(login_id) {
            return Err(Error::Invalid);
        }
        let login_id = login_id.to_owned();
        self.transact(move |state, _| Ok(state.subscriptions.get(&login_id).cloned()))
            .await
    }

    /// Nonblocking exact endpoint/key check at the end of the pre-POST gate.
    /// Busy state fails closed instead of waiting after workspace authorization.
    pub(crate) fn current_subscription(
        &self,
        login_id: &str,
        expected: &Subscription,
    ) -> Result<bool, Error> {
        if !valid_id(login_id) {
            return Err(Error::Invalid);
        }
        let state = self.inner.state.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => Error::Busy,
            std::sync::TryLockError::Poisoned(_) => Error::Unavailable,
        })?;
        if self.inner.closed.load(Ordering::Acquire) || state.failed {
            return Err(Error::Unavailable);
        }
        Ok(state.subscriptions.get(login_id) == Some(expected))
    }

    /// Prepare inside this store's bounded blocking admission without exporting
    /// its VAPID scalar. This is not permission to send later: delivery must
    /// recheck authoritative auth, exact subscription and cancellation itself.
    pub async fn prepare_for_delivery(
        &self,
        access: &SessionAccess,
        expected: &Subscription,
        subject_origin: &str,
        payload: &[u8],
    ) -> Result<crate::push_crypto::Prepared, Error> {
        use crate::push_crypto;
        authorized(access)?;
        if payload.len() > push_crypto::MAX_PLAINTEXT || subject_origin.len() > 2048 {
            return Err(Error::Invalid);
        }
        validate_subscription(expected)?;
        let access = access.clone();
        let expected = expected.clone();
        let subject_origin = subject_origin.to_owned();
        let payload = payload.to_vec();
        let inner = self.inner.clone();
        self.transact(move |state, cancelled| {
            let check = || {
                authorized(&access)?;
                if state.subscriptions.get(&access.id) != Some(&expected) {
                    return Err(Error::Unauthorized);
                }
                if cancelled.load(Ordering::Acquire) || inner.closed.load(Ordering::Acquire) {
                    return Err(Error::Unavailable);
                }
                Ok(())
            };
            check()?;
            let identity =
                push_crypto::SigningIdentity::from_parts(&state.public_key, &state.private_key)
                    .map_err(|_| Error::Unavailable)?;
            let result = push_crypto::prepare(
                &identity,
                &expected,
                &subject_origin,
                &access.id,
                &payload,
                now().timestamp(),
            )
            .map_err(|error| match error {
                push_crypto::Error::Invalid | push_crypto::Error::TooLarge => Error::Invalid,
                push_crypto::Error::Unavailable => Error::Unavailable,
            })?;
            check()?;
            Ok(result)
        })
        .await
    }

    /// `live` is an authoritative, nonblocking auth lookup evaluated under the
    /// store transaction lock. It must return Err on unknown/busy state, never
    /// turn uncertainty into `false`. Auth lock ordering is store -> auth.
    pub async fn subscribe<F>(
        &self,
        access: &SessionAccess,
        sub: Subscription,
        live: F,
    ) -> Result<(), Error>
    where
        F: Fn(&str) -> Result<bool, Error> + Send + 'static,
    {
        authorized(access)?;
        validate_subscription(&sub)?;
        let access = access.clone();
        let inner = self.inner.clone();
        self.transact(move |state, cancelled| {
            authorized(&access)?;
            if !live(&access.id)? {
                return Err(Error::Unauthorized);
            }
            let mut next = BTreeMap::new();
            for (id, old) in &state.subscriptions {
                if id != &access.id && old.endpoint != sub.endpoint && live(id)? {
                    next.insert(id.clone(), old.clone());
                }
            }
            next.insert(access.id.clone(), sub);
            if next.len() > MAX_SUBSCRIPTIONS {
                return Err(Error::Busy);
            }
            authorized(&access)?;
            if !live(&access.id)? {
                return Err(Error::Unauthorized);
            }
            if cancelled.load(Ordering::Acquire) || inner.closed.load(Ordering::Acquire) {
                return Err(Error::Unavailable);
            }
            state.save(next, || {
                authorized(&access)?;
                if !live(&access.id)? {
                    return Err(Error::Unauthorized);
                }
                authorized(&access)?;
                if cancelled.load(Ordering::Acquire) || inner.closed.load(Ordering::Acquire) {
                    return Err(Error::Unavailable);
                }
                Ok(())
            })
        })
        .await
    }

    /// Empty/None guard is a user unsubscribe; delivery passes the exact endpoint
    /// it attempted so a stale 404/410 cannot remove a replacement subscription.
    pub async fn remove(&self, login_id: &str, endpoint_guard: Option<&str>) -> Result<(), Error> {
        if !valid_id(login_id) || endpoint_guard.is_some_and(|s| s.len() > 2048) {
            return Err(Error::Invalid);
        }
        let login_id = login_id.to_owned();
        let guard = endpoint_guard.map(str::to_owned);
        let inner = self.inner.clone();
        self.transact(move |state, cancelled| {
            let Some(old) = state.subscriptions.get(&login_id) else {
                return Ok(());
            };
            if guard
                .as_deref()
                .is_some_and(|v| !v.is_empty() && v != old.endpoint)
            {
                return Ok(());
            }
            let mut next = state.subscriptions.clone();
            next.remove(&login_id);
            if cancelled.load(Ordering::Acquire) || inner.closed.load(Ordering::Acquire) {
                return Err(Error::Unavailable);
            }
            state.save(next, || {
                if cancelled.load(Ordering::Acquire) || inner.closed.load(Ordering::Acquire) {
                    return Err(Error::Unavailable);
                }
                Ok(())
            })
        })
        .await
    }

    /// Close admission, wait for both occupied slots, then release the lifetime
    /// lock even when other Store clones remain alive.
    pub async fn shutdown(&self) {
        self.inner.closed.store(true, Ordering::Release);
        if let Ok(_drained) = self.inner.slots.clone().acquire_many_owned(IO_SLOTS).await {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state._lock.take();
        }
    }
}

#[cfg(test)]
#[path = "push_state_tests.rs"]
mod tests;
