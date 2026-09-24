use super::*;
use hmux_model::SessionIdentity;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::AtomicU64,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-e2e-workspace-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(root)
    }
    fn store(&self) -> Store {
        Store::new(PrivateDir::open(&self.0).unwrap())
    }
    fn dir(&self) -> PrivateDir {
        PrivateDir::open(&self.0)
            .unwrap()
            .create_private_child(OsStr::new("shared-workspace"))
            .unwrap()
    }
    fn file(&self) -> PathBuf {
        self.0.join("shared-workspace/workspace.json")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn id(n: usize) -> SessionIdentity {
    SessionIdentity {
        id: format!("${n}"),
        created_at: 100 + n as i64,
    }
}
fn catalog() -> Vec<SessionLineage> {
    (1..=3)
        .map(|n| {
            let id = id(n);
            SessionLineage {
                id: id.id,
                created_at: id.created_at,
                ..SessionLineage::default()
            }
        })
        .collect()
}
fn open(n: usize) -> Change {
    Change {
        operation_id: format!("operation-open-{n:04}"),
        tabs: vec![id(n)],
        ..Change::default()
    }
}

#[tokio::test]
async fn real_storage_matches_go_oracle_and_polls_do_not_rewrite() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/workspace-v1/go-oracle.json"
    ))
    .unwrap();
    for row in oracle["syncs"].as_array().unwrap() {
        let change = serde_json::from_value(row["change"].clone()).unwrap();
        let sessions: Option<Vec<SessionLineage>> =
            serde_json::from_value(row["sessions"].clone()).unwrap();
        let reply = store
            .sync(
                None,
                change,
                move || Ok(sessions.unwrap_or_default()),
                || true,
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&reply).unwrap(),
            row["result"],
            "{}",
            row["name"]
        );
        if fixture.file().exists() {
            assert!(Snapshot::decode(&fs::read(fixture.file()).unwrap())
                .unwrap()
                .conflict
                .is_empty());
        }
    }
    let inode = fs::metadata(fixture.file()).unwrap().ino();
    store
        .sync(None, None, || Ok(vec![]), || true)
        .await
        .unwrap();
    assert_eq!(fs::metadata(fixture.file()).unwrap().ino(), inode);
    store.shutdown().await;
}

#[tokio::test]
async fn restart_scopes_and_current_workspace_handoff() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let saved = store
        .sync(None, Some(open(1)), || Ok(catalog()), || true)
        .await
        .unwrap();
    assert_eq!(saved.revision, 1);
    let bad = store
        .sync(
            None,
            None,
            || {
                Ok(vec![SessionLineage {
                    id: "$9".into(),
                    created_at: 0,
                    restored_from: Some(id(1)),
                }])
            },
            || true,
        )
        .await;
    assert_eq!(bad, Err(Error::Invalid));
    assert_eq!(
        Snapshot::decode(&fs::read(fixture.file()).unwrap()).unwrap(),
        saved
    );
    let account = "a".repeat(64);
    let other = "b".repeat(64);
    let guest = store
        .sync(
            Some(account.clone()),
            Some(open(2)),
            || Ok(catalog()),
            || true,
        )
        .await
        .unwrap();
    assert_eq!(guest.tabs, vec![id(2)]);
    assert!(store
        .sync(Some(other), None, || Ok(catalog()), || true)
        .await
        .unwrap()
        .tabs
        .is_empty());
    store.shutdown().await;
    let reloaded = fixture.store();
    assert_eq!(
        reloaded
            .sync(None, None, || Ok(catalog()), || true)
            .await
            .unwrap(),
        saved
    );
    assert_eq!(
        reloaded
            .sync(Some(account), None, || Ok(catalog()), || true)
            .await
            .unwrap(),
        guest
    );
    assert_eq!(fs::metadata(fixture.file()).unwrap().mode() & 0o777, 0o600);
    reloaded.shutdown().await;
    if let Some(root) = std::env::var_os("HMUX_RUST_WORKSPACE_HANDOFF") {
        let store =
            Store::new(PrivateDir::open(&PathBuf::from(root).canonicalize().unwrap()).unwrap());
        assert_eq!(
            store
                .sync(None, Some(open(1)), || Ok(catalog()), || true)
                .await
                .unwrap(),
            saved
        );
        store.shutdown().await;
    }
}

#[tokio::test]
#[ignore = "requires isolated Rust/Go workspace handoff via make rust-compat"]
async fn reload_workspace_after_current_go_write() {
    let root = std::env::var_os("HMUX_RUST_WORKSPACE_HANDOFF").expect("handoff root");
    let store = Store::new(PrivateDir::open(&PathBuf::from(root).canonicalize().unwrap()).unwrap());
    let got = store
        .sync(None, None, || Ok(catalog()), || true)
        .await
        .unwrap();
    assert_eq!(got.revision, 2);
    assert_eq!(got.tabs, vec![id(1), id(2)]);
    assert_eq!(
        got.applied,
        vec!["operation-open-0001", "operation-go-000002"]
    );
    store.shutdown().await;
}

#[tokio::test]
async fn concurrent_owners_merge_and_fetch_precedes_file_lock() {
    let fixture = Fixture::new();
    let one = fixture.store();
    let two = fixture.store();
    let (a, b) = tokio::join!(
        one.sync(None, Some(open(1)), || Ok(catalog()), || true),
        two.sync(None, Some(open(2)), || Ok(catalog()), || true)
    );
    assert!(a.is_ok() && b.is_ok());
    let dir = fixture.dir();
    let got = one
        .sync(
            None,
            None,
            move || {
                let _lock = dir
                    .try_lock(OsStr::new("lock"))
                    .unwrap()
                    .expect("fetch must precede workspace lock");
                Ok(catalog())
            },
            || true,
        )
        .await
        .unwrap();
    assert_eq!(got.revision, 2);
    assert!(got.tabs.contains(&id(1)) && got.tabs.contains(&id(2)));
    one.shutdown().await;
    two.shutdown().await;
}

#[tokio::test]
async fn invalid_private_state_and_scope_are_never_replaced() {
    for which in [
        "directory",
        "lock",
        "workspace.json",
        "hardlink",
        "public",
        "corrupt",
        "oversize",
    ] {
        let fixture = Fixture::new();
        let store = fixture.store();
        if which == "directory" {
            std::os::unix::fs::symlink(&fixture.0, fixture.0.join("shared-workspace")).unwrap();
        } else {
            let _dir = fixture.dir();
            if matches!(which, "lock" | "workspace.json") {
                std::os::unix::fs::symlink(
                    fixture.0.join("outside"),
                    fixture.0.join("shared-workspace").join(which),
                )
                .unwrap();
            } else {
                let raw = match which {
                    "corrupt" => b"{}".to_vec(),
                    "oversize" => vec![b' '; workspace::MAX_BYTES + 1],
                    _ => serde_json::to_vec(&Snapshot::empty()).unwrap(),
                };
                fs::write(fixture.file(), raw).unwrap();
                fs::set_permissions(
                    fixture.file(),
                    fs::Permissions::from_mode(if which == "public" { 0o644 } else { 0o600 }),
                )
                .unwrap();
                if which == "hardlink" {
                    fs::hard_link(fixture.file(), fixture.0.join("alias")).unwrap();
                }
            }
        }
        let before = fs::read(fixture.file()).ok();
        assert!(
            store
                .sync(None, Some(open(1)), || Ok(catalog()), || true)
                .await
                .is_err(),
            "{which}"
        );
        assert_eq!(fs::read(fixture.file()).ok(), before, "{which}");
        store.shutdown().await;
    }
    let fixture = Fixture::new();
    let store = fixture.store();
    for scope in ["..", "a/b", "", "ABC", "0123"] {
        assert_eq!(
            store
                .sync(
                    Some(scope.into()),
                    None,
                    || panic!("invalid scope must not fetch"),
                    || true
                )
                .await,
            Err(Error::Invalid)
        );
    }
    assert!(fs::read_dir(&fixture.0).unwrap().next().is_none());
    store.shutdown().await;
}

#[tokio::test]
async fn revoked_waiter_and_lock_deadline_cannot_change_state() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let dir = fixture.dir();
    let lock = dir.try_lock(OsStr::new("lock")).unwrap().unwrap();
    let allowed = Arc::new(AtomicBool::new(true));
    let check = allowed.clone();
    let (send, started) = tokio::sync::oneshot::channel();
    let pending = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .sync(
                    None,
                    Some(open(1)),
                    move || {
                        send.send(()).unwrap();
                        Ok(catalog())
                    },
                    move || check.load(Ordering::Acquire),
                )
                .await
        }
    });
    started.await.unwrap();
    allowed.store(false, Ordering::Release);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .unwrap()
            .unwrap(),
        Err(Error::Cancelled)
    );
    let started = Instant::now();
    assert_eq!(
        store
            .sync(None, Some(open(1)), || Ok(catalog()), || true)
            .await,
        Err(Error::Busy)
    );
    assert!(started.elapsed() >= LOCK_WAIT);
    assert!(!fixture.file().exists());
    drop(lock);
    store.shutdown().await;
}

#[tokio::test]
async fn cancelled_callers_keep_work_slots_and_shutdown_joins_workers() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let mut releases = Vec::new();
    for _ in 0..WORKERS {
        let (release, wait) = std::sync::mpsc::channel();
        releases.push(release);
        let (send, started) = tokio::sync::oneshot::channel();
        let task = tokio::spawn({
            let store = store.clone();
            async move {
                store
                    .sync(
                        None,
                        Some(open(1)),
                        move || {
                            send.send(()).unwrap();
                            wait.recv().unwrap();
                            Ok(catalog())
                        },
                        || true,
                    )
                    .await
            }
        });
        started.await.unwrap();
        task.abort();
        let _ = task.await;
    }
    assert_eq!(
        store.sync(None, None, || Ok(catalog()), || true).await,
        Err(Error::Busy)
    );
    let closing = tokio::spawn({
        let store = store.clone();
        async move { store.shutdown().await }
    });
    tokio::task::yield_now().await;
    assert!(!closing.is_finished());
    for release in releases {
        release.send(()).unwrap();
    }
    tokio::time::timeout(Duration::from_secs(1), closing)
        .await
        .unwrap()
        .unwrap();
    assert!(!fixture.file().exists());
    assert_eq!(store.inner.admission.available_permits(), WORKERS);
    assert_eq!(
        store.sync(None, None, || Ok(catalog()), || true).await,
        Err(Error::Cancelled)
    );
}
