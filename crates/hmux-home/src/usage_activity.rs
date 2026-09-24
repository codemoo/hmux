//! One persistent, bounded owner of local JSONL activity. Paths and transcript
//! bytes never enter diagnostics, and synchronous I/O stays in one admitted worker.
use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Utc};
use hmux_usage::{
    activity::{self, ActivitySnapshot, ActivitySource, Tracker},
    Provider,
};
use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

const MAX_ROOTS: usize = 130;
const MAX_DIRS: usize = 2048;
const MAX_ENTRIES: usize = 65536;
const MAX_FILES: usize = 16384;
const MAX_PATH: usize = activity::MAX_PATH_BYTES;
const MAX_FILE_BYTES: u64 = 16 << 30;
const MAX_TAIL_BYTES: usize = activity::MAX_LINE_BYTES + (64 << 10);
const MAX_BOOTSTRAP_BYTES: usize = 64 << 20;
const MAX_LIVE_BYTES: usize = 16 << 20;
const SCAN_DEADLINE: Duration = Duration::from_secs(4);
const RECONCILE: Duration = Duration::from_secs(300);
static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct Options {
    pub claude_projects_root: PathBuf,
    pub codex_sessions_root: PathBuf,
    pub claude_swap_sessions_root: Option<PathBuf>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Busy,
    Cancelled,
    Worker,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Issue {
    Missing,
    Permission,
    Unsafe,
    Limit,
    Io,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics {
    pub observed: bool,
    pub issue: Option<Issue>,
    pub incomplete: bool,
    pub scanned_files: usize,
    pub rejected_files: usize,
    pub bytes_read: usize,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub claude: ActivitySnapshot,
    pub codex: ActivitySnapshot,
    pub claude_diagnostics: Diagnostics,
    pub codex_diagnostics: Diagnostics,
}
#[derive(Clone)]
pub struct Reader {
    inner: Arc<Mutex<State>>,
}
impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reader([redacted])")
    }
}
struct State {
    options: Options,
    claude: Tracker,
    codex: Tracker,
    roots: BTreeMap<PathBuf, RootState>,
    bootstrapped: bool,
}
#[derive(Default)]
struct RootState {
    offsets: BTreeMap<PathBuf, Offset>,
    hot_dirs: BTreeSet<PathBuf>,
    reconciled: Option<Instant>,
}
#[derive(Clone, Copy)]
struct Offset {
    pos: u64,
    dev: u64,
    ino: u64,
    discarding: bool,
}
#[derive(Clone)]
struct Root {
    path: PathBuf,
    provider: Provider,
    account: i64,
}
#[derive(Clone)]
struct Candidate {
    path: PathBuf,
    len: u64,
    dev: u64,
    ino: u64,
    modified: SystemTime,
}
struct Budget {
    bytes: usize,
    files: usize,
    dirs: usize,
    entries: usize,
    deadline: Instant,
}
impl Budget {
    fn check(&self, cancel: &CancellationToken) -> Result<(), Error> {
        if cancel.is_cancelled() || Instant::now() >= self.deadline {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}
fn valid(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().as_bytes().len() <= MAX_PATH
        && path.components().collect::<PathBuf>() == path
        && !path
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
}
fn flags_dir() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
fn open_dir(path: &Path) -> Result<File, Issue> {
    if !valid(path) {
        return Err(Issue::Unsafe);
    }
    let mut dir = File::from(fs::open("/", flags_dir(), Mode::empty()).map_err(classify)?);
    for part in path.components() {
        if let Component::Normal(name) = part {
            dir = File::from(fs::openat(&dir, name, flags_dir(), Mode::empty()).map_err(classify)?);
        }
    }
    Ok(dir)
}
fn classify(err: rustix::io::Errno) -> Issue {
    match err {
        rustix::io::Errno::NOENT => Issue::Missing,
        rustix::io::Errno::ACCESS | rustix::io::Errno::PERM => Issue::Permission,
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => Issue::Unsafe,
        _ => Issue::Io,
    }
}
fn note(d: &mut Diagnostics, issue: Issue) {
    d.incomplete = true;
    if d.issue.is_none() {
        d.issue = Some(issue);
    }
}
fn list(
    root: &Path,
    dirs: Option<&BTreeSet<PathBuf>>,
    budget: &mut Budget,
    cancel: &CancellationToken,
    d: &mut Diagnostics,
) -> Result<Vec<Candidate>, Error> {
    let mut stack = if let Some(dirs) = dirs {
        dirs.iter().cloned().collect::<Vec<_>>()
    } else {
        vec![root.to_owned()]
    };
    let mut found = Vec::new();
    while let Some(path) = stack.pop() {
        budget.check(cancel)?;
        if budget.dirs >= MAX_DIRS {
            note(d, Issue::Limit);
            break;
        }
        budget.dirs += 1;
        let dir = match open_dir(&path) {
            Ok(dir) => dir,
            Err(e) => {
                note(d, e);
                continue;
            }
        };
        let mut reader = match Dir::read_from(&dir) {
            Ok(r) => r,
            Err(e) => {
                note(d, classify(e));
                continue;
            }
        };
        while let Some(entry) = reader.read() {
            budget.check(cancel)?;
            if budget.entries >= MAX_ENTRIES {
                note(d, Issue::Limit);
                return Ok(found);
            }
            budget.entries += 1;
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    note(d, classify(e));
                    break;
                }
            };
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if name == "." || name == ".." {
                continue;
            }
            if name.as_bytes().is_empty() || name.as_bytes().contains(&b'/') {
                note(d, Issue::Unsafe);
                continue;
            }
            let child = path.join(name);
            if !valid(&child) {
                d.rejected_files += 1;
                continue;
            }
            let stat = match fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(s) => s,
                Err(e) => {
                    note(d, classify(e));
                    continue;
                }
            };
            match FileType::from_raw_mode(stat.st_mode) {
                FileType::Directory if dirs.is_none() => {
                    if stack.len() + budget.dirs >= MAX_DIRS {
                        note(d, Issue::Limit);
                    } else {
                        stack.push(child);
                    }
                }
                FileType::RegularFile if child.extension().is_some_and(|e| e == "jsonl") => {
                    if budget.files >= MAX_FILES {
                        note(d, Issue::Limit);
                        return Ok(found);
                    }
                    if stat.st_uid != rustix::process::geteuid().as_raw()
                        || stat.st_nlink != 1
                        || stat.st_size < 0
                        || stat.st_size as u64 > MAX_FILE_BYTES
                        || stat.st_mode & 0o022 != 0
                    {
                        d.rejected_files += 1;
                        note(d, Issue::Unsafe);
                        continue;
                    }
                    let modified = if stat.st_mtime >= 0 {
                        SystemTime::UNIX_EPOCH
                            .checked_add(Duration::from_secs(stat.st_mtime as u64))
                            .unwrap_or(SystemTime::UNIX_EPOCH)
                    } else {
                        SystemTime::UNIX_EPOCH
                    };
                    found.push(Candidate {
                        path: child,
                        len: stat.st_size as u64,
                        dev: stat.st_dev as u64,
                        ino: stat.st_ino as u64,
                        modified,
                    });
                    budget.files += 1;
                }
                FileType::Symlink => {
                    d.rejected_files += 1;
                    note(d, Issue::Unsafe);
                }
                _ => {}
            }
        }
    }
    Ok(found)
}
fn open_file(root: &Path, c: &Candidate) -> Result<File, Issue> {
    let file = crate::records::open_record(root, &c.path).map_err(|_| Issue::Unsafe)?;
    let meta = file.metadata().map_err(|_| Issue::Io)?;
    if meta.nlink() != 1
        || meta.dev() != c.dev
        || meta.ino() != c.ino
        || meta.len() > MAX_FILE_BYTES
        || meta.mode() & 0o022 != 0
    {
        return Err(Issue::Unsafe);
    }
    Ok(file)
}
fn local_day(time: DateTime<Utc>) -> NaiveDate {
    time.with_timezone(&Local).date_naive()
}
struct Consume<'a> {
    root: &'a Root,
    candidate: &'a Candidate,
    now: DateTime<Utc>,
    tracker: &'a mut Tracker,
    budget: &'a mut Budget,
    cancel: &'a CancellationToken,
    diagnostics: &'a mut Diagnostics,
    skip_first: bool,
    since: Option<NaiveDate>,
}
fn consume(file: &mut File, offset: &mut Offset, args: Consume<'_>) -> Result<bool, Error> {
    let Consume {
        root,
        candidate: c,
        now,
        tracker,
        budget,
        cancel,
        diagnostics: d,
        skip_first,
        since,
    } = args;
    if file.seek(SeekFrom::Start(offset.pos)).is_err() {
        note(d, Issue::Io);
        return Ok(false);
    }
    let path = match c.path.to_str() {
        Some(p) => p,
        None => {
            note(d, Issue::Unsafe);
            return Ok(false);
        }
    };
    let mut line = Vec::with_capacity(4096);
    let mut buf = [0u8; 65536];
    let mut position = offset.pos;
    let mut complete = offset.pos;
    let mut discarding = offset.discarding;
    let mut skip = skip_first;
    let max = MAX_TAIL_BYTES;
    let mut read_total = 0;
    while position < c.len && read_total < max && budget.bytes > 0 {
        budget.check(cancel)?;
        let allowed = buf
            .len()
            .min((c.len - position) as usize)
            .min(max - read_total)
            .min(budget.bytes);
        if allowed == 0 {
            break;
        }
        let n = match file.read(&mut buf[..allowed]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => {
                note(d, Issue::Io);
                break;
            }
        };
        budget.bytes -= n;
        d.bytes_read += n;
        read_total += n;
        for &byte in &buf[..n] {
            position += 1;
            if skip {
                if byte == b'\n' {
                    skip = false;
                    complete = position;
                }
                continue;
            }
            if discarding {
                if byte == b'\n' {
                    discarding = false;
                    complete = position;
                } else {
                    complete = position;
                }
                continue;
            }
            if byte == b'\n' {
                if let Some(mut event) = activity::parse_line(root.provider, &line, path) {
                    let event_day = event
                        .timestamp
                        .map(local_day)
                        .unwrap_or_else(|| local_day(now));
                    if since.is_none_or(|day| event.timestamp.is_some_and(|_| event_day >= day)) {
                        event.account_number = root.account;
                        event.source = Some(ActivitySource::Jsonl);
                        tracker.ingest(&event, now, local_day(now), event_day);
                    }
                }
                line.clear();
                complete = position;
            } else if line.len() < activity::MAX_LINE_BYTES {
                line.push(byte);
            } else {
                line.clear();
                discarding = true;
                complete = position;
            }
        }
        offset.pos = complete;
        offset.discarding = discarding;
    }
    offset.pos = complete;
    offset.discarding = discarding;
    if position < c.len {
        note(d, Issue::Limit);
    }
    Ok(position == c.len && !skip)
}
impl Reader {
    pub fn new(options: Options, now: DateTime<Utc>) -> Result<Self, Error> {
        if !valid(&options.claude_projects_root)
            || !valid(&options.codex_sessions_root)
            || options
                .claude_swap_sessions_root
                .as_ref()
                .is_some_and(|p| !valid(p))
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(State {
                options,
                claude: Tracker::new(now),
                codex: Tracker::new(now),
                roots: BTreeMap::new(),
                bootstrapped: false,
            })),
        })
    }
    pub async fn sample(
        &self,
        now: DateTime<Utc>,
        cancel: &CancellationToken,
    ) -> Result<Sample, Error> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let permit = SLOTS
            .get_or_init(|| Arc::new(Semaphore::new(1)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let inner = self.inner.clone();
        let child = cancel.child_token();
        let _guard = child.clone().drop_guard();
        let (tx, rx) = oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let _permit: OwnedSemaphorePermit = permit;
            let result = inner
                .lock()
                .map_err(|_| Error::Worker)
                .and_then(|mut state| state.sample_sync(now, &child));
            let _ = tx.send(result);
        });
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(SCAN_DEADLINE, rx) => result.map_err(|_| Error::Cancelled)?.map_err(|_| Error::Worker)?,
        }
    }
}
impl State {
    fn discover(
        &self,
        budget: &mut Budget,
        cancel: &CancellationToken,
        d: &mut Diagnostics,
    ) -> Result<Vec<Root>, Error> {
        let mut roots = vec![
            Root {
                path: self.options.claude_projects_root.clone(),
                provider: Provider::Claude,
                account: 0,
            },
            Root {
                path: self.options.codex_sessions_root.clone(),
                provider: Provider::Codex,
                account: 0,
            },
        ];
        let Some(swap) = self.options.claude_swap_sessions_root.as_ref() else {
            return Ok(roots);
        };
        let dir = match open_dir(swap) {
            Ok(v) => v,
            Err(Issue::Missing) => return Ok(roots),
            Err(e) => {
                note(d, e);
                return Ok(roots);
            }
        };
        let mut reader = Dir::read_from(&dir).map_err(|_| Error::Worker)?;
        while let Some(entry) = reader.read() {
            budget.check(cancel)?;
            if roots.len() >= MAX_ROOTS || budget.entries >= MAX_ENTRIES {
                note(d, Issue::Limit);
                break;
            }
            budget.entries += 1;
            let entry = match entry {
                Ok(v) => v,
                Err(e) => {
                    note(d, classify(e));
                    break;
                }
            };
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            let Some((prefix, _)) = name.split_once('-') else {
                continue;
            };
            let Ok(account) = prefix.parse::<i64>() else {
                continue;
            };
            if account <= 0 || !prefix.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let path = swap.join(name).join("projects");
            if open_dir(&path).is_ok() {
                roots.push(Root {
                    path,
                    provider: Provider::Claude,
                    account,
                });
            }
        }
        roots.sort_by(|a, b| a.path.cmp(&b.path));
        roots.dedup_by(|a, b| a.path == b.path && a.provider == b.provider);
        Ok(roots)
    }
    fn sample_sync(
        &mut self,
        now: DateTime<Utc>,
        cancel: &CancellationToken,
    ) -> Result<Sample, Error> {
        let mut budget = Budget {
            bytes: if self.bootstrapped {
                MAX_LIVE_BYTES
            } else {
                MAX_BOOTSTRAP_BYTES
            },
            files: 0,
            dirs: 0,
            entries: 0,
            deadline: Instant::now() + SCAN_DEADLINE,
        };
        let mut cd = Diagnostics {
            observed: true,
            ..Default::default()
        };
        let mut xd = Diagnostics {
            observed: true,
            ..Default::default()
        };
        let roots = self.discover(&mut budget, cancel, &mut cd)?;
        let initial = !self.bootstrapped;
        let day = local_day(now);
        let day_start = Local
            .with_ymd_and_hms(day.year(), day.month(), day.day(), 0, 0, 0)
            .earliest()
            .map(SystemTime::from)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut bootstrap_candidates = Vec::new();
        for root in roots {
            budget.check(cancel)?;
            let d = if root.provider == Provider::Claude {
                &mut cd
            } else {
                &mut xd
            };
            if !self.roots.contains_key(&root.path) && self.roots.len() >= MAX_ROOTS {
                note(d, Issue::Limit);
                continue;
            }
            let mut tracked = self.roots.values().map(|r| r.offsets.len()).sum::<usize>();
            let state = self.roots.entry(root.path.clone()).or_default();
            let full = initial
                || state
                    .reconciled
                    .is_none_or(|last| last.elapsed() >= RECONCILE);
            let files = list(
                &root.path,
                if full { None } else { Some(&state.hot_dirs) },
                &mut budget,
                cancel,
                d,
            )?;
            d.scanned_files += files.len();
            let mut new_hot_dirs = BTreeSet::new();
            if full {
                new_hot_dirs.insert(root.path.clone());
                let cutoff = SystemTime::from(now)
                    .checked_sub(Duration::from_secs(24 * 3600))
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                for candidate in &files {
                    if candidate.modified >= cutoff {
                        new_hot_dirs
                            .insert(candidate.path.parent().unwrap_or(&root.path).to_owned());
                    }
                }
            }
            if initial {
                // Baseline every discovered file before reading any backfill.
                for c in files {
                    if !state.offsets.contains_key(&c.path) {
                        if tracked >= MAX_FILES {
                            note(d, Issue::Limit);
                            break;
                        }
                        tracked += 1;
                    }
                    state.offsets.insert(
                        c.path.clone(),
                        Offset {
                            pos: c.len,
                            dev: c.dev,
                            ino: c.ino,
                            discarding: false,
                        },
                    );
                    if c.len > 0 && c.modified >= day_start {
                        bootstrap_candidates.push((root.clone(), c));
                    }
                }
            } else {
                let mut seen = BTreeSet::new();
                for c in files {
                    budget.check(cancel)?;
                    if !state.offsets.contains_key(&c.path) {
                        if tracked >= MAX_FILES {
                            note(d, Issue::Limit);
                            break;
                        }
                        tracked += 1;
                    }
                    seen.insert(c.path.clone());
                    let offset = state.offsets.entry(c.path.clone()).or_insert(Offset {
                        pos: 0,
                        dev: c.dev,
                        ino: c.ino,
                        discarding: false,
                    });
                    if offset.dev != c.dev || offset.ino != c.ino || c.len < offset.pos {
                        *offset = Offset {
                            pos: 0,
                            dev: c.dev,
                            ino: c.ino,
                            discarding: false,
                        };
                    }
                    if c.len <= offset.pos {
                        continue;
                    }
                    let mut file = match open_file(&root.path, &c) {
                        Ok(f) => f,
                        Err(e) => {
                            note(d, e);
                            continue;
                        }
                    };
                    let tracker = if root.provider == Provider::Claude {
                        &mut self.claude
                    } else {
                        &mut self.codex
                    };
                    let _ = consume(
                        &mut file,
                        offset,
                        Consume {
                            root: &root,
                            candidate: &c,
                            now,
                            tracker,
                            budget: &mut budget,
                            cancel,
                            diagnostics: d,
                            skip_first: false,
                            since: None,
                        },
                    )?;
                }
                if full && !d.incomplete {
                    state.offsets.retain(|p, _| seen.contains(p));
                }
            }
            if full && !d.incomplete {
                state.hot_dirs = new_hot_dirs;
                state.reconciled = Some(Instant::now());
            }
        }
        if initial {
            // No activity is ingested until every reachable root has been baselined.
            // A cancelled bootstrap can therefore safely retry from scratch.
            self.bootstrapped = true;
            bootstrap_candidates.sort_by(|a, b| b.1.modified.cmp(&a.1.modified));
            for (index, (root, c)) in bootstrap_candidates.iter().enumerate() {
                if budget.bytes == 0 {
                    for (pending, _) in &bootstrap_candidates[index..] {
                        note(
                            if pending.provider == Provider::Claude {
                                &mut cd
                            } else {
                                &mut xd
                            },
                            Issue::Limit,
                        );
                    }
                    break;
                }
                let d = if root.provider == Provider::Claude {
                    &mut cd
                } else {
                    &mut xd
                };
                budget.check(cancel)?;
                let mut file = match open_file(&root.path, c) {
                    Ok(f) => f,
                    Err(e) => {
                        note(d, e);
                        continue;
                    }
                };
                let size = c.len.min(MAX_TAIL_BYTES as u64).min(budget.bytes as u64);
                let mut offset = Offset {
                    pos: c.len - size,
                    dev: c.dev,
                    ino: c.ino,
                    discarding: false,
                };
                let tracker = if root.provider == Provider::Claude {
                    &mut self.claude
                } else {
                    &mut self.codex
                };
                let reached_eof = consume(
                    &mut file,
                    &mut offset,
                    Consume {
                        root,
                        candidate: c,
                        now,
                        tracker,
                        budget: &mut budget,
                        cancel,
                        diagnostics: d,
                        skip_first: c.len > size,
                        since: Some(day),
                    },
                )?;
                if reached_eof && (offset.pos < c.len || offset.discarding) {
                    if let Some(stored) = self
                        .roots
                        .get_mut(&root.path)
                        .and_then(|state| state.offsets.get_mut(&c.path))
                    {
                        // Only a fully read tail has a trustworthy partial-line boundary.
                        // Otherwise the EOF baseline prevents replay of omitted history.
                        stored.pos = offset.pos;
                        stored.discarding = offset.discarding;
                    }
                }
            }
        }
        Ok(Sample {
            claude: self.claude.snapshot(now, day),
            codex: self.codex.snapshot(now, day),
            claude_diagnostics: cd,
            codex_diagnostics: xd,
        })
    }
}

#[cfg(test)]
#[path = "usage_activity_tests.rs"]
mod tests;
