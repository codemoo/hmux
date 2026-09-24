use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::sync::atomic::{AtomicU64, Ordering};
static ID: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-e2e-usage-auth-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::create_dir(path.join(".codex")).unwrap();
        Self(path)
    }
    fn path(&self) -> PathBuf {
        self.0.join(".codex/auth.json")
    }
    fn write(&self, token: &str) {
        std::fs::write(self.path(), format!(r#"{{"OPENAI_API_KEY":"{token}"}}"#)).unwrap();
        std::fs::set_permissions(self.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn cache_external_rotation_and_missing_authority() {
    let temp = Temp::new();
    temp.write("first-synthetic");
    let store = Store::new(&temp.0).unwrap();
    let cancel = CancellationToken::new();
    let first = store.load(Provider::Codex, false, &cancel).await.unwrap();
    let same = store.load(Provider::Codex, false, &cancel).await.unwrap();
    assert!(Arc::ptr_eq(&first, &same));
    let old = temp.0.join("old");
    std::fs::rename(temp.path(), &old).unwrap();
    temp.write("other-synthetic");
    let rotated = store.load(Provider::Codex, false, &cancel).await.unwrap();
    assert_eq!(rotated.access_token(), "other-synthetic");
    assert!(!Arc::ptr_eq(&first, &rotated));
    std::fs::remove_file(temp.path()).unwrap();
    assert_eq!(
        store
            .load(Provider::Codex, false, &cancel)
            .await
            .unwrap_err(),
        Failure::CredentialMissing
    );
    assert!(store.0.cache[1].lock().unwrap().is_none());
    temp.write("third-synthetic");
    assert_eq!(
        store
            .load(Provider::Codex, false, &cancel)
            .await
            .unwrap()
            .access_token(),
        "third-synthetic"
    );
    cancel.cancel();
    assert_eq!(
        store
            .load(Provider::Codex, false, &cancel)
            .await
            .unwrap_err(),
        Failure::CredentialIo
    );
}

#[test]
fn unsafe_files_and_parents_are_rejected_without_blocking() {
    let temp = Temp::new();
    temp.write("fake");
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(inspect(&temp.path()).is_err());
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(temp.path(), temp.0.join("hard")).unwrap();
    assert!(inspect(&temp.path()).is_err());
    std::fs::remove_file(temp.0.join("hard")).unwrap();
    assert!(inspect(&temp.path()).is_ok());
    std::fs::rename(temp.path(), temp.0.join("original")).unwrap();
    symlink(temp.0.join("original"), temp.path()).unwrap();
    assert!(inspect(&temp.path()).is_err());
    std::fs::remove_file(temp.path()).unwrap();
    assert!(std::process::Command::new("mkfifo")
        .arg(temp.path())
        .status()
        .unwrap()
        .success());
    assert!(inspect(&temp.path()).is_err());
    std::fs::remove_file(temp.path()).unwrap();
    std::fs::remove_dir(temp.0.join(".codex")).unwrap();
    std::fs::create_dir(temp.0.join("elsewhere")).unwrap();
    symlink(temp.0.join("elsewhere"), temp.0.join(".codex")).unwrap();
    temp.write("fake");
    assert!(inspect(&temp.path()).is_err());
}

#[test]
fn replacement_during_read_retries_once_and_never_returns_obsolete_token() {
    let temp = Temp::new();
    temp.write("first");
    let mut cache = None;
    let mut replaced = false;
    let token = read_current(
        &temp.path(),
        Provider::Codex,
        true,
        &mut cache,
        &CancellationToken::new(),
        Instant::now() + TIMEOUT,
        || {
            if !replaced {
                std::fs::rename(temp.path(), temp.0.join("old")).unwrap();
                temp.write("second");
                replaced = true;
            }
        },
    )
    .unwrap();
    assert_eq!(token.access_token(), "second");
    let mut times = 0;
    assert_eq!(
        read_current(
            &temp.path(),
            Provider::Codex,
            true,
            &mut cache,
            &CancellationToken::new(),
            Instant::now() + TIMEOUT,
            || {
                std::fs::rename(temp.path(), temp.0.join(format!("replaced-{times}"))).unwrap();
                temp.write("third");
                times += 1;
            }
        )
        .unwrap_err(),
        Failure::CredentialIo
    );
    assert_eq!(times, 2);
}

#[test]
fn raw_size_and_malformed_data_are_bounded() {
    let temp = Temp::new();
    let file = File::create(temp.path()).unwrap();
    file.set_len(LIMIT + 1).unwrap();
    assert!(inspect(&temp.path()).is_err());
    temp.write("fake");
    std::fs::write(temp.path(), b"{broken").unwrap();
    assert_eq!(
        read_current(
            &temp.path(),
            Provider::Codex,
            false,
            &mut None,
            &CancellationToken::new(),
            Instant::now() + TIMEOUT,
            || {}
        )
        .unwrap_err(),
        Failure::CredentialMalformed
    );
}
