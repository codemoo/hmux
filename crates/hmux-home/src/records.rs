//! Synchronous, bounded provider record binding. Call from an admitted blocking worker.
//! Transcript contents are untrusted metadata, never instructions.
use crate::binding::{Binding, Status};
use rustix::fs::{self, AtFlags, Dir, Mode, OFlags};
use rustix::io::Errno;
use serde::Deserialize;
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

const HEADER_MAX: usize = 128 * 1024;
const TAIL_MAX: usize = 8 * 1024 * 1024;
const LINE_MAX: usize = 2 * 1024 * 1024;
const PATHS_MAX: usize = 1024;
const SWAP_ROOTS_MAX: usize = 128;
const PROJECTS_MAX: usize = 512;
const CHUNK: usize = 64 * 1024;

// Decode only fields that affect a binding or display metadata. Serde skips
// unknown transcript subtrees without building Value arrays/maps in memory.
// Option fields match Go's zero value on missing/null while rejecting wrong types.
#[derive(Deserialize)]
struct CodexHeader<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    payload: Option<CodexHeaderPayload<'a>>,
}
#[derive(Deserialize)]
struct CodexHeaderPayload<'a> {
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    source: Option<&'a RawValue>,
}
#[derive(Deserialize)]
struct CodexEvent<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    payload: Option<CodexEventPayload<'a>>,
}
#[derive(Deserialize)]
struct CodexEventPayload<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
}
#[derive(Deserialize)]
struct ClaudeRegistry<'a> {
    pid: Option<i64>,
    #[serde(rename = "sessionId", borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    status: Option<Cow<'a, str>>,
    #[serde(rename = "statusUpdatedAt")]
    updated: Option<i64>,
}
#[derive(Deserialize)]
struct ClaudeEvent<'a> {
    #[serde(borrow)]
    message: Option<ClaudeEventMessage<'a>>,
}
#[derive(Deserialize)]
struct ClaudeEventMessage<'a> {
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
}

/// Fixed error categories; provider paths, IDs and transcript bytes are never formatted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unavailable,
    Cancelled,
    Limit,
}

fn check(cancel: &CancellationToken, deadline: Instant) -> Result<(), Error> {
    if cancel.is_cancelled() || Instant::now() >= deadline {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
fn clean_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
        && !path
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
}
fn owned(uid: u32) -> bool {
    uid == rustix::process::geteuid().as_raw()
}
fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
fn file_flags() -> OFlags {
    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK
}
fn open_dir(path: &Path) -> Result<File, Error> {
    if !clean_absolute(path) {
        return Err(Error::Unavailable);
    }
    let mut dir =
        File::from(fs::open("/", dir_flags(), Mode::empty()).map_err(|_| Error::Unavailable)?);
    for component in path.components() {
        if let Component::Normal(name) = component {
            dir = File::from(
                fs::openat(&dir, name, dir_flags(), Mode::empty())
                    .map_err(|_| Error::Unavailable)?,
            );
        }
    }
    Ok(dir)
}
// A missing primary registry permits a swap-only Claude installation. Any
// present but rejected slot makes another root's PID association uncertain.
// Walk by descriptors so this probe never follows a symlinked component.
fn registry_slot_present(path: &Path) -> bool {
    if !clean_absolute(path) {
        return true;
    }
    let Ok(mut dir) = fs::open("/", dir_flags(), Mode::empty()) else {
        return true;
    };
    let mut parts = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .peekable();
    while let Some(name) = parts.next() {
        if parts.peek().is_none() {
            return fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW)
                .map_or_else(|error| error != Errno::NOENT, |_| true);
        }
        dir = match fs::openat(&dir, name, dir_flags(), Mode::empty()) {
            Ok(next) => next,
            Err(Errno::NOENT) => return false,
            Err(_) => return true,
        };
    }
    true
}
/// Open a user-owned regular provider record below a configured root, without following
/// links at any component. The final path and held fd must identify the same inode.
pub fn open_record(root: &Path, path: &Path) -> Result<File, Error> {
    open_record_inner(root, path, || {})
}
fn open_record_inner(root: &Path, path: &Path, before_open: impl FnOnce()) -> Result<File, Error> {
    if !clean_absolute(root) || !clean_absolute(path) {
        return Err(Error::Unavailable);
    }
    let relative = path.strip_prefix(root).map_err(|_| Error::Unavailable)?;
    if relative.as_os_str().is_empty() {
        return Err(Error::Unavailable);
    }
    let mut dir = open_dir(root)?;
    let mut parts = relative.components().peekable();
    let name = loop {
        let Some(Component::Normal(name)) = parts.next() else {
            return Err(Error::Unavailable);
        };
        if parts.peek().is_none() {
            break name;
        }
        dir = File::from(
            fs::openat(&dir, name, dir_flags(), Mode::empty()).map_err(|_| Error::Unavailable)?,
        );
    };
    let before =
        fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| Error::Unavailable)?;
    before_open();
    let file = File::from(
        fs::openat(&dir, name, file_flags(), Mode::empty()).map_err(|_| Error::Unavailable)?,
    );
    let held = file.metadata().map_err(|_| Error::Unavailable)?;
    let after =
        fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| Error::Unavailable)?;
    if !held.is_file()
        || !owned(held.uid())
        || !owned(before.st_uid)
        || !owned(after.st_uid)
        || i128::from(before.st_dev) != i128::from(held.dev())
        || before.st_ino != held.ino()
        || i128::from(after.st_dev) != i128::from(held.dev())
        || after.st_ino != held.ino()
    {
        return Err(Error::Unavailable);
    }
    Ok(file)
}
fn read_bounded(
    mut file: impl Read,
    cap: usize,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut chunk = [0u8; CHUNK];
    loop {
        check(cancel, deadline)?;
        let size = file.read(&mut chunk).map_err(|_| Error::Unavailable)?;
        if size == 0 {
            return Ok(out);
        }
        if out.len() + size > cap {
            return Err(Error::Limit);
        }
        out.extend_from_slice(&chunk[..size]);
    }
}
fn first_line(
    mut file: File,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        check(cancel, deadline)?;
        let n = file.read(&mut chunk).map_err(|_| Error::Unavailable)?;
        if n == 0 {
            return Err(Error::Unavailable);
        }
        let take = chunk[..n]
            .iter()
            .position(|b| *b == b'\n')
            .map_or(n, |i| i + 1);
        if out.len() + take > HEADER_MAX {
            return Err(Error::Limit);
        }
        out.extend_from_slice(&chunk[..take]);
        if take < n || out.last() == Some(&b'\n') {
            return Ok(out);
        }
    }
}
fn tail_lines(
    file: &mut File,
    cancel: &CancellationToken,
    deadline: Instant,
    mut visit: impl FnMut(&[u8]),
) -> Result<(), Error> {
    let size = file.metadata().map_err(|_| Error::Unavailable)?.len();
    let start = size.saturating_sub(TAIL_MAX as u64);
    file.seek(SeekFrom::Start(start))
        .map_err(|_| Error::Unavailable)?;
    // A live provider can append while we scan. Read the section measured
    // above rather than chasing the growing EOF and discarding all metadata.
    let clone = file.try_clone().map_err(|_| Error::Unavailable)?;
    let mut data = read_bounded(clone.take(size - start), TAIL_MAX, cancel, deadline)?;
    if start > 0 {
        let Some(i) = data.iter().position(|b| *b == b'\n') else {
            return Ok(());
        };
        data.drain(..=i);
    }
    for line in data.split(|b| *b == b'\n') {
        check(cancel, deadline)?;
        if !line.is_empty() && line.len() <= LINE_MAX {
            visit(line);
        }
    }
    Ok(())
}
fn token(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}
fn model(value: &str) -> Option<String> {
    let mut safe = String::new();
    for c in value.chars() {
        let c = if c.is_control()
            || matches!(c as u32, 0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069)
        {
            ' '
        } else {
            c
        };
        if safe.len() + c.len_utf8() > 128 {
            break;
        }
        safe.push(c);
    }
    let safe = safe.trim();
    let bytes = safe.as_bytes();
    if bytes.is_empty()
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'+' | b'-'))
    {
        None
    } else {
        Some(safe.to_owned())
    }
}
fn timestamp(value: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(value).map_or(0, |t| t.timestamp())
}
fn codex_root(path: &Path) -> Option<PathBuf> {
    if !clean_absolute(path) {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
        return None;
    }
    let day = path.parent()?;
    let month = day.parent()?;
    let year = month.parent()?;
    let root = year.parent()?;
    if root.file_name()? != OsStr::new("sessions") {
        return None;
    }
    let date = format!(
        "{}/{}/{}",
        year.file_name()?.to_str()?,
        month.file_name()?.to_str()?,
        day.file_name()?.to_str()?
    );
    if date.len() != 10 || chrono::NaiveDate::parse_from_str(&date, "%Y/%m/%d").is_err() {
        return None;
    }
    Some(root.to_path_buf())
}
fn codex_events(binding: &mut Binding, cancel: &CancellationToken, deadline: Instant) {
    let Ok(mut file) = open_record(&binding.root, &binding.path) else {
        return;
    };
    let _ = tail_lines(&mut file, cancel, deadline, |line| {
        let Ok(event) = serde_json::from_slice::<CodexEvent<'_>>(line) else {
            return;
        };
        let kind = event.kind.as_deref().unwrap_or("");
        let payload = event.payload.as_ref();
        if kind == "turn_context" {
            let m = payload
                .and_then(|p| p.model.as_deref())
                .filter(|s| !s.is_empty())
                .or(event.model.as_deref());
            if let Some(m) = m.and_then(model) {
                binding.model = m;
            }
        }
        let event_type = if kind == "task_started" || kind == "task_complete" {
            kind
        } else {
            payload.and_then(|p| p.kind.as_deref()).unwrap_or("")
        };
        match event_type {
            "task_started" => {
                binding.state = "working".into();
                binding.working_since = timestamp(event.timestamp.as_deref().unwrap_or(""));
            }
            "task_complete" => {
                binding.state = "idle".into();
                binding.working_since = 0;
            }
            _ => {}
        }
    });
}
/// Bind only the exact open descriptors discovered for one Codex PID.
pub fn bind_codex(
    mut base: Binding,
    file_pid: i32,
    paths: &[PathBuf],
    scan_model: bool,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Binding {
    if paths.len() > PATHS_MAX {
        base.status = Status::Ambiguous;
        return base;
    }
    let mut seen = HashSet::new();
    let mut match_one = None;
    let mut unknown = false;
    for path in paths {
        if check(cancel, deadline).is_err() {
            base.status = Status::Unavailable;
            return base;
        }
        if !seen.insert(path) {
            continue;
        }
        let Some(root) = codex_root(path) else {
            continue;
        };
        let Ok(file) = open_record(&root, path) else {
            unknown = true;
            continue;
        };
        let Ok(raw) = first_line(file, cancel, deadline) else {
            unknown = true;
            continue;
        };
        let Some(end) = raw.iter().position(|b| *b == b'\n') else {
            unknown = true;
            continue;
        };
        let Ok(header) = serde_json::from_slice::<CodexHeader<'_>>(&raw[..end]) else {
            unknown = true;
            continue;
        };
        let id = header
            .payload
            .as_ref()
            .and_then(|p| p.id.as_deref())
            .unwrap_or("");
        let filename = path.file_name().and_then(OsStr::to_str).unwrap_or("");
        if header.kind.as_deref() != Some("session_meta")
            || !token(id)
            || !filename.ends_with(&format!("-{id}.jsonl"))
        {
            unknown = true;
            continue;
        }
        let source = header.payload.as_ref().and_then(|p| p.source);
        if source.is_some_and(|source| source.get().starts_with('{')) {
            continue;
        }
        let source = source.and_then(|source| serde_json::from_str::<String>(source.get()).ok());
        if !matches!(source.as_deref(), Some("cli" | "exec" | "vscode")) {
            unknown = true;
            continue;
        }
        if match_one.is_some() {
            unknown = true;
            break;
        }
        let mut b = base.clone();
        b.file_pid = file_pid;
        b.root = root;
        b.path = path.clone();
        b.record_id = id.to_owned();
        b.status = Status::Ready;
        match_one = Some(b);
    }
    if check(cancel, deadline).is_err() {
        base.status = Status::Unavailable;
        return base;
    }
    if unknown {
        base.status = Status::Ambiguous;
        return base;
    }
    let Some(mut bound) = match_one else {
        base.status = Status::Unavailable;
        return base;
    };
    if scan_model {
        codex_events(&mut bound, cancel, deadline);
    }
    if check(cancel, deadline).is_err() {
        base.status = Status::Unavailable;
        return base;
    }
    bound
}
fn entries(
    dir: &File,
    limit: usize,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Vec<String>, Error> {
    let mut reader = Dir::read_from(dir).map_err(|_| Error::Unavailable)?;
    let mut names = Vec::new();
    while let Some(entry) = reader.read() {
        check(cancel, deadline)?;
        let entry = entry.map_err(|_| Error::Unavailable)?;
        let name = entry.file_name().to_str().map_err(|_| Error::Unavailable)?;
        if name == "." || name == ".." {
            continue;
        }
        if name.is_empty() || name.contains('/') {
            return Err(Error::Unavailable);
        }
        names.push(name.to_owned());
        if names.len() > limit {
            return Err(Error::Limit);
        }
    }
    Ok(names)
}
fn claude_roots(
    home: &Path,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Vec<PathBuf>, Error> {
    let mut roots = vec![home.join(".claude")];
    let parent = home.join(".claude-swap-backup/sessions");
    let Ok(dir) = open_dir(&parent) else {
        return Ok(roots);
    };
    for name in entries(&dir, SWAP_ROOTS_MAX, cancel, deadline)? {
        let path = parent.join(name);
        if open_dir(&path).is_ok() {
            roots.push(path);
        }
    }
    Ok(roots)
}
fn claude_state(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "active" | "busy" | "running" | "working" => "working",
        "idle" | "waiting" => "idle",
        _ => "",
    }
}
fn bind_claude_inner(
    home: &Path,
    mut base: Binding,
    scan_model: bool,
    cancel: &CancellationToken,
    deadline: Instant,
    mut before_recheck: impl FnMut(&Path),
) -> Binding {
    if base.provider_pid <= 0 || !clean_absolute(home) {
        return base;
    }
    let roots = match claude_roots(home, cancel, deadline) {
        Ok(roots) => roots,
        Err(Error::Cancelled) => return base,
        Err(_) => {
            base.status = Status::Ambiguous;
            return base;
        }
    };
    let current_root = home.join(".claude");
    let mut found = None;
    let mut uncertain = false;
    for root in roots {
        if check(cancel, deadline).is_err() {
            base.status = Status::Unavailable;
            return base;
        }
        let registry_root = root.join("sessions");
        let registry_path = registry_root.join(format!("{}.json", base.provider_pid));
        let Ok(file) = open_record(&registry_root, &registry_path) else {
            if root == current_root && registry_slot_present(&registry_path) {
                uncertain = true;
            }
            continue;
        };
        let raw = match read_bounded(file, HEADER_MAX, cancel, deadline) {
            Ok(raw) => raw,
            Err(Error::Cancelled) => return base,
            Err(_) => {
                uncertain = true;
                continue;
            }
        };
        let Ok(record) = serde_json::from_slice::<ClaudeRegistry<'_>>(&raw) else {
            if root == current_root {
                uncertain = true;
            }
            continue;
        };
        let id = record.id.as_deref().unwrap_or("");
        if record.pid != Some(i64::from(base.provider_pid)) || !token(id) {
            if root == current_root {
                uncertain = true;
            }
            continue;
        }
        let projects_root = root.join("projects");
        let Ok(projects) = open_dir(&projects_root) else {
            uncertain = true;
            continue;
        };
        let names = match entries(&projects, PROJECTS_MAX, cancel, deadline) {
            Ok(names) => names,
            Err(Error::Cancelled) => return base,
            Err(_) => {
                uncertain = true;
                continue;
            }
        };
        let mut transcript = None;
        for name in names {
            if check(cancel, deadline).is_err() {
                base.status = Status::Unavailable;
                return base;
            }
            let folder = projects_root.join(name);
            if open_dir(&folder).is_err() {
                continue;
            }
            let candidate = folder.join(format!("{id}.jsonl"));
            if open_record(&projects_root, &candidate).is_ok()
                && transcript.replace(candidate).is_some()
            {
                base.status = Status::Ambiguous;
                return base;
            }
        }
        let Some(path) = transcript else {
            if root == current_root {
                uncertain = true;
            }
            continue;
        };
        let mut b = base.clone();
        b.root = projects_root;
        b.path = path;
        b.record_id = id.to_owned();
        b.file_pid = base.provider_pid;
        b.state = claude_state(record.status.as_deref().unwrap_or("")).into();
        if b.state == "working" {
            let updated = record.updated.unwrap_or(0);
            if updated > 0 {
                b.working_since = updated / 1000;
            }
        }
        b.status = Status::Ready;
        if scan_model {
            if let Ok(mut file) = open_record(&b.root, &b.path) {
                let _ = tail_lines(&mut file, cancel, deadline, |line| {
                    if let Ok(event) = serde_json::from_slice::<ClaudeEvent<'_>>(line) {
                        if let Some(m) = event
                            .message
                            .as_ref()
                            .and_then(|message| message.model.as_deref())
                            .and_then(model)
                        {
                            b.model = m;
                        }
                    }
                });
            }
        }
        before_recheck(&registry_path);
        let Ok(again_file) = open_record(&registry_root, &registry_path) else {
            uncertain = true;
            continue;
        };
        let again = match read_bounded(again_file, HEADER_MAX, cancel, deadline) {
            Ok(again) => again,
            Err(Error::Cancelled) => return base,
            Err(_) => {
                uncertain = true;
                continue;
            }
        };
        if raw != again {
            uncertain = true;
            continue;
        }
        if found.replace(b).is_some() {
            base.status = Status::Ambiguous;
            return base;
        }
    }
    if check(cancel, deadline).is_err() {
        base.status = Status::Unavailable;
        return base;
    }
    if uncertain {
        base.status = Status::Ambiguous;
        return base;
    }
    found.unwrap_or(base)
}
/// Bind Claude's exact PID registry to one transcript, then verify registry bytes again.
pub fn bind_claude(
    home: &Path,
    base: Binding,
    scan_model: bool,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Binding {
    bind_claude_inner(home, base, scan_model, cancel, deadline, |_| {})
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
