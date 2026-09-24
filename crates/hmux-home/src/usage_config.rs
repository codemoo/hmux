//! Captured, bounded Home usage options. Construction reads environment values
//! only. The optional codex-lb key file is read after startup by load_lb.
use crate::{usage_activity, usage_lb};
use rustix::fs::{self, Mode, OFlags};
use std::{
    ffi::{OsStr, OsString},
    fmt,
    fs::File,
    io::Read,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Component, Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

const MAX_PATH_BYTES: usize = 2048;
const MAX_EXEC_PATH_BYTES: usize = 64 << 10;
const MAX_KEY_FILE_BYTES: u64 = 64 << 10;
const MAX_KEY_BYTES: usize = 4096;
const LOAD_TIMEOUT: Duration = Duration::from_secs(5);
static KEY_SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();

#[derive(Clone)]
pub struct Options {
    pub(crate) home: PathBuf,
    pub(crate) path: OsString,
    pub(crate) activity: Option<usage_activity::Options>,
    pub(crate) cswap: bool,
    pub(crate) accounts: Option<PathBuf>,
    pub(crate) lb: Option<LbOptions>,
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UsageOptions([redacted])")
    }
}

#[derive(Clone)]
pub(crate) struct LbOptions {
    endpoint: usage_lb::Endpoint,
    env_key: Option<String>,
}
impl fmt::Debug for LbOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LbOptions([redacted])")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidHome,
    InvalidPath,
}
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidHome => "UsageConfigError::InvalidHome",
            Self::InvalidPath => "UsageConfigError::InvalidPath",
        })
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
impl std::error::Error for Error {}

impl Options {
    /// Capture environment policy without touching provider files or network.
    pub fn capture(home: &Path, path: &OsStr) -> Result<Self, Error> {
        Self::from_env(home, path, |name| std::env::var_os(name))
    }

    /// Injectable environment getter for callers and synthetic tests.
    pub fn from_env(
        home: &Path,
        path: &OsStr,
        get: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self, Error> {
        if !valid_home(home) {
            return Err(Error::InvalidHome);
        }
        let path_bytes = path.as_bytes();
        if path_bytes.is_empty()
            || path_bytes.len() > MAX_EXEC_PATH_BYTES
            || path_bytes.contains(&0)
        {
            return Err(Error::InvalidPath);
        }

        let cswap = !is_disabled(&get, "TOKEN_USAGE_DISABLE_CLAUDE_SWAP");
        let activity = if is_disabled(&get, "TOKEN_USAGE_DISABLE_JSONL") {
            None
        } else {
            let claude = source_path(
                home,
                &get,
                "TOKEN_USAGE_CLAUDE_PROJECTS",
                ".claude/projects",
            );
            let codex = source_path(home, &get, "TOKEN_USAGE_CODEX_SESSIONS", ".codex/sessions");
            match (claude, codex) {
                (Some(claude_projects_root), Some(codex_sessions_root)) => {
                    let claude_swap_sessions_root = if !cswap
                        || is_disabled(&get, "TOKEN_USAGE_DISABLE_CLAUDE_SWAP_SESSIONS")
                    {
                        None
                    } else {
                        source_path(
                            home,
                            &get,
                            "TOKEN_USAGE_CLAUDE_SWAP_SESSIONS_ROOT",
                            ".claude-swap-backup/sessions",
                        )
                    };
                    Some(usage_activity::Options {
                        claude_projects_root,
                        codex_sessions_root,
                        claude_swap_sessions_root,
                    })
                }
                _ => None,
            }
        };
        let accounts = if is_disabled(&get, "TOKEN_USAGE_DISABLE_CODEX_ACCOUNTS") {
            None
        } else {
            source_path(
                home,
                &get,
                "TOKEN_USAGE_CODEX_ACCOUNTS",
                ".config/token-usage/codex-lb-accounts.json",
            )
        };
        let lb = if is_disabled(&get, "TOKEN_USAGE_DISABLE_CODEX_LB") {
            None
        } else {
            let url = first_nonempty(&get, &["TOKEN_USAGE_CODEX_LB_URL", "CODEX_LB_BASE_URL"]);
            let endpoint = match url {
                Some(Ok(url)) => usage_lb::Endpoint::parse(&url).ok(),
                Some(Err(())) => None,
                None => usage_lb::Endpoint::parse("").ok(),
            };
            endpoint.map(|endpoint| LbOptions {
                endpoint,
                env_key: ["TOKEN_USAGE_CODEX_LB_API_KEY", "CODEX_LB_API_KEY"]
                    .into_iter()
                    .filter_map(get)
                    .filter_map(|raw| raw.to_str().and_then(normalize_key).map(str::to_owned))
                    .next(),
            })
        };
        Ok(Self {
            home: home.to_path_buf(),
            path: path.to_os_string(),
            activity,
            cswap,
            accounts,
            lb,
        })
    }

    /// Load optional codex-lb configuration. A private key file is opened only
    /// after startup, in one admitted blocking worker. The worker owns its slot
    /// until it exits even if its caller cancels or times out.
    pub async fn load_lb(
        &self,
        cancel: &CancellationToken,
    ) -> Option<(usage_lb::Endpoint, String)> {
        if cancel.is_cancelled() {
            return None;
        }
        let lb = self.lb.as_ref()?;
        if let Some(key) = &lb.env_key {
            return Some((lb.endpoint.clone(), key.clone()));
        }
        let permit = KEY_SLOTS
            .get_or_init(|| Arc::new(Semaphore::new(1)))
            .clone()
            .try_acquire_owned()
            .ok()?;
        let file = self.home.join(".codex/lb-api-key");
        let endpoint = lb.endpoint.clone();
        let child = cancel.child_token();
        let worker_cancel = child.clone();
        let worker = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            read_private_key(&file, &worker_cancel)
        });
        let _guard = child.clone().drop_guard();
        let key = tokio::select! {
            biased;
            _ = child.cancelled() => return None,
            result = tokio::time::timeout(LOAD_TIMEOUT, worker) =>
                result.ok()?.ok()??,
        };
        Some((endpoint, key))
    }
}

fn is_disabled(get: &impl Fn(&str) -> Option<OsString>, name: &str) -> bool {
    get(name).as_deref() == Some(OsStr::new("1"))
}

fn valid_home(home: &Path) -> bool {
    let bytes = home.as_os_str().as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_PATH_BYTES
        && !bytes.contains(&0)
        && home.is_absolute()
        && home.components().collect::<PathBuf>() == home
        && !home
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
}

fn valid_source(path: &Path) -> bool {
    let bytes = path.as_os_str().as_bytes();
    path.is_absolute()
        && bytes.len() <= MAX_PATH_BYTES
        && !bytes.contains(&0)
        && path.components().collect::<PathBuf>() == path
        && !path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
}

fn source_path(
    home: &Path,
    get: &impl Fn(&str) -> Option<OsString>,
    name: &str,
    default: &str,
) -> Option<PathBuf> {
    let path = match get(name) {
        None => home.join(default),
        Some(raw) => match raw.to_str() {
            Some(text) if text.trim().is_empty() => home.join(default),
            Some(text) => PathBuf::from(text.trim()),
            None => return None,
        },
    };
    valid_source(&path).then_some(path)
}

// Preserve Go's first nonempty URL precedence. An invalid first value disables
// this optional source; it does not silently fall through to another endpoint.
fn first_nonempty(
    get: &impl Fn(&str) -> Option<OsString>,
    names: &[&str],
) -> Option<Result<String, ()>> {
    for name in names {
        let Some(raw) = get(name) else { continue };
        match raw.to_str() {
            Some(text) if !text.trim().is_empty() => return Some(Ok(text.trim().to_owned())),
            Some(_) => {}
            None if !raw.is_empty() => return Some(Err(())),
            None => {}
        }
    }
    None
}

fn normalize_key(value: &str) -> Option<&str> {
    let value = value.trim();
    (value.len() <= MAX_KEY_BYTES
        && !value.is_empty()
        && value.bytes().all(|b| (b'!'..=b'~').contains(&b)))
    .then_some(value)
}

fn same_revision(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    after.is_file()
        && after.dev() == before.dev()
        && after.ino() == before.ino()
        && after.uid() == before.uid()
        && after.nlink() == before.nlink()
        && after.mode() == before.mode()
        && after.len() == before.len()
        && after.mtime() == before.mtime()
        && after.mtime_nsec() == before.mtime_nsec()
        && after.ctime() == before.ctime()
        && after.ctime_nsec() == before.ctime_nsec()
}

fn read_private_key(path: &Path, cancel: &CancellationToken) -> Option<String> {
    if cancel.is_cancelled() {
        return None;
    }
    // Descriptor walk rejects symlinks in every path component. NONBLOCK
    // prevents a FIFO from stalling an admitted worker before metadata checks.
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut dir = fs::open("/", flags | OFlags::DIRECTORY, Mode::empty()).ok()?;
    let mut parts = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .peekable();
    let (file, leaf) = loop {
        let name = parts.next()?;
        if parts.peek().is_none() {
            break (
                File::from(fs::openat(&dir, name, flags | OFlags::NONBLOCK, Mode::empty()).ok()?),
                name.to_os_string(),
            );
        }
        dir = fs::openat(&dir, name, flags | OFlags::DIRECTORY, Mode::empty()).ok()?;
    };
    let mut file = file;
    let before = file.metadata().ok()?;
    if !before.is_file()
        || before.uid() != rustix::process::geteuid().as_raw()
        || before.nlink() != 1
        || before.mode() & 0o077 != 0
        || before.len() < 1
        || before.len() > MAX_KEY_FILE_BYTES
    {
        return None;
    }
    if cancel.is_cancelled() {
        return None;
    }
    let mut data = Vec::with_capacity((before.len() as usize).min(MAX_KEY_FILE_BYTES as usize));
    file.by_ref()
        .take(MAX_KEY_FILE_BYTES + 1)
        .read_to_end(&mut data)
        .ok()?;
    let after = file.metadata().ok()?;
    if cancel.is_cancelled() || data.len() as u64 != before.len() || !same_revision(&before, &after)
    {
        return None;
    }
    // Reopen relative to the held parent descriptor. An atomic CLI rotation
    // between the first open and this check cannot return the old key.
    let current =
        File::from(fs::openat(&dir, &leaf, flags | OFlags::NONBLOCK, Mode::empty()).ok()?);
    if cancel.is_cancelled() || !same_revision(&before, &current.metadata().ok()?) {
        return None;
    }
    let value = std::str::from_utf8(&data).ok()?;
    normalize_key(value).map(str::to_owned)
}

#[cfg(test)]
#[path = "usage_config_tests.rs"]
mod tests;
