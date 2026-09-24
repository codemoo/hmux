//! Go-compatible Home recovery checkpoint and restored identity lineage.
//! Transactions hold recovery/state.lock through capture, restore and commit.
use crate::{
    catalog::{TmuxCatalogReader, TmuxSocket},
    sessionstate,
};
use hmux_core::{
    command::{CommandRunner, CommandSpec},
    PrivateDir,
};
use hmux_model::{
    safe_text, validate_session_id,
    workspace::{self, SessionLineage},
    Catalog, Session, SessionIdentity,
};
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

mod restore;
const MAX_STATE: usize = 16 * 1024 * 1024;
const SEP: &str = "|:hmux-recovery-v1:|";
const PANE_FORMAT: &str = "#{session_id}|:hmux-recovery-v1:|#{window_id}|:hmux-recovery-v1:|#{window_index}|:hmux-recovery-v1:|#{window_name}|:hmux-recovery-v1:|#{window_layout}|:hmux-recovery-v1:|#{window_active}|:hmux-recovery-v1:|#{pane_id}|:hmux-recovery-v1:|#{pane_index}|:hmux-recovery-v1:|#{pane_active}|:hmux-recovery-v1:|#{pane_current_path}|:hmux-recovery-v1:|#{pane_pid}";
const LOCK: Duration = Duration::from_secs(2);
const OP: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Unavailable,
    Busy,
    Changed,
    Cancelled,
    Command,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "recovery {self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeReference {
    pub provider: String,
    pub session_id: String,
    pub config_dir: String,
}
impl ResumeReference {
    fn valid(&self) -> bool {
        matches!(self.provider.as_str(), "codex" | "claude")
            && (1..=128).contains(&self.session_id.len())
            && self
                .session_id
                .bytes()
                .enumerate()
                .all(|(i, b)| b.is_ascii_alphanumeric() || (i > 0 && b == b'-'))
            && clean_absolute(&self.config_dir)
    }
}
fn null_vec<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Vec<T>, D::Error> {
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}
fn null_map<'de, D: Deserializer<'de>, V: DeserializeOwned>(
    d: D,
) -> Result<BTreeMap<String, V>, D::Error> {
    Ok(Option::<BTreeMap<String, V>>::deserialize(d)?.unwrap_or_default())
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    #[serde(default, deserialize_with = "null_vec")]
    sessions: Vec<SavedSession>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedSession {
    identity: SessionIdentity,
    name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    alias: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    hidden: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    profile: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde(deserialize_with = "null_vec")]
    tags: Vec<String>,
    windows: Vec<SavedWindow>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedWindow {
    index: u32,
    name: String,
    layout: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    active: bool,
    panes: Vec<SavedPane>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedPane {
    index: u32,
    cwd: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resume: Option<ResumeReference>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mapping {
    from: SessionIdentity,
    to: SessionIdentity,
    name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[serde(deserialize_with = "null_map")]
    panes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    gate: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    boot_id: String,
    snapshot: Snapshot,
    #[serde(default, deserialize_with = "null_map")]
    completed: BTreeMap<String, Mapping>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[serde(deserialize_with = "null_map")]
    intents: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskState {
    version: i32,
    boot_id: String,
    checkpoint: Snapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<Pending>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde(deserialize_with = "null_vec")]
    mappings: Vec<Mapping>,
    updated_at: String,
}
fn key(id: &SessionIdentity) -> String {
    format!("{}/{}", id.id, id.created_at)
}
fn valid_id(id: &SessionIdentity) -> bool {
    validate_session_id(&id.id).is_ok() && id.created_at > 0
}
fn valid_text(s: &str, max: usize, nonempty: bool) -> bool {
    (!nonempty || !s.is_empty()) && s.len() <= max && !s.chars().any(char::is_control)
}
fn clean_absolute(s: &str) -> bool {
    s.len() <= 4096
        && valid_text(s, 4096, true)
        && Path::new(s).is_absolute()
        && Path::new(s).components().collect::<PathBuf>() == Path::new(s)
        && !s.contains("/../")
        && !s.ends_with("/..")
        && !s.contains("/./")
}
fn valid_name(s: &str) -> bool {
    valid_text(s, 512, true) && !s.contains([':', '.'])
}
fn valid_tmux(s: &str, prefix: u8) -> bool {
    (2..=14).contains(&s.len())
        && s.as_bytes()[0] == prefix
        && s.as_bytes()[1..].iter().all(u8::is_ascii_digit)
}
fn valid_layout(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 6
        || bytes.len() > 65536
        || bytes[4] != b','
        || !bytes[..4]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    {
        return false;
    }
    fn number(bytes: &[u8], at: &mut usize) -> bool {
        let begin = *at;
        while *at < bytes.len() && bytes[*at].is_ascii_digit() {
            *at += 1;
        }
        *at > begin && *at - begin <= 10
    }
    fn cell(bytes: &[u8], at: &mut usize, depth: usize) -> bool {
        if depth > 128 {
            return false;
        }
        for delimiter in [b'x', b',', b','] {
            if !number(bytes, at) || bytes.get(*at) != Some(&delimiter) {
                return false;
            }
            *at += 1;
        }
        if !number(bytes, at) {
            return false;
        }
        match bytes.get(*at) {
            Some(b',') => {
                *at += 1;
                number(bytes, at)
            }
            Some(b'[' | b'{') => {
                let close = if bytes[*at] == b'[' { b']' } else { b'}' };
                *at += 1;
                if !cell(bytes, at, depth + 1) {
                    return false;
                }
                let mut count = 1;
                while bytes.get(*at) == Some(&b',') {
                    *at += 1;
                    count += 1;
                    if !cell(bytes, at, depth + 1) {
                        return false;
                    }
                }
                if count < 2 || bytes.get(*at) != Some(&close) {
                    return false;
                }
                *at += 1;
                true
            }
            _ => false,
        }
    }
    let mut at = 5;
    cell(bytes, &mut at, 0) && at == bytes.len()
}
fn validate_snapshot(s: &Snapshot) -> Result<(), Error> {
    if s.sessions.len() > 512 {
        return Err(Error::Invalid);
    }
    let (mut names, mut ids) = (BTreeSet::new(), BTreeSet::new());
    let (mut windows, mut panes) = (0, 0);
    for x in &s.sessions {
        if !valid_id(&x.identity)
            || !valid_name(&x.name)
            || !names.insert(&x.name)
            || !ids.insert(&x.identity.id)
            || x.windows.is_empty()
            || x.windows.len() > 512
            || safe_text(&x.alias, 256) != x.alias
            || safe_text(&x.profile, 128) != x.profile
            || safe_text(&x.label, 256) != x.label
            || x.tags.len() > 128
            || x.tags.iter().any(|t| safe_text(t, 128) != *t)
        {
            return Err(Error::Invalid);
        }
        let mut wi = BTreeSet::new();
        let mut active_w = 0;
        for w in &x.windows {
            windows += 1;
            if w.index > 1_000_000
                || !wi.insert(w.index)
                || !valid_text(&w.name, 256, true)
                || !valid_layout(&w.layout)
                || w.panes.is_empty()
                || w.panes.len() > 512
            {
                return Err(Error::Invalid);
            }
            active_w += usize::from(w.active);
            let mut pi = BTreeSet::new();
            let mut active_p = 0;
            for p in &w.panes {
                panes += 1;
                if p.index > 1_000_000
                    || !pi.insert(p.index)
                    || !clean_absolute(&p.cwd)
                    || p.resume.as_ref().is_some_and(|r| !r.valid())
                {
                    return Err(Error::Invalid);
                }
                active_p += usize::from(p.active);
            }
            if active_p != 1 {
                return Err(Error::Invalid);
            }
        }
        if active_w != 1 {
            return Err(Error::Invalid);
        }
    }
    if windows > 4096 || panes > 8192 {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn validate_mapping(m: &Mapping) -> Result<(), Error> {
    if !valid_id(&m.from)
        || !valid_id(&m.to)
        || !valid_name(&m.name)
        || m.panes.len() > 8192
        || m.panes.iter().any(|(k, v)| {
            let parts: Vec<_> = k.split('/').collect();
            parts.len() != 2
                || parts.iter().any(|p| p.parse::<u32>().is_err())
                || !valid_tmux(v, b'%')
        })
        || (!m.gate.is_empty()
            && (!clean_absolute(&m.gate)
                || Path::new(&m.gate).file_name() != Some(OsStr::new("ready"))
                || !Path::new(&m.gate)
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|n| n.to_string_lossy().starts_with("launch-"))))
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn validate_state(s: &DiskState) -> Result<(), Error> {
    if s.version != 1
        || !valid_text(&s.boot_id, 256, true)
        || chrono::DateTime::parse_from_rfc3339(&s.updated_at).is_err()
    {
        return Err(Error::Invalid);
    }
    validate_snapshot(&s.checkpoint)?;
    if let Some(p) = &s.pending {
        if !valid_text(&p.boot_id, 256, true) || p.completed.len() > 512 || p.intents.len() > 512 {
            return Err(Error::Invalid);
        }
        validate_snapshot(&p.snapshot)?;
        for (k, m) in &p.completed {
            validate_mapping(m)?;
            if *k != key(&m.from) {
                return Err(Error::Invalid);
            }
        }
        for (k, path) in &p.intents {
            if !p.snapshot.sessions.iter().any(|s| key(&s.identity) == *k)
                || !clean_absolute(path)
                || Path::new(path).file_name() != Some(OsStr::new("ready"))
            {
                return Err(Error::Invalid);
            }
        }
    }
    if s.mappings.len() > 512 {
        return Err(Error::Invalid);
    }
    let (mut from, mut to) = (BTreeSet::new(), BTreeSet::new());
    for m in &s.mappings {
        validate_mapping(m)?;
        if !from.insert(key(&m.from)) || !to.insert(key(&m.to)) {
            return Err(Error::Invalid);
        }
    }
    Ok(())
}

pub type ResolveFuture =
    Pin<Box<dyn Future<Output = Result<BTreeMap<i32, ResumeReference>, Error>> + Send>>;
pub type Resolver = Arc<dyn Fn(Vec<i32>) -> ResolveFuture + Send + Sync>;
/// One checkpoint loop per connected Home. Shutdown waits for an admitted save.
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub struct Checkpoint {
    stop: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for Checkpoint {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl Checkpoint {
    pub async fn shutdown(mut self) {
        self.stop.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
#[derive(Clone)]
pub struct Store {
    state_dir: PathBuf,
    reader: TmuxCatalogReader,
    runner: CommandRunner,
    tmux: PathBuf,
    socket: Option<TmuxSocket>,
    resolver: Resolver,
    boot: Option<Arc<dyn Fn() -> Result<String, Error> + Send + Sync>>,
    admission: Arc<Semaphore>,
    operation_cancel: Option<CancellationToken>,
    operation_deadline: Option<Instant>,
}
impl Store {
    pub fn new(
        state_dir: PathBuf,
        tmux: PathBuf,
        socket: Option<TmuxSocket>,
        runner: CommandRunner,
        resolver: Resolver,
    ) -> Result<Self, Error> {
        let reader = TmuxCatalogReader::new(tmux.clone(), socket.clone(), Duration::from_secs(15))
            .map_err(|_| Error::Invalid)?;
        if !state_dir.is_absolute() {
            return Err(Error::Invalid);
        }
        Ok(Self {
            state_dir,
            reader,
            runner,
            tmux,
            socket,
            resolver,
            boot: None,
            admission: Arc::new(Semaphore::new(1)),
            operation_cancel: None,
            operation_deadline: None,
        })
    }
    /// Synthetic reboot fixtures can supply a deterministic boot identity.
    pub fn with_boot_id(
        mut self,
        boot: Arc<dyn Fn() -> Result<String, Error> + Send + Sync>,
    ) -> Self {
        self.boot = Some(boot);
        self
    }
    fn check_cancel(&self) -> Result<(), Error> {
        if self
            .operation_cancel
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
            || self.operation_deadline.is_some_and(|d| Instant::now() >= d)
        {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
    fn dir(&self) -> Result<PrivateDir, Error> {
        PrivateDir::open_or_create_trusted(&self.state_dir)
            .and_then(|d| d.create_private_child(OsStr::new("recovery")))
            .map_err(|_| Error::Unavailable)
    }
    fn read(dir: &PrivateDir) -> Result<Option<DiskState>, Error> {
        let raw = match dir.read_private(OsStr::new("state.json"), MAX_STATE) {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Error::Unavailable),
        };
        let mut de = serde_json::Deserializer::from_slice(&raw);
        let state = DiskState::deserialize(&mut de).map_err(|_| Error::Invalid)?;
        de.end().map_err(|_| Error::Invalid)?;
        validate_state(&state)?;
        Ok(Some(state))
    }
    fn write(dir: &PrivateDir, s: &mut DiskState) -> Result<(), Error> {
        s.version = 1;
        s.updated_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
        validate_state(s)?;
        let mut raw = serde_json::to_vec(s).map_err(|_| Error::Invalid)?;
        raw.push(b'\n');
        if raw.len() > MAX_STATE {
            return Err(Error::Invalid);
        }
        dir.write_atomic_private(OsStr::new("state.json"), &raw)
            .map_err(|_| Error::Unavailable)
    }
    async fn system_boot_id(&self) -> Result<String, Error> {
        #[cfg(target_os = "linux")]
        {
            use std::io::Read;
            let mut raw = String::new();
            std::fs::File::open("/proc/sys/kernel/random/boot_id")
                .map_err(|_| Error::Unavailable)?
                .take(129)
                .read_to_string(&mut raw)
                .map_err(|_| Error::Unavailable)?;
            if raw.len() > 128 {
                return Err(Error::Invalid);
            }
            Ok(format!("linux-{}", raw.trim()))
        }
        #[cfg(target_os = "macos")]
        {
            let spec = CommandSpec::new("/usr/sbin/sysctl", 1024, Duration::from_secs(5))
                .args(["-n", "kern.boottime"]);
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let work = self.runner.run_cancelable(spec, receiver);
            tokio::pin!(work);
            let raw = if let Some(stop) = &self.operation_cancel {
                tokio::select! { _=stop.cancelled()=>{drop(sender);let _=work.await;return Err(Error::Cancelled)}, result=&mut work=>result.map_err(|_|Error::Unavailable)?.stdout }
            } else {
                work.await.map_err(|_| Error::Unavailable)?.stdout
            };
            darwin_boot_id(&raw)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(Error::Unavailable)
        }
    }
    async fn boot_id(&self) -> Result<String, Error> {
        let v = if let Some(f) = &self.boot {
            f()?
        } else {
            self.system_boot_id().await?
        };
        if valid_text(&v, 256, true) {
            Ok(v)
        } else {
            Err(Error::Invalid)
        }
    }
    /// Read the last committed mapping without taking the writer lock.
    pub fn apply(&self, catalog: &mut Catalog) -> Result<(), Error> {
        let dir = match PrivateDir::open(&self.state_dir.join("recovery")) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(Error::Unavailable),
        };
        let state = match Self::read(&dir) {
            Ok(x) => x,
            Err(_) => Self::read(&dir)?,
        };
        let Some(s) = state else { return Ok(()) };
        let map: BTreeMap<_, _> = s.mappings.iter().map(|m| (key(&m.to), m)).collect();
        for session in catalog.sessions.iter_mut().flatten() {
            if let Some(m) = map
                .get(&format!("{}/{}", session.id, session.created_at))
                .filter(|m| m.name == session.name)
            {
                session.restored_from = Some(m.from.clone());
            }
        }
        Ok(())
    }
    /// Boot synchronization completes before catalog service starts. The returned
    /// owner checkpoints every 30 seconds and must be shut down with the connector.
    pub async fn prepare_checkpoint(&self, parent: CancellationToken) -> Result<Checkpoint, Error> {
        self.transaction(Operation::Sync, Some(parent.clone()))
            .await?;
        if parent.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let store = self.clone();
        let stop = parent.child_token();
        let cancelled = stop.clone();
        let task = tokio::spawn(async move {
            let mut wait = Duration::from_secs(30);
            loop {
                tokio::select! {
                    _ = cancelled.cancelled() => break,
                    _ = tokio::time::sleep(wait) => {}
                }
                let result = store
                    .transaction(Operation::Save, Some(cancelled.clone()))
                    .await;
                wait = if result.is_err() {
                    Duration::from_secs(5)
                } else {
                    Duration::from_secs(30)
                };
            }
        });
        Ok(Checkpoint {
            stop,
            task: Some(task),
        })
    }
    pub async fn sync(&self) -> Result<(), Error> {
        self.transaction(Operation::Sync, None).await
    }
    pub async fn sync_cancelable(&self, stop: CancellationToken) -> Result<(), Error> {
        self.transaction(Operation::Sync, Some(stop)).await
    }
    pub async fn save(&self) -> Result<(), Error> {
        self.transaction(Operation::Save, None).await
    }
    pub async fn save_cancelable(&self, stop: CancellationToken) -> Result<(), Error> {
        self.transaction(Operation::Save, Some(stop)).await
    }
    pub async fn restore(&self) -> Result<(), Error> {
        self.transaction(Operation::Restore, None).await
    }
    pub async fn restore_cancelable(&self, stop: CancellationToken) -> Result<(), Error> {
        self.transaction(Operation::Restore, Some(stop)).await
    }
    async fn transaction(
        &self,
        op: Operation,
        parent: Option<CancellationToken>,
    ) -> Result<(), Error> {
        let permit = self
            .admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let cancel = parent.map(|p| p.child_token()).unwrap_or_default();
        let _caller = CancelOnDrop(cancel.clone());
        let mut s = self.clone();
        s.operation_cancel = Some(cancel);
        s.operation_deadline = Some(
            Instant::now()
                + if matches!(op, Operation::Save) {
                    Duration::from_secs(5)
                } else {
                    Duration::from_secs(30)
                },
        );
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            s.check_cancel()?;
            let dir = s.dir()?;
            let _lock = dir
                .lock_for(OsStr::new("state.lock"), LOCK)
                .map_err(|_| Error::Busy)?;
            s.check_cancel()?;
            handle.block_on(async {
                let boot = s.boot_id().await?;
                let mut state = Self::read(&dir)?;
                s.check_cancel()?;
                match op {
                    Operation::Sync | Operation::Save if state.is_none() => {
                        let shot = s.capture().await?;
                        s.check_cancel()?;
                        let mut initial = DiskState {
                            version: 1,
                            boot_id: boot,
                            checkpoint: shot,
                            pending: None,
                            mappings: vec![],
                            updated_at: String::new(),
                        };
                        Self::write(&dir, &mut initial)
                    }
                    _ => {
                        let mut current = state.take().ok_or(Error::Unavailable)?;
                        if matches!(op, Operation::Save)
                            && (current.boot_id != boot || current.pending.is_some())
                        {
                            return Err(Error::Changed);
                        }
                        if matches!(op, Operation::Restore)
                            || current.boot_id != boot
                            || current.pending.is_some()
                        {
                            s.restore_locked(&dir, &boot, &mut current).await
                        } else {
                            current.checkpoint = s.capture().await?;
                            current.pending = None;
                            s.check_cancel()?;
                            Self::write(&dir, &mut current)
                        }
                    }
                }
            })
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    async fn command(&self, args: Vec<String>, limit: usize) -> Result<Vec<u8>, Error> {
        self.check_cancel()?;
        let timeout = self
            .operation_deadline
            .map_or(Duration::from_secs(15), |d| {
                Duration::from_secs(15).min(d.saturating_duration_since(Instant::now()))
            });
        if timeout.is_zero() {
            return Err(Error::Cancelled);
        }
        let mut spec = CommandSpec::new(self.tmux.clone(), limit, timeout);
        if let Some(socket) = &self.socket {
            spec = match socket {
                TmuxSocket::Name(n) => spec.args(["-L".to_owned(), n.clone()]),
                TmuxSocket::Path(p) => {
                    spec.args(["-S".to_owned(), p.to_string_lossy().into_owned()])
                }
            };
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let work = self.runner.run_cancelable(spec.args(args), receiver);
        tokio::pin!(work);
        if let Some(stop) = &self.operation_cancel {
            tokio::select! {
                _ = stop.cancelled() => { drop(sender); let _ = work.await; Err(Error::Cancelled) },
                result = &mut work => result.map(|o|o.stdout).map_err(|_|Error::Command),
            }
        } else {
            work.await.map(|o| o.stdout).map_err(|_| Error::Command)
        }
    }
    async fn basic(&self) -> Result<Catalog, Error> {
        self.check_cancel()?;
        if let Some(stop) = &self.operation_cancel {
            self.reader
                .read_basic_cancelable(
                    &self.runner,
                    stop,
                    tokio::time::Instant::now()
                        + self
                            .operation_deadline
                            .map_or(Duration::from_secs(15), |d| {
                                Duration::from_secs(15)
                                    .min(d.saturating_duration_since(Instant::now()))
                            }),
                )
                .await
                .map_err(|_| {
                    if stop.is_cancelled() {
                        Error::Cancelled
                    } else {
                        Error::Command
                    }
                })
        } else {
            self.reader
                .read_basic(&self.runner)
                .await
                .map_err(|_| Error::Command)
        }
    }
    async fn capture(&self) -> Result<Snapshot, Error> {
        let mut catalog = self.basic().await?;
        let metadata = sessionstate::Store::new(self.state_dir.clone());
        metadata
            .apply(&mut catalog)
            .map_err(|_| Error::Unavailable)?;
        metadata
            .apply_visibility(&mut catalog)
            .map_err(|_| Error::Unavailable)?;
        let sessions = catalog.sessions.unwrap_or_default();
        if sessions.len() > 512 {
            return Err(Error::Invalid);
        }
        if sessions.is_empty() {
            return Ok(Snapshot::default());
        }
        let raw = self
            .command(
                vec![
                    "list-panes".into(),
                    "-a".into(),
                    "-F".into(),
                    PANE_FORMAT.into(),
                ],
                MAX_STATE * 2,
            )
            .await?;
        let rows = std::str::from_utf8(&raw)
            .map_err(|_| Error::Invalid)?
            .trim_end_matches('\n');
        if rows.lines().count() > 8192 {
            return Err(Error::Invalid);
        }
        let mut by_id: BTreeMap<String, SavedSession> = sessions
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    SavedSession {
                        identity: SessionIdentity {
                            id: s.id.clone(),
                            created_at: s.created_at,
                        },
                        name: s.name.clone(),
                        alias: s.alias.clone(),
                        hidden: s.hidden,
                        profile: s.profile.clone(),
                        label: s.label.clone(),
                        tags: s.tags.clone().unwrap_or_default(),
                        windows: vec![],
                    },
                )
            })
            .collect();
        let mut pids = Vec::new();
        let mut pid_position = BTreeMap::new();
        let (mut pane_ids, mut window_ids) = (
            BTreeSet::new(),
            BTreeMap::<(String, String), (u32, String, String, bool)>::new(),
        );
        for row in rows.lines() {
            let f: Vec<_> = row.trim_end_matches('\r').split(SEP).collect();
            if f.len() != 11 {
                return Err(Error::Invalid);
            }
            let Some(session) = by_id.get_mut(f[0]) else {
                continue;
            };
            if !valid_tmux(f[1], b'@') || !valid_tmux(f[6], b'%') || !pane_ids.insert(f[6]) {
                return Err(Error::Invalid);
            }
            let wi = f[2].parse::<u32>().map_err(|_| Error::Invalid)?;
            let pi = f[7].parse::<u32>().map_err(|_| Error::Invalid)?;
            let wa = flag(f[5])?;
            let pa = flag(f[8])?;
            let pid = f[10].parse::<i32>().map_err(|_| Error::Invalid)?;
            if !(1..=1 << 30).contains(&pid) || pid_position.contains_key(&pid) {
                return Err(Error::Invalid);
            }
            let wk = (f[0].to_owned(), f[1].to_owned());
            let wm = (wi, f[3].to_owned(), f[4].to_owned(), wa);
            if let Some(old) = window_ids.insert(wk.clone(), wm.clone()) {
                if old != wm {
                    return Err(Error::Invalid);
                }
            }
            let win = match session.windows.iter().position(|w| w.index == wi) {
                Some(i) => &mut session.windows[i],
                None => {
                    session.windows.push(SavedWindow {
                        index: wi,
                        name: f[3].into(),
                        layout: f[4].into(),
                        active: wa,
                        panes: vec![],
                    });
                    session.windows.last_mut().ok_or(Error::Invalid)?
                }
            };
            if win.name != f[3]
                || win.layout != f[4]
                || win.active != wa
                || win.panes.iter().any(|p| p.index == pi)
            {
                return Err(Error::Invalid);
            }
            win.panes.push(SavedPane {
                index: pi,
                cwd: f[9].into(),
                active: pa,
                resume: None,
            });
            pid_position.insert(pid, (f[0].to_owned(), wi, pi));
            pids.push(pid);
        }
        self.check_cancel()?;
        let bindings = (self.resolver)(pids).await?;
        self.check_cancel()?;
        for (pid, r) in bindings {
            if !r.valid() {
                return Err(Error::Invalid);
            }
            let (sid, wi, pi) = pid_position.get(&pid).ok_or(Error::Invalid)?;
            let pane = by_id
                .get_mut(sid)
                .and_then(|s| s.windows.iter_mut().find(|w| w.index == *wi))
                .and_then(|w| w.panes.iter_mut().find(|p| p.index == *pi))
                .ok_or(Error::Invalid)?;
            pane.resume = Some(r);
        }
        let again = self.basic().await?.sessions.unwrap_or_default();
        if again.len() != sessions.len()
            || again.iter().any(|s| {
                !sessions.iter().any(|old| {
                    old.id == s.id && old.created_at == s.created_at && old.name == s.name
                })
            })
        {
            return Err(Error::Changed);
        }
        let raw_again = self
            .command(
                vec![
                    "list-panes".into(),
                    "-a".into(),
                    "-F".into(),
                    PANE_FORMAT.into(),
                ],
                MAX_STATE * 2,
            )
            .await?;
        if raw != raw_again {
            return Err(Error::Changed);
        }
        let mut shot = Snapshot {
            sessions: by_id.into_values().collect(),
        };
        for s in &mut shot.sessions {
            for w in &mut s.windows {
                w.panes.sort_by_key(|p| p.index)
            }
            s.windows.sort_by_key(|w| w.index)
        }
        validate_snapshot(&shot)?;
        Ok(shot)
    }
}

#[cfg(any(target_os = "macos", test))]
fn darwin_boot_id(raw: &[u8]) -> Result<String, Error> {
    let text = std::str::from_utf8(raw).map_err(|_| Error::Invalid)?;
    let (_, fields) = text.split_once("sec = ").ok_or(Error::Invalid)?;
    let (seconds, tail) = fields.split_once(", usec = ").ok_or(Error::Invalid)?;
    let micros = &tail[..tail.bytes().take_while(u8::is_ascii_digit).count()];
    if !(1..=20).contains(&seconds.len())
        || !seconds.bytes().all(|b| b.is_ascii_digit())
        || !(1..=6).contains(&micros.len())
    {
        return Err(Error::Invalid);
    }
    Ok(format!("darwin-{seconds}-{micros}"))
}
#[derive(Clone, Copy)]
enum Operation {
    Sync,
    Save,
    Restore,
}
fn flag(s: &str) -> Result<bool, Error> {
    match s {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(Error::Invalid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicUsize, Ordering},
    };
    fn temp(name: &str) -> PathBuf {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).unwrap();
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("hmux-e2e-recovery-{name}-{}", restore::hex(&bytes)));
        std::fs::create_dir(&root).unwrap();
        root
    }
    fn sample() -> Snapshot {
        Snapshot {
            sessions: vec![SavedSession {
                identity: SessionIdentity {
                    id: "$1".into(),
                    created_at: 100,
                },
                name: "work".into(),
                alias: "Alias".into(),
                hidden: false,
                profile: String::new(),
                label: String::new(),
                tags: vec![],
                windows: vec![SavedWindow {
                    index: 0,
                    name: "editor".into(),
                    layout: "b1e2,80x24,0,0,2".into(),
                    active: true,
                    panes: vec![SavedPane {
                        index: 0,
                        cwd: "/tmp".into(),
                        active: true,
                        resume: None,
                    }],
                }],
            }],
        }
    }
    #[test]
    fn parses_darwin_boot_identity_with_kernel_suffix() {
        assert_eq!(
            darwin_boot_id(b"{ sec = 1700000000, usec = 123456 } Tue Nov 14 00:00:00 2023\n"),
            Ok("darwin-1700000000-123456".into())
        );
        assert_eq!(
            darwin_boot_id(b"{ sec = 1, usec = 0 }\n"),
            Ok("darwin-1-0".into())
        );
        for raw in [
            b"{ sec = , usec = 0 }".as_slice(),
            b"{ sec = 1, usec = 1234567 }",
            b"{ sec = 1, usec = }",
            b"sysctl: unavailable",
        ] {
            assert_eq!(darwin_boot_id(raw), Err(Error::Invalid));
        }
    }
    #[test]
    fn rejects_topology_and_unsafe_layout_without_panicking() {
        let mut shot = sample();
        assert_eq!(validate_snapshot(&shot), Ok(()));
        let duplicate = shot.sessions[0].windows[0].panes[0].clone();
        shot.sessions[0].windows[0].panes.push(duplicate);
        assert_eq!(validate_snapshot(&shot), Err(Error::Invalid));
        assert!(!valid_layout("éé,80x24,0,0,2"));
        assert!(!valid_layout("b1e2,80x24,0,0,2garbage"));
    }
    #[test]
    fn workspace_composes_two_boots() {
        let mut live = sample();
        live.sessions[0].identity = SessionIdentity {
            id: "$3".into(),
            created_at: 300,
        };
        let a = Mapping {
            from: SessionIdentity {
                id: "$1".into(),
                created_at: 100,
            },
            to: SessionIdentity {
                id: "$2".into(),
                created_at: 200,
            },
            name: "work".into(),
            panes: BTreeMap::new(),
            gate: String::new(),
        };
        let b = Mapping {
            from: a.to.clone(),
            to: live.sessions[0].identity.clone(),
            name: "work".into(),
            panes: BTreeMap::new(),
            gate: String::new(),
        };
        let links = restore::workspace_lineages(&[a], [b].as_slice(), &live);
        assert!(links
            .iter()
            .any(|x| x.restored_from.as_ref().is_some_and(|p| p.id == "$1") && x.id == "$3"));
    }
    #[test]
    fn reads_go_null_collections_and_does_not_create_on_apply() {
        let raw = br#"{"version":1,"boot_id":"boot-a","checkpoint":{"sessions":null},"pending":{"boot_id":"boot-b","snapshot":{"sessions":null},"completed":null,"intents":null},"mappings":null,"updated_at":"2026-09-24T00:00:00Z"}"#;
        let parsed: DiskState = serde_json::from_slice(raw).unwrap();
        validate_state(&parsed).unwrap();
        assert!(parsed.checkpoint.sessions.is_empty());
        assert!(parsed.pending.unwrap().completed.is_empty());
        let root = temp("apply-missing");
        let store = Store::new(
            root.join("state"),
            PathBuf::from("/bin/sh"),
            None,
            CommandRunner::new(1).unwrap(),
            Arc::new(|_| Box::pin(async { Ok(BTreeMap::new()) })),
        )
        .unwrap();
        let mut catalog = Catalog::default();
        store.apply(&mut catalog).unwrap();
        assert!(!root.join("state").exists());
        let _ = std::fs::remove_dir_all(root);
    }
    #[tokio::test]
    async fn restores_saved_topology_and_commits_exact_lineage_with_fake_tmux() {
        let root = temp("restore");
        let mode = root.join("mode");
        std::fs::write(&mode, "old").unwrap();
        let tool = root.join("fake-tmux");
        let script = format!(
            r#"#!/bin/sh
root='{}'
mode=$(cat "$root/mode")
sep='|:hmux-sep-v1:|'
rec='|:hmux-recovery-v1:|'
case "$1" in
list-sessions)
  case "$mode" in
    old) printf '%s\n' '$1|:hmux-sep-v1:|work|:hmux-sep-v1:|100|:hmux-sep-v1:|100|:hmux-sep-v1:|0|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|0';;
    created) name=$(cat "$root/name"); printf '%s\n' "\$2${{sep}}${{name}}${{sep}}200${{sep}}200${{sep}}0${{sep}}1${{sep}}${{sep}}0";;
    renamed) printf '%s\n' '$2|:hmux-sep-v1:|work|:hmux-sep-v1:|200|:hmux-sep-v1:|200|:hmux-sep-v1:|0|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|0';;
  esac;;
list-windows)
  case "$mode" in
    old) printf '%s\n' '$1|:hmux-sep-v1:|editor|:hmux-sep-v1:|1|:hmux-sep-v1:|/tmp|:hmux-sep-v1:|sh|:hmux-sep-v1:|80|:hmux-sep-v1:|24|:hmux-sep-v1:|101';;
    created|renamed) printf '%s\n' '$2|:hmux-sep-v1:|editor|:hmux-sep-v1:|1|:hmux-sep-v1:|/tmp|:hmux-sep-v1:|sh|:hmux-sep-v1:|80|:hmux-sep-v1:|24|:hmux-sep-v1:|202';;
  esac;;
list-panes)
  if [ "$2" = '-s' ]; then printf '%s\n' '%4|:hmux-recovery-v1:|0|:hmux-recovery-v1:|0';
  elif [ "$mode" = old ]; then printf '%s\n' '$1|:hmux-recovery-v1:|@2|:hmux-recovery-v1:|0|:hmux-recovery-v1:|editor|:hmux-recovery-v1:|b1e2,80x24,0,0,2|:hmux-recovery-v1:|1|:hmux-recovery-v1:|%3|:hmux-recovery-v1:|0|:hmux-recovery-v1:|1|:hmux-recovery-v1:|/tmp|:hmux-recovery-v1:|101';
  else printf '%s\n' '$2|:hmux-recovery-v1:|@3|:hmux-recovery-v1:|0|:hmux-recovery-v1:|editor|:hmux-recovery-v1:|b1e2,80x24,0,0,2|:hmux-recovery-v1:|1|:hmux-recovery-v1:|%4|:hmux-recovery-v1:|0|:hmux-recovery-v1:|1|:hmux-recovery-v1:|/tmp|:hmux-recovery-v1:|202'; fi;;
new-session)
  while [ "$#" -gt 0 ]; do if [ "$1" = '-s' ]; then printf '%s' "$2" > "$root/name"; break; fi; shift; done
  printf '%s' created > "$root/mode"
  printf '%s\n' '$2|:hmux-recovery-v1:|@3|:hmux-recovery-v1:|%4|:hmux-recovery-v1:|200|:hmux-recovery-v1:|0';;
display-message)
  for item in "$@"; do fmt="$item"; done
  case "$fmt" in
    *pane_start_command*) exit 2;;
    *session_name*session_created*) name=$(cat "$root/name"); printf '%s\n' "\$2${{rec}}${{name}}${{rec}}200";;
    *session_id*session_created*) printf '%s\n' '$2|:hmux-recovery-v1:|200';;
    *session_name*) if [ "$mode" = renamed ]; then printf 'work\n'; else cat "$root/name"; printf '\n'; fi;;
    *) exit 2;;
  esac;;
rename-session) printf '%s' renamed > "$root/mode";;
select-layout|select-pane|select-window) :;;
*) exit 2;;
esac
"#,
            root.display()
        );
        std::fs::write(&tool, script).unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700)).unwrap();
        let boot = Arc::new(AtomicUsize::new(0));
        let selected = boot.clone();
        let store = Store::new(
            root.join("state"),
            tool,
            None,
            CommandRunner::new(2).unwrap(),
            Arc::new(|_| Box::pin(async { Ok(BTreeMap::new()) })),
        )
        .unwrap()
        .with_boot_id(Arc::new(move || {
            Ok(if selected.load(Ordering::SeqCst) == 0 {
                "boot-a"
            } else {
                "boot-b"
            }
            .into())
        }));
        store.sync().await.unwrap();
        std::fs::write(&mode, "empty").unwrap();
        boot.store(1, Ordering::SeqCst);
        store.sync().await.unwrap();
        let state = Store::read(&store.dir().unwrap()).unwrap().unwrap();
        assert!(state.pending.is_none());
        assert_eq!(state.mappings.len(), 1);
        assert_eq!(state.mappings[0].from.id, "$1");
        assert_eq!(state.mappings[0].to.id, "$2");
        assert_eq!(
            std::fs::read_to_string(&state.mappings[0].gate).unwrap(),
            "ready\n"
        );
        let mut catalog = Catalog {
            sessions: Some(vec![Session {
                id: "$2".into(),
                created_at: 200,
                name: "work".into(),
                ..Session::default()
            }]),
            ..Catalog::default()
        };
        store.apply(&mut catalog).unwrap();
        assert_eq!(
            catalog.sessions.as_ref().unwrap()[0]
                .restored_from
                .as_ref()
                .unwrap()
                .id,
            "$1"
        );
        let _ = std::fs::remove_dir_all(root);
    }
    #[tokio::test]
    async fn checkpoint_uses_go_state_schema_and_refreshes_bindings() {
        let root = temp("capture");
        let tool = root.join("fake-tmux");
        let script="#!/bin/sh\ncase \"$1\" in\nlist-sessions) printf '%s\\n' '$1|:hmux-sep-v1:|work|:hmux-sep-v1:|100|:hmux-sep-v1:|100|:hmux-sep-v1:|0|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|0';;\nlist-windows) printf '%s\\n' '$1|:hmux-sep-v1:|editor|:hmux-sep-v1:|1|:hmux-sep-v1:|/tmp|:hmux-sep-v1:|sh|:hmux-sep-v1:|80|:hmux-sep-v1:|24|:hmux-sep-v1:|101';;\nlist-panes) printf '%s\\n' '$1|:hmux-recovery-v1:|@2|:hmux-recovery-v1:|0|:hmux-recovery-v1:|editor|:hmux-recovery-v1:|b1e2,80x24,0,0,2|:hmux-recovery-v1:|1|:hmux-recovery-v1:|%3|:hmux-recovery-v1:|0|:hmux-recovery-v1:|1|:hmux-recovery-v1:|/tmp|:hmux-recovery-v1:|101';;\n*) exit 2;;\nesac\n";
        std::fs::write(&tool, script).unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700)).unwrap();
        let n = Arc::new(AtomicUsize::new(0));
        let count = n.clone();
        let resolver: Resolver = Arc::new(move |pids| {
            assert_eq!(pids, vec![101]);
            let seq = count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                Ok(BTreeMap::from([(
                    101,
                    ResumeReference {
                        provider: "codex".into(),
                        session_id: format!("session-{seq}"),
                        config_dir: "/tmp/codex".into(),
                    },
                )]))
            })
        });
        let store = Store::new(
            root.join("state"),
            tool,
            None,
            CommandRunner::new(2).unwrap(),
            resolver,
        )
        .unwrap()
        .with_boot_id(Arc::new(|| Ok("boot-a".into())));
        store.sync().await.unwrap();
        let dir = store.dir().unwrap();
        let state = Store::read(&dir).unwrap().unwrap();
        assert_eq!(
            state.checkpoint.sessions[0].windows[0].panes[0]
                .resume
                .as_ref()
                .unwrap()
                .session_id,
            "session-0"
        );
        store.save().await.unwrap();
        let state = Store::read(&dir).unwrap().unwrap();
        assert_eq!(
            state.checkpoint.sessions[0].windows[0].panes[0]
                .resume
                .as_ref()
                .unwrap()
                .session_id,
            "session-1"
        );
        let other = store.clone().with_boot_id(Arc::new(|| Ok("boot-b".into())));
        assert_eq!(other.save().await, Err(Error::Changed));
        let _ = std::fs::remove_dir_all(root);
    }
}
