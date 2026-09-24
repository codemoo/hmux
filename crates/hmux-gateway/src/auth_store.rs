//! Persistent account authentication and session transactions. This is an owner
//! for synthetic/private state, not an HTTP policy or cookie implementation.
use crate::admission::PasswordAdmission;
use crate::auth::{self, Credentials, PersistedSession, PersistedSessionFile};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration as ChronoDuration, NaiveDateTime, SecondsFormat, Utc};
use hmux_core::PrivateDir;
use serde::Serialize;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::net::IpAddr;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use subtle::ConstantTimeEq;
use tokio::sync::{watch, Semaphore};

const CREDENTIAL_LIMIT: usize = 4096;
const IO_WORKERS: usize = 8;
const MAX_ACCOUNTS: usize = 9;
const MAX_BACKUPS_PER_ACCOUNT: usize = 16;
const MAX_BACKUPS: usize = MAX_ACCOUNTS * MAX_BACKUPS_PER_ACCOUNT;
const SEEN_PERSIST_INTERVAL: i64 = 5 * 60;
static OPEN_ADMISSION: OnceLock<Arc<Semaphore>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStatus {
    Invalid,
    Succeeded,
    TotpRequired,
    RateLimited,
    StorageUnavailable,
}
pub struct LoginResult {
    pub status: LoginStatus,
    pub token: Option<String>,
}
impl fmt::Debug for LoginResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginResult")
            .field("status", &self.status)
            .field("token_present", &self.token.is_some())
            .finish()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityStatus {
    Ok,
    Forbidden,
    RateLimited,
    StorageUnavailable,
    StaleSession,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    Busy,
    StorageUnavailable,
    SessionNotFound,
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Busy => "authentication store is busy",
            Self::StorageUnavailable => "authentication storage is unavailable",
            Self::SessionNotFound => "session not found",
        })
    }
}
impl std::error::Error for StoreError {}

pub struct LoginRequest {
    pub username: String,
    pub password: String,
    pub code: String,
    /// Trusted, normalized address key, not a browser-controlled header.
    pub source: String,
    pub ip: String,
    pub browser: String,
    pub now: DateTime<Utc>,
}
pub struct SecurityRequest {
    pub token: String,
    pub password: String,
    pub code: String,
    pub enabled: bool,
    pub now: DateTime<Utc>,
}
#[derive(Clone)]
pub struct SessionAccess {
    pub id: String,
    pub expires_at: DateTime<Utc>,
    pub csrf: String,
    pub username: String,
    pub profile: String,
    pub cancelled: watch::Receiver<bool>,
}
impl SessionAccess {
    pub async fn wait_cancelled(&mut self) {
        while !*self.cancelled.borrow() && self.cancelled.changed().await.is_ok() {}
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub location: String,
    pub id: String,
    pub browser: String,
    pub ip: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub current: bool,
}

struct Account {
    credentials: Credentials,
    path: PathBuf,
    profile: String,
}
struct SessionCancel {
    sender: watch::Sender<bool>,
}
impl SessionCancel {
    fn new() -> Arc<Self> {
        let (sender, _) = watch::channel(false);
        Arc::new(Self { sender })
    }
    fn cancel(&self) {
        self.sender.send_replace(true);
    }
}
#[derive(Default)]
struct ConnectionRegistry {
    closed: bool,
    sessions: Vec<Weak<SessionCancel>>,
}
impl ConnectionRegistry {
    fn sync(&mut self, sessions: &HashMap<String, Session>) {
        self.sessions.clear();
        for session in sessions.values() {
            if self.closed {
                session.cancel();
            }
            self.sessions.push(Arc::downgrade(&session.cancel));
        }
    }
    fn cancel_all(&mut self) {
        self.closed = true;
        self.sessions.retain(|weak| {
            if let Some(cancel) = weak.upgrade() {
                cancel.cancel();
                true
            } else {
                false
            }
        });
    }
}
struct Session {
    id: String,
    username: String,
    profile: String,
    fingerprint: String,
    browser: String,
    ip: String,
    created: DateTime<Utc>,
    seen: DateTime<Utc>,
    persisted_seen: DateTime<Utc>,
    expires: DateTime<Utc>,
    cancel: Arc<SessionCancel>,
}
impl Session {
    fn from_row(row: PersistedSession) -> io::Result<Self> {
        let parse = |raw: &str| {
            DateTime::parse_from_rfc3339(raw)
                .map(|v| v.with_timezone(&Utc))
                .map_err(|_| invalid_data("invalid session time"))
        };
        let cancel = SessionCancel::new();
        Ok(Self {
            id: row.id,
            username: row.username,
            profile: row.profile,
            fingerprint: row.credential_fingerprint,
            browser: row.browser,
            ip: row.ip,
            created: parse(&row.created_at)?,
            seen: parse(&row.last_seen_at)?,
            persisted_seen: parse(&row.last_seen_at)?,
            expires: parse(&row.expires_at)?,
            cancel,
        })
    }
    fn persisted(&self, hash: &str) -> PersistedSession {
        PersistedSession {
            token_hash: hash.to_owned(),
            id: self.id.clone(),
            username: self.username.clone(),
            profile: self.profile.clone(),
            credential_fingerprint: self.fingerprint.clone(),
            browser: self.browser.clone(),
            ip: self.ip.clone(),
            created_at: go_time(self.created),
            last_seen_at: go_time(self.seen),
            expires_at: go_time(self.expires),
        }
    }
    fn cancel(&self) {
        self.cancel.cancel();
    }
}
struct State {
    path: PathBuf,
    sessions_path: PathBuf,
    accounts: HashMap<String, Account>,
    sessions: HashMap<String, Session>,
    attempts: HashMap<String, Vec<DateTime<Utc>>>,
    security_attempts: HashMap<String, Vec<DateTime<Utc>>>,
    storage_failed: bool,
}
impl State {
    fn account(&self, username: &str) -> Option<&Account> {
        self.accounts.get(username)
    }
    fn fail_storage(&mut self) {
        self.storage_failed = true;
        for session in self.sessions.values() {
            session.cancel();
        }
    }
    fn save_sessions(&mut self, next: &HashMap<String, Session>) -> Result<(), StoreError> {
        let result = write_sessions(&self.sessions_path, next);
        if result.is_err() {
            self.fail_storage();
        }
        result.map_err(|_| StoreError::StorageUnavailable)
    }
    fn prune(&mut self, now: DateTime<Utc>) -> Result<(), StoreError> {
        if self.storage_failed {
            return Err(StoreError::StorageUnavailable);
        }
        if !self.sessions.values().any(|s| now >= s.expires) {
            return Ok(());
        }
        let mut next = clone_sessions(&self.sessions);
        next.retain(|_, s| now < s.expires);
        self.save_sessions(&next)?;
        for (key, session) in &self.sessions {
            if !next.contains_key(key) {
                session.cancel();
            }
        }
        self.sessions = next;
        Ok(())
    }
}
fn clone_sessions(sessions: &HashMap<String, Session>) -> HashMap<String, Session> {
    sessions
        .iter()
        .map(|(k, s)| {
            (
                k.clone(),
                Session {
                    id: s.id.clone(),
                    username: s.username.clone(),
                    profile: s.profile.clone(),
                    fingerprint: s.fingerprint.clone(),
                    browser: s.browser.clone(),
                    ip: s.ip.clone(),
                    created: s.created,
                    seen: s.seen,
                    persisted_seen: s.persisted_seen,
                    expires: s.expires,
                    cancel: s.cancel.clone(),
                },
            )
        })
        .collect()
}

#[derive(Clone)]
pub struct AuthStore {
    state: Arc<Mutex<State>>,
    io: Arc<Semaphore>,
    kdf: PasswordAdmission,
    connections: Arc<Mutex<ConnectionRegistry>>,
    closed: Arc<AtomicBool>,
}
impl AuthStore {
    /// Open Go v1 credentials, optional additional accounts, and persisted sessions.
    /// All filesystem work occurs in a blocking worker before the store is exposed.
    pub async fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let permit = OPEN_ADMISSION
            .get_or_init(|| Arc::new(Semaphore::new(2)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| io::Error::other("authentication startup is busy"))?;
        let state = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let path = if path.is_absolute() {
                path
            } else {
                std::env::current_dir()?.join(path)
            };
            load_state(path, utc_now())
        })
        .await
        .map_err(io::Error::other)??;
        let mut connections = ConnectionRegistry::default();
        connections.sync(&state.sessions);
        Ok(Self {
            state: Arc::new(Mutex::new(state)),
            io: Arc::new(Semaphore::new(IO_WORKERS)),
            kdf: PasswordAdmission::default(),
            connections: Arc::new(Mutex::new(connections)),
            closed: Arc::new(AtomicBool::new(false)),
        })
    }
    async fn transact<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut State) -> T + Send + 'static,
    ) -> Result<T, StoreError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(StoreError::StorageUnavailable);
        }
        let permit = self
            .io
            .clone()
            .try_acquire_owned()
            .map_err(|_| StoreError::Busy)?;
        if self.closed.load(Ordering::Acquire) {
            return Err(StoreError::StorageUnavailable);
        }
        let state = self.state.clone();
        let connections = self.connections.clone();
        let closed = self.closed.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut guard = match state.lock() {
                Ok(guard) => guard,
                Err(poison) => {
                    let mut guard = poison.into_inner();
                    guard.fail_storage();
                    connections
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .cancel_all();
                    return Err(StoreError::StorageUnavailable);
                }
            };
            if closed.load(Ordering::Acquire) {
                return Err(StoreError::StorageUnavailable);
            }
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&mut guard))) {
                Ok(value) => {
                    connections
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .sync(&guard.sessions);
                    Ok(value)
                }
                Err(_) => {
                    guard.fail_storage();
                    connections
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .cancel_all();
                    Err(StoreError::StorageUnavailable)
                }
            }
        })
        .await
        .map_err(|_| StoreError::StorageUnavailable)?
    }
    /// Performs one login attempt. The two-worker KDF permit stays in the blocking
    /// closure even if this future or its JoinHandle is dropped or aborted. Once a
    /// disk transaction starts, it may commit after the caller disconnects.
    pub async fn login(&self, request: LoginRequest) -> LoginResult {
        let invalid = || LoginResult {
            status: LoginStatus::Invalid,
            token: None,
        };
        if request.username.len() > 80 || request.password.len() > 128 {
            return invalid();
        }
        let origin = if request.source.len() > 80 {
            "unknown".to_owned()
        } else {
            request.source.clone()
        };
        let ip = if request.ip == "unknown" || request.ip.parse::<IpAddr>().is_ok() {
            request.ip.clone()
        } else {
            "unknown".into()
        };
        let browser = if valid_text(&request.browser, 80) {
            request.browser.clone()
        } else {
            "Unknown browser".into()
        };
        let username = request.username.clone();
        let now = request.now;
        let snapshot = match self
            .transact(move |state| {
                if state.storage_failed {
                    return Err(LoginStatus::StorageUnavailable);
                }
                if !allow_attempt(&mut state.attempts, origin, now, 1024) {
                    return Err(LoginStatus::RateLimited);
                }
                let account = state.account(&username).unwrap_or_else(|| {
                    state
                        .accounts
                        .values()
                        .find(|account| account.profile.is_empty())
                        .expect("primary account")
                });
                Ok((
                    account.credentials.clone(),
                    account.profile.clone(),
                    account.credentials.fingerprint(),
                ))
            })
            .await
        {
            Ok(Ok(value)) => value,
            Ok(Err(status)) => {
                return LoginResult {
                    status,
                    token: None,
                }
            }
            Err(_) => {
                return LoginResult {
                    status: LoginStatus::RateLimited,
                    token: None,
                }
            }
        };
        let (credentials, profile, fingerprint) = snapshot;
        let password = request.password;
        let salt = credentials.salt.clone();
        let hash = match self
            .kdf
            .try_spawn(move || auth::derive_password(&password, &salt))
        {
            Ok(job) => match job.await {
                Ok(hash) => hash,
                Err(_) => return invalid(),
            },
            Err(_) => {
                return LoginResult {
                    status: LoginStatus::RateLimited,
                    token: None,
                }
            }
        };
        if !bool::from(hash.ct_eq(credentials.hash.as_slice()))
            || !bool::from(
                request
                    .username
                    .as_bytes()
                    .ct_eq(credentials.username.as_bytes()),
            )
        {
            return invalid();
        }
        let step = if !credentials.totp_disabled && !request.code.is_empty() {
            credentials
                .match_code(&request.code, now.timestamp())
                .unwrap_or(-1)
        } else {
            -1
        };
        let username = request.username;
        let result = self
            .transact(move |state| {
                if state.storage_failed {
                    return LoginResult {
                        status: LoginStatus::StorageUnavailable,
                        token: None,
                    };
                }
                let Some(account) = state.account(&username) else {
                    return invalid();
                };
                if account.profile != profile || account.credentials.fingerprint() != fingerprint {
                    return invalid();
                }
                if !account.credentials.totp_disabled {
                    if request.code.is_empty() {
                        return LoginResult {
                            status: LoginStatus::TotpRequired,
                            token: None,
                        };
                    }
                    if step < 0 || step <= account.credentials.last_step {
                        return invalid();
                    }
                }
                if !account.credentials.totp_disabled {
                    let account = state.accounts.get_mut(&username).expect("account exists");
                    let expected = account.credentials.clone();
                    let mut next = expected.clone();
                    next.last_step = step;
                    if write_credential(&account.path, &expected, &next).is_err() {
                        state.fail_storage();
                        return LoginResult {
                            status: LoginStatus::StorageUnavailable,
                            token: None,
                        };
                    }
                    account.credentials = next;
                }
                if state.prune(now).is_err() {
                    return LoginResult {
                        status: LoginStatus::StorageUnavailable,
                        token: None,
                    };
                }
                let mut next = clone_sessions(&state.sessions);
                if next.values().filter(|s| s.username == username).count()
                    >= auth::MAX_SESSIONS_PER_ACCOUNT
                {
                    if let Some((oldest, _)) = next
                        .iter()
                        .filter(|(_, s)| s.username == username)
                        .min_by_key(|(_, s)| s.seen)
                        .map(|(k, s)| (k.clone(), s.seen))
                    {
                        next.remove(&oldest);
                    }
                }
                let token = loop {
                    match random_token() {
                        Ok(token) if !next.contains_key(&auth::token_hash(&token)) => break token,
                        Ok(_) => continue,
                        Err(_) => {
                            state.fail_storage();
                            return LoginResult {
                                status: LoginStatus::StorageUnavailable,
                                token: None,
                            };
                        }
                    }
                };
                let id = loop {
                    match random_token() {
                        Ok(id) if !next.values().any(|s| s.id == id) => break id,
                        Ok(_) => continue,
                        Err(_) => {
                            state.fail_storage();
                            return LoginResult {
                                status: LoginStatus::StorageUnavailable,
                                token: None,
                            };
                        }
                    }
                };
                let cancel = SessionCancel::new();
                next.insert(
                    auth::token_hash(&token),
                    Session {
                        id,
                        username: username.clone(),
                        profile: profile.clone(),
                        fingerprint: state.accounts[&username].credentials.fingerprint(),
                        browser,
                        ip,
                        created: now,
                        seen: now,
                        persisted_seen: now,
                        expires: now + ChronoDuration::seconds(auth::LOGIN_LIFETIME_SECONDS),
                        cancel,
                    },
                );
                if state.save_sessions(&next).is_err() {
                    return LoginResult {
                        status: LoginStatus::StorageUnavailable,
                        token: None,
                    };
                }
                for (key, session) in &state.sessions {
                    if !next.contains_key(key) {
                        session.cancel();
                    }
                }
                state.sessions = next;
                LoginResult {
                    status: LoginStatus::Succeeded,
                    token: Some(token),
                }
            })
            .await;
        result.unwrap_or(LoginResult {
            status: LoginStatus::StorageUnavailable,
            token: None,
        })
    }
    /// Checked session access for HTTP. `None` means missing or expired;
    /// admission pressure and persistence failure remain distinct errors.
    pub async fn access(
        &self,
        token: &str,
        touch: bool,
        now: DateTime<Utc>,
    ) -> Result<Option<SessionAccess>, StoreError> {
        let key = auth::token_hash(token);
        let csrf = auth::csrf_token(token);
        self.transact(move |state| {
            state.prune(now)?;
            let Some(session) = state.sessions.get(&key) else {
                return Ok(None);
            };
            if touch
                && now
                    .signed_duration_since(session.persisted_seen)
                    .num_seconds()
                    >= SEEN_PERSIST_INTERVAL
            {
                let mut next = clone_sessions(&state.sessions);
                let updated = next.get_mut(&key).expect("session exists");
                updated.seen = now;
                updated.persisted_seen = now;
                state.save_sessions(&next)?;
                state.sessions = next;
            } else if touch {
                state.sessions.get_mut(&key).expect("session exists").seen = now;
            }
            let session = state.sessions.get(&key).expect("session exists");
            Ok(Some(SessionAccess {
                id: session.id.clone(),
                expires_at: session.expires,
                csrf,
                username: session.username.clone(),
                profile: session.profile.clone(),
                cancelled: session.cancel.sender.subscribe(),
            }))
        })
        .await?
    }
    /// Compatibility wrapper for non-HTTP callers. HTTP must use `access`.
    pub async fn get(&self, token: &str, touch: bool, now: DateTime<Utc>) -> Option<SessionAccess> {
        self.access(token, touch, now).await.ok().flatten()
    }
    /// Internal push lookup only. A public login ID is never a bearer token.
    /// This must stay nonblocking because subscription transactions call it
    /// while holding the push-state lock (push -> auth lock order).
    pub(crate) fn push_login_by_id(
        &self,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<SessionAccess>, StoreError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(StoreError::StorageUnavailable);
        }
        let state = self.state.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => StoreError::Busy,
            std::sync::TryLockError::Poisoned(_) => StoreError::StorageUnavailable,
        })?;
        if state.storage_failed || self.closed.load(Ordering::Acquire) {
            return Err(StoreError::StorageUnavailable);
        }
        let Some(session) = state.sessions.values().find(|session| session.id == id) else {
            return Ok(None);
        };
        let Some(account) = state.accounts.get(&session.username) else {
            return Ok(None);
        };
        if session.expires <= now
            || session.profile != account.profile
            || session.fingerprint != account.credentials.fingerprint()
            || *session.cancel.sender.borrow()
        {
            return Ok(None);
        }
        Ok(Some(SessionAccess {
            id: session.id.clone(),
            expires_at: session.expires,
            csrf: String::new(),
            username: session.username.clone(),
            profile: session.profile.clone(),
            cancelled: session.cancel.sender.subscribe(),
        }))
    }
    pub async fn identity_checked(
        &self,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<(String, String)>, StoreError> {
        let key = auth::token_hash(token);
        self.transact(move |state| {
            state.prune(now)?;
            Ok(state
                .sessions
                .get(&key)
                .map(|s| (s.username.clone(), s.profile.clone())))
        })
        .await?
    }
    pub async fn identity(&self, token: &str, now: DateTime<Utc>) -> Option<(String, String)> {
        self.identity_checked(token, now).await.ok().flatten()
    }
    pub async fn list_sessions_checked(
        &self,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Vec<SessionInfo>>, StoreError> {
        let key = auth::token_hash(token);
        self.transact(move |state| {
            state.prune(now)?;
            let Some(current) = state.sessions.get(&key) else {
                return Ok(None);
            };
            let mut result: Vec<_> = state
                .sessions
                .iter()
                .filter(|(_, s)| s.username == current.username && s.profile == current.profile)
                .map(|(candidate, s)| SessionInfo {
                    location: String::new(),
                    id: s.id.clone(),
                    browser: s.browser.clone(),
                    ip: s.ip.clone(),
                    created_at: s.created,
                    last_seen_at: s.seen,
                    expires_at: s.expires,
                    current: candidate == &key,
                })
                .collect();
            result.sort_by(|a, b| {
                b.current
                    .cmp(&a.current)
                    .then_with(|| b.last_seen_at.cmp(&a.last_seen_at))
                    .then_with(|| a.id.cmp(&b.id))
            });
            Ok(Some(result))
        })
        .await?
    }
    pub async fn list_sessions(&self, token: &str, now: DateTime<Utc>) -> Option<Vec<SessionInfo>> {
        self.list_sessions_checked(token, now).await.ok().flatten()
    }
    pub async fn revoke(
        &self,
        token: &str,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let key = auth::token_hash(token);
        let id = id.to_owned();
        self.transact(move |state| {
            state.prune(now)?;
            if !auth::valid_token(&id) {
                return Err(StoreError::SessionNotFound);
            }
            let current = state
                .sessions
                .get(&key)
                .ok_or(StoreError::SessionNotFound)?;
            let target = state
                .sessions
                .iter()
                .find(|(_, s)| {
                    s.id == id && s.username == current.username && s.profile == current.profile
                })
                .map(|(k, _)| k.clone())
                .ok_or(StoreError::SessionNotFound)?;
            let mut next = clone_sessions(&state.sessions);
            next.remove(&target);
            state.save_sessions(&next)?;
            state.sessions[&target].cancel();
            state.sessions = next;
            Ok(target == key)
        })
        .await?
    }
    pub async fn logout(&self, token: &str) -> Result<(), StoreError> {
        let key = auth::token_hash(token);
        self.transact(move |state| {
            if state.storage_failed {
                return Err(StoreError::StorageUnavailable);
            }
            if !state.sessions.contains_key(&key) {
                return Ok(());
            }
            let mut next = clone_sessions(&state.sessions);
            next.remove(&key);
            state.save_sessions(&next)?;
            state.sessions[&key].cancel();
            state.sessions = next;
            Ok(())
        })
        .await?
    }
    pub async fn totp_enabled_checked(
        &self,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<bool>, StoreError> {
        let key = auth::token_hash(token);
        self.transact(move |state| {
            state.prune(now)?;
            let Some(session) = state.sessions.get(&key) else {
                return Ok(None);
            };
            let Some(account) = state.account(&session.username) else {
                return Ok(None);
            };
            Ok((session.profile == account.profile
                && session.fingerprint == account.credentials.fingerprint())
            .then_some(!account.credentials.totp_disabled))
        })
        .await?
    }
    pub async fn totp_enabled(&self, token: &str, now: DateTime<Utc>) -> Option<bool> {
        self.totp_enabled_checked(token, now).await.ok().flatten()
    }
    pub async fn set_totp_enabled(&self, request: SecurityRequest) -> SecurityStatus {
        if request.password.len() > 128 || request.code.len() > 16 {
            return SecurityStatus::Forbidden;
        }
        let key = auth::token_hash(&request.token);
        let now = request.now;
        let snapshot = match self
            .transact({
                let key = key.clone();
                move |state| {
                    if state.prune(now).is_err() {
                        return Err(SecurityStatus::StorageUnavailable);
                    }
                    let session = state
                        .sessions
                        .get(&key)
                        .ok_or(SecurityStatus::StaleSession)?;
                    let account = state
                        .account(&session.username)
                        .ok_or(SecurityStatus::StaleSession)?;
                    let fingerprint = account.credentials.fingerprint();
                    if session.profile != account.profile || session.fingerprint != fingerprint {
                        return Err(SecurityStatus::StaleSession);
                    }
                    let snapshot = (
                        session.username.clone(),
                        account.profile.clone(),
                        account.credentials.clone(),
                        fingerprint,
                    );
                    let account_key = format!("{}\0{}", snapshot.0, snapshot.1);
                    if !allow_attempt(&mut state.security_attempts, account_key, now, 32) {
                        return Err(SecurityStatus::RateLimited);
                    }
                    Ok(snapshot)
                }
            })
            .await
        {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(status)) => return status,
            Err(_) => return SecurityStatus::StorageUnavailable,
        };
        let (username, profile, credentials, fingerprint) = snapshot;
        let password = request.password;
        let salt = credentials.salt.clone();
        let hash = match self
            .kdf
            .try_spawn(move || auth::derive_password(&password, &salt))
        {
            Ok(job) => match job.await {
                Ok(hash) => hash,
                Err(_) => return SecurityStatus::Forbidden,
            },
            Err(_) => return SecurityStatus::RateLimited,
        };
        let step = credentials
            .match_code(&request.code, now.timestamp())
            .unwrap_or(-1);
        if !bool::from(hash.ct_eq(credentials.hash.as_slice())) || step < 0 {
            return SecurityStatus::Forbidden;
        }
        self.transact(move |state| {
            if state.prune(now.max(utc_now())).is_err() {
                return SecurityStatus::StorageUnavailable;
            }
            let Some(session) = state.sessions.get(&key) else {
                return SecurityStatus::StaleSession;
            };
            if session.username != username
                || session.profile != profile
                || session.fingerprint != fingerprint
            {
                return SecurityStatus::StaleSession;
            }
            let Some(account) = state.account(&username) else {
                return SecurityStatus::StaleSession;
            };
            if account.profile != profile || account.credentials.fingerprint() != fingerprint {
                return SecurityStatus::StaleSession;
            }
            if step <= account.credentials.last_step {
                return SecurityStatus::Forbidden;
            }
            if request.enabled != account.credentials.totp_disabled {
                return SecurityStatus::Ok;
            }
            let path = account.path.clone();
            let current = account.credentials.clone();
            if backup_credentials(&state.path, &path, &username, &current, now).is_err() {
                state.fail_storage();
                return SecurityStatus::StorageUnavailable;
            }
            let mut changed = current.clone();
            changed.totp_disabled = !request.enabled;
            changed.last_step = step;
            if write_credential(&path, &current, &changed).is_err() {
                state.fail_storage();
                return SecurityStatus::StorageUnavailable;
            }
            state
                .accounts
                .get_mut(&username)
                .expect("account exists")
                .credentials = changed.clone();
            let mut next = clone_sessions(&state.sessions);
            next.retain(|candidate, s| {
                candidate == &key || s.username != username || s.profile != profile
            });
            next.get_mut(&key).expect("current exists").fingerprint = changed.fingerprint();
            if state.save_sessions(&next).is_err() {
                return SecurityStatus::StorageUnavailable;
            }
            for (candidate, session) in &state.sessions {
                if !next.contains_key(candidate) {
                    session.cancel();
                }
            }
            state.sessions = next;
            SecurityStatus::Ok
        })
        .await
        .unwrap_or(SecurityStatus::StorageUnavailable)
    }
    /// Cancel live work independently of the normal I/O admission pool. Stored
    /// session rows remain valid for a fresh AuthStore after service restart.
    pub async fn close_connections(&self) {
        self.connections
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .cancel_all();
    }

    /// Stop admission and cancel active access immediately, then join admitted
    /// password and persistence work, including jobs whose HTTP waiter vanished.
    /// Transactions already executing may finish their atomic write. Do not put
    /// this drain behind the runtime's resolver shutdown timeout.
    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        self.close_connections().await;
        self.kdf.shutdown().await;
        let _drained = self.io.acquire_many(IO_WORKERS as u32).await;
        self.io.close();
    }
}

fn load_state(path: PathBuf, now: DateTime<Utc>) -> io::Result<State> {
    let mut accounts = HashMap::new();
    let primary = read_credential(&path)?;
    accounts.insert(
        primary.username.clone(),
        Account {
            credentials: primary,
            path: path.clone(),
            profile: String::new(),
        },
    );
    let users_path = suffix(&path, ".users");
    if users_path.exists() {
        let _private = open_private_account_dir(&users_path)?;
        let mut account_entries = 0;
        for entry in fs::read_dir(&users_path)? {
            let entry = entry?;
            account_entries += 1;
            if account_entries > MAX_ACCOUNTS - 1 {
                return Err(invalid_data("too many configured accounts"));
            }
            if !entry.file_type()?.is_file() || entry.path().extension() != Some(OsStr::new("json"))
            {
                return Err(invalid_data("invalid account file"));
            }
            let c = read_credential(&entry.path())?;
            let profile = auth::account_profile(&c.username);
            if accounts
                .insert(
                    c.username.clone(),
                    Account {
                        credentials: c,
                        path: entry.path(),
                        profile,
                    },
                )
                .is_some()
            {
                return Err(invalid_data("duplicate account"));
            }
        }
    }
    let sessions_path = suffix(&path, ".sessions");
    let mut state = State {
        path,
        sessions_path,
        accounts,
        sessions: HashMap::new(),
        attempts: HashMap::new(),
        security_attempts: HashMap::new(),
        storage_failed: false,
    };
    let raw = match read_private_path(&state.sessions_path, auth::SESSION_FILE_LIMIT) {
        Ok(raw) => Some(raw),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if let Some(raw) = raw {
        let file = auth::parse_session_file(&raw).map_err(invalid_data)?;
        let account_map = state
            .accounts
            .iter()
            .map(|(username, account)| {
                (
                    username.clone(),
                    (account.profile.clone(), account.credentials.clone()),
                )
            })
            .collect();
        let validated =
            auth::validate_session_file(&file, now, &account_map).map_err(invalid_data)?;
        for row in validated.retained {
            let key = row.token_hash.clone();
            state.sessions.insert(key, Session::from_row(row)?);
        }
        if validated.dirty {
            write_sessions(&state.sessions_path, &state.sessions).map_err(io::Error::other)?;
        }
    }
    Ok(state)
}
fn open_private_account_dir(path: &Path) -> io::Result<PrivateDir> {
    let dir = PrivateDir::open(path)?;
    if fs::symlink_metadata(path)?.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "account directory must be private",
        ));
    }
    Ok(dir)
}
fn parent_and_name(path: &Path) -> io::Result<(PrivateDir, &OsStr)> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_data("missing private parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid_data("missing file basename"))?;
    Ok((PrivateDir::open(parent)?, name))
}
fn read_private_path(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let (dir, name) = parent_and_name(path)?;
    dir.read_private(name, limit)
}
fn read_credential(path: &Path) -> io::Result<Credentials> {
    auth::parse_credentials(&read_private_path(path, CREDENTIAL_LIMIT)?).map_err(invalid_data)
}
fn write_credential(
    path: &Path,
    expected: &Credentials,
    credential: &Credentials,
) -> io::Result<()> {
    let (dir, name) = parent_and_name(path)?;
    let raw = dir.read_private(name, CREDENTIAL_LIMIT)?;
    let current = auth::parse_credentials(&raw).map_err(invalid_data)?;
    if current.fingerprint() != expected.fingerprint() || current.last_step != expected.last_step {
        return Err(invalid_data("credentials changed outside gateway"));
    }
    dir.write_atomic_private(name, credential.go_json().as_bytes())
        .map_err(io::Error::other)
}
fn write_sessions(path: &Path, sessions: &HashMap<String, Session>) -> io::Result<()> {
    match read_private_path(path, auth::SESSION_FILE_LIMIT) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut rows: Vec<_> = sessions.iter().map(|(key, s)| s.persisted(key)).collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    let file = PersistedSessionFile {
        version: auth::SESSION_FILE_VERSION,
        sessions: Some(rows),
    };
    let raw = file.go_json();
    if raw.len() > auth::SESSION_FILE_LIMIT {
        return Err(invalid_data("session file too large"));
    }
    let (dir, name) = parent_and_name(path)?;
    dir.write_atomic_private(name, raw.as_bytes())
        .map_err(io::Error::other)
}
fn backup_credentials(
    root: &Path,
    path: &Path,
    username: &str,
    expected: &Credentials,
    now: DateTime<Utc>,
) -> io::Result<()> {
    let disk = read_credential(path)?;
    if disk.fingerprint() != expected.fingerprint() || disk.last_step != expected.last_step {
        return Err(invalid_data("credentials changed outside gateway"));
    }
    let raw = read_private_path(path, CREDENTIAL_LIMIT)?;
    let backup_path = suffix(root, ".backups");
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(&backup_path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let dir = open_private_account_dir(&backup_path)?;
    let prefix = format!("{}-", auth::account_profile(username));
    let mut all = Vec::new();
    let mut own = Vec::new();
    for entry in fs::read_dir(&backup_path)? {
        let entry = entry?;
        if all.len() >= MAX_BACKUPS {
            return Err(invalid_data("too many credential backups"));
        }
        let name = entry.file_name();
        if !entry.file_type()?.is_file() {
            return Err(invalid_data("invalid backup entry"));
        }
        let Some(when) = backup_time(&name) else {
            return Err(invalid_data("invalid backup name"));
        };
        dir.read_private(&name, CREDENTIAL_LIMIT)?;
        let item = (when, name);
        if item.1.to_string_lossy().starts_with(&prefix) {
            own.push(item.clone());
        }
        all.push(item);
    }
    all.sort_by_key(|v| v.0);
    own.sort_by_key(|v| v.0);
    if own.len() >= MAX_BACKUPS_PER_ACCOUNT {
        fs::remove_file(backup_path.join(&own[0].1))?;
        all.retain(|v| v.1 != own[0].1);
    }
    if all.len() >= MAX_BACKUPS {
        fs::remove_file(backup_path.join(&all[0].1))?;
    }
    let name = format!("{}{}.json", prefix, now.format("%Y%m%dT%H%M%S%.9fZ"));
    if backup_path.join(&name).symlink_metadata().is_ok() {
        return Err(invalid_data("backup timestamp collision"));
    }
    dir.write_atomic_private(OsStr::new(&name), &raw)
        .map_err(io::Error::other)
}
fn backup_time(name: &OsStr) -> Option<DateTime<Utc>> {
    let name = name.to_str()?;
    let prefix = name.get(..64)?;
    if name.as_bytes().get(64) != Some(&b'-')
        || !name.ends_with(".json")
        || !prefix
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let timestamp = name.get(65..name.len().checked_sub(5)?)?;
    NaiveDateTime::parse_from_str(timestamp, "%Y%m%dT%H%M%S%.9fZ")
        .ok()
        .map(|v| v.and_utc())
}
fn allow_attempt(
    map: &mut HashMap<String, Vec<DateTime<Utc>>>,
    key: String,
    now: DateTime<Utc>,
    cap: usize,
) -> bool {
    map.retain(|_, values| {
        values
            .last()
            .is_some_and(|last| now.signed_duration_since(*last) < ChronoDuration::minutes(1))
    });
    if let Some(values) = map.get_mut(&key) {
        values.retain(|attempt| now.signed_duration_since(*attempt) < ChronoDuration::minutes(1));
        if values.len() >= 5 {
            return false;
        }
        values.push(now);
        return true;
    }
    if map.len() >= cap {
        return false;
    }
    map.insert(key, vec![now]);
    true
}
fn valid_text(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && !value.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}')
}
fn random_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
fn utc_now() -> DateTime<Utc> {
    std::time::SystemTime::now().into()
}
fn go_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}
fn suffix(path: &Path, value: &str) -> PathBuf {
    let mut result = path.as_os_str().to_os_string();
    result.push(value);
    PathBuf::from(result)
}
fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::{Hmac, Mac};
    use sha1::Sha1;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);
    const PASSWORD: &str = "correct horse battery staple";
    struct Fixture {
        root: PathBuf,
        path: PathBuf,
    }
    impl Fixture {
        fn new(extra_account: bool) -> Self {
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "hmux-auth-store-{}-{}",
                std::process::id(),
                FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder.create(&root).unwrap();
            let path = root.join("credentials.json");
            let owner = credential("owner", 1);
            fs::write(&path, owner.go_json()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            if extra_account {
                let users = suffix(&path, ".users");
                builder.create(&users).unwrap();
                let guest = users.join("guest.json");
                fs::write(&guest, credential("guest", 2).go_json()).unwrap();
                fs::set_permissions(guest, fs::Permissions::from_mode(0o600)).unwrap();
            }
            Self { root, path }
        }
        async fn store(&self) -> AuthStore {
            AuthStore::open(&self.path).await.unwrap()
        }
        fn code(&self, username: &str, now: DateTime<Utc>) -> String {
            let credential = if username == "owner" {
                read_credential(&self.path).unwrap()
            } else {
                read_credential(&suffix(&self.path, ".users").join("guest.json")).unwrap()
            };
            let secret = auth::decode_totp_secret(&credential.totp_secret).unwrap();
            let step = now.timestamp() / 30;
            let mut mac = Hmac::<Sha1>::new_from_slice(&secret).unwrap();
            mac.update(&(step as u64).to_be_bytes());
            let digest = mac.finalize().into_bytes();
            let offset = usize::from(digest[19] & 15);
            let number = (u32::from_be_bytes(digest[offset..offset + 4].try_into().unwrap())
                & 0x7fff_ffff)
                % 1_000_000;
            format!("{number:06}")
        }
        fn request(&self, username: &str, now: DateTime<Utc>) -> LoginRequest {
            LoginRequest {
                username: username.into(),
                password: PASSWORD.into(),
                code: self.code(username, now),
                source: "192.0.2.1".into(),
                ip: "192.0.2.1".into(),
                browser: "Test Browser".into(),
                now,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
    fn credential(username: &str, secret: u8) -> Credentials {
        Credentials {
            username: username.into(),
            salt: vec![secret; 32],
            hash: auth::derive_password(PASSWORD, &[secret; 32]).to_vec(),
            totp_secret: data_encoding::BASE32_NOPAD.encode(&[secret; 20]),
            last_step: 0,
            totp_disabled: false,
        }
    }
    #[tokio::test]
    async fn transaction_panic_and_poison_fail_closed() {
        let fixture = Fixture::new(false);
        let store = fixture.store().await;
        let now = utc_now();
        let token = store
            .login(fixture.request("owner", now))
            .await
            .token
            .unwrap();
        let access = store.access(&token, false, now).await.unwrap().unwrap();
        let result = store
            .transact::<()>(|_| panic!("synthetic transaction fault"))
            .await;
        assert_eq!(result, Err(StoreError::StorageUnavailable));
        assert!(*access.cancelled.borrow());
        assert!(matches!(
            store.access(&token, false, now).await,
            Err(StoreError::StorageUnavailable)
        ));

        let fixture = Fixture::new(false);
        let store = fixture.store().await;
        let token = store
            .login(fixture.request("owner", now))
            .await
            .token
            .unwrap();
        let access = store.access(&token, false, now).await.unwrap().unwrap();
        let state = store.state.clone();
        assert!(std::thread::spawn(move || {
            let _guard = state.lock().unwrap();
            panic!("synthetic lock poison");
        })
        .join()
        .is_err());
        assert!(matches!(
            store.access(&token, false, now).await,
            Err(StoreError::StorageUnavailable)
        ));
        assert!(*access.cancelled.borrow());
    }

    #[tokio::test]
    async fn shutdown_joins_detached_kdf_and_persistence_and_rejects_queued_writes() {
        let fixture = Fixture::new(false);
        let store = fixture.store().await;
        let now = utc_now();
        let token = store
            .login(fixture.request("owner", now))
            .await
            .token
            .unwrap();
        let mut access = store.access(&token, false, now).await.unwrap().unwrap();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let writing = tokio::spawn({
            let store = store.clone();
            async move {
                store
                    .transact(move |state| {
                        let _ = entered.send(());
                        let _ = gate.recv();
                        // A transaction already running may finish its atomic write.
                        let next = HashMap::new();
                        state.save_sessions(&next)?;
                        state.sessions = next;
                        Ok::<_, StoreError>(())
                    })
                    .await
            }
        });
        started.await.unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let queued = tokio::spawn({
            let store = store.clone();
            let ran = ran.clone();
            async move {
                store
                    .transact(move |_| ran.store(true, Ordering::Release))
                    .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while store.io.available_permits() != IO_WORKERS - 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let (kdf_entered, kdf_started) = tokio::sync::oneshot::channel();
        let (kdf_release, kdf_gate) = std::sync::mpsc::channel();
        let kdf = store
            .kdf
            .try_spawn(move || {
                let _ = kdf_entered.send(());
                let _ = kdf_gate.recv();
            })
            .unwrap();
        kdf_started.await.unwrap();
        drop(kdf);
        let shutdown = tokio::spawn({
            let store = store.clone();
            async move { store.shutdown().await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), access.wait_cancelled())
            .await
            .unwrap();
        assert!(!shutdown.is_finished());
        assert_eq!(
            store.transact(|_| ()).await,
            Err(StoreError::StorageUnavailable)
        );
        kdf_release.send(()).unwrap();
        assert!(!shutdown.is_finished());
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), shutdown)
            .await
            .unwrap()
            .unwrap();
        writing.await.unwrap().unwrap().unwrap();
        assert_eq!(queued.await.unwrap(), Err(StoreError::StorageUnavailable));
        assert!(!ran.load(Ordering::Acquire));
        assert!(store.kdf.try_spawn(|| ()).is_err());
        store.shutdown().await; // repeat drain is safe with closed semaphores
        let restarted = fixture.store().await;
        assert!(restarted
            .access(&token, false, now)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn checked_access_reports_busy_and_shutdown_bypasses_saturated_io() {
        let fixture = Fixture::new(false);
        let store = fixture.store().await;
        let now = utc_now();
        let token = store
            .login(fixture.request("owner", now))
            .await
            .token
            .unwrap();
        let access = store.access(&token, false, now).await.unwrap().unwrap();
        let permits: Vec<_> = (0..IO_WORKERS)
            .map(|_| store.io.clone().try_acquire_owned().unwrap())
            .collect();
        assert!(matches!(
            store.access(&token, false, now).await,
            Err(StoreError::Busy)
        ));
        assert!(matches!(
            store.identity_checked(&token, now).await,
            Err(StoreError::Busy)
        ));
        assert!(matches!(
            store.list_sessions_checked(&token, now).await,
            Err(StoreError::Busy)
        ));
        assert!(matches!(
            store.totp_enabled_checked(&token, now).await,
            Err(StoreError::Busy)
        ));
        assert_eq!(
            store
                .set_totp_enabled(SecurityRequest {
                    token: token.clone(),
                    password: PASSWORD.to_owned(),
                    code: fixture.code("owner", now),
                    enabled: false,
                    now,
                })
                .await,
            SecurityStatus::StorageUnavailable
        );
        // The real HTTP route must treat exhausted store admission as a service
        // error, not expire the cookie or blame the client's security attempts.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = tokio_util::sync::CancellationToken::new();
        let gateway = Arc::new(
            crate::http_auth::Gateway::new(
                "https://hmux.example",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                Arc::new(store.clone()),
            )
            .unwrap(),
        );
        let server = tokio::spawn(gateway.serve(listener, shutdown.clone()));
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!("POST /api/account/security HTTP/1.1\r\nHost: hmux.example\r\nOrigin: https://hmux.example\r\nCookie: __Host-hmux={token}\r\nX-CSRF-Token: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", access.csrf);
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            socket.read_to_end(&mut response),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 503"));
        shutdown.cancel();
        let mut access = access;
        tokio::time::timeout(std::time::Duration::from_secs(2), access.wait_cancelled())
            .await
            .unwrap();
        assert!(!server.is_finished());
        drop(permits);
        server.await.unwrap().unwrap();
        let restarted = fixture.store().await;
        assert!(restarted
            .access(&token, false, now)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn push_id_lookup_distinguishes_busy_from_gone_and_checks_account_state() {
        let fixture = Fixture::new(true);
        let store = fixture.store().await;
        let now = utc_now();
        let owner = store
            .login(fixture.request("owner", now))
            .await
            .token
            .unwrap();
        let access = store.access(&owner, false, now).await.unwrap().unwrap();
        assert_eq!(
            store
                .push_login_by_id(&access.id, now)
                .unwrap()
                .unwrap()
                .username,
            "owner"
        );
        assert!(store.push_login_by_id("missing", now).unwrap().is_none());
        let lock = store.state.lock().unwrap();
        assert!(matches!(
            store.push_login_by_id(&access.id, now),
            Err(StoreError::Busy)
        ));
        drop(lock);
        {
            let mut state = store.state.lock().unwrap();
            state
                .sessions
                .values_mut()
                .find(|session| session.id == access.id)
                .unwrap()
                .profile = "different".into();
        }
        assert!(store.push_login_by_id(&access.id, now).unwrap().is_none());
        {
            let mut state = store.state.lock().unwrap();
            state
                .sessions
                .values_mut()
                .find(|session| session.id == access.id)
                .unwrap()
                .profile
                .clear();
        }
        assert!(store.push_login_by_id(&access.id, now).unwrap().is_some());
        store.logout(&owner).await.unwrap();
        assert!(store.push_login_by_id(&access.id, now).unwrap().is_none());
    }

    #[test]
    fn credential_write_rejects_symlinks_hardlinks_and_disk_changes() {
        let fixture = Fixture::new(false);
        let expected = read_credential(&fixture.path).unwrap();
        let mut next = expected.clone();
        next.last_step = 42;
        let backing = fixture.root.join("backing.json");
        fs::rename(&fixture.path, &backing).unwrap();
        std::os::unix::fs::symlink(&backing, &fixture.path).unwrap();
        assert!(write_credential(&fixture.path, &expected, &next).is_err());
        assert_eq!(read_credential(&backing).unwrap().last_step, 0);
        fs::remove_file(&fixture.path).unwrap();
        fs::rename(&backing, &fixture.path).unwrap();

        let linked = fixture.root.join("linked.json");
        let unchanged = fs::read(&fixture.path).unwrap();
        fs::hard_link(&fixture.path, &linked).unwrap();
        assert!(write_credential(&fixture.path, &expected, &next).is_err());
        assert_eq!(fs::read(&linked).unwrap(), unchanged);
        fs::remove_file(&linked).unwrap();

        let mut disk = expected.clone();
        disk.last_step = 1;
        fs::write(&fixture.path, disk.go_json()).unwrap();
        assert!(write_credential(&fixture.path, &expected, &next).is_err());
        assert_eq!(read_credential(&fixture.path).unwrap().last_step, 1);
        disk.last_step = 0;
        disk.hash[0] ^= 0x80;
        fs::write(&fixture.path, disk.go_json()).unwrap();
        assert!(write_credential(&fixture.path, &expected, &next).is_err());
        assert_eq!(read_credential(&fixture.path).unwrap().hash, disk.hash);
    }

    #[test]
    fn malformed_unicode_backup_name_is_rejected_without_panic() {
        use std::os::unix::ffi::OsStrExt;
        let name = format!("{}é-20260101T000000.000000000Z.json", "a".repeat(63));
        assert!(backup_time(OsStr::new(&name)).is_none());
        assert!(backup_time(OsStr::from_bytes(b"bad\xff.json")).is_none());
    }

    #[test]
    fn source_and_security_attempt_tables_are_bounded() {
        let now = utc_now();
        let mut sources = HashMap::new();
        for index in 0..1024 {
            assert!(allow_attempt(
                &mut sources,
                format!("source-{index}"),
                now,
                1024
            ));
        }
        assert!(!allow_attempt(&mut sources, "extra".into(), now, 1024));
        for _ in 1..5 {
            assert!(allow_attempt(&mut sources, "source-0".into(), now, 1024));
        }
        assert!(!allow_attempt(&mut sources, "source-0".into(), now, 1024));
        assert!(allow_attempt(
            &mut sources,
            "extra".into(),
            now + ChronoDuration::minutes(1),
            1024
        ));
        assert_eq!(sources.len(), 1);
        let mut security = HashMap::new();
        for index in 0..32 {
            assert!(allow_attempt(
                &mut security,
                format!("account-{index}"),
                now,
                32
            ));
        }
        assert!(!allow_attempt(&mut security, "extra".into(), now, 32));
    }

    #[test]
    fn public_debug_redacts_bearer_and_session_json_uses_go_fields() {
        let login = LoginResult {
            status: LoginStatus::Succeeded,
            token: Some("synthetic-secret".into()),
        };
        assert!(!format!("{login:?}").contains("synthetic-secret"));
        let now = utc_now();
        let row = SessionInfo {
            location: String::new(),
            id: "public-id".into(),
            browser: "Test".into(),
            ip: "unknown".into(),
            created_at: now,
            last_seen_at: now,
            expires_at: now,
            current: true,
        };
        let value = serde_json::to_value(row).unwrap();
        assert_eq!(value["id"], "public-id");
        assert!(value.get("last_seen_at").is_some());
        assert_eq!(value["current"], true);
    }

    #[tokio::test]
    async fn additional_account_directory_must_be_private() {
        let fixture = Fixture::new(true);
        let users = suffix(&fixture.path, ".users");
        fs::set_permissions(&users, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(AuthStore::open(&fixture.path).await.is_err());
    }

    #[tokio::test]
    async fn restart_uses_only_token_hash_and_retains_login() {
        let fixture = Fixture::new(false);
        let store = fixture.store().await;
        let now = utc_now();
        let mut challenge = fixture.request("owner", now);
        challenge.code.clear();
        assert_eq!(
            store.login(challenge).await.status,
            LoginStatus::TotpRequired
        );
        assert!(!suffix(&fixture.path, ".sessions").exists());
        assert_eq!(read_credential(&fixture.path).unwrap().last_step, 0);
        let login = store.login(fixture.request("owner", now)).await;
        assert_eq!(login.status, LoginStatus::Succeeded);
        let token = login.token.unwrap();
        let disk = fs::read(suffix(&fixture.path, ".sessions")).unwrap();
        assert!(!disk
            .windows(token.len())
            .any(|bytes| bytes == token.as_bytes()));
        assert!(String::from_utf8_lossy(&disk).contains(&auth::token_hash(&token)));
        let access = store.get(&token, false, now).await.unwrap();
        assert!(!access.id.is_empty());
        assert_eq!(access.expires_at, now + ChronoDuration::days(7));
        let restarted = fixture.store().await;
        assert_eq!(
            restarted.get(&token, false, now).await.unwrap().csrf,
            access.csrf
        );
    }
    #[tokio::test]
    async fn concurrent_same_totp_step_mints_only_one_login() {
        let fixture = Fixture::new(false);
        let store = fixture.store().await;
        let now = utc_now();
        let (first, second) = tokio::join!(
            store.login(fixture.request("owner", now)),
            store.login(fixture.request("owner", now))
        );
        let statuses = [first.status, second.status];
        assert_eq!(
            statuses
                .iter()
                .filter(|&&s| s == LoginStatus::Succeeded)
                .count(),
            1
        );
        assert_eq!(
            statuses
                .iter()
                .filter(|&&s| s == LoginStatus::Invalid)
                .count(),
            1
        );
        let restarted = fixture.store().await;
        assert_eq!(
            restarted.login(fixture.request("owner", now)).await.status,
            LoginStatus::Invalid
        );
    }
    #[tokio::test]
    async fn changed_credentials_during_kdf_cannot_mint_login() {
        let fixture = Fixture::new(false);
        let store = fixture.store().await;
        let now = utc_now();
        let request = fixture.request("owner", now);
        let running = {
            let store = store.clone();
            tokio::spawn(async move { store.login(request).await })
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while store.kdf.available() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        {
            let mut state = store.state.lock().unwrap();
            state.accounts.get_mut("owner").unwrap().credentials.hash[0] ^= 0x80;
        }
        assert_eq!(running.await.unwrap().status, LoginStatus::Invalid);
    }
    #[tokio::test]
    async fn account_scoped_revoke_and_expiry_cancel_access() {
        let fixture = Fixture::new(true);
        let store = fixture.store().await;
        let now = utc_now();
        let owner = store
            .login(fixture.request("owner", now))
            .await
            .token
            .unwrap();
        let guest = store
            .login(fixture.request("guest", now))
            .await
            .token
            .unwrap();
        let guest_access = store.get(&guest, false, now).await.unwrap();
        assert_eq!(
            store.revoke(&owner, &guest_access.id, now).await,
            Err(StoreError::SessionNotFound)
        );
        assert_eq!(store.list_sessions(&owner, now).await.unwrap().len(), 1);
        assert!(store.revoke(&guest, &guest_access.id, now).await.unwrap());
        assert!(*guest_access.cancelled.borrow());
        assert!(store.get(&owner, false, now).await.is_some());
        assert!(store.get(&guest, false, now).await.is_none());
        let at_expiry = now + ChronoDuration::days(7);
        assert!(store.get(&owner, false, at_expiry).await.is_none());
        assert!(store.list_sessions(&owner, at_expiry).await.is_none());
    }
    #[tokio::test]
    async fn ninth_login_evicts_oldest_seen_and_persists_eight() {
        let fixture = Fixture::new(false);
        let store = fixture.store().await;
        let now = utc_now();
        let mut oldest_token = String::new();
        let mut oldest_cancel = None;
        {
            let mut state = store.state.lock().unwrap();
            let fingerprint = state.accounts["owner"].credentials.fingerprint();
            for index in 0..auth::MAX_SESSIONS_PER_ACCOUNT {
                let token = random_token().unwrap();
                let cancel = SessionCancel::new();
                let receiver = cancel.sender.subscribe();
                if index == 0 {
                    oldest_token = token.clone();
                    oldest_cancel = Some(receiver);
                }
                let seen = now - ChronoDuration::minutes((8 - index) as i64);
                state.sessions.insert(
                    auth::token_hash(&token),
                    Session {
                        id: random_token().unwrap(),
                        username: "owner".into(),
                        profile: String::new(),
                        fingerprint: fingerprint.clone(),
                        browser: "Test Browser".into(),
                        ip: "unknown".into(),
                        created: now - ChronoDuration::hours(1),
                        seen,
                        persisted_seen: seen,
                        expires: now - ChronoDuration::hours(1) + ChronoDuration::days(7),
                        cancel,
                    },
                );
            }
            write_sessions(&state.sessions_path, &state.sessions).unwrap();
        }
        let new_token = store
            .login(fixture.request("owner", now))
            .await
            .token
            .unwrap();
        assert!(*oldest_cancel.unwrap().borrow());
        assert!(store.get(&oldest_token, false, now).await.is_none());
        assert_eq!(store.list_sessions(&new_token, now).await.unwrap().len(), 8);
        let restarted = fixture.store().await;
        assert_eq!(
            restarted
                .list_sessions(&new_token, now)
                .await
                .unwrap()
                .len(),
            8
        );
    }

    #[tokio::test]
    async fn activity_write_is_throttled_without_sliding_expiry() {
        let fixture = Fixture::new(false);
        let store = fixture.store().await;
        let created = utc_now();
        let token = store
            .login(fixture.request("owner", created))
            .await
            .token
            .unwrap();
        let session_path = suffix(&fixture.path, ".sessions");
        let before = fs::read(&session_path).unwrap();
        let early = created + ChronoDuration::minutes(4);
        assert!(store.get(&token, true, early).await.is_some());
        assert_eq!(fs::read(&session_path).unwrap(), before);
        let due = created + ChronoDuration::minutes(5);
        let access = store.get(&token, true, due).await.unwrap();
        assert_ne!(fs::read(&session_path).unwrap(), before);
        assert_eq!(access.expires_at, created + ChronoDuration::days(7));
        assert!(store.get(&token, false, access.expires_at).await.is_none());
    }

    #[tokio::test]
    async fn failed_save_cancels_every_session_and_stays_closed() {
        let fixture = Fixture::new(false);
        let store = fixture.store().await;
        let now = utc_now();
        let token = store
            .login(fixture.request("owner", now))
            .await
            .token
            .unwrap();
        let access = store.get(&token, false, now).await.unwrap();
        let session_path = suffix(&fixture.path, ".sessions");
        fs::set_permissions(&session_path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            store.logout(&token).await,
            Err(StoreError::StorageUnavailable)
        );
        assert!(*access.cancelled.borrow());
        assert!(store.get(&token, false, now).await.is_none());
        fs::set_permissions(&session_path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            store
                .login(fixture.request("owner", now + ChronoDuration::seconds(31)))
                .await
                .status,
            LoginStatus::StorageUnavailable
        );
    }
    #[tokio::test]
    async fn totp_toggle_backs_up_credentials_and_revokes_only_same_account() {
        let fixture = Fixture::new(true);
        let store = fixture.store().await;
        let start = utc_now();
        let current = store
            .login(fixture.request("owner", start))
            .await
            .token
            .unwrap();
        let later = start + ChronoDuration::seconds(31);
        let other = store
            .login(fixture.request("owner", later))
            .await
            .token
            .unwrap();
        let guest = store
            .login(fixture.request("guest", start))
            .await
            .token
            .unwrap();
        let other_access = store.get(&other, false, later).await.unwrap();
        let toggle_at = start + ChronoDuration::seconds(62);
        let status = store
            .set_totp_enabled(SecurityRequest {
                token: current.clone(),
                password: PASSWORD.into(),
                code: fixture.code("owner", toggle_at),
                enabled: false,
                now: toggle_at,
            })
            .await;
        assert_eq!(status, SecurityStatus::Ok);
        assert!(*other_access.cancelled.borrow());
        assert_eq!(store.totp_enabled(&current, toggle_at).await, Some(false));
        assert!(store.get(&guest, false, toggle_at).await.is_some());
        assert!(store.get(&other, false, toggle_at).await.is_none());
        let backup_path = suffix(&fixture.path, ".backups");
        let entries = fs::read_dir(&backup_path)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        let backup = fs::read(entries[0].path()).unwrap();
        assert!(!auth::parse_credentials(&backup).unwrap().totp_disabled);
        assert!(read_credential(&fixture.path).unwrap().totp_disabled);
        let restarted = fixture.store().await;
        assert_eq!(
            restarted.totp_enabled(&current, toggle_at).await,
            Some(false)
        );
        let enable_at = start + ChronoDuration::seconds(93);
        assert_eq!(
            restarted
                .set_totp_enabled(SecurityRequest {
                    token: current.clone(),
                    password: PASSWORD.into(),
                    code: fixture.code("owner", enable_at),
                    enabled: true,
                    now: enable_at
                })
                .await,
            SecurityStatus::Ok
        );
        assert_eq!(
            restarted.totp_enabled(&current, enable_at).await,
            Some(true)
        );
        assert_eq!(fs::read_dir(backup_path).unwrap().count(), 2);
    }
}
