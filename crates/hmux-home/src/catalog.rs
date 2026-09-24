//! Bounded, read-only tmux catalog snapshot. Provider/process inspection is separate.

use chrono::{DateTime, Utc};
use hmux_core::command::{CommandRunner, CommandSpec, RunError, RunErrorKind};
use hmux_model::{safe_text, validate_session_id, Catalog, Session, PROTOCOL_VERSION};
use std::collections::HashMap;
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SEPARATOR: &str = "|:hmux-sep-v1:|";
const SESSION_LIMIT: usize = 16 * 1024 * 1024;
const WINDOW_LIMIT: usize = 32 * 1024 * 1024;
const MAX_SESSIONS: usize = 10_000;
const MAX_WINDOWS: usize = 100_000;

const SESSION_FORMAT: &str = "#{session_id}|:hmux-sep-v1:|#{session_name}|:hmux-sep-v1:|#{session_created}|:hmux-sep-v1:|#{session_activity}|:hmux-sep-v1:|#{session_attached}|:hmux-sep-v1:|#{session_windows}|:hmux-sep-v1:|#{@hmux_app_view}|:hmux-sep-v1:|#{session_group_attached}";
const WINDOW_FORMAT: &str = "#{session_id}|:hmux-sep-v1:|#{window_name}|:hmux-sep-v1:|#{window_active}|:hmux-sep-v1:|#{pane_current_path}|:hmux-sep-v1:|#{pane_current_command}|:hmux-sep-v1:|#{window_width}|:hmux-sep-v1:|#{window_height}|:hmux-sep-v1:|#{pane_pid}";

#[derive(Debug)]
pub enum CatalogError {
    InvalidOption,
    Command(RunError),
    Parse(&'static str),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOption => f.write_str("invalid tmux catalog option"),
            Self::Command(error) => write!(f, "tmux catalog command: {error}"),
            Self::Parse(message) => write!(f, "invalid tmux catalog: {message}"),
        }
    }
}
impl std::error::Error for CatalogError {}

/// The caller supplies an executable and optional socket. No shell is involved.
/// `-L` accepts a tmux socket name; `-S` accepts an absolute socket path.
#[derive(Clone)]
pub struct TmuxCatalogReader {
    executable: PathBuf,
    socket: Option<TmuxSocket>,
    timeout: Duration,
}

#[derive(Clone)]
pub enum TmuxSocket {
    Name(String),
    Path(PathBuf),
}

impl TmuxCatalogReader {
    pub(crate) fn terminal_target(&self) -> Result<crate::view::Target, crate::view::Error> {
        crate::view::Target::new(self.executable.clone(), self.socket.clone())
    }
    pub fn new(
        executable: PathBuf,
        socket: Option<TmuxSocket>,
        timeout: Duration,
    ) -> Result<Self, CatalogError> {
        if !executable.is_absolute()
            || executable.as_os_str().as_bytes().len() > 1024
            || executable.as_os_str().as_bytes().contains(&0)
            || timeout.is_zero()
            || timeout > Duration::from_secs(30)
        {
            return Err(CatalogError::InvalidOption);
        }
        match &socket {
            Some(TmuxSocket::Name(name))
                if name.is_empty()
                    || name.len() > 100
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') =>
            {
                return Err(CatalogError::InvalidOption)
            }
            Some(TmuxSocket::Path(path)) if !path.is_absolute() || path.as_os_str().is_empty() => {
                return Err(CatalogError::InvalidOption)
            }
            _ => {}
        }
        Ok(Self {
            executable,
            socket,
            timeout,
        })
    }

    /// At most two finite-admission commands. A missing server yields an empty catalog.
    pub async fn read_basic(&self, runner: &CommandRunner) -> Result<Catalog, CatalogError> {
        let sessions = match runner
            .run(self.command(SESSION_LIMIT, ["list-sessions", "-F", SESSION_FORMAT]))
            .await
        {
            Ok(output) => output.stdout,
            Err(error) if no_tmux_server(&error) => return Ok(empty_catalog(now_utc())),
            Err(error) => return Err(CatalogError::Command(error)),
        };
        let windows = runner
            .run(self.command(WINDOW_LIMIT, ["list-windows", "-a", "-F", WINDOW_FORMAT]))
            .await
            .map_err(CatalogError::Command)?
            .stdout;
        parse_basic_catalog(&sessions, &windows, now_utc())
    }

    /// Read-only fresh identity lookup for metadata transactions. Cancellation
    /// kills/reaps only the direct query child and never starts another query.
    pub(crate) async fn read_basic_cancelable(
        &self,
        runner: &CommandRunner,
        stop: &tokio_util::sync::CancellationToken,
        deadline: tokio::time::Instant,
    ) -> Result<Catalog, CatalogError> {
        async fn run(
            runner: &CommandRunner,
            command: CommandSpec,
            stop: &tokio_util::sync::CancellationToken,
            deadline: tokio::time::Instant,
        ) -> Result<Vec<u8>, CatalogError> {
            if stop.is_cancelled() {
                return Err(CatalogError::InvalidOption);
            }
            let (cancel, receiver) = tokio::sync::oneshot::channel();
            let work = runner.run_cancelable(command, receiver);
            tokio::pin!(work);
            tokio::select! {
                biased;
                _ = stop.cancelled() => { drop(cancel); let _ = work.await; Err(CatalogError::InvalidOption) },
                _ = tokio::time::sleep_until(deadline) => { drop(cancel); let _ = work.await; Err(CatalogError::InvalidOption) },
                result = &mut work => result.map(|v| v.stdout).map_err(CatalogError::Command),
            }
        }
        let sessions = match run(
            runner,
            self.command(SESSION_LIMIT, ["list-sessions", "-F", SESSION_FORMAT]),
            stop,
            deadline,
        )
        .await
        {
            Ok(raw) => raw,
            Err(CatalogError::Command(error)) if no_tmux_server(&error) => {
                return Ok(empty_catalog(now_utc()))
            }
            Err(error) => return Err(error),
        };
        let windows = run(
            runner,
            self.command(WINDOW_LIMIT, ["list-windows", "-a", "-F", WINDOW_FORMAT]),
            stop,
            deadline,
        )
        .await?;
        parse_basic_catalog(&sessions, &windows, now_utc())
    }

    pub(crate) fn command<const N: usize>(&self, limit: usize, args: [&str; N]) -> CommandSpec {
        let mut spec = CommandSpec::new(self.executable.clone(), limit, self.timeout);
        if let Some(socket) = &self.socket {
            spec = match socket {
                TmuxSocket::Name(name) => spec.arg("-L").arg(name),
                TmuxSocket::Path(path) => spec.arg("-S").arg(path),
            };
        }
        spec.args(args)
    }
}

pub(crate) fn no_tmux_server(error: &RunError) -> bool {
    if error.kind() != RunErrorKind::Exit {
        return false;
    }
    let Some(stderr) = error.stderr() else {
        return false;
    };
    let message = safe_text(&String::from_utf8_lossy(stderr).to_lowercase(), 4096);
    message.contains("no server running on ")
        || (message.contains("error connecting to ")
            && message.contains("no such file or directory"))
}

fn now_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    DateTime::<Utc>::from_timestamp(secs.try_into().unwrap_or(0), 0)
        .expect("valid epoch")
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn empty_catalog(generated_at: String) -> Catalog {
    Catalog {
        protocol_version: PROTOCOL_VERSION,
        generated_at,
        sessions: Some(Vec::new()),
        host_metrics: None,
    }
}

/// Pure parser for synthetic fixtures. Both outputs are checked before allocation.
pub fn parse_basic_catalog(
    sessions: &[u8],
    windows: &[u8],
    generated_at: String,
) -> Result<Catalog, CatalogError> {
    if sessions.len() > SESSION_LIMIT || windows.len() > WINDOW_LIMIT {
        return Err(CatalogError::Parse("output limit exceeded"));
    }
    let mut catalog = empty_catalog(generated_at);
    let entries = catalog.sessions.as_mut().expect("initialized");
    let mut by_id = HashMap::new();
    for row in rows(sessions, MAX_SESSIONS)? {
        let fields = fields(row, "session row")?;
        validate_session_id(fields[0]).map_err(|_| CatalogError::Parse("session id"))?;
        if fields[6] == "1" {
            continue;
        }
        if by_id.contains_key(fields[0]) {
            return Err(CatalogError::Parse("duplicate session id"));
        }
        let created_at = number(fields[2], "session_created")?;
        let activity_at = number(fields[3], "session_activity")?;
        let direct = count(fields[4], "session_attached")?;
        let attached = if fields[7].is_empty() {
            direct
        } else {
            count(fields[7], "session_group_attached")?
        };
        let window_count = count(fields[5], "session_windows")?;
        let session = Session {
            id: fields[0].to_owned(),
            name: safe_text(fields[1], 512),
            created_at,
            activity_at,
            attached,
            window_count,
            kind: "shell".into(),
            runtime: "process".into(),
            state: "running".into(),
            ..Session::default()
        };
        by_id.insert(session.id.clone(), entries.len());
        entries.push(session);
    }
    for row in rows(windows, MAX_WINDOWS)? {
        let fields = fields(row, "window row")?;
        let Some(&index) = by_id.get(fields[0]) else {
            continue;
        };
        let session = &mut entries[index];
        let name = safe_text(fields[1], 256);
        session
            .window_names
            .get_or_insert_with(Vec::new)
            .push(name.clone());
        if fields[2] == "1" {
            session.active_window = name;
            session.current_path = safe_text(fields[3], 2048);
            session.current_command = safe_text(fields[4], 256);
            session.width = dimension(fields[5], "window_width")?;
            session.height = dimension(fields[6], "window_height")?;
            session.pane_pid = pane_pid(fields[7])?;
        }
    }
    for session in entries.iter_mut() {
        session.process = safe_text(&session.current_command, 128);
        if session.process.is_empty() {
            session.process = "shell".into();
        }
    }
    entries.sort_unstable_by(|a, b| {
        b.activity_at
            .cmp(&a.activity_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(catalog)
}

fn rows(raw: &[u8], max: usize) -> Result<impl Iterator<Item = &str> + '_, CatalogError> {
    let text = std::str::from_utf8(raw).map_err(|_| CatalogError::Parse("non-UTF-8 output"))?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    // Check the count before parsing, without allocating a row table.
    if !text.is_empty() && text.as_bytes().iter().filter(|&&b| b == b'\n').count() >= max {
        return Err(CatalogError::Parse("row count limit exceeded"));
    }
    Ok((!text.is_empty())
        .then(|| text.split('\n'))
        .into_iter()
        .flatten())
}

fn fields<'a>(row: &'a str, kind: &'static str) -> Result<[&'a str; 8], CatalogError> {
    let mut pieces = row.split(SEPARATOR);
    let mut fields = [""; 8];
    for field in &mut fields {
        *field = pieces.next().ok_or(CatalogError::Parse(kind))?;
    }
    if pieces.next().is_some() {
        return Err(CatalogError::Parse(kind));
    }
    Ok(fields)
}

fn number(text: &str, field: &'static str) -> Result<i64, CatalogError> {
    let value = text
        .parse::<i64>()
        .map_err(|_| CatalogError::Parse(field))?;
    if value < 0 {
        return Err(CatalogError::Parse(field));
    }
    Ok(value)
}
fn count(text: &str, field: &'static str) -> Result<i64, CatalogError> {
    let value = number(text, field)?;
    if value > 10_000 {
        return Err(CatalogError::Parse(field));
    }
    Ok(value)
}
fn dimension(text: &str, field: &'static str) -> Result<i64, CatalogError> {
    if text.is_empty() {
        return Ok(0);
    }
    let value = number(text, field)?;
    if value > 100_000 {
        return Err(CatalogError::Parse(field));
    }
    Ok(value)
}
fn pane_pid(text: &str) -> Result<i64, CatalogError> {
    if text.is_empty() {
        return Ok(0);
    }
    let value = number(text, "pane_pid")?;
    if !(1..=1 << 30).contains(&value) {
        return Err(CatalogError::Parse("pane_pid"));
    }
    Ok(value)
}
