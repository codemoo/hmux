//! Bounded private lifecycle logs. A writer owns one current and one rotated
//! file, each at most 1 MiB. All operations use a pinned trusted directory fd.
use crate::{check_name, check_private_file, PrivateDir};
use rustix::fs::{self, AtFlags, Mode, OFlags};
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Write},
    path::Path,
};
pub const LIMIT: usize = 1 << 20;
pub struct Log {
    dir: PrivateDir,
    name: OsString,
    file: Option<File>,
    size: u64,
}
impl Log {
    pub fn open(path: &Path) -> io::Result<Self> {
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("invalid log path"))?;
        check_name(name)?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("invalid log directory"))?;
        let mut log = Self {
            dir: PrivateDir::open_or_create_trusted(parent)?,
            name: name.to_owned(),
            file: None,
            size: 0,
        };
        log.reopen()?;
        Ok(log)
    }
    fn reopen(&mut self) -> io::Result<()> {
        let file = File::from(fs::openat(
            &self.dir.file,
            &self.name,
            OFlags::WRONLY
                | OFlags::APPEND
                | OFlags::CREATE
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )?);
        let info = file.metadata()?;
        check_private_file(&info)?;
        if info.len() > LIMIT as u64 {
            return Err(io::Error::other("unsafe service log size"));
        }
        self.size = info.len();
        self.file = Some(file);
        Ok(())
    }
    fn rotate(&mut self) -> io::Result<()> {
        let mut previous = self.name.clone();
        previous.push(".1");
        // Validate existing destination through its inode before replacement.
        match self.dir.read_private(&previous, LIMIT) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // Refuse to rotate an unrelated replacement of the current pathname.
        use std::os::unix::fs::MetadataExt;
        let path = fs::statat(&self.dir.file, &self.name, AtFlags::SYMLINK_NOFOLLOW)?;
        let file = self
            .file
            .as_ref()
            .ok_or_else(|| io::Error::other("log closed"))?
            .metadata()?;
        if path.st_dev as u64 != file.dev() || path.st_ino != file.ino() {
            return Err(io::Error::other("service log identity changed"));
        }
        self.file.take();
        fs::renameat(&self.dir.file, &self.name, &self.dir.file, &previous)?;
        self.reopen()?;
        self.dir.file.sync_all()
    }
}
impl Write for Log {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > LIMIT {
            return Err(io::Error::other("log entry exceeds limit"));
        }
        if self.size + bytes.len() as u64 > LIMIT as u64 {
            self.rotate()?;
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("log closed"))?;
        let written = file.write(bytes)?;
        self.size += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("log closed"))?
            .flush()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, DirBuilderExt, MetadataExt};
    #[test]
    fn bounds_rotation_and_rejects_untrusted_files_without_modifying_them() {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("hmux-log-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let path = root.join("home.log");
        let mut log = Log::open(&path).unwrap();
        log.write_all(&vec![b'x'; LIMIT]).unwrap();
        log.write_all(b"next").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 4);
        assert_eq!(
            std::fs::metadata(root.join("home.log.1")).unwrap().len(),
            LIMIT as u64
        );
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert!(log.write(&vec![0; LIMIT + 1]).is_err());
        drop(log);
        symlink("home.log", root.join("link")).unwrap();
        assert!(Log::open(&root.join("link")).is_err());
        std::fs::hard_link(&path, root.join("hard")).unwrap();
        assert!(Log::open(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"next");
        std::fs::remove_dir_all(root).unwrap();
    }
}
