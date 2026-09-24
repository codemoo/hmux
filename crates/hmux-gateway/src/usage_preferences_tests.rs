use super::*;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::AtomicU64,
    time::{Duration, SystemTime},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-e2e-preferences-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn store(&self) -> Store {
        Store::new(PrivateDir::open(&self.0).unwrap())
    }
    fn path(&self, access: &SessionAccess) -> PathBuf {
        self.0.join(filename(&key(access)))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn access(name: &str, profile: &str) -> SessionAccess {
    static LIVE: std::sync::OnceLock<tokio::sync::watch::Sender<bool>> = std::sync::OnceLock::new();
    let cancelled = LIVE
        .get_or_init(|| tokio::sync::watch::channel(false).0)
        .subscribe();
    SessionAccess {
        id: "synthetic-id".into(),
        expires_at: (SystemTime::now() + Duration::from_secs(3600)).into(),
        csrf: "synthetic-csrf".into(),
        username: name.into(),
        profile: profile.into(),
        cancelled,
    }
}

#[tokio::test]
async fn revoked_queued_writes_and_expired_or_closed_access_cannot_commit() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let (cancel, cancelled) = tokio::sync::watch::channel(false);
    let owner = SessionAccess {
        cancelled,
        ..access("one", "")
    };
    let (started, start) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let blocker = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .transact(move |_| {
                    started.send(()).unwrap();
                    wait.recv().unwrap();
                    Ok(Preferences::default())
                })
                .await
        }
    });
    start.await.unwrap();
    let pending = tokio::spawn({
        let store = store.clone();
        let owner = owner.clone();
        async move { store.set(&owner, Preferences::default()).await }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while store.inner.admission.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancel.send_replace(true);
    release.send(()).unwrap();
    blocker.await.unwrap().unwrap();
    assert_eq!(pending.await.unwrap(), Err(Error::Unauthorized));
    assert!(!fixture.path(&owner).exists());
    let expired = SessionAccess {
        expires_at: (SystemTime::now() - Duration::from_secs(1)).into(),
        ..access("expired", "")
    };
    assert_eq!(
        store.set(&expired, Preferences::default()).await,
        Err(Error::Unauthorized)
    );
    cancel.send_replace(false);
    drop(cancel);
    assert_eq!(
        store.set(&owner, Preferences::default()).await,
        Err(Error::Unauthorized)
    );
    assert!(!fixture.path(&owner).exists());
    assert!(!fixture.path(&expired).exists());
    store.shutdown().await;
}

#[tokio::test]
async fn settings_survive_restart_and_remain_account_and_profile_scoped() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let owner = access("one", "profile-one");
    let guest = access("two", "profile-two");
    let mut next = Preferences::default();
    next.claude.enabled = false;
    next.codex.source = "cli".into();
    let saved = store.set(&owner, next.clone()).await.unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(store.set(&owner, next).await, Err(Error::Conflict));
    assert_eq!(store.get(&guest).await.unwrap(), Preferences::default());
    assert_eq!(
        store.get(&access("one", "different")).await.unwrap(),
        Preferences::default()
    );
    store.shutdown().await;
    let reloaded = fixture.store();
    assert_eq!(reloaded.get(&owner).await.unwrap(), saved);
    assert_eq!(
        fs::metadata(fixture.path(&owner))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        Preferences::decode(&fs::read(fixture.path(&owner)).unwrap()).unwrap(),
        saved
    );
    if let Some(path) = std::env::var_os("HMUX_RUST_PREFERENCES_HANDOFF") {
        let store =
            Store::new(PrivateDir::open(&PathBuf::from(path).canonicalize().unwrap()).unwrap());
        let mut next = saved.clone();
        next.revision = 0;
        assert_eq!(store.set(&owner, next).await.unwrap(), saved);
        store.shutdown().await;
    }
}

#[tokio::test]
#[ignore = "requires isolated Rust/Go handoff (external artifact; see tests/RUST.md)"]
async fn reload_current_preferences_after_go_update() {
    let path =
        std::env::var_os("HMUX_RUST_PREFERENCES_HANDOFF").expect("handoff directory required");
    let store = Store::new(PrivateDir::open(&PathBuf::from(path).canonicalize().unwrap()).unwrap());
    let saved = store.get(&access("one", "profile-one")).await.unwrap();
    assert_eq!(saved.revision, 2);
    assert!(!saved.claude.enabled);
    assert!(!saved.codex.enabled);
    assert_eq!(saved.claude.source, "cswap");
    assert_eq!(saved.codex.source, "cli");
    store.shutdown().await;
}

#[test]
fn go_oracle_matches_defaults_storage_key_json_and_decode_contracts() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/usage-preferences-v1/go-oracle.json"
    ))
    .unwrap();
    let owner = access(
        oracle["username"].as_str().unwrap(),
        oracle["profile"].as_str().unwrap(),
    );
    assert_eq!(
        filename(&key(&owner)),
        format!("{}.json", oracle["key"].as_str().unwrap())
    );
    assert_eq!(
        serde_json::to_value(Preferences::default()).unwrap(),
        oracle["default"]
    );
    let saved = Preferences::decode(oracle["saved_json"].as_str().unwrap().as_bytes()).unwrap();
    assert_eq!(serde_json::to_value(&saved).unwrap(), oracle["saved"]);
    assert_eq!(
        serde_json::to_string(&saved).unwrap(),
        oracle["saved_json"].as_str().unwrap()
    );
    for case in oracle["cases"].as_array().unwrap() {
        assert_eq!(
            Preferences::decode(case["json"].as_str().unwrap().as_bytes()).is_ok(),
            case["valid"].as_bool().unwrap(),
            "{case}"
        );
    }
}

#[tokio::test]
async fn concurrent_stale_writers_commit_one_revision_and_external_edits_conflict() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let owner = access("one", "");
    let (a, b) = tokio::join!(
        store.set(&owner, Preferences::default()),
        store.set(&owner, Preferences::default())
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(a == Err(Error::Conflict) || b == Err(Error::Conflict));
    let saved = store.get(&owner).await.unwrap();
    let mut changed = saved.clone();
    changed.codex.enabled = false;
    fs::write(fixture.path(&owner), serde_json::to_vec(&changed).unwrap()).unwrap();
    assert_eq!(store.set(&owner, saved).await, Err(Error::Conflict));
    assert_eq!(store.get(&owner).await.unwrap(), changed);
}

#[tokio::test]
async fn corrupt_or_nonprivate_original_is_not_overwritten_even_after_cache() {
    let fixture = Fixture::new();
    let owner = access("one", "");
    for raw in [b"{}".as_slice(), &vec![b'x'; MAX_BYTES + 1]] {
        let store = fixture.store();
        fs::write(fixture.path(&owner), raw).unwrap();
        fs::set_permissions(fixture.path(&owner), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(store.get(&owner).await, Err(Error::Unavailable));
        assert_eq!(
            store.set(&owner, Preferences::default()).await,
            Err(Error::Unavailable)
        );
        assert_eq!(fs::read(fixture.path(&owner)).unwrap(), raw);
    }
    fs::remove_file(fixture.path(&owner)).unwrap();
    let store = fixture.store();
    let saved = store.set(&owner, Preferences::default()).await.unwrap();
    let original = fs::read(fixture.path(&owner)).unwrap();
    fs::set_permissions(fixture.path(&owner), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        store.set(&owner, saved.clone()).await,
        Err(Error::Unavailable)
    );
    fs::set_permissions(fixture.path(&owner), fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(fixture.path(&owner), fixture.0.join("extra-link")).unwrap();
    assert_eq!(store.set(&owner, saved).await, Err(Error::Unavailable));
    assert_eq!(fs::read(fixture.path(&owner)).unwrap(), original);
}

#[tokio::test]
async fn cancelled_callers_keep_actual_work_admission_and_shutdown_joins_it() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let owner = access("one", "");
    // One worker holds the state mutex; another is admitted waiting for it.
    let (started, start) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let first = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .transact(move |_| {
                    let _ = started.send(());
                    wait.recv().unwrap();
                    Ok(Preferences::default())
                })
                .await
        }
    });
    start.await.unwrap();
    first.abort();
    let _ = first.await;
    let second = tokio::spawn({
        let store = store.clone();
        async move { store.get(&access("two", "")).await }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while store.inner.admission.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    second.abort();
    let _ = second.await;
    assert_eq!(store.get(&owner).await, Err(Error::Busy));
    let closing = tokio::spawn({
        let store = store.clone();
        async move { store.shutdown().await }
    });
    tokio::task::yield_now().await;
    assert!(!closing.is_finished());
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), closing)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(store.get(&owner).await, Err(Error::Unavailable));
    assert_eq!(store.inner.admission.available_permits(), IO_WORKERS);
}

#[tokio::test]
async fn cache_is_bounded_and_hits_need_no_io_worker() {
    let fixture = Fixture::new();
    let store = fixture.store();
    for i in 0..CACHE_ACCOUNTS + 3 {
        store
            .get(&access(&format!("account-{i}"), ""))
            .await
            .unwrap();
    }
    assert_eq!(
        store.inner.state.lock().unwrap().cache.len(),
        CACHE_ACCOUNTS
    );
    let permits = store
        .inner
        .admission
        .clone()
        .try_acquire_many_owned(IO_WORKERS as u32)
        .unwrap();
    assert_eq!(
        store.get(&access("account-11", "")).await.unwrap(),
        Preferences::default()
    );
    assert_eq!(
        store.get(&access("evicted-or-new", "")).await,
        Err(Error::Busy)
    );
    drop(permits);
}

#[test]
fn decode_rejects_missing_unknown_positional_and_out_of_range_preferences() {
    let good = serde_json::to_value(Preferences::default()).unwrap();
    for bad in [
        serde_json::json!({}),
        serde_json::json!([]),
        {
            let mut v = good.clone();
            v["codex"] = serde_json::json!([true, "cli"]);
            v
        },
        {
            let mut v = good.clone();
            v["revision"] = serde_json::json!(1_u64 << 53);
            v
        },
        {
            let mut v = good.clone();
            v["claude"]["source"] = serde_json::json!("codex-lb");
            v
        },
        {
            let mut v = good.clone();
            v["unknown"] = serde_json::json!(true);
            v
        },
    ] {
        assert_eq!(
            Preferences::decode(&serde_json::to_vec(&bad).unwrap()),
            Err(Error::Invalid)
        );
    }
    let mut nullable = good.clone();
    nullable["revision"] = serde_json::Value::Null;
    nullable["codex"]["enabled"] = serde_json::Value::Null;
    let parsed = Preferences::decode(&serde_json::to_vec(&nullable).unwrap()).unwrap();
    assert_eq!(parsed.revision, 0);
    assert!(!parsed.codex.enabled);
}
