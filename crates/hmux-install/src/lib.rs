//! Bounded, durable installation of the Home executable pair.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const NAMES: [&str; 2] = ["hmux-web", "hmux-agent"];
const LOCK: &str = ".hmux-install.lock";
const JOURNAL: &str = ".hmux-install.journal";
const LIMIT: u64 = 256 * 1024 * 1024;
const JOURNAL_LIMIT: u64 = 16 * 1024;
const LOCK_WAIT: Duration = Duration::from_secs(2);
const JOURNAL_TEMP: &str = ".hmux-install.journal.tmp-";

#[derive(Clone, Serialize, Deserialize)]
struct Image {
    dev: u64,
    ino: u64,
    mode: u32,
    len: u64,
    hash: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    Preparing,
    Ready,
}
#[derive(Serialize, Deserialize)]
struct Entry {
    old: Option<Image>,
    new: Option<Image>,
    backup: Option<String>,
    stage: String,
    restore: String,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    version: u8,
    phase: Phase,
    entries: [Entry; 2],
}
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum Fault {
    Backup,
    Stage,
    Replace(usize),
}

fn bad(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn absent(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}
fn normalize(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut out = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::RootDir => out.push("/"),
            Component::Normal(name) => out.push(name),
            Component::CurDir => (),
            Component::ParentDir => {
                out.pop();
            }
            Component::Prefix(_) => return Err(bad("unsupported path prefix")),
        }
    }
    Ok(out)
}
fn directory(path: &Path, create: bool) -> io::Result<PathBuf> {
    let path = normalize(path)?;
    let uid = rustix::process::getuid().as_raw();
    for parent in path.ancestors() {
        match fs::symlink_metadata(parent) {
            Ok(meta) => {
                let sticky_root = meta.uid() == 0 && meta.mode() & 0o1000 != 0;
                if !meta.is_dir()
                    || (meta.uid() != uid && meta.uid() != 0)
                    || (meta.mode() & 0o022 != 0 && !sticky_root)
                {
                    return Err(bad("directory is not trusted or traverses a symlink"));
                }
            }
            Err(e) if create && e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
    }
    if create {
        fs::create_dir_all(&path)?;
    }
    let meta = fs::symlink_metadata(&path)?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o022 != 0 {
        return Err(bad("installation directory must be owner-controlled"));
    }
    Ok(path)
}
fn open_binary(path: &Path, executable: bool) -> io::Result<(File, Metadata)> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != rustix::process::getuid().as_raw()
        || meta.mode() & 0o022 != 0
        || meta.nlink() != 1
        || meta.len() == 0
        || meta.len() > LIMIT
        || (executable && meta.mode() & 0o111 == 0)
    {
        return Err(bad(
            "binary must be a nonempty owner-controlled regular file with required executable mode",
        ));
    }
    Ok((file, meta))
}
fn old_binary(path: &Path) -> io::Result<Option<(File, Metadata)>> {
    match open_binary(path, false) {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == io::ErrorKind::NotFound && absent(path)? => Ok(None),
        Err(e) => Err(e),
    }
}
fn hash(file: &mut File, len: u64) -> io::Result<String> {
    if len == 0 || len > LIMIT {
        return Err(bad("binary size is invalid"));
    }
    file.rewind()?;
    let mut left = len;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    while left > 0 {
        let want = buffer.len().min(left as usize);
        let n = file.read(&mut buffer[..want])?;
        if n == 0 {
            return Err(bad("binary shrank during hash"));
        }
        digest.update(&buffer[..n]);
        left -= n as u64;
    }
    let mut extra = [0u8; 1];
    if file.read(&mut extra)? != 0 || file.metadata()?.len() != len {
        return Err(bad("binary grew during hash"));
    }
    file.rewind()?;
    Ok(format!("{:x}", digest.finalize()))
}
fn image(file: &mut File, meta: &Metadata) -> io::Result<Image> {
    Ok(Image {
        dev: meta.dev(),
        ino: meta.ino(),
        mode: meta.mode() & 0o7777,
        len: meta.len(),
        hash: hash(file, meta.len())?,
    })
}
fn same(path: &Path, expected: &Image, inode: bool) -> io::Result<bool> {
    let Some((mut file, meta)) = old_binary(path)? else {
        return Ok(false);
    };
    if meta.len() != expected.len
        || meta.mode() & 0o7777 != expected.mode
        || (inode && (meta.dev() != expected.dev || meta.ino() != expected.ino))
    {
        return Ok(false);
    }
    Ok(hash(&mut file, expected.len)? == expected.hash)
}
fn sync_dir(dir: &Path) -> io::Result<()> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)?
        .sync_all()
}
fn private(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}
fn lock(dir: &Path) -> io::Result<File> {
    let path = dir.join(LOCK);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != rustix::process::getuid().as_raw()
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
    {
        return Err(bad("unsafe installation lock"));
    }
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => break,
            Err(error) => {
                let error: io::Error = error.into();
                if error.kind() != io::ErrorKind::WouldBlock {
                    return Err(error);
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "installation lock is busy",
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    let current = fs::symlink_metadata(path)?;
    if current.dev() != meta.dev() || current.ino() != meta.ino() {
        return Err(bad("installation lock changed"));
    }
    Ok(file)
}
fn copy(input: &mut File, output: &mut File, len: u64) -> io::Result<String> {
    input.rewind()?;
    let mut left = len;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    while left != 0 {
        let want = buffer.len().min(left as usize);
        let n = input.read(&mut buffer[..want])?;
        if n == 0 {
            return Err(bad("binary changed during copy"));
        }
        output.write_all(&buffer[..n])?;
        digest.update(&buffer[..n]);
        left -= n as u64;
    }
    let mut extra = [0u8; 1];
    if input.read(&mut extra)? != 0 || input.metadata()?.len() != len {
        return Err(bad("binary grew during copy"));
    }
    output.sync_all()?;
    input.rewind()?;
    Ok(format!("{:x}", digest.finalize()))
}
fn id() -> io::Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| bad("clock before epoch"))?;
    Ok(format!("{}-{}", now.as_nanos(), std::process::id()))
}
fn valid_id(value: &str) -> bool {
    let Some((time, pid)) = value.split_once('-') else {
        return false;
    };
    !time.is_empty()
        && !pid.is_empty()
        && time.bytes().all(|b| b.is_ascii_digit())
        && pid.bytes().all(|b| b.is_ascii_digit())
}
fn validate(journal: &Journal) -> io::Result<()> {
    if journal.version != 2 {
        return Err(bad("unknown installation journal"));
    }
    for (name, entry) in NAMES.iter().zip(&journal.entries) {
        let suffix = |value: &str, kind: &str| {
            value
                .strip_prefix(&format!(".{name}.{kind}-"))
                .is_some_and(valid_id)
        };
        if !suffix(&entry.stage, "stage")
            || !suffix(&entry.restore, "restore")
            || entry.backup.as_ref().is_some_and(|v| !suffix(v, "backup"))
            || entry.old.is_some() != entry.backup.is_some()
            || (journal.phase == Phase::Ready && entry.new.is_none())
        {
            return Err(bad("unsafe installation journal"));
        }
    }
    Ok(())
}
fn read_journal(dir: &Path) -> io::Result<Option<Journal>> {
    let path = dir.join(JOURNAL);
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound && absent(&path)? => return Ok(None),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != rustix::process::getuid().as_raw()
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
        || meta.len() > JOURNAL_LIMIT
    {
        return Err(bad("unsafe installation journal"));
    }
    let mut data = Vec::with_capacity((meta.len() + 1) as usize);
    (&mut file).take(JOURNAL_LIMIT + 1).read_to_end(&mut data)?;
    if data.len() as u64 > JOURNAL_LIMIT || file.metadata()?.len() != meta.len() {
        return Err(bad("installation journal changed or grew during read"));
    }
    let journal: Journal = serde_json::from_slice(&data)?;
    validate(&journal)?;
    Ok(Some(journal))
}
fn write_journal(dir: &Path, journal: &Journal) -> io::Result<()> {
    validate(journal)?;
    let bytes = serde_json::to_vec(journal)?;
    if bytes.len() as u64 > JOURNAL_LIMIT {
        return Err(bad("installation journal is too large"));
    }
    let temporary = dir.join(format!("{JOURNAL_TEMP}{}", id()?));
    let mut file = private(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    let destination = dir.join(JOURNAL);
    if !absent(&destination)? {
        read_journal(dir)?;
    }
    fs::rename(&temporary, destination)?;
    sync_dir(dir)
}
fn remove_owned(path: &Path) -> io::Result<()> {
    if absent(path)? {
        return Ok(());
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file()
        || meta.uid() != rustix::process::getuid().as_raw()
        || meta.mode() & 0o022 != 0
        || meta.nlink() != 1
        || meta.len() > LIMIT
    {
        return Err(bad("managed installation file changed"));
    }
    fs::remove_file(path)
}
fn cleanup_journal_temps(dir: &Path) -> io::Result<()> {
    for item in fs::read_dir(dir)? {
        let item = item?;
        let name = item.file_name();
        let name = name.to_string_lossy();
        if name.strip_prefix(JOURNAL_TEMP).is_some_and(valid_id) {
            remove_owned(&item.path())?;
        }
    }
    sync_dir(dir)
}
fn backup_id(name: &str, binary: &str) -> Option<String> {
    let suffix = name.strip_prefix(&format!(".{binary}.backup-"))?;
    valid_id(suffix).then(|| suffix.to_owned())
}
fn backup_order(id: &str) -> Option<(u128, u32)> {
    let (time, pid) = id.split_once('-')?;
    Some((time.parse().ok()?, pid.parse().ok()?))
}
fn prune_backups(dir: &Path) -> io::Result<()> {
    let mut newest: [Option<((u128, u32), String)>; 2] = [None, None];
    for item in fs::read_dir(dir)? {
        let item = item?;
        let name = item.file_name();
        let name = name.to_string_lossy();
        for (index, binary) in NAMES.iter().enumerate() {
            if let Some(id) = backup_id(&name, binary) {
                let Some(order) = backup_order(&id) else {
                    return Err(bad("invalid backup id"));
                };
                let meta = fs::symlink_metadata(item.path())?;
                if !meta.is_file()
                    || meta.uid() != rustix::process::getuid().as_raw()
                    || meta.mode() & 0o022 != 0
                    || meta.nlink() != 1
                    || meta.len() == 0
                    || meta.len() > LIMIT
                {
                    return Err(bad("managed backup changed"));
                }
                if newest[index]
                    .as_ref()
                    .is_none_or(|(previous, _)| order > *previous)
                {
                    newest[index] = Some((order, id));
                }
            }
        }
    }
    for item in fs::read_dir(dir)? {
        let item = item?;
        let name = item.file_name();
        let name = name.to_string_lossy();
        for (index, binary) in NAMES.iter().enumerate() {
            if let Some(id) = backup_id(&name, binary) {
                if newest[index]
                    .as_ref()
                    .is_some_and(|(_, current)| current == &id)
                {
                    continue;
                }
                remove_owned(&item.path())?;
            }
        }
    }
    sync_dir(dir)
}

fn old_state(dir: &Path, name: &str, old: &Option<Image>) -> io::Result<bool> {
    match old {
        Some(old) => same(&dir.join(name), old, false),
        None => absent(&dir.join(name)),
    }
}
fn initial_state(dir: &Path, name: &str, old: &Option<Image>) -> io::Result<bool> {
    match old {
        Some(old) => same(&dir.join(name), old, true),
        None => absent(&dir.join(name)),
    }
}
fn recover_locked(dir: &Path) -> io::Result<()> {
    cleanup_journal_temps(dir)?;
    let Some(journal) = read_journal(dir)? else {
        return prune_backups(dir);
    };
    if journal.phase == Phase::Ready {
        // Validate the entire pair and both backups before changing either target.
        for (name, entry) in NAMES.iter().zip(&journal.entries) {
            let new = entry
                .new
                .as_ref()
                .ok_or_else(|| bad("ready image missing"))?;
            if !old_state(dir, name, &entry.old)? {
                if !same(&dir.join(name), new, true)? {
                    return Err(bad("installed binary changed; refusing unsafe rollback"));
                }
                if let (Some(old), Some(backup)) = (&entry.old, &entry.backup) {
                    if !same(&dir.join(backup), old, false)? {
                        return Err(bad("backup changed; refusing rollback"));
                    }
                }
            }
        }
        for (name, entry) in NAMES.iter().zip(&journal.entries) {
            if old_state(dir, name, &entry.old)? {
                continue;
            }
            let target = dir.join(name);
            let new = entry
                .new
                .as_ref()
                .ok_or_else(|| bad("ready image missing"))?;
            if !same(&target, new, true)? {
                return Err(bad("installed binary changed during rollback"));
            }
            if let (Some(old), Some(backup)) = (&entry.old, &entry.backup) {
                let restore = dir.join(&entry.restore);
                // A partial restore from a previous crash is safe to discard; target is still new.
                remove_owned(&restore)?;
                let (mut input, _) = open_binary(&dir.join(backup), false)?;
                let mut output = private(&restore)?;
                if copy(&mut input, &mut output, old.len)? != old.hash {
                    return Err(bad("backup changed during rollback"));
                }
                output.set_permissions(fs::Permissions::from_mode(old.mode))?;
                output.sync_all()?;
                sync_dir(dir)?;
                if !same(&target, new, true)? {
                    return Err(bad("installed binary changed before rollback"));
                }
                fs::rename(restore, target)?;
            } else {
                if !same(&target, new, true)? {
                    return Err(bad("installed binary changed before rollback"));
                }
                fs::remove_file(target)?;
            }
            sync_dir(dir)?;
        }
    }
    // Preparing cannot have changed targets. Both phases own these predictable scratch names.
    for entry in &journal.entries {
        remove_owned(&dir.join(&entry.stage))?;
        remove_owned(&dir.join(&entry.restore))?;
        if let Some(backup) = &entry.backup {
            remove_owned(&dir.join(backup))?;
        }
    }
    sync_dir(dir)?;
    fs::remove_file(dir.join(JOURNAL))?;
    sync_dir(dir)?;
    prune_backups(dir)
}

/// Roll back a pending paired installation after validating both targets and backups.
pub fn recover(bin: &Path) -> io::Result<()> {
    let bin = directory(bin, false)?;
    let _lock = lock(&bin)?;
    recover_locked(&bin)
}

fn install_inner(source: &Path, bin: &Path, fault: Option<Fault>) -> io::Result<()> {
    let source = directory(source, false)?;
    let bin = directory(bin, true)?;
    if source == bin {
        return Err(bad("source and installation directories must differ"));
    }
    let _lock = lock(&bin)?;
    recover_locked(&bin)?;
    let mut sources = Vec::new();
    for name in NAMES {
        sources.push(open_binary(&source.join(name), true)?);
    }
    let mut originals = Vec::new();
    for name in NAMES {
        originals.push(match old_binary(&bin.join(name))? {
            Some((mut file, meta)) => Some(image(&mut file, &meta)?),
            None => None,
        });
    }
    let id = id()?;
    let entries: Vec<_> = NAMES
        .iter()
        .enumerate()
        .map(|(index, name)| Entry {
            old: originals[index].clone(),
            new: None,
            backup: originals[index]
                .as_ref()
                .map(|_| format!(".{name}.backup-{id}")),
            stage: format!(".{name}.stage-{id}"),
            restore: format!(".{name}.restore-{id}"),
        })
        .collect();
    let mut journal = Journal {
        version: 2,
        phase: Phase::Preparing,
        entries: entries
            .try_into()
            .map_err(|_| bad("incomplete binary pair"))?,
    };
    // From this point every scratch file is named in a durable preparation manifest.
    write_journal(&bin, &journal)?;
    for (index, name) in NAMES.iter().enumerate() {
        let entry = &mut journal.entries[index];
        if let (Some(old), Some(backup)) = (&entry.old, &entry.backup) {
            if !initial_state(&bin, name, &Some(old.clone()))? {
                return Err(bad("installed binary changed before backup"));
            }
            let (mut input, _) = open_binary(&bin.join(name), false)?;
            let mut output = private(&bin.join(backup))?;
            if copy(&mut input, &mut output, old.len)? != old.hash {
                return Err(bad("installed binary changed during backup"));
            }
            output.set_permissions(fs::Permissions::from_mode(old.mode))?;
            output.sync_all()?;
            if fault == Some(Fault::Backup) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "synthetic backup interruption",
                ));
            }
        }
        let mut output = private(&bin.join(&entry.stage))?;
        let source_len = sources[index].1.len();
        let digest = copy(&mut sources[index].0, &mut output, source_len)?;
        let current = sources[index].0.metadata()?;
        let first = &sources[index].1;
        if current.dev() != first.dev()
            || current.ino() != first.ino()
            || current.len() != first.len()
            || current.mtime() != first.mtime()
            || current.mtime_nsec() != first.mtime_nsec()
            || current.ctime() != first.ctime()
            || current.ctime_nsec() != first.ctime_nsec()
        {
            return Err(bad("source binary changed during copy"));
        }
        output.set_permissions(fs::Permissions::from_mode(0o755))?;
        output.sync_all()?;
        let meta = output.metadata()?;
        entry.new = Some(Image {
            dev: meta.dev(),
            ino: meta.ino(),
            mode: 0o755,
            len: meta.len(),
            hash: digest,
        });
        if fault == Some(Fault::Stage) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "synthetic staging interruption",
            ));
        }
    }
    sync_dir(&bin)?;
    for (name, entry) in NAMES.iter().zip(&journal.entries) {
        if !initial_state(&bin, name, &entry.old)? {
            return Err(bad("installed binary changed before activation"));
        }
    }
    journal.phase = Phase::Ready;
    write_journal(&bin, &journal)?;
    for (index, (name, entry)) in NAMES.iter().zip(&journal.entries).enumerate() {
        if !initial_state(&bin, name, &entry.old)? {
            return Err(bad("installed binary changed during activation"));
        }
        let new = entry
            .new
            .as_ref()
            .ok_or_else(|| bad("ready image missing"))?;
        if !same(&bin.join(&entry.stage), new, true)? {
            return Err(bad("staged binary changed"));
        }
        fs::rename(bin.join(&entry.stage), bin.join(name))?;
        sync_dir(&bin)?;
        if fault == Some(Fault::Replace(index)) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "synthetic activation interruption",
            ));
        }
    }
    fs::remove_file(bin.join(JOURNAL))?;
    sync_dir(&bin)?;
    prune_backups(&bin)
}

/// Install `hmux-web` and `hmux-agent` as a recoverable pair; backups are retained.
pub fn install(source: &Path, bin: &Path) -> io::Result<()> {
    install_inner(source, bin, None)
}

#[cfg(test)]
mod tests;
