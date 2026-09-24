//! Bounded, Go-compatible private staging for streamed browser uploads.
//! This synchronous owner runs only on bounded blocking workers. Its `.lock`
//! guard is held through body receipt, durable commit, and completion delivery.
use bytes::Bytes;
use hmux_protocol::protobuf::types::UploadHeader;
use hmux_protocol::{actions, protobuf::types as p};
use rustix::fs::{self, AtFlags, Dir, FlockOperation, Mode, OFlags};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fmt,
    fs::File,
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

const FILE_MAX: i64 = 32 << 20;
const REQUEST_MAX: i64 = 128 << 20;
const SPOOL_MAX: i64 = 512 << 20;
const MANIFEST_RESERVATION: i64 = 65_537;
const MAX_STAGES: usize = 100;
const MAX_SCAN: usize = 1024;
const CHUNK_MAX: usize = 256 << 10;
const WEB_TTL: i64 = 3 * 60 * 60;
const INCOMING_TTL: i64 = 10 * 60;
const POLL: Duration = Duration::from_millis(25);

/// Fixed, redacted categories. Paths and file contents never appear in errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Root,
    Header,
    Lock,
    Quota,
    Unsafe,
    Size,
    State,
    Io,
    Time,
    Cancelled,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Root => "file-stage root unavailable or unsafe",
            Self::Header => "file-stage header invalid",
            Self::Lock => "file-stage spool lock unavailable",
            Self::Quota => "file-stage spool quota exhausted",
            Self::Unsafe => "file-stage spool contains an unsafe entry",
            Self::Size => "file-stage size mismatch",
            Self::State => "file-stage operation unavailable",
            Self::Io => "file-stage I/O failed",
            Self::Time => "file-stage time invalid",
            Self::Cancelled => "file-stage operation cancelled",
        })
    }
}
impl std::error::Error for Error {}

struct Root {
    path: PathBuf,
    dir: File,
}

/// Stable directory-fd owner. Clones share the same private spool directory.
#[derive(Clone)]
pub struct Store(Arc<Root>);

struct CurrentFile {
    file: File,
    hash: Sha256,
    written: i64,
}

pub struct Stage {
    store: Store,
    _lock: File,
    incoming: File,
    name: String,
    final_name: Option<String>,
    header: UploadHeader,
    stage_id: String,
    started_unix: i64,
    cancel: CancellationToken,
    deadline: Instant,
    current: Option<CurrentFile>,
    hashes: Vec<String>,
    total_written: i64,
    accepted: bool,
    failed: bool,
}

#[derive(Serialize)]
struct Response<'a> {
    protocol_version: u32,
    request_id: &'a str,
    stage_id: &'a str,
    session: ResponseSession<'a>,
    expires_at_unix: i64,
    files: Vec<ResponseFile>,
}
#[derive(Serialize)]
struct ResponseSession<'a> {
    id: &'a str,
    created_at: i64,
}
#[derive(Serialize)]
struct ResponseFile {
    index: usize,
    path: String,
    size: i64,
    sha256: String,
}

fn check_work(cancel: &CancellationToken, deadline: Instant) -> Result<(), Error> {
    if cancel.is_cancelled() {
        Err(Error::Cancelled)
    } else if Instant::now() >= deadline {
        Err(Error::Time)
    } else {
        Ok(())
    }
}

fn valid_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_header(h: &UploadHeader) -> Result<(), Error> {
    if h.protocol_version != 1
        || !valid_hex(&h.request_id, 32)
        || h.file_count == 0
        || h.file_count > 16
        || h.file_count as usize != h.files.len()
        || !(1..=REQUEST_MAX).contains(&h.total_bytes)
    {
        return Err(Error::Header);
    }
    let session = h.session.as_ref().ok_or(Error::Header)?;
    let digits = session.id.strip_prefix('$').ok_or(Error::Header)?;
    if session.created_at < 1
        || !(1..=31).contains(&digits.len())
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(Error::Header);
    }
    let mut total = 0i64;
    for (index, file) in h.files.iter().enumerate() {
        if file.index as usize != index
            || !(1..=FILE_MAX).contains(&file.size)
            || file.extension.len() > 16
            || (!file.extension.is_empty()
                && !file
                    .extension
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()))
        {
            return Err(Error::Header);
        }
        total = total.checked_add(file.size).ok_or(Error::Header)?;
    }
    if total != h.total_bytes {
        return Err(Error::Header);
    }
    Ok(())
}

fn checked_root(path: &Path) -> Result<File, Error> {
    let clean: PathBuf = path.components().collect();
    let path_text = path.to_str().ok_or(Error::Root)?;
    if !path.is_absolute()
        || clean.as_os_str() != path.as_os_str()
        || path.file_name() != Some(OsStr::new("staged-files-v1"))
        || path.parent().and_then(Path::file_name) != Some(OsStr::new("hmux"))
        || path_text
            .bytes()
            .any(|byte| matches!(byte, 0 | b'\r' | b'\n' | b'\t' | 0x1b))
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err(Error::Root);
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut dir = File::from(fs::open("/", flags, Mode::empty()).map_err(|_| Error::Root)?);
    let owner = rustix::process::geteuid().as_raw();
    let components: Vec<_> = path.components().collect();
    for (index, part) in components.iter().enumerate() {
        let Component::Normal(name) = part else {
            continue;
        };
        let private = index >= components.len() - 2;
        let next = match fs::openat(&dir, *name, flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => {
                match fs::mkdirat(&dir, *name, Mode::from_raw_mode(0o700)) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(_) => return Err(Error::Root),
                }
                fs::openat(&dir, *name, flags, Mode::empty()).map_err(|_| Error::Root)?
            }
            Err(_) => return Err(Error::Root),
        };
        dir = File::from(next);
        let metadata = dir.metadata().map_err(|_| Error::Root)?;
        if !metadata.is_dir()
            || (private && (metadata.uid() != owner || metadata.mode() & 0o077 != 0))
            || (!private && metadata.uid() != owner && metadata.uid() != 0)
            || (!private
                && metadata.mode() & 0o022 != 0
                && !(metadata.uid() == 0 && metadata.mode() & 0o1000 != 0))
        {
            return Err(Error::Root);
        }
    }
    Ok(dir)
}

fn private_file(file: &File) -> Result<u64, Error> {
    let metadata = file.metadata().map_err(|_| Error::Unsafe)?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
    {
        return Err(Error::Unsafe);
    }
    Ok(metadata.len())
}

fn open_dir(parent: &File, name: &OsStr) -> Result<File, Error> {
    let file = File::from(
        fs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| Error::Unsafe)?,
    );
    let metadata = file.metadata().map_err(|_| Error::Unsafe)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(Error::Unsafe);
    }
    Ok(file)
}

fn entries(dir: &File, limit: usize) -> Result<Vec<String>, Error> {
    let mut reader = Dir::read_from(dir).map_err(|_| Error::Unsafe)?;
    let mut names = Vec::new();
    while let Some(entry) = reader.read() {
        let entry = entry.map_err(|_| Error::Unsafe)?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name = std::str::from_utf8(bytes).map_err(|_| Error::Unsafe)?;
        if name.is_empty() || name.contains('/') || name.contains('\0') {
            return Err(Error::Unsafe);
        }
        names.push(name.to_owned());
        if names.len() > limit {
            return Err(Error::Quota);
        }
    }
    Ok(names)
}

fn incoming_name(value: &str) -> bool {
    value
        .strip_prefix(".incoming-")
        .is_some_and(|id| valid_hex(id, 32))
}
fn committed_name(value: &str) -> Option<i64> {
    let (expiry, id) = value.split_once('-')?;
    if expiry.len() != 10 || !expiry.bytes().all(|byte| byte.is_ascii_digit()) || !valid_hex(id, 32)
    {
        return None;
    }
    expiry.parse().ok()
}
fn staged_file_name(value: &str) -> bool {
    let (stem, extension) = match value.split_once('.') {
        Some((_, "")) => return false,
        Some((stem, extension)) => (stem, extension),
        None => (value, ""),
    };
    let Some(digits) = stem.strip_prefix("file-") else {
        return false;
    };
    digits.len() == 4
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && (extension.is_empty()
            || (extension.len() <= 16
                && extension
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())))
}

fn safe_stage_files(dir: &File) -> Result<(Vec<String>, i64), Error> {
    let names = entries(dir, 17)?;
    let mut size = 0i64;
    for name in &names {
        if name != "manifest.json" && !staged_file_name(name) {
            return Err(Error::Unsafe);
        }
        let file = File::from(
            fs::openat(
                dir,
                name.as_str(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| Error::Unsafe)?,
        );
        let len = private_file(&file)?;
        size = size
            .checked_add(i64::try_from(len).map_err(|_| Error::Quota)?)
            .ok_or(Error::Quota)?;
        if size > SPOOL_MAX {
            return Err(Error::Quota);
        }
    }
    Ok((names, size))
}

fn remove_stage(root: &File, name: &str) -> Result<(), Error> {
    if !incoming_name(name) && committed_name(name).is_none() {
        return Err(Error::Unsafe);
    }
    let child = match open_dir(root, OsStr::new(name)) {
        Ok(child) => child,
        Err(Error::Unsafe)
            if matches!(
                fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW),
                Err(rustix::io::Errno::NOENT)
            ) =>
        {
            return Ok(())
        }
        Err(error) => return Err(error),
    };
    let (names, _) = safe_stage_files(&child)?;
    for file in names {
        fs::unlinkat(&child, file.as_str(), AtFlags::empty()).map_err(|_| Error::Io)?;
    }
    drop(child);
    fs::unlinkat(root, name, AtFlags::REMOVEDIR).map_err(|_| Error::Io)
}

fn modified_unix(file: &File) -> Result<i64, Error> {
    file.metadata()
        .map_err(|_| Error::Unsafe)?
        .modified()
        .map_err(|_| Error::Unsafe)?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Unsafe)
        .and_then(|value| i64::try_from(value.as_secs()).map_err(|_| Error::Unsafe))
}

fn sweep_locked(
    root: &Root,
    now: i64,
    reserve: i64,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<(), Error> {
    if now < 1 || !(0..=SPOOL_MAX).contains(&reserve) {
        return Err(Error::Time);
    }
    let mut used = 0i64;
    let mut stages = 0usize;
    let mut expired_names = Vec::new();
    for name in entries(&root.dir, MAX_SCAN)? {
        check_work(cancel, deadline)?;
        if name == ".lock" {
            continue;
        }
        let child = open_dir(&root.dir, OsStr::new(&name))?;
        let expired = if incoming_name(&name) {
            now.saturating_sub(modified_unix(&child)?) >= INCOMING_TTL
        } else if let Some(expires) = committed_name(&name) {
            expires <= now
        } else {
            return Err(Error::Unsafe);
        };
        let (_, size) = safe_stage_files(&child)?;
        if expired {
            expired_names.push(name);
            continue;
        }
        stages += 1;
        used = used.checked_add(size).ok_or(Error::Quota)?;
        if used > SPOOL_MAX {
            return Err(Error::Quota);
        }
    }
    // Finish validating the complete bounded root before deleting any entry.
    for name in expired_names {
        check_work(cancel, deadline)?;
        remove_stage(&root.dir, &name)?;
    }
    if reserve > SPOOL_MAX - used || (reserve > 0 && stages >= MAX_STAGES) {
        return Err(Error::Quota);
    }
    Ok(())
}

fn root_lock(root: &File, cancel: &CancellationToken, deadline: Instant) -> Result<File, Error> {
    let file = File::from(
        fs::openat(
            root,
            ".lock",
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| Error::Lock)?,
    );
    private_file(&file).map_err(|_| Error::Lock)?;
    loop {
        check_work(cancel, deadline)?;
        match fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(file),
            Err(rustix::io::Errno::INTR) => continue,
            Err(rustix::io::Errno::WOULDBLOCK) => {
                thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
            }
            Err(_) => return Err(Error::Lock),
        }
    }
}

fn file_name(header: &UploadHeader, index: usize) -> String {
    let ext = &header.files[index].extension;
    if ext.is_empty() {
        format!("file-{:04}", index + 1)
    } else {
        format!("file-{:04}.{ext}", index + 1)
    }
}

impl Store {
    pub fn open(root: PathBuf) -> Result<Self, Error> {
        let dir = checked_root(&root)?;
        Ok(Self(Arc::new(Root { path: root, dir })))
    }

    pub fn begin(
        &self,
        header: UploadHeader,
        started_unix: i64,
        cancel: CancellationToken,
        deadline: Instant,
    ) -> Result<Stage, Error> {
        validate_header(&header)?;
        if started_unix < 1 {
            return Err(Error::Time);
        }
        check_work(&cancel, deadline)?;
        let lock = root_lock(&self.0.dir, &cancel, deadline)?;
        sweep_locked(
            &self.0,
            started_unix,
            header.total_bytes + MANIFEST_RESERVATION,
            &cancel,
            deadline,
        )?;
        check_work(&cancel, deadline)?;
        let mut entropy = [0u8; 16];
        getrandom::fill(&mut entropy).map_err(|_| Error::Io)?;
        let stage_id = entropy
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let name = format!(".incoming-{stage_id}");
        fs::mkdirat(&self.0.dir, name.as_str(), Mode::from_raw_mode(0o700))
            .map_err(|_| Error::Io)?;
        let incoming = match open_dir(&self.0.dir, OsStr::new(&name)) {
            Ok(dir) => dir,
            Err(error) => {
                let _ = remove_stage(&self.0.dir, &name);
                return Err(error);
            }
        };
        Ok(Stage {
            store: self.clone(),
            _lock: lock,
            incoming,
            name,
            final_name: None,
            header,
            stage_id,
            started_unix,
            cancel,
            deadline,
            current: None,
            hashes: Vec::new(),
            total_written: 0,
            accepted: false,
            failed: false,
        })
    }

    pub fn sweep(
        &self,
        now_unix: i64,
        cancel: CancellationToken,
        deadline: Instant,
    ) -> Result<(), Error> {
        let _lock = root_lock(&self.0.dir, &cancel, deadline)?;
        sweep_locked(&self.0, now_unix, 0, &cancel, deadline)
    }
}

impl Stage {
    fn check(&self) -> Result<(), Error> {
        if self.failed || self.final_name.is_some() {
            return Err(Error::State);
        }
        check_work(&self.cancel, self.deadline)
    }

    pub fn write(&mut self, data: &[u8]) -> Result<i64, Error> {
        self.check()?;
        if data.len() > CHUNK_MAX
            || self
                .total_written
                .checked_add(data.len() as i64)
                .is_none_or(|total| total > self.header.total_bytes)
        {
            self.failed = true;
            return Err(Error::Size);
        }
        let result = self.write_inner(data);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn write_inner(&mut self, mut data: &[u8]) -> Result<i64, Error> {
        while !data.is_empty() {
            self.check()?;
            let index = self.hashes.len();
            let expected = self.header.files.get(index).ok_or(Error::Size)?.size;
            if self.current.is_none() {
                let name = file_name(&self.header, index);
                let file = File::from(
                    fs::openat(
                        &self.incoming,
                        name.as_str(),
                        OFlags::WRONLY
                            | OFlags::CREATE
                            | OFlags::EXCL
                            | OFlags::NOFOLLOW
                            | OFlags::CLOEXEC,
                        Mode::from_raw_mode(0o600),
                    )
                    .map_err(|_| Error::Io)?,
                );
                self.current = Some(CurrentFile {
                    file,
                    hash: Sha256::new(),
                    written: 0,
                });
            }
            let current = self.current.as_mut().ok_or(Error::State)?;
            let amount = usize::try_from(expected - current.written)
                .map_err(|_| Error::Size)?
                .min(data.len());
            current
                .file
                .write_all(&data[..amount])
                .map_err(|_| Error::Io)?;
            current.hash.update(&data[..amount]);
            current.written += amount as i64;
            self.total_written += amount as i64;
            data = &data[amount..];
            if current.written == expected {
                let finished = self.current.take().ok_or(Error::State)?;
                finished.file.sync_all().map_err(|_| Error::Io)?;
                let digest = finished.hash.finalize();
                self.hashes
                    .push(digest.iter().map(|byte| format!("{byte:02x}")).collect());
            }
        }
        Ok(self.total_written)
    }

    pub fn commit(&mut self, completed_unix: i64) -> Result<Bytes, Error> {
        let result = self.commit_typed(completed_unix)?;
        let raw = actions::response_payload(&p::Response {
            id: String::new(),
            error: String::new(),
            result: Some(p::response::Result::Staged(Box::new(result))),
        })
        .map_err(|_| Error::Io)?;
        let mut compact = raw.to_vec();
        compact.push(b'\n');
        Ok(Bytes::from(compact))
    }
    pub fn commit_typed(&mut self, completed_unix: i64) -> Result<p::StageResult, Error> {
        self.check()?;
        if self.total_written != self.header.total_bytes
            || self.current.is_some()
            || self.hashes.len() != self.header.files.len()
        {
            self.failed = true;
            return Err(Error::Size);
        }
        if completed_unix < self.started_unix {
            self.failed = true;
            return Err(Error::Time);
        }
        let expiry = completed_unix.checked_add(WEB_TTL).ok_or(Error::Time)?;
        if !(1_000_000_000..=9_999_999_999).contains(&expiry) {
            self.failed = true;
            return Err(Error::Time);
        }
        let final_name = format!("{expiry}-{id}", id = self.stage_id);
        let session = self.header.session.as_ref().ok_or(Error::Header)?;
        let mut files = Vec::with_capacity(self.hashes.len());
        for index in 0..self.hashes.len() {
            let path = self
                .store
                .0
                .path
                .join(&final_name)
                .join(file_name(&self.header, index));
            let path = path.to_str().ok_or(Error::Root)?;
            if path.len() > 4096
                || path
                    .bytes()
                    .any(|byte| matches!(byte, 0 | b'\r' | b'\n' | b'\t' | 0x1b))
            {
                self.failed = true;
                return Err(Error::Root);
            }
            files.push(ResponseFile {
                index,
                path: path.into(),
                size: self.header.files[index].size,
                sha256: self.hashes[index].clone(),
            });
        }
        let response = Response {
            protocol_version: 1,
            request_id: &self.header.request_id,
            stage_id: &self.stage_id,
            session: ResponseSession {
                id: &session.id,
                created_at: session.created_at,
            },
            expires_at_unix: expiry,
            files,
        };
        struct Count(usize);
        impl Write for Count {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self
                    .0
                    .checked_add(bytes.len())
                    .filter(|n| *n <= 65_536)
                    .ok_or_else(|| std::io::Error::other("manifest size"))?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut count = Count(1);
        serde_json::to_writer(&mut count, &response).map_err(|_| Error::Size)?;
        let compact_len = count.0;
        if compact_len > 65_536 {
            self.failed = true;
            return Err(Error::Size);
        }
        let mut manifest = serde_json::to_vec_pretty(&response).map_err(|_| Error::Io)?;
        manifest.push(b'\n');
        if manifest.len() > 65_536 {
            self.failed = true;
            return Err(Error::Size);
        }
        let mut file = File::from(
            fs::openat(
                &self.incoming,
                "manifest.json",
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|_| Error::Io)?,
        );
        file.write_all(&manifest).map_err(|_| Error::Io)?;
        file.sync_all().map_err(|_| Error::Io)?;
        drop(file);
        self.incoming.sync_all().map_err(|_| Error::Io)?;
        check_work(&self.cancel, self.deadline)?;
        fs::renameat(
            &self.store.0.dir,
            self.name.as_str(),
            &self.store.0.dir,
            final_name.as_str(),
        )
        .map_err(|_| Error::Io)?;
        self.final_name = Some(final_name);
        self.store.0.dir.sync_all().map_err(|_| Error::Io)?;
        Ok(p::StageResult {
            protocol_version: 1,
            request_id: self.header.request_id.clone(),
            stage_id: self.stage_id.clone(),
            session: Some(p::Session {
                id: session.id.clone(),
                created_at: session.created_at,
            }),
            expires_at_unix: expiry,
            files: response
                .files
                .into_iter()
                .map(|v| p::StageFile {
                    index: v.index as u32,
                    path: v.path,
                    size: v.size,
                    sha256: v.sha256,
                })
                .collect(),
        })
    }

    /// Call only after the completion bytes have been delivered successfully.
    pub fn accept(mut self) {
        if self.final_name.is_some() {
            self.accepted = true;
        }
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if self.accepted {
            return;
        }
        self.current.take();
        let name = self.final_name.as_deref().unwrap_or(&self.name);
        let _ = remove_stage(&self.store.0.dir, name);
    }
}

#[cfg(test)]
#[path = "filestage_tests.rs"]
mod tests;
