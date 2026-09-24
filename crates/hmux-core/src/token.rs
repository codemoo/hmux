//! Shared connector-token reader. No generation, logging or credential mutation.
use crate::PrivateDir;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use std::{io, path::Path};

pub fn valid(value: &str) -> bool {
    value.len() == 43
        && URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|bytes| bytes.len() == 32)
}

/// Synchronous startup-only read: at most 256 bytes, private single-link regular
/// file, no symlink ancestors, no content-bearing errors. The canonical unpadded
/// 32-byte token format matches tokens emitted by Go enrollment. Noncanonical
/// hand-edited base64 (interior newlines or nonzero padding bits) is rejected.
pub fn load(path: &Path) -> io::Result<String> {
    let read = || {
        let parent = path.parent().ok_or_else(invalid)?;
        let name = path.file_name().ok_or_else(invalid)?;
        let raw = PrivateDir::open(parent)?.read_private(name, 256)?;
        let raw = String::from_utf8(raw).map_err(|_| invalid())?;
        let value = raw.trim();
        if !valid(value) {
            return Err(invalid());
        }
        Ok(value.to_owned())
    };
    read().map_err(|_: io::Error| invalid())
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "connector token unavailable or invalid",
    )
}
