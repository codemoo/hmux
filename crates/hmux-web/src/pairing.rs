//! Private, explicitly transferred Home connection file. No network discovery or secret output.
use crate::args::invalid;
use hmux_core::PrivateDir;
use serde::Deserialize;
use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionFile {
    schema: u32,
    pub endpoint: String,
    token: String,
}

impl ConnectionFile {
    pub fn retain(&self, home: &Path) -> io::Result<PathBuf> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(io::Error::other)?;
        let parent = home
            .join(".config/hmux/connections")
            .join(format!("import-{:032x}", u128::from_le_bytes(nonce)));
        let dir = PrivateDir::open_or_create_trusted(&parent)?;
        let bytes = serde_json::to_vec(
            &serde_json::json!({"schema":1,"endpoint":self.endpoint,"token":self.token}),
        )
        .map_err(|_| invalid("could not save connection file"))?;
        dir.write_new_private(OsStr::new("home-connection.json"), &bytes)
            .map_err(io::Error::other)?;
        Ok(parent.join("home-connection.json"))
    }
    pub fn read(path: &Path) -> io::Result<Self> {
        let result = || {
            let dir = PrivateDir::open_existing_trusted(
                path.parent()
                    .ok_or_else(|| invalid("invalid connection file"))?,
            )?;
            let bytes = dir
                .read_private(
                    path.file_name()
                        .ok_or_else(|| invalid("invalid connection file"))?,
                    8192,
                )
                .map_err(io::Error::other)?;
            let file: Self =
                serde_json::from_slice(&bytes).map_err(|_| invalid("invalid connection file"))?;
            if file.schema != 1
                || !hmux_core::token::valid(&file.token)
                || hmux_home::dial::Endpoint::parse(&file.endpoint).is_err()
            {
                return Err(invalid("invalid connection file"));
            }
            Ok(file)
        };
        result().map_err(|_: io::Error| invalid("connection file must be private, owner-controlled and valid; credentials were not changed"))
    }

    pub fn check_target(&self, config: &Path) -> io::Result<()> {
        let path = config.join("web/connector.token");
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
            Ok(_) => self.matches(&path),
        }
    }

    fn matches(&self, path: &Path) -> io::Result<()> {
        if hmux_core::token::load(path).is_ok_and(|v| v == self.token) {
            Ok(())
        } else {
            Err(invalid("Home already has a different or unsafe connector token; use a separate --config-dir or resolve the existing connection first"))
        }
    }

    pub fn install_token(&self, config: &Path) -> io::Result<PathBuf> {
        let dir = config.join("web");
        let owner = PrivateDir::open_or_create_trusted(&dir)?;
        let path = dir.join("connector.token");
        // Publish once, never overwrite an existing or concurrently created token.
        if owner
            .write_new_private(
                OsStr::new("connector.token"),
                format!("{}\n", self.token).as_bytes(),
            )
            .is_err()
        {
            self.matches(&path)?;
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
    };
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let mut nonce = [0; 8];
            getrandom::fill(&mut nonce).unwrap();
            let p = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "hmux-e2e-pairing-{}-{}",
                std::process::id(),
                u64::from_le_bytes(nonce)
            ));
            fs::DirBuilder::new().mode(0o700).create(&p).unwrap();
            Self(p)
        }
        fn file(&self, token: &str) -> PathBuf {
            let p = self.0.join("connection.json");
            fs::write(&p, serde_json::json!({"schema":1,"endpoint":"wss://hmux.example/connect","token":token}).to_string()).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
            p
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn imports_once_and_preserves_existing_token_on_mismatch() {
        let f = Fixture::new();
        let file = f.file(&"A".repeat(43));
        let first = ConnectionFile::read(&file).unwrap();
        let cfg = f.0.join("config");
        first.check_target(&cfg).unwrap();
        let target = first.install_token(&cfg).unwrap();
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        first.install_token(&cfg).unwrap();
        let previous = fs::read(&target).unwrap();
        let other = ConnectionFile::read(&f.file(&format!("{}E", "B".repeat(42)))).unwrap();
        assert!(other.check_target(&cfg).is_err());
        assert!(other.install_token(&cfg).is_err());
        assert_eq!(fs::read(&target).unwrap(), previous);
    }
    #[test]
    fn rejects_untrusted_malformed_and_symlink_files_without_secret_errors() {
        let f = Fixture::new();
        let path = f.file(&"A".repeat(43));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(ConnectionFile::read(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let link = f.0.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(ConnectionFile::read(&link).is_err());
        for raw in ["secret-not-json", "{}", &"x".repeat(8193)] {
            fs::write(&path, raw).unwrap();
            let error = ConnectionFile::read(&path).err().unwrap().to_string();
            assert!(!error.contains(raw));
        }
    }
}
