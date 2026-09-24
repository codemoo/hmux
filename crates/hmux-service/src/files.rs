//! Directory-relative, streaming service-file publication. No credential content
//! or whole executable is retained in memory. Existing configuration is backed up.
use hmux_core::PrivateDir;
use rustix::fs::{self as unix, AtFlags, Mode, OFlags};
use std::{
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io::{self, Read, Seek, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
const BINARY_LIMIT: u64 = 256 << 20;
static NEXT_PAIR_ID: AtomicU64 = AtomicU64::new(0);
fn bad(message: &str) -> io::Error {
    io::Error::other(message)
}
fn directory(path: &Path, create: bool) -> io::Result<PrivateDir> {
    if create {
        PrivateDir::open_or_create_trusted(path)
    } else {
        PrivateDir::open_existing_trusted(path)
    }
}
fn open(dir: &PrivateDir, name: &OsStr, max: u64, private: bool) -> io::Result<File> {
    let file = File::from(unix::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != rustix::process::getuid().as_raw()
        || meta.nlink() != 1
        || meta.mode() & if private { 0o077 } else { 0o022 } != 0
        || meta.len() > max
    {
        return Err(bad(
            "service file must be a bounded owner-controlled regular file",
        ));
    }
    Ok(file)
}
pub(crate) fn read(path: &Path, max: u64, private: bool) -> io::Result<Vec<u8>> {
    let dir = directory(path.parent().ok_or_else(|| bad("invalid path"))?, false)?;
    let file = open(
        &dir,
        path.file_name().ok_or_else(|| bad("invalid path"))?,
        max,
        private,
    )?;
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(bad("service file exceeds byte limit"));
    }
    Ok(bytes)
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    (
        a.dev(),
        a.ino(),
        a.mode(),
        a.len(),
        a.mtime(),
        a.mtime_nsec(),
        a.ctime(),
        a.ctime_nsec(),
    ) == (
        b.dev(),
        b.ino(),
        b.mode(),
        b.len(),
        b.mtime(),
        b.mtime_nsec(),
        b.ctime(),
        b.ctime_nsec(),
    )
}
fn sync(dir: &PrivateDir) -> io::Result<()> {
    unix::fsync(dir).map_err(Into::into)
}
fn create(dir: &PrivateDir, name: &OsStr) -> io::Result<File> {
    Ok(File::from(unix::openat(
        dir,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o600),
    )?))
}
fn stamp() -> io::Result<String> {
    Ok(format!(
        "{}-{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| bad("invalid clock"))?
            .as_nanos(),
        std::process::id(),
        NEXT_PAIR_ID.fetch_add(1, Ordering::Relaxed)
    ))
}
fn suffixed(name: &OsStr, suffix: &str) -> OsString {
    let mut value = name.to_os_string();
    value.push(suffix);
    value
}
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component, PathBuf};

const PAIR_JOURNAL: &str = ".hmux-service-pair.journal";
const PAIR_JOURNAL_TEMP: &str = ".hmux-service-pair.journal.tmp";
const PAIR_RETAINED: &str = ".hmux-service-pair.retained";
const PAIR_RETAINED_TEMP: &str = ".hmux-service-pair.retained.tmp";
const PAIR_META_LIMIT: u64 = 16 << 10;
const SERVICE_LIMIT: u64 = 64 << 10;

#[derive(Clone, Serialize, Deserialize)]
struct PairImage {
    dev: u64,
    ino: u64,
    mode: u32,
    len: u64,
    hash: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct BackupRef {
    target: String,
    id: String,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Retained {
    service_name: String,
    service: Option<BackupRef>,
    binary: Option<BackupRef>,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum PairPhase {
    Preparing,
    Ready,
    Committed,
}
#[derive(Serialize, Deserialize)]
struct PairJournal {
    version: u8,
    phase: PairPhase,
    id: String,
    service_name: String,
    binary_path: String,
    skip_binary: bool,
    old_service: Option<PairImage>,
    new_service: Option<PairImage>,
    old_binary: Option<PairImage>,
    new_binary: Option<PairImage>,
    previous: Option<Retained>,
}
fn pair_id(value: &str) -> bool {
    let parts: Vec<_> = value.split('-').collect();
    (parts.len() == 2 || parts.len() == 3)
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|x| x.is_ascii_digit()))
}
fn pair_path(value: &str) -> io::Result<PathBuf> {
    let path = PathBuf::from(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        || path.file_name().is_none()
    {
        return Err(bad("invalid paired service path"));
    }
    Ok(path)
}
fn pair_text(path: &Path) -> io::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| bad("paired service path must be UTF-8"))
}
fn pair_name(name: &OsStr, kind: &str, id: &str) -> OsString {
    suffixed(name, &format!(".hmux-pair-{kind}-{id}"))
}
fn pair_open(
    dir: &PrivateDir,
    name: &OsStr,
    max: u64,
    private: bool,
) -> io::Result<Option<(File, Metadata)>> {
    match open(dir, name, max, private) {
        Ok(file) => {
            let meta = file.metadata()?;
            Ok(Some((file, meta)))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
fn pair_hash(file: &mut File, len: u64) -> io::Result<String> {
    if len == 0 || len > BINARY_LIMIT {
        return Err(bad("paired file size is invalid"));
    }
    file.rewind()?;
    let mut digest = Sha256::new();
    let mut left = len;
    let mut buf = [0u8; 64 * 1024];
    while left > 0 {
        let want = buf.len().min(left as usize);
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            return Err(bad("paired file shrank during hash"));
        }
        digest.update(&buf[..n]);
        left -= n as u64;
    }
    let mut extra = [0u8; 1];
    if file.read(&mut extra)? != 0 || file.metadata()?.len() != len {
        return Err(bad("paired file grew during hash"));
    }
    file.rewind()?;
    Ok(format!("{:x}", digest.finalize()))
}
fn pair_image(file: &mut File, meta: &Metadata) -> io::Result<PairImage> {
    Ok(PairImage {
        dev: meta.dev(),
        ino: meta.ino(),
        mode: meta.mode() & 0o7777,
        len: meta.len(),
        hash: pair_hash(file, meta.len())?,
    })
}
fn pair_matches(
    dir: &PrivateDir,
    name: &OsStr,
    expected: &PairImage,
    inode: bool,
) -> io::Result<bool> {
    let Some((mut file, meta)) = pair_open(dir, name, BINARY_LIMIT, false)? else {
        return Ok(false);
    };
    if meta.len() != expected.len
        || meta.mode() & 0o7777 != expected.mode
        || inode && (meta.dev() != expected.dev || meta.ino() != expected.ino)
    {
        return Ok(false);
    }
    Ok(pair_hash(&mut file, expected.len)? == expected.hash)
}
fn pair_absent(dir: &PrivateDir, name: &OsStr) -> io::Result<bool> {
    Ok(pair_open(dir, name, BINARY_LIMIT, false)?.is_none())
}
fn pair_original(
    dir: &PrivateDir,
    name: &OsStr,
    old: &Option<PairImage>,
    inode: bool,
) -> io::Result<bool> {
    match old {
        Some(image) => pair_matches(dir, name, image, inode),
        None => pair_absent(dir, name),
    }
}
fn pair_copy(input: &mut File, output: &mut File, len: u64) -> io::Result<String> {
    if len == 0 || len > BINARY_LIMIT {
        return Err(bad("paired source size is invalid"));
    }
    input.rewind()?;
    let mut digest = Sha256::new();
    let mut left = len;
    let mut buf = [0u8; 64 * 1024];
    while left > 0 {
        let want = buf.len().min(left as usize);
        let n = input.read(&mut buf[..want])?;
        if n == 0 {
            return Err(bad("paired source shrank during copy"));
        }
        output.write_all(&buf[..n])?;
        digest.update(&buf[..n]);
        left -= n as u64;
    }
    let mut extra = [0u8; 1];
    if input.read(&mut extra)? != 0 || input.metadata()?.len() != len {
        return Err(bad("paired source grew during copy"));
    }
    output.sync_all()?;
    input.rewind()?;
    Ok(format!("{:x}", digest.finalize()))
}
fn pair_remove(dir: &PrivateDir, name: &OsStr, max: u64) -> io::Result<()> {
    if pair_open(dir, name, max, false)?.is_some() {
        unix::unlinkat(dir, name, AtFlags::empty())?;
    }
    Ok(())
}
fn pair_read_meta<T: for<'a> Deserialize<'a>>(
    dir: &PrivateDir,
    name: &OsStr,
) -> io::Result<Option<T>> {
    let Some((mut file, meta)) = pair_open(dir, name, PAIR_META_LIMIT, true)? else {
        return Ok(None);
    };
    let mut bytes = Vec::with_capacity((meta.len() + 1) as usize);
    (&mut file)
        .take(PAIR_META_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > PAIR_META_LIMIT || file.metadata()?.len() != meta.len() {
        return Err(bad("paired metadata grew during read"));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| bad("invalid paired metadata"))
}
fn pair_write_meta<T: Serialize>(
    dir: &PrivateDir,
    temp: &str,
    final_name: &str,
    data: &T,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(data).map_err(|_| bad("invalid paired metadata"))?;
    if bytes.len() as u64 > PAIR_META_LIMIT {
        return Err(bad("paired metadata too large"));
    }
    pair_remove(dir, OsStr::new(temp), PAIR_META_LIMIT)?;
    let mut file = create(dir, OsStr::new(temp))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    unix::renameat(dir, OsStr::new(temp), dir, OsStr::new(final_name))?;
    sync(dir)
}
fn validate_backup_ref(value: &BackupRef) -> io::Result<()> {
    let path = pair_path(&value.target)?;
    if path.file_name() != Some(OsStr::new("hmux-web")) || !pair_id(&value.id) {
        return Err(bad("invalid retained backup"));
    }
    Ok(())
}
fn validate_retained(value: &Retained, service_name: &str) -> io::Result<()> {
    if value.service_name != service_name {
        return Err(bad("retained service name changed"));
    }
    if let Some(reference) = &value.service {
        if !pair_id(&reference.id)
            || pair_path(&reference.target)?.file_name() != Some(OsStr::new(service_name))
        {
            return Err(bad("invalid retained service backup"));
        }
    }
    if let Some(reference) = &value.binary {
        validate_backup_ref(reference)?;
    }
    Ok(())
}
fn validate_pair(journal: &PairJournal, service_name: &str) -> io::Result<()> {
    if journal.version != 1
        || journal.service_name != service_name
        || !pair_id(&journal.id)
        || pair_path(&journal.binary_path)?.file_name() != Some(OsStr::new("hmux-web"))
        || (journal.phase != PairPhase::Preparing
            && (journal.new_service.is_none() || journal.new_binary.is_none()))
        || (journal.skip_binary && journal.old_binary.is_none())
    {
        return Err(bad("invalid paired publication journal"));
    }
    if let Some(previous) = &journal.previous {
        validate_retained(previous, service_name)?;
    }
    Ok(())
}
fn pair_retained(dir: &PrivateDir, service_name: &str) -> io::Result<Option<Retained>> {
    let value: Option<Retained> = pair_read_meta(dir, OsStr::new(PAIR_RETAINED))?;
    if let Some(value) = &value {
        validate_retained(value, service_name)?;
    }
    Ok(value)
}
fn pair_journal(dir: &PrivateDir, service_name: &str) -> io::Result<Option<PairJournal>> {
    let value: Option<PairJournal> = pair_read_meta(dir, OsStr::new(PAIR_JOURNAL))?;
    if let Some(value) = &value {
        validate_pair(value, service_name)?;
    }
    Ok(value)
}
fn pair_backup(dir: &PrivateDir, name: &OsStr, old: &PairImage, id: &str) -> io::Result<()> {
    if !pair_matches(dir, name, old, true)? {
        return Err(bad("paired target changed before backup"));
    }
    let (mut input, _) = pair_open(dir, name, BINARY_LIMIT, false)?
        .ok_or_else(|| bad("paired target disappeared"))?;
    let backup = pair_name(name, "backup", id);
    let mut output = create(dir, &backup)?;
    if pair_copy(&mut input, &mut output, old.len)? != old.hash {
        return Err(bad("paired target changed during backup"));
    }
    output.set_permissions(std::fs::Permissions::from_mode(old.mode))?;
    output.sync_all()?;
    sync(dir)
}
fn pair_cleanup(
    dir: &PrivateDir,
    name: &OsStr,
    id: &str,
    backup: bool,
    max: u64,
) -> io::Result<()> {
    pair_remove(dir, &pair_name(name, "stage", id), max)?;
    pair_remove(dir, &pair_name(name, "restore", id), max)?;
    if backup {
        pair_remove(dir, &pair_name(name, "backup", id), max)?;
    }
    sync(dir)
}
fn pair_restore(
    dir: &PrivateDir,
    name: &OsStr,
    old: &Option<PairImage>,
    new: &PairImage,
    id: &str,
    max: u64,
) -> io::Result<()> {
    if pair_original(dir, name, old, false)? {
        return Ok(());
    }
    if !pair_matches(dir, name, new, true)? {
        return Err(bad("paired target changed; refusing rollback"));
    }
    if let Some(old) = old {
        let backup = pair_name(name, "backup", id);
        if !pair_matches(dir, &backup, old, false)? {
            return Err(bad("paired backup changed; refusing rollback"));
        }
        let restore = pair_name(name, "restore", id);
        pair_remove(dir, &restore, max)?;
        let (mut input, _) =
            pair_open(dir, &backup, max, false)?.ok_or_else(|| bad("paired backup disappeared"))?;
        let mut output = create(dir, &restore)?;
        if pair_copy(&mut input, &mut output, old.len)? != old.hash {
            return Err(bad("paired backup changed during restore"));
        }
        output.set_permissions(std::fs::Permissions::from_mode(old.mode))?;
        output.sync_all()?;
        sync(dir)?;
        if !pair_matches(dir, name, new, true)? {
            return Err(bad("paired target changed before restore"));
        }
        unix::renameat(dir, &restore, dir, name)?;
    } else {
        if !pair_matches(dir, name, new, true)? {
            return Err(bad("paired target changed before removal"));
        }
        unix::unlinkat(dir, name, AtFlags::empty())?;
    }
    sync(dir)
}
fn pair_backup_ref(target: &Path, id: &str) -> io::Result<BackupRef> {
    Ok(BackupRef {
        target: pair_text(target)?,
        id: id.to_owned(),
    })
}
fn pair_remove_ref(reference: &BackupRef, max: u64) -> io::Result<()> {
    let target = pair_path(&reference.target)?;
    let dir = directory(
        target
            .parent()
            .ok_or_else(|| bad("invalid retained target"))?,
        false,
    )?;
    let name = target
        .file_name()
        .ok_or_else(|| bad("invalid retained target"))?;
    pair_remove(&dir, &pair_name(name, "backup", &reference.id), max)?;
    sync(&dir)
}
fn pair_commit(
    service_path: &Path,
    service_dir: &PrivateDir,
    binary_path: &Path,
    binary_dir: &PrivateDir,
    journal: &PairJournal,
) -> io::Result<()> {
    let service_name = service_path
        .file_name()
        .ok_or_else(|| bad("invalid service path"))?;
    let binary_name = binary_path
        .file_name()
        .ok_or_else(|| bad("invalid binary path"))?;
    let new_service = journal
        .new_service
        .as_ref()
        .ok_or_else(|| bad("missing staged service"))?;
    let new_binary = journal
        .new_binary
        .as_ref()
        .ok_or_else(|| bad("missing staged binary"))?;
    if !pair_matches(service_dir, service_name, new_service, true)?
        || !pair_matches(binary_dir, binary_name, new_binary, true)?
    {
        return Err(bad("paired targets changed after commit"));
    }
    if let Some(old) = &journal.old_service {
        if !pair_matches(
            service_dir,
            &pair_name(service_name, "backup", &journal.id),
            old,
            false,
        )? {
            return Err(bad("service backup changed"));
        }
    }
    if !journal.skip_binary {
        if let Some(old) = &journal.old_binary {
            if !pair_matches(
                binary_dir,
                &pair_name(binary_name, "backup", &journal.id),
                old,
                false,
            )? {
                return Err(bad("binary backup changed"));
            }
        }
    }
    let mut current = journal.previous.clone().unwrap_or(Retained {
        service_name: journal.service_name.clone(),
        service: None,
        binary: None,
    });
    if journal.old_service.is_some() {
        current.service = Some(pair_backup_ref(service_path, &journal.id)?);
    }
    if !journal.skip_binary && journal.old_binary.is_some() {
        current.binary = Some(pair_backup_ref(binary_path, &journal.id)?);
    }
    let observed = pair_retained(service_dir, &journal.service_name)?;
    let previous = journal.previous.clone();
    if observed != previous && observed.as_ref() != Some(&current) {
        return Err(bad("retained backup pointer changed"));
    }
    if observed.as_ref() != Some(&current) {
        pair_write_meta(service_dir, PAIR_RETAINED_TEMP, PAIR_RETAINED, &current)?;
    }
    if let Some(previous) = &previous {
        if previous.service != current.service {
            if let Some(reference) = &previous.service {
                pair_remove_ref(reference, SERVICE_LIMIT)?;
            }
        }
        if previous.binary != current.binary {
            if let Some(reference) = &previous.binary {
                pair_remove_ref(reference, BINARY_LIMIT)?;
            }
        }
    }
    pair_cleanup(service_dir, service_name, &journal.id, false, SERVICE_LIMIT)?;
    pair_cleanup(binary_dir, binary_name, &journal.id, false, BINARY_LIMIT)?;
    unix::unlinkat(service_dir, OsStr::new(PAIR_JOURNAL), AtFlags::empty())?;
    sync(service_dir)
}

const REMOVE_JOURNAL: &str = ".hmux-service-remove.journal";
const REMOVE_TEMP: &str = ".hmux-service-remove.tmp";
#[derive(Serialize, Deserialize)]
struct RemoveJournal {
    version: u8,
    service_name: String,
    id: String,
    old: PairImage,
    previous: Option<Retained>,
}
fn recover_remove(path: &Path, dir: &PrivateDir, service_name: &str) -> io::Result<()> {
    pair_remove(dir, OsStr::new(REMOVE_TEMP), PAIR_META_LIMIT)?;
    let Some(journal): Option<RemoveJournal> = pair_read_meta(dir, OsStr::new(REMOVE_JOURNAL))?
    else {
        return Ok(());
    };
    if journal.version != 1
        || journal.service_name != service_name
        || !pair_id(&journal.id)
        || journal.old.len > SERVICE_LIMIT
    {
        return Err(bad("invalid service removal journal"));
    }
    if let Some(previous) = &journal.previous {
        validate_retained(previous, service_name)?;
    }
    let name = OsStr::new(service_name);
    let backup = pair_name(name, "backup", &journal.id);
    if pair_matches(dir, name, &journal.old, true)? {
        if !pair_absent(dir, &backup)? {
            return Err(bad("removal backup already exists"));
        }
        unix::renameat(dir, name, dir, &backup)?;
        sync(dir)?;
    } else if !pair_absent(dir, name)? {
        return Err(bad("service changed during removal"));
    }
    if !pair_matches(dir, &backup, &journal.old, true)? {
        return Err(bad("service removal backup changed"));
    }
    let mut current = journal.previous.clone().unwrap_or(Retained {
        service_name: service_name.into(),
        service: None,
        binary: None,
    });
    current.service = Some(pair_backup_ref(path, &journal.id)?);
    let observed = pair_retained(dir, service_name)?;
    if observed != journal.previous && observed.as_ref() != Some(&current) {
        return Err(bad("retained backup pointer changed"));
    }
    if observed.as_ref() != Some(&current) {
        pair_write_meta(dir, PAIR_RETAINED_TEMP, PAIR_RETAINED, &current)?;
    }
    if let Some(previous) = &journal.previous {
        if let Some(reference) = &previous.service {
            if current.service.as_ref() != Some(reference) {
                pair_remove_ref(reference, SERVICE_LIMIT)?;
            }
        }
    }
    unix::unlinkat(dir, OsStr::new(REMOVE_JOURNAL), AtFlags::empty())?;
    sync(dir)
}
/// Remove only the service definition, retaining one timestamped backup and
/// preserving the binary backup. A crash resumes the recorded removal on retry.
pub(crate) fn remove_backed_up(path: &Path) -> io::Result<()> {
    recover_pair(path)?;
    let dir = directory(
        path.parent().ok_or_else(|| bad("invalid service path"))?,
        false,
    )?;
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| bad("invalid service name"))?;
    let (mut file, meta) = pair_open(&dir, OsStr::new(name), SERVICE_LIMIT, true)?
        .ok_or_else(|| bad("service definition missing"))?;
    let journal = RemoveJournal {
        version: 1,
        service_name: name.into(),
        id: stamp()?,
        old: pair_image(&mut file, &meta)?,
        previous: pair_retained(&dir, name)?,
    };
    pair_write_meta(&dir, REMOVE_TEMP, REMOVE_JOURNAL, &journal)?;
    recover_remove(path, &dir, name)
}

/// Recover or finalize an interrupted two-file service publication. Call under
/// the service change lock before every lifecycle action.
pub(crate) fn recover_pair(service_path: &Path) -> io::Result<()> {
    let service_name = service_path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| bad("invalid service name"))?;
    let service_dir = directory(
        service_path
            .parent()
            .ok_or_else(|| bad("invalid service path"))?,
        false,
    )?;
    pair_remove(&service_dir, OsStr::new(PAIR_JOURNAL_TEMP), PAIR_META_LIMIT)?;
    pair_remove(
        &service_dir,
        OsStr::new(PAIR_RETAINED_TEMP),
        PAIR_META_LIMIT,
    )?;
    let journal = pair_journal(&service_dir, service_name)?;
    if journal.is_none() {
        return recover_remove(service_path, &service_dir, service_name);
    }
    if pair_read_meta::<RemoveJournal>(&service_dir, OsStr::new(REMOVE_JOURNAL))?.is_some() {
        return Err(bad("conflicting service publication and removal journals"));
    }
    let journal = journal.unwrap();
    let binary_path = pair_path(&journal.binary_path)?;
    let binary_name = binary_path
        .file_name()
        .ok_or_else(|| bad("invalid binary path"))?;
    let binary_dir = directory(
        binary_path
            .parent()
            .ok_or_else(|| bad("invalid binary path"))?,
        false,
    )?;
    if journal.phase == PairPhase::Committed {
        return pair_commit(
            service_path,
            &service_dir,
            &binary_path,
            &binary_dir,
            &journal,
        );
    }
    if journal.phase == PairPhase::Ready {
        let new_service = journal
            .new_service
            .as_ref()
            .ok_or_else(|| bad("missing staged service"))?;
        let new_binary = journal
            .new_binary
            .as_ref()
            .ok_or_else(|| bad("missing staged binary"))?;
        let service_old = pair_original(
            &service_dir,
            OsStr::new(service_name),
            &journal.old_service,
            false,
        )?;
        let binary_old = pair_original(&binary_dir, binary_name, &journal.old_binary, false)?;
        if !service_old && !pair_matches(&service_dir, OsStr::new(service_name), new_service, true)?
            || !binary_old && !pair_matches(&binary_dir, binary_name, new_binary, true)?
        {
            return Err(bad("paired target changed; refusing unsafe rollback"));
        }
        if !service_old {
            if let Some(old) = &journal.old_service {
                if !pair_matches(
                    &service_dir,
                    &pair_name(OsStr::new(service_name), "backup", &journal.id),
                    old,
                    false,
                )? {
                    return Err(bad("service backup changed"));
                }
            }
        }
        if !binary_old {
            if let Some(old) = &journal.old_binary {
                if !pair_matches(
                    &binary_dir,
                    &pair_name(binary_name, "backup", &journal.id),
                    old,
                    false,
                )? {
                    return Err(bad("binary backup changed"));
                }
            }
        }
        pair_restore(
            &service_dir,
            OsStr::new(service_name),
            &journal.old_service,
            new_service,
            &journal.id,
            SERVICE_LIMIT,
        )?;
        if !journal.skip_binary {
            pair_restore(
                &binary_dir,
                binary_name,
                &journal.old_binary,
                new_binary,
                &journal.id,
                BINARY_LIMIT,
            )?;
        }
    }
    pair_cleanup(
        &service_dir,
        OsStr::new(service_name),
        &journal.id,
        true,
        SERVICE_LIMIT,
    )?;
    if !journal.skip_binary {
        pair_cleanup(&binary_dir, binary_name, &journal.id, true, BINARY_LIMIT)?;
    }
    unix::unlinkat(&service_dir, OsStr::new(PAIR_JOURNAL), AtFlags::empty())?;
    sync(&service_dir)
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum PairFault {
    Stage,
    Service,
    Binary,
    Commit,
}
fn publish_pair_inner(
    service_path: &Path,
    service_bytes: &[u8],
    source_binary: &Path,
    target_binary: &Path,
    fault: Option<PairFault>,
) -> io::Result<()> {
    if service_bytes.is_empty() || service_bytes.len() as u64 > SERVICE_LIMIT {
        return Err(bad("service definition size is invalid"));
    }
    let service_name = service_path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| bad("invalid service name"))?;
    let binary_name = target_binary
        .file_name()
        .ok_or_else(|| bad("invalid binary path"))?;
    if binary_name != OsStr::new("hmux-web") {
        return Err(bad("service binary destination must end in hmux-web"));
    }
    pair_path(&pair_text(service_path)?)?;
    pair_path(&pair_text(target_binary)?)?;
    let service_dir = directory(
        service_path
            .parent()
            .ok_or_else(|| bad("invalid service path"))?,
        true,
    )?;
    let binary_dir = directory(
        target_binary
            .parent()
            .ok_or_else(|| bad("invalid binary path"))?,
        true,
    )?;
    let source_dir = directory(
        source_binary
            .parent()
            .ok_or_else(|| bad("invalid source binary"))?,
        false,
    )?;
    let source_name = source_binary
        .file_name()
        .ok_or_else(|| bad("invalid source binary"))?;
    let (mut source, source_meta) = pair_open(&source_dir, source_name, BINARY_LIMIT, false)?
        .ok_or_else(|| bad("source binary missing"))?;
    if source_meta.len() == 0 || source_meta.mode() & 0o111 == 0 {
        return Err(bad("service source is not executable"));
    }
    let old_service = match pair_open(&service_dir, OsStr::new(service_name), SERVICE_LIMIT, true)?
    {
        Some((mut file, meta)) => Some(pair_image(&mut file, &meta)?),
        None => None,
    };
    let old_binary = match pair_open(&binary_dir, binary_name, BINARY_LIMIT, false)? {
        Some((mut file, meta)) => Some(pair_image(&mut file, &meta)?),
        None => None,
    };
    let skip_binary = source_binary == target_binary;
    if skip_binary && old_binary.is_none() {
        return Err(bad("same-path service source disappeared"));
    }
    let previous = pair_retained(&service_dir, service_name)?;
    let id = stamp()?;
    let mut journal = PairJournal {
        version: 1,
        phase: PairPhase::Preparing,
        id,
        service_name: service_name.into(),
        binary_path: pair_text(target_binary)?,
        skip_binary,
        old_service,
        new_service: None,
        old_binary,
        new_binary: None,
        previous,
    };
    pair_write_meta(&service_dir, PAIR_JOURNAL_TEMP, PAIR_JOURNAL, &journal)?;
    if let Some(old) = &journal.old_service {
        pair_backup(&service_dir, OsStr::new(service_name), old, &journal.id)?;
    }
    if !skip_binary {
        if let Some(old) = &journal.old_binary {
            pair_backup(&binary_dir, binary_name, old, &journal.id)?;
        }
    }
    let service_stage = pair_name(OsStr::new(service_name), "stage", &journal.id);
    let mut output = create(&service_dir, &service_stage)?;
    output.write_all(service_bytes)?;
    output.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    output.sync_all()?;
    let meta = output.metadata()?;
    journal.new_service = Some(PairImage {
        dev: meta.dev(),
        ino: meta.ino(),
        mode: 0o600,
        len: meta.len(),
        hash: format!("{:x}", Sha256::digest(service_bytes)),
    });
    if fault == Some(PairFault::Stage) {
        return Err(bad("synthetic stage interruption"));
    }
    if skip_binary {
        journal.new_binary = journal.old_binary.clone();
    } else {
        let binary_stage = pair_name(binary_name, "stage", &journal.id);
        let mut output = create(&binary_dir, &binary_stage)?;
        let digest = pair_copy(&mut source, &mut output, source_meta.len())?;
        if !same(&source_meta, &source.metadata()?) {
            return Err(bad("source binary changed during staging"));
        }
        output.set_permissions(std::fs::Permissions::from_mode(0o755))?;
        output.sync_all()?;
        let meta = output.metadata()?;
        journal.new_binary = Some(PairImage {
            dev: meta.dev(),
            ino: meta.ino(),
            mode: 0o755,
            len: meta.len(),
            hash: digest,
        });
    }
    sync(&service_dir)?;
    sync(&binary_dir)?;
    if !pair_original(
        &service_dir,
        OsStr::new(service_name),
        &journal.old_service,
        true,
    )? || !pair_original(&binary_dir, binary_name, &journal.old_binary, true)?
    {
        return Err(bad("paired target changed before activation"));
    }
    journal.phase = PairPhase::Ready;
    pair_write_meta(&service_dir, PAIR_JOURNAL_TEMP, PAIR_JOURNAL, &journal)?;
    let new_service = journal
        .new_service
        .as_ref()
        .ok_or_else(|| bad("missing staged service"))?;
    if !pair_matches(&service_dir, &service_stage, new_service, true)?
        || !pair_original(
            &service_dir,
            OsStr::new(service_name),
            &journal.old_service,
            true,
        )?
    {
        return Err(bad("service target changed before activation"));
    }
    unix::renameat(
        &service_dir,
        &service_stage,
        &service_dir,
        OsStr::new(service_name),
    )?;
    sync(&service_dir)?;
    if fault == Some(PairFault::Service) {
        return Err(bad("synthetic service interruption"));
    }
    if !skip_binary {
        let new_binary = journal
            .new_binary
            .as_ref()
            .ok_or_else(|| bad("missing staged binary"))?;
        let binary_stage = pair_name(binary_name, "stage", &journal.id);
        if !pair_matches(&binary_dir, &binary_stage, new_binary, true)?
            || !pair_original(&binary_dir, binary_name, &journal.old_binary, true)?
        {
            return Err(bad("binary target changed before activation"));
        }
        unix::renameat(&binary_dir, &binary_stage, &binary_dir, binary_name)?;
        sync(&binary_dir)?;
    }
    if fault == Some(PairFault::Binary) {
        return Err(bad("synthetic binary interruption"));
    }
    journal.phase = PairPhase::Committed;
    pair_write_meta(&service_dir, PAIR_JOURNAL_TEMP, PAIR_JOURNAL, &journal)?;
    if fault == Some(PairFault::Commit) {
        return Err(bad("synthetic commit interruption"));
    }
    pair_commit(
        service_path,
        &service_dir,
        target_binary,
        &binary_dir,
        &journal,
    )
}

/// Publish the service definition and executable as one recoverable operation.
/// The caller must hold the persistent service change lock throughout this call.
pub(crate) fn publish_pair(
    service_path: &Path,
    service_bytes: &[u8],
    source_binary: &Path,
    target_binary: &Path,
) -> io::Result<()> {
    if service_path.parent().is_some_and(Path::exists) {
        recover_pair(service_path)?;
    }
    match publish_pair_inner(
        service_path,
        service_bytes,
        source_binary,
        target_binary,
        None,
    ) {
        Ok(()) => Ok(()),
        Err(error) => match recover_pair(service_path) {
            Ok(()) => Err(error),
            Err(recovery) => Err(bad(&format!(
                "publication failed: {error}; recovery requires retry: {recovery}"
            ))),
        },
    }
}

#[cfg(test)]
mod pair_tests {
    use super::*;
    use std::os::unix::fs::symlink;
    struct Root {
        path: PathBuf,
        service: PathBuf,
        binary: PathBuf,
        source: PathBuf,
    }
    impl Root {
        fn new() -> Self {
            let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
            let path = base.join(format!(
                "hmux-e2e-pair-{}-{}",
                std::process::id(),
                stamp().unwrap()
            ));
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            for name in ["service", "bin", "src"] {
                std::fs::create_dir(path.join(name)).unwrap();
            }
            Self {
                service: path.join("service/service.plist"),
                binary: path.join("bin/hmux-web"),
                source: path.join("src/hmux-web"),
                path,
            }
        }
        fn source(&self, data: &[u8]) {
            std::fs::write(&self.source, data).unwrap();
            std::fs::set_permissions(&self.source, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        fn publish(&self, definition: &[u8]) {
            publish_pair(&self.service, definition, &self.source, &self.binary).unwrap();
        }
        fn bytes(&self, path: &Path) -> Vec<u8> {
            std::fs::read(path).unwrap()
        }
        fn backup_count(&self, dir: &Path) -> usize {
            std::fs::read_dir(dir)
                .unwrap()
                .filter(|item| {
                    item.as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .contains(".hmux-pair-backup-")
                })
                .count()
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
    #[test]
    fn first_install_and_upgrades_keep_one_backup_with_original_modes() {
        let r = Root::new();
        r.source(b"binary-0");
        r.publish(b"service-0");
        assert_eq!(r.bytes(&r.binary), b"binary-0");
        assert_eq!(r.bytes(&r.service), b"service-0");
        for n in 1..4 {
            r.source(format!("binary-{n}").as_bytes());
            r.publish(format!("service-{n}").as_bytes());
            assert_eq!(r.backup_count(r.service.parent().unwrap()), 1);
            assert_eq!(r.backup_count(r.binary.parent().unwrap()), 1);
        }
        let dir = directory(r.binary.parent().unwrap(), false).unwrap();
        let retained = pair_retained(
            &directory(r.service.parent().unwrap(), false).unwrap(),
            "service.plist",
        )
        .unwrap()
        .unwrap();
        let backup = pair_name(
            OsStr::new("hmux-web"),
            "backup",
            &retained.binary.unwrap().id,
        );
        let meta = pair_open(&dir, &backup, BINARY_LIMIT, false)
            .unwrap()
            .unwrap()
            .1;
        assert_eq!(meta.mode() & 0o777, 0o755);
    }
    #[test]
    fn recovery_before_and_between_renames_restores_original_pair() {
        for fault in [PairFault::Stage, PairFault::Service, PairFault::Binary] {
            let r = Root::new();
            r.source(b"binary-old");
            r.publish(b"service-old");
            r.source(b"binary-new");
            assert!(publish_pair_inner(
                &r.service,
                b"service-new",
                &r.source,
                &r.binary,
                Some(fault)
            )
            .is_err());
            recover_pair(&r.service).unwrap();
            assert_eq!(r.bytes(&r.service), b"service-old");
            assert_eq!(r.bytes(&r.binary), b"binary-old");
            assert!(pair_journal(
                &directory(r.service.parent().unwrap(), false).unwrap(),
                "service.plist"
            )
            .unwrap()
            .is_none());
            assert_eq!(r.backup_count(r.binary.parent().unwrap()), 0);
        }
    }
    #[test]
    fn committed_interruption_finishes_new_pair_and_bounded_retention() {
        let r = Root::new();
        r.source(b"binary-old");
        r.publish(b"service-old");
        r.source(b"binary-new");
        assert!(publish_pair_inner(
            &r.service,
            b"service-new",
            &r.source,
            &r.binary,
            Some(PairFault::Commit)
        )
        .is_err());
        recover_pair(&r.service).unwrap();
        assert_eq!(r.bytes(&r.service), b"service-new");
        assert_eq!(r.bytes(&r.binary), b"binary-new");
        assert_eq!(r.backup_count(r.service.parent().unwrap()), 1);
        assert_eq!(r.backup_count(r.binary.parent().unwrap()), 1);
    }
    #[test]
    fn same_binary_source_and_target_is_not_replaced() {
        let r = Root::new();
        r.source(b"binary-current");
        r.publish(b"service-old");
        let inode = std::fs::metadata(&r.binary).unwrap().ino();
        publish_pair(&r.service, b"service-new", &r.binary, &r.binary).unwrap();
        assert_eq!(r.bytes(&r.service), b"service-new");
        assert_eq!(r.bytes(&r.binary), b"binary-current");
        assert_eq!(std::fs::metadata(&r.binary).unwrap().ino(), inode);
    }
    #[test]
    fn changed_target_refuses_unsafe_rollback() {
        let r = Root::new();
        r.source(b"binary-old");
        r.publish(b"service-old");
        r.source(b"binary-new");
        publish_pair_inner(
            &r.service,
            b"service-new",
            &r.source,
            &r.binary,
            Some(PairFault::Service),
        )
        .unwrap_err();
        std::fs::write(&r.service, b"other-service").unwrap();
        assert!(recover_pair(&r.service).is_err());
        assert_eq!(r.bytes(&r.service), b"other-service");
        assert_eq!(r.bytes(&r.binary), b"binary-old");
    }
    #[test]
    fn symlinked_source_and_invalid_second_file_leave_installed_pair() {
        let r = Root::new();
        r.source(b"binary-old");
        r.publish(b"service-old");
        std::fs::rename(&r.source, r.source.with_file_name("real")).unwrap();
        symlink("real", &r.source).unwrap();
        assert!(publish_pair(&r.service, b"service-new", &r.source, &r.binary).is_err());
        assert_eq!(r.bytes(&r.service), b"service-old");
        assert_eq!(r.bytes(&r.binary), b"binary-old");
    }
}

#[cfg(test)]
mod pair_more_tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    fn root() -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = base.join(format!(
            "hmux-e2e-pair-extra-{}-{}",
            std::process::id(),
            stamp().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["svc", "bin", "src"] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        (
            root.clone(),
            root.join("svc/service.plist"),
            root.join("bin/hmux-web"),
            root.join("src/hmux-web"),
        )
    }
    fn source(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[test]
    fn failed_later_upgrade_preserves_previous_retained_pair() {
        let (root, service, binary, src) = root();
        source(&src, b"binary-0");
        publish_pair(&service, b"service-0", &src, &binary).unwrap();
        source(&src, b"binary-1");
        publish_pair(&service, b"service-1", &src, &binary).unwrap();
        let dir = directory(service.parent().unwrap(), false).unwrap();
        let before = pair_retained(&dir, "service.plist").unwrap().unwrap();
        source(&src, b"binary-2");
        publish_pair_inner(
            &service,
            b"service-2",
            &src,
            &binary,
            Some(PairFault::Service),
        )
        .unwrap_err();
        recover_pair(&service).unwrap();
        assert_eq!(std::fs::read(&service).unwrap(), b"service-1");
        assert_eq!(std::fs::read(&binary).unwrap(), b"binary-1");
        let after = pair_retained(&dir, "service.plist").unwrap().unwrap();
        assert_eq!(before.service, after.service);
        assert_eq!(before.binary, after.binary);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn partial_stage_and_symlink_target_are_rejected_safely() {
        let (root, service, binary, src) = root();
        source(&src, b"binary-old");
        publish_pair(&service, b"service-old", &src, &binary).unwrap();
        source(&src, b"binary-new");
        publish_pair_inner(
            &service,
            b"service-new",
            &src,
            &binary,
            Some(PairFault::Stage),
        )
        .unwrap_err();
        let dir = directory(service.parent().unwrap(), false).unwrap();
        let journal = pair_journal(&dir, "service.plist").unwrap().unwrap();
        std::fs::write(
            service.with_file_name(pair_name(OsStr::new("service.plist"), "stage", &journal.id)),
            b"partial",
        )
        .unwrap();
        recover_pair(&service).unwrap();
        assert_eq!(std::fs::read(&service).unwrap(), b"service-old");
        std::fs::rename(&binary, binary.with_file_name("old-web")).unwrap();
        symlink("old-web", &binary).unwrap();
        assert!(publish_pair(&service, b"service-new", &src, &binary).is_err());
        assert_eq!(std::fs::read(&service).unwrap(), b"service-old");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn metadata_and_hash_reads_reject_growth() {
        let (root, service, binary, src) = root();
        source(&src, b"binary-old");
        publish_pair(&service, b"service-old", &src, &binary).unwrap();
        let dir = directory(service.parent().unwrap(), false).unwrap();
        let mut file = create(&dir, OsStr::new(PAIR_JOURNAL)).unwrap();
        file.write_all(&vec![b'x'; PAIR_META_LIMIT as usize + 1])
            .unwrap();
        file.sync_all().unwrap();
        assert!(recover_pair(&service).is_err());
        unix::unlinkat(&dir, OsStr::new(PAIR_JOURNAL), AtFlags::empty()).unwrap();
        let source_dir = directory(src.parent().unwrap(), false).unwrap();
        let (mut file, meta) = pair_open(&source_dir, OsStr::new("hmux-web"), BINARY_LIMIT, false)
            .unwrap()
            .unwrap();
        let mut append = std::fs::OpenOptions::new().append(true).open(&src).unwrap();
        append.write_all(b"growth").unwrap();
        assert!(pair_hash(&mut file, meta.len()).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
