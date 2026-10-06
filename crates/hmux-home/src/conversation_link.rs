//! Explicit administrator-selected transcripts, never an inferred daemon binding.
//! Catalog/recovery keep exact FD bindings; notifications need a separate explicit opt-in.
use crate::{
    binding::{Binding, Provider, Status},
    inspection::Error,
    records,
};
use hmux_core::PrivateDir;
use hmux_model::SessionIdentity;
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

const LIMIT: usize = 8192;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Link {
    version: u32,
    identity: SessionIdentity,
    pane: i32,
    provider_pid: i32,
    process_stamp: String,
    record_id: String,
    path: PathBuf,
    device: u64,
    inode: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    notification_key: String,
}
fn name(identity: &SessionIdentity) -> Result<String, Error> {
    hmux_model::validate_session_id(&identity.id).map_err(|_| Error::Invalid)?;
    if identity.created_at < 1 {
        return Err(Error::Invalid);
    }
    Ok(format!("{}.json", identity.id))
}
pub(crate) fn load(state: &Path, identity: &SessionIdentity) -> Result<Option<Link>, Error> {
    let name = name(identity)?;
    let dir = match PrivateDir::open_existing_trusted(&state.join("conversation-links")) {
        Ok(dir) => dir,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Error::Unavailable),
    };
    match dir.read_private(OsStr::new(&name), LIMIT) {
        Ok(raw) => serde_json::from_slice(&raw).map_err(|_| Error::Unavailable),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Error::Unavailable),
    }
}
pub(crate) fn save(
    state: &Path,
    identity: &SessionIdentity,
    link: Option<&Link>,
) -> Result<(), Error> {
    save_checked(state, identity, link, None)
}
pub(crate) fn snapshot(state: &Path, identity: &SessionIdentity) -> Result<Option<Vec<u8>>, Error> {
    let name = name(identity)?;
    let dir = match PrivateDir::open_existing_trusted(&state.join("conversation-links")) {
        Ok(dir) => dir,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Error::Unavailable),
    };
    match dir.read_private(OsStr::new(&name), LIMIT) {
        Ok(raw) => Ok(Some(raw)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Error::Unavailable),
    }
}
pub(crate) fn save_recovered(
    state: &Path,
    identity: &SessionIdentity,
    link: &Link,
    previous: Option<Vec<u8>>,
) -> Result<(), Error> {
    save_checked(state, identity, Some(link), Some(previous))
}
fn save_checked(
    state: &Path,
    identity: &SessionIdentity,
    link: Option<&Link>,
    expected: Option<Option<Vec<u8>>>,
) -> Result<(), Error> {
    let name = name(identity)?;
    let raw = serde_json::to_vec(&link).map_err(|_| Error::Invalid)?;
    if raw.len() > LIMIT {
        return Err(Error::Invalid);
    }
    let dir = PrivateDir::open_or_create_trusted(state)
        .and_then(|d| d.create_private_child(OsStr::new("conversation-links")))
        .map_err(|_| Error::Unavailable)?;
    let _lock = dir
        .lock_for(OsStr::new("links.lock"), std::time::Duration::from_secs(1))
        .map_err(|_| Error::Busy)?;
    let existing = match dir.read_private(OsStr::new(&name), LIMIT) {
        Ok(raw) => Some(raw),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(_) => return Err(Error::Unavailable),
    };
    if expected.is_some_and(|previous| previous != existing) {
        return Err(Error::Unavailable);
    }
    if link.is_none() {
        if let Some(raw) = existing {
            let previous: Option<Link> =
                serde_json::from_slice(&raw).map_err(|_| Error::Unavailable)?;
            if previous.is_some_and(|p| p.identity != *identity) {
                return Err(Error::Invalid);
            }
            rustix::fs::unlinkat(&dir, OsStr::new(&name), rustix::fs::AtFlags::empty())
                .map_err(|_| Error::Unavailable)?;
            rustix::fs::fsync(&dir).map_err(|_| Error::Unavailable)?;
        }
        return Ok(());
    }
    // No background collector: cap explicit retained records at 128 (at most 1 MiB).
    if existing.is_none() {
        let entries = rustix::fs::Dir::read_from(&dir).map_err(|_| Error::Unavailable)?;
        let mut count = 0;
        for entry in entries {
            let entry = entry.map_err(|_| Error::Unavailable)?;
            if [b".".as_slice(), b"..", b"links.lock"].contains(&entry.file_name().to_bytes()) {
                continue;
            }
            count += 1;
            if count >= 128 {
                return Err(Error::Unavailable);
            }
        }
    }
    dir.write_atomic_private(OsStr::new(&name), &raw)
        .map_err(|_| Error::Unavailable)
}
impl Link {
    pub(crate) fn enable_notifications(&mut self) -> Result<(), Error> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| Error::Unavailable)?;
        self.notification_key = nonce.iter().map(|b| format!("{b:02x}")).collect();
        Ok(())
    }
    pub(crate) fn notification_owner(&self) -> Option<String> {
        (self.notification_key.len() == 32
            && self.notification_key.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("link:{}:{}", self.process_stamp, self.notification_key))
    }
    pub(crate) fn new(
        identity: SessionIdentity,
        pane: i32,
        base: &Binding,
        stamp: String,
        record: (String, PathBuf),
        stop: &CancellationToken,
        deadline: Instant,
    ) -> Result<Self, Error> {
        name(&identity)?;
        if base.provider != Provider::Codex
            || base.provider_pid < 1
            || pane < 1
            || stamp.is_empty()
            || stamp.len() > 128
            || record.0.len() > 128
            || record.1.as_os_str().len() > 4096
        {
            return Err(Error::Invalid);
        }
        // file_pid=0 explicitly represents selection, not a claim of an open FD.
        let bound = records::bind_codex(
            Binding::unavailable(Provider::Codex, base.provider_pid),
            0,
            std::slice::from_ref(&record.1),
            false,
            stop,
            deadline,
        );
        if bound.status != Status::Ready || bound.record_id != record.0 {
            return Err(Error::Unavailable);
        }
        let stat = records::open_record(&bound.root, &bound.path)
            .and_then(|f| f.metadata().map_err(|_| records::Error::Unavailable))
            .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            version: 1,
            identity,
            pane,
            provider_pid: base.provider_pid,
            process_stamp: stamp,
            record_id: record.0,
            path: record.1,
            device: stat.dev(),
            inode: stat.ino(),
            notification_key: String::new(),
        })
    }
    pub(crate) fn matches(&self, identity: &SessionIdentity, pane: i32, base: &Binding) -> bool {
        self.version == 1
            && self.identity == *identity
            && self.pane == pane
            && base.provider == Provider::Codex
            && self.provider_pid == base.provider_pid
    }
    pub(crate) fn resolve(
        &self,
        stamp: &str,
        stop: &CancellationToken,
        deadline: Instant,
    ) -> Result<Binding, Error> {
        if self.process_stamp != stamp {
            return Err(Error::Unavailable);
        }
        let bound = records::bind_codex(
            Binding::unavailable(Provider::Codex, self.provider_pid),
            0,
            std::slice::from_ref(&self.path),
            false,
            stop,
            deadline,
        );
        if bound.status != Status::Ready || bound.record_id != self.record_id {
            return Err(Error::Unavailable);
        }
        let stat = records::open_record(&bound.root, &bound.path)
            .and_then(|f| f.metadata().map_err(|_| records::Error::Unavailable))
            .map_err(|_| Error::Unavailable)?;
        if stat.dev() != self.device || stat.ino() != self.inode {
            return Err(Error::Unavailable);
        }
        Ok(bound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
        time::Duration,
    };
    #[test]
    fn selected_record_validates_identity_storage_and_replacement() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("hmux-link-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let path = dir.join("sessions/2026/09/27/rollout-example-thread-one.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let header =
            b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"thread-one\",\"source\":\"cli\"}}\n";
        fs::write(&path, header).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let identity = SessionIdentity {
            id: "$7".into(),
            created_at: 17,
        };
        let base = Binding::unavailable(Provider::Codex, 90);
        let stop = CancellationToken::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        assert!(Link::new(
            identity.clone(),
            80,
            &base,
            "stamp".into(),
            ("wrong".into(), path.clone()),
            &stop,
            deadline
        )
        .is_err());
        let link = Link::new(
            identity.clone(),
            80,
            &base,
            "stamp".into(),
            ("thread-one".into(), path.clone()),
            &stop,
            deadline,
        )
        .unwrap();
        assert!(link.matches(&identity, 80, &base));
        assert!(!link.matches(&identity, 81, &base));
        assert!(!link.matches(&identity, 80, &Binding::unavailable(Provider::Codex, 91)));
        assert!(!link.matches(
            &SessionIdentity {
                id: "$7".into(),
                created_at: 18
            },
            80,
            &base
        ));
        assert!(link.resolve("other-start", &stop, deadline).is_err());
        assert_eq!(link.resolve("stamp", &stop, deadline).unwrap().file_pid, 0);
        save(&dir, &identity, Some(&link)).unwrap();
        let previous = snapshot(&dir, &identity).unwrap();
        assert!(save_recovered(&dir, &identity, &link, None).is_err());
        assert!(save_recovered(&dir, &identity, &link, previous.clone()).is_ok());
        let mut newer = Link::new(
            identity.clone(),
            80,
            &base,
            "stamp".into(),
            ("thread-one".into(), path.clone()),
            &stop,
            deadline,
        )
        .unwrap();
        newer.enable_notifications().unwrap();
        save(&dir, &identity, Some(&newer)).unwrap();
        assert!(save_recovered(&dir, &identity, &link, previous).is_err());
        assert!(load(&dir, &identity)
            .unwrap()
            .unwrap()
            .notification_owner()
            .is_some());
        assert_eq!(
            fs::metadata(dir.join("conversation-links/$7.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(load(&dir, &identity)
            .unwrap()
            .unwrap()
            .resolve("stamp", &stop, deadline)
            .is_ok());
        save(&dir, &identity, None).unwrap();
        assert!(load(&dir, &identity).unwrap().is_none());
        assert!(!dir.join("conversation-links/$7.json").exists());
        let old = path.with_extension("old");
        fs::rename(&path, &old).unwrap();
        fs::write(&path, header).unwrap();
        assert!(link.resolve("stamp", &stop, deadline).is_err());
        fs::remove_file(&path).unwrap();
        symlink(&old, &path).unwrap();
        assert!(link.resolve("stamp", &stop, deadline).is_err());
        for i in 0..128 {
            fs::write(
                dir.join(format!("conversation-links/${}.json", i + 1000)),
                b"null",
            )
            .unwrap();
        }
        assert!(save(&dir, &identity, Some(&link)).is_err());
        fs::remove_file(dir.join("conversation-links/$1000.json")).unwrap();
        save(&dir, &identity, Some(&link)).unwrap();
        assert!(save(
            &dir,
            &SessionIdentity {
                id: "$7".into(),
                created_at: 18
            },
            None
        )
        .is_err());
        assert!(dir.join("conversation-links/$7.json").exists());
        save(&dir, &identity, None).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }
}
