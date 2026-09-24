//! Lifetime Home connector lock, interoperable with Go's homeservice.LockConnector.
//! The outer connector must acquire it before dialing and retain it until every
//! connection and owned worker has stopped, including all reconnect delays.

use hmux_core::{FileLock, PrivateDir};
use std::{ffi::OsStr, fmt, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    StateDirectory,
    LockFile,
    AlreadyRunning,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::StateDirectory => "Home state directory unavailable or unsafe",
            Self::LockFile => "Home connector lock unavailable or unsafe",
            Self::AlreadyRunning => "a Home connector is already running for this state directory",
        })
    }
}
impl std::error::Error for Error {}

/// The persistent inode is never removed, renamed, truncated or rewritten.
/// Dropping this owner releases the OS lock. It is deliberately not cloneable.
pub struct ConnectorLock {
    _guard: FileLock,
}
impl ConnectorLock {
    /// Synchronous startup I/O. Errors contain no path or file contents.
    pub fn acquire(state_dir: &Path) -> Result<Self, Error> {
        let directory =
            PrivateDir::open_or_create_trusted(state_dir).map_err(|_| Error::StateDirectory)?;
        let guard = directory
            .try_lock(OsStr::new("home-connector.lock"))
            .map_err(|_| Error::LockFile)?
            .ok_or(Error::AlreadyRunning)?;
        Ok(Self { _guard: guard })
    }
}
