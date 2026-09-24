#![forbid(unsafe_code)]
//! Private, directory-relative file operations and Go-compatible advisory locks.
//!
//! The caller creates the state directory with restrictive permissions. Paths passed
//! to [`PrivateDir::open`] must be absolute and have no symlink components. The
//! opened directory must belong to the effective user and must not be group/world
//! writable. All subsequent operations are relative to its stable directory fd.
//! Callers still own transaction ordering and must hold the appropriate lock
//! across the complete read/modify/write operation.

pub mod command;
pub mod log;
pub mod runtime;
pub mod token;
pub mod workspace;

use rustix::fs::{self, AtFlags, FlockOperation, Mode, OFlags};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PRIVATE_MODE: Mode = Mode::from_raw_mode(0o600);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// An opened, owner-controlled state directory. No method accepts nested paths.
pub struct PrivateDir {
    file: File,
}

impl std::os::fd::AsFd for PrivateDir {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.file)
    }
}

impl PrivateDir {
    /// Open or create a clean absolute service directory, checking every ancestor.
    /// This follows the Home service's Go trust policy: ancestors belong to root
    /// or the effective user and are not group/world writable, except root-owned
    /// sticky directories. The final directory must belong to the effective user.
    /// Missing components are created as 0700 through already checked descriptors;
    /// no symlink is followed and existing permissions are never modified.
    /// This is synchronous startup I/O, not an async request-path operation.
    pub fn open_or_create_trusted(path: &Path) -> io::Result<Self> {
        Self::open_trusted(path, true)
    }

    /// Apply the service trust policy without creating any missing directory.
    pub fn open_existing_trusted(path: &Path) -> io::Result<Self> {
        Self::open_trusted(path, false)
    }

    fn open_trusted(path: &Path, create: bool) -> io::Result<Self> {
        let clean: std::path::PathBuf = path.components().collect();
        if !path.is_absolute()
            || clean.as_os_str() != path.as_os_str()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Err(invalid_input(
                "service directory must be a clean absolute path",
            ));
        }
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut file = File::from(fs::open("/", flags, Mode::empty())?);
        check_service_directory(&file.metadata()?, false)?;
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    let next = match fs::openat(&file, name, flags, Mode::empty()) {
                        Ok(fd) => fd,
                        Err(rustix::io::Errno::NOENT) if create => {
                            match fs::mkdirat(&file, name, Mode::from_raw_mode(0o700)) {
                                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                                Err(error) => return Err(error.into()),
                            }
                            fs::openat(&file, name, flags, Mode::empty())?
                        }
                        Err(error) => return Err(error.into()),
                    };
                    file = File::from(next);
                    check_service_directory(&file.metadata()?, false)?;
                }
                _ => return Err(invalid_input("service directory contains . or ..")),
            }
        }
        check_service_directory(&file.metadata()?, true)?;
        Ok(Self { file })
    }

    /// Create or open a private immediate child through this stable directory fd.
    /// Existing permissions are checked, never relaxed; symlinks are rejected.
    pub fn create_private_child(&self, name: &OsStr) -> io::Result<Self> {
        check_name(name)?;
        let created = match fs::mkdirat(&self.file, name, Mode::from_raw_mode(0o700)) {
            Ok(()) => true,
            Err(rustix::io::Errno::EXIST) => false,
            Err(error) => return Err(error.into()),
        };
        let file = File::from(fs::openat(
            &self.file,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let metadata = file.metadata()?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "child directory must be private and owner-controlled",
            ));
        }
        if created {
            self.file.sync_all()?;
        }
        Ok(Self { file })
    }

    /// Open an absolute directory path without following symlinks in any component.
    /// Ancestor directory ownership is the caller's trust policy; the final
    /// directory must be owned by this process's effective user and not writable
    /// by group or others. This method never creates or changes permissions.
    pub fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(invalid_input("state directory path must be absolute"));
        }
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut file = File::from(fs::open("/", flags, Mode::empty())?);
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    file = File::from(fs::openat(&file, name, flags, Mode::empty())?);
                }
                _ => return Err(invalid_input("state directory contains . or ..")),
            }
        }
        let metadata = file.metadata()?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o022 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "state directory must be a real, owner-controlled directory",
            ));
        }
        Ok(Self { file })
    }

    /// Read at most `max_bytes` from an existing private regular, single-link file.
    /// A changed pathname cannot redirect the already opened file descriptor.
    /// The size is checked both before and during the read, so growth is bounded.
    pub fn read_private(&self, name: &OsStr, max_bytes: usize) -> io::Result<Vec<u8>> {
        check_name(name)?;
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let mut file = File::from(fs::openat(&self.file, name, flags, Mode::empty())?);
        let metadata = file.metadata()?;
        check_private_file(&metadata)?;
        if metadata.len() > max_bytes as u64 {
            return Err(size_error());
        }
        let read_limit = u64::try_from(max_bytes)
            .ok()
            .and_then(|limit| limit.checked_add(1))
            .ok_or_else(|| invalid_input("read limit is too large"))?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(read_limit)
            .read_to_end(&mut bytes)?;
        if bytes.len() > max_bytes {
            return Err(size_error());
        }
        Ok(bytes)
    }

    /// Atomically replace one basename with a new private 0600 regular file.
    /// A `BeforeCommit` error leaves the old target untouched by this call;
    /// `AfterCommit` means rename succeeded but parent-directory sync failed,
    /// so durability is uncertain and the new target must not be assumed absent.
    pub fn write_atomic_private(&self, name: &OsStr, data: &[u8]) -> Result<(), WriteError> {
        self.write_atomic_with_sync(name, data, || self.file.sync_all())
    }

    /// Publish a complete private file only if its name is still absent. Used by
    /// credential enrollment: an earlier existence check is not sufficient to
    /// prevent a concurrent initializer from replacing another account's secret.
    pub fn write_new_private(&self, name: &OsStr, data: &[u8]) -> Result<(), WriteError> {
        check_name(name).map_err(WriteError::BeforeCommit)?;
        let (temp_name, mut file) = self.create_temp().map_err(WriteError::BeforeCommit)?;
        let cleanup = TempCleanup {
            dir: &self.file,
            name: temp_name,
            renamed: false,
        };
        fs::fchmod(&file, PRIVATE_MODE).map_err(|e| WriteError::BeforeCommit(e.into()))?;
        file.write_all(data).map_err(WriteError::BeforeCommit)?;
        file.sync_all().map_err(WriteError::BeforeCommit)?;
        fs::linkat(
            &self.file,
            &cleanup.name,
            &self.file,
            name,
            AtFlags::empty(),
        )
        .map_err(|e| WriteError::BeforeCommit(e.into()))?;
        // Remove the staging link before any consumer validates nlink == 1.
        fs::unlinkat(&self.file, &cleanup.name, AtFlags::empty())
            .map_err(|e| WriteError::AfterCommit(e.into()))?;
        drop(cleanup);
        self.file.sync_all().map_err(WriteError::AfterCommit)
    }

    fn write_atomic_with_sync(
        &self,
        name: &OsStr,
        data: &[u8],
        sync_directory: impl FnOnce() -> io::Result<()>,
    ) -> Result<(), WriteError> {
        check_name(name).map_err(WriteError::BeforeCommit)?;
        let (temp_name, mut temp_file) = self.create_temp().map_err(WriteError::BeforeCommit)?;
        let mut cleanup = TempCleanup {
            dir: &self.file,
            name: temp_name,
            renamed: false,
        };
        fs::fchmod(&temp_file, PRIVATE_MODE)
            .map_err(|error| WriteError::BeforeCommit(error.into()))?;
        temp_file
            .write_all(data)
            .map_err(WriteError::BeforeCommit)?;
        temp_file.sync_all().map_err(WriteError::BeforeCommit)?;
        drop(temp_file);
        fs::renameat(&self.file, &cleanup.name, &self.file, name)
            .map_err(|error| WriteError::BeforeCommit(error.into()))?;
        cleanup.renamed = true;
        sync_directory().map_err(WriteError::AfterCommit)
    }

    fn create_temp(&self) -> io::Result<(OsString, File)> {
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        for _ in 0..16 {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let name = OsString::from(format!(".hmux-{}-{nanos}-{sequence}", std::process::id()));
            match fs::openat(&self.file, &name, flags, PRIVATE_MODE) {
                Ok(fd) => return Ok((name, File::from(fd))),
                Err(rustix::io::Errno::EXIST) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not reserve a private temporary file",
        ))
    }

    /// Try a Go-compatible `LOCK_EX | LOCK_NB` on a persistent lock inode.
    /// `Ok(None)` means another holder owns it. Dropping the guard unlocks it.
    pub fn try_lock(&self, name: &OsStr) -> io::Result<Option<FileLock>> {
        let file = self.open_lock_file(name)?;
        loop {
            match fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(Some(FileLock { file })),
                Err(rustix::io::Errno::INTR) => continue,
                Err(rustix::io::Errno::WOULDBLOCK) => return Ok(None),
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Poll at 25 ms intervals until the lock is acquired or `max_wait` expires.
    /// A zero wait attempts acquisition once. This is a synchronous operation.
    pub fn lock_for(&self, name: &OsStr, max_wait: Duration) -> Result<FileLock, LockError> {
        let start = Instant::now();
        loop {
            if let Some(lock) = self.try_lock(name).map_err(LockError::Io)? {
                return Ok(lock);
            }
            let remaining = max_wait.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(LockError::Busy);
            }
            thread::sleep(remaining.min(POLL_INTERVAL));
        }
    }

    fn open_lock_file(&self, name: &OsStr) -> io::Result<File> {
        check_name(name)?;
        let flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let file = match fs::openat(
            &self.file,
            name,
            flags | OFlags::CREATE | OFlags::EXCL,
            PRIVATE_MODE,
        ) {
            Ok(fd) => {
                let file = File::from(fd);
                fs::fchmod(&file, PRIVATE_MODE)?;
                file
            }
            Err(rustix::io::Errno::EXIST) => {
                File::from(fs::openat(&self.file, name, flags, Mode::empty())?)
            }
            Err(error) => return Err(error.into()),
        };
        check_private_file(&file.metadata()?)?;
        Ok(file)
    }
}

fn check_service_directory(metadata: &std::fs::Metadata, final_directory: bool) -> io::Result<()> {
    let owner = metadata.uid();
    let current = rustix::process::geteuid().as_raw();
    let root_sticky = owner == 0 && metadata.mode() & 0o1000 != 0;
    if !metadata.is_dir()
        || (owner != 0 && owner != current)
        || (metadata.mode() & 0o022 != 0 && !root_sticky)
        || (final_directory && owner != current)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "service directory must be owner-controlled",
        ));
    }
    Ok(())
}

fn check_name(name: &OsStr) -> io::Result<()> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes == b"."
        || bytes == b".."
        || bytes.contains(&b'/')
        || bytes.contains(&0)
    {
        return Err(invalid_input("expected a single file basename"));
    }
    Ok(())
}

fn check_private_file(metadata: &std::fs::Metadata) -> io::Result<()> {
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file must be a private single-link regular file owned by the effective user",
        ));
    }
    Ok(())
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn size_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "private file exceeds size limit",
    )
}

struct TempCleanup<'a> {
    dir: &'a File,
    name: OsString,
    renamed: bool,
}

impl Drop for TempCleanup<'_> {
    fn drop(&mut self) {
        if !self.renamed {
            let _ = fs::unlinkat(self.dir, &self.name, AtFlags::empty());
        }
    }
}

#[derive(Debug)]
pub enum WriteError {
    BeforeCommit(io::Error),
    AfterCommit(io::Error),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BeforeCommit(error) => write!(f, "atomic write failed before rename: {error}"),
            Self::AfterCommit(error) => write!(
                f,
                "atomic write renamed target but directory sync failed: {error}"
            ),
        }
    }
}

impl std::error::Error for WriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::BeforeCommit(error) | Self::AfterCommit(error) => error,
        })
    }
}

#[derive(Debug)]
pub enum LockError {
    Busy,
    Io(io::Error),
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => write!(f, "file lock is busy"),
            Self::Io(error) => write!(f, "file lock failed: {error}"),
        }
    }
}

impl std::error::Error for LockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Busy => None,
            Self::Io(error) => Some(error),
        }
    }
}

/// Exclusive advisory lock. Keep this guard alive for the entire transaction.
/// The lock inode is deliberately never removed by this crate.
pub struct FileLock {
    file: File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::flock(&self.file, FlockOperation::Unlock);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::path::PathBuf;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().canonicalize().unwrap();
            for _ in 0..16 {
                let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
                let path = root.join(format!("hmux-core-test-{}-{id}", std::process::id()));
                let mut builder = std::fs::DirBuilder::new();
                builder.mode(0o700);
                match builder.create(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("fixture: {error}"),
                }
            }
            panic!("could not create fixture");
        }

        fn dir(&self) -> PrivateDir {
            PrivateDir::open(&self.0).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn new_private_publish_never_overwrites_concurrent_winner_or_symlink() {
        let fixture = Fixture::new();
        let dir = std::sync::Arc::new(fixture.dir());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let workers: Vec<_> = (0..4)
            .map(|index| {
                let dir = dir.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    dir.write_new_private(OsStr::new("secret"), &[index])
                        .is_ok()
                })
            })
            .collect();
        assert_eq!(
            workers
                .into_iter()
                .map(|w| usize::from(w.join().unwrap()))
                .sum::<usize>(),
            1
        );
        let initial = dir.read_private(OsStr::new("secret"), 1).unwrap();
        assert!(dir
            .write_new_private(OsStr::new("secret"), b"overwrite")
            .is_err());
        assert_eq!(dir.read_private(OsStr::new("secret"), 1).unwrap(), initial);
        std::os::unix::fs::symlink("secret", fixture.0.join("link")).unwrap();
        assert!(dir
            .write_new_private(OsStr::new("link"), b"overwrite")
            .is_err());
        assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 2);
    }

    #[test]
    fn private_child_rejects_links_nonprivate_paths_and_reuses_stable_fd() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let parent = fixture.dir();
        let child = parent.create_private_child(OsStr::new("settings")).unwrap();
        assert_eq!(
            std::fs::metadata(fixture.0.join("settings"))
                .unwrap()
                .mode()
                & 0o777,
            0o700
        );
        child
            .write_atomic_private(OsStr::new("item"), b"safe")
            .unwrap();
        assert_eq!(
            parent
                .create_private_child(OsStr::new("settings"))
                .unwrap()
                .read_private(OsStr::new("item"), 4)
                .unwrap(),
            b"safe"
        );
        for name in ["../other", ".", "a/b"] {
            assert!(parent.create_private_child(OsStr::new(name)).is_err());
        }
        symlink(fixture.0.join("settings"), fixture.0.join("link")).unwrap();
        assert!(parent.create_private_child(OsStr::new("link")).is_err());
        std::fs::set_permissions(
            fixture.0.join("settings"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(parent.create_private_child(OsStr::new("settings")).is_err());
        // Existing directory permissions must not be silently changed.
        assert_eq!(
            std::fs::metadata(fixture.0.join("settings"))
                .unwrap()
                .mode()
                & 0o777,
            0o755
        );
    }

    #[test]
    fn rejects_symlinked_and_public_directories() {
        let fixture = Fixture::new();
        let link = fixture.0.with_extension("link");
        std::os::unix::fs::symlink(&fixture.0, &link).unwrap();
        assert!(PrivateDir::open(&link).is_err());
        std::fs::remove_file(&link).unwrap();

        std::fs::set_permissions(&fixture.0, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(PrivateDir::open(&fixture.0).is_err());
    }

    #[test]
    fn private_read_checks_mode_type_symlink_and_bound() {
        let fixture = Fixture::new();
        let dir = fixture.dir();
        let file = fixture.0.join("secret");
        std::fs::write(&file, b"1234").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(dir.read_private(OsStr::new("secret"), 4).unwrap(), b"1234");
        assert_eq!(
            dir.read_private(OsStr::new("secret"), 3)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );

        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            dir.read_private(OsStr::new("secret"), 4)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o644
        );
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink("secret", fixture.0.join("alias")).unwrap();
        assert!(dir.read_private(OsStr::new("alias"), 4).is_err());
        std::fs::create_dir(fixture.0.join("subdir")).unwrap();
        assert!(dir.read_private(OsStr::new("subdir"), 4).is_err());
    }

    #[test]
    fn atomic_replace_is_private_and_precommit_failure_preserves_target() {
        let fixture = Fixture::new();
        let dir = fixture.dir();
        dir.write_atomic_private(OsStr::new("secret"), b"old")
            .unwrap();
        dir.write_atomic_private(OsStr::new("secret"), b"new")
            .unwrap();
        assert_eq!(dir.read_private(OsStr::new("secret"), 3).unwrap(), b"new");
        assert_eq!(
            std::fs::metadata(fixture.0.join("secret"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        std::fs::create_dir(fixture.0.join("blocked")).unwrap();
        let error = dir
            .write_atomic_private(OsStr::new("blocked"), b"data")
            .unwrap_err();
        assert!(matches!(error, WriteError::BeforeCommit(_)));
        assert!(fixture.0.join("blocked").is_dir());
        assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 2);
        assert!(matches!(
            dir.write_atomic_private(OsStr::new("../secret"), b"bad"),
            Err(WriteError::BeforeCommit(_))
        ));
        assert_eq!(std::fs::read(fixture.0.join("secret")).unwrap(), b"new");
    }

    #[test]
    fn post_rename_sync_failure_reports_uncertain_durability() {
        let fixture = Fixture::new();
        let dir = fixture.dir();
        let error = dir
            .write_atomic_with_sync(OsStr::new("secret"), b"new", || {
                Err(io::Error::other("synthetic sync failure"))
            })
            .unwrap_err();
        assert!(matches!(error, WriteError::AfterCommit(_)));
        assert_eq!(std::fs::read(fixture.0.join("secret")).unwrap(), b"new");
    }

    #[test]
    fn lock_contention_timeout_release_and_persistent_inode() {
        let fixture = Fixture::new();
        let dir = fixture.dir();
        let first = dir.try_lock(OsStr::new("state.lock")).unwrap().unwrap();
        let inode = std::fs::metadata(fixture.0.join("state.lock"))
            .unwrap()
            .ino();
        assert!(dir.try_lock(OsStr::new("state.lock")).unwrap().is_none());
        assert!(matches!(
            dir.lock_for(OsStr::new("state.lock"), Duration::from_millis(30)),
            Err(LockError::Busy)
        ));
        drop(first);
        let second = dir
            .lock_for(OsStr::new("state.lock"), Duration::ZERO)
            .unwrap();
        assert_eq!(
            std::fs::metadata(fixture.0.join("state.lock"))
                .unwrap()
                .ino(),
            inode
        );
        drop(second);
        assert!(fixture.0.join("state.lock").exists());
        assert_eq!(
            std::fs::metadata(fixture.0.join("state.lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn lock_rejects_symlink_and_public_existing_file() {
        let fixture = Fixture::new();
        let dir = fixture.dir();
        let path = fixture.0.join("state.lock");
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(
            dir.try_lock(OsStr::new("state.lock")).err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o666
        );
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("other", &path).unwrap();
        assert!(dir.try_lock(OsStr::new("state.lock")).is_err());
    }

    #[test]
    fn owned_reads_and_connector_lock_reject_hard_links() {
        let fixture = Fixture::new();
        let dir = fixture.dir();
        dir.write_atomic_private(OsStr::new("secret"), b"synthetic")
            .unwrap();
        std::fs::hard_link(
            fixture.0.join("secret"),
            fixture.0.join("home-connector.lock"),
        )
        .unwrap();
        assert_eq!(
            std::fs::metadata(fixture.0.join("secret")).unwrap().nlink(),
            2
        );
        assert_eq!(
            dir.read_private(OsStr::new("secret"), 64)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            dir.try_lock(OsStr::new("home-connector.lock"))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            std::fs::read(fixture.0.join("secret")).unwrap(),
            b"synthetic"
        );
    }
}
