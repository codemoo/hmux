use super::*;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::AtomicU64,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-e2e-completions-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    async fn store(&self) -> Store {
        Store::open(PrivateDir::open(&self.0).unwrap())
            .await
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn access(username: &str, profile: &str) -> SessionAccess {
    static LIVE: std::sync::OnceLock<tokio::sync::watch::Sender<bool>> = std::sync::OnceLock::new();
    SessionAccess {
        id: "device-one".into(),
        username: username.into(),
        profile: profile.into(),
        csrf: "synthetic".into(),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        cancelled: LIVE
            .get_or_init(|| tokio::sync::watch::channel(false).0)
            .subscribe(),
    }
}
fn event(n: usize, created_at: i64) -> CompletionEvent {
    CompletionEvent {
        id: format!("{n:064x}"),
        session: hmux_protocol::protobuf::types::Session {
            id: "$1".into(),
            created_at,
        },
        completed_at: format!("2026-10-13T00:00:{:02}Z", n % 60),
    }
}
fn live(events: &[&CompletionEvent]) -> HashSet<SessionIdentity> {
    events
        .iter()
        .map(|e| SessionIdentity {
            id: e.session.id.clone(),
            created_at: e.session.created_at,
        })
        .collect()
}
fn request(e: &CompletionEvent) -> ReadRequest {
    ReadRequest {
        session: SessionIdentity {
            id: e.session.id.clone(),
            created_at: e.session.created_at,
        },
        event_id: e.id.clone(),
    }
}

#[tokio::test]
async fn same_account_devices_share_reads_other_accounts_and_lifetimes_do_not() {
    let fixture = Fixture::new();
    let store = fixture.store().await;
    let first = event(1, 10);
    store.record(&first).await.unwrap();
    let a = access("alice", "");
    let b = SessionAccess {
        id: "device-two".into(),
        ..a.clone()
    };
    assert!(store.get(&b, live(&[&first])).await.unwrap().entries[0].unread);
    store
        .acknowledge(&a, request(&first), live(&[&first]), || true)
        .await
        .unwrap();
    assert!(!store.get(&b, live(&[&first])).await.unwrap().entries[0].unread);
    for other in [access("bob", ""), access("alice", "other")] {
        assert!(store.get(&other, live(&[&first])).await.unwrap().entries[0].unread);
    }
    let second = event(2, 10);
    store.record(&second).await.unwrap();
    assert_eq!(
        store
            .acknowledge(&b, request(&first), live(&[&second]), || true)
            .await,
        Err(Error::Conflict)
    );
    assert!(store.get(&a, live(&[&second])).await.unwrap().entries[0].unread);
    let replaced = event(3, 11);
    store.record(&replaced).await.unwrap();
    let snapshot = store.get(&a, live(&[&replaced])).await.unwrap();
    assert_eq!(snapshot.entries.len(), 1);
    assert!(snapshot.entries[0].unread);
    assert_eq!(
        store
            .acknowledge(&a, request(&second), live(&[&replaced]), || true)
            .await,
        Err(Error::Conflict)
    );
    store.shutdown().await;
}

#[tokio::test]
async fn restart_preserves_events_receipts_and_duplicate_or_old_events_cannot_rearm() {
    let fixture = Fixture::new();
    let store = fixture.store().await;
    let e = event(2, 10);
    let a = access("alice", "");
    store.record(&e).await.unwrap();
    let done = store
        .acknowledge(&a, request(&e), live(&[&e]), || true)
        .await
        .unwrap();
    store.record(&e).await.unwrap();
    store.record(&event(1, 10)).await.unwrap();
    assert_eq!(store.get(&a, live(&[&e])).await.unwrap(), done);
    store.shutdown().await;
    let reopened = fixture.store().await;
    assert_eq!(reopened.get(&a, live(&[&e])).await.unwrap(), done);
    assert_eq!(
        fs::metadata(fixture.0.join(EVENTS_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(fixture.0.join(account_key(&a)))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn revoked_or_changed_generation_and_unsafe_targets_cannot_commit() {
    let fixture = Fixture::new();
    let store = fixture.store().await;
    let e = event(1, 10);
    store.record(&e).await.unwrap();
    let (cancel, cancelled) = tokio::sync::watch::channel(false);
    let a = SessionAccess {
        cancelled,
        ..access("alice", "")
    };
    assert_eq!(
        store
            .acknowledge(&a, request(&e), live(&[&e]), || false)
            .await,
        Err(Error::Conflict)
    );
    cancel.send_replace(true);
    assert_eq!(
        store
            .acknowledge(&a, request(&e), live(&[&e]), || true)
            .await,
        Err(Error::Unauthorized)
    );
    cancel.send_replace(false);
    store
        .acknowledge(&a, request(&e), live(&[&e]), || true)
        .await
        .unwrap();
    fs::set_permissions(
        fixture.0.join(account_key(&a)),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert_eq!(
        store
            .acknowledge(&a, request(&e), live(&[&e]), || true)
            .await,
        Err(Error::Unavailable)
    );
    store.shutdown().await;
}

#[tokio::test]
async fn retention_and_cache_are_bounded_and_corrupt_storage_fails_closed() {
    let fixture = Fixture::new();
    let store = fixture.store().await;
    for n in 0..MAX_ENTRIES + 10 {
        store.record(&event(n, n as i64 + 1)).await.unwrap();
    }
    assert_eq!(
        store.0.state.lock().unwrap().ledger.events.len(),
        MAX_ENTRIES
    );
    for n in 0..CACHE_ACCOUNTS + 10 {
        store
            .get(&access(&format!("account{n}"), ""), HashSet::new())
            .await
            .unwrap();
    }
    assert_eq!(store.0.state.lock().unwrap().reads.len(), CACHE_ACCOUNTS);
    assert_eq!(
        Store::open(PrivateDir::open(&fixture.0).unwrap())
            .await
            .err(),
        Some(Error::Busy)
    );
    store.shutdown().await;
    fs::write(
        fixture.0.join(EVENTS_FILE),
        b"{\"revision\":0,\"events\":[],\"unknown\":true}",
    )
    .unwrap();
    assert_eq!(
        Store::open(PrivateDir::open(&fixture.0).unwrap())
            .await
            .err(),
        Some(Error::Unavailable)
    );
}

#[tokio::test]
async fn delayed_ack_rechecks_revocation_before_atomic_write() {
    let fixture = Fixture::new();
    let store = fixture.store().await;
    let e = event(1, 10);
    store.record(&e).await.unwrap();
    let (cancel, cancelled) = tokio::sync::watch::channel(false);
    let a = SessionAccess {
        cancelled,
        ..access("alice", "")
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
                    Ok(())
                })
                .await
        }
    });
    start.await.unwrap();
    let pending = tokio::spawn({
        let store = store.clone();
        let a = a.clone();
        async move {
            store
                .acknowledge(&a, request(&e), live(&[&e]), || true)
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while store.0.admission.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancel.send_replace(true);
    release.send(()).unwrap();
    blocker.await.unwrap().unwrap();
    assert_eq!(pending.await.unwrap(), Err(Error::Unauthorized));
    assert!(!fixture.0.join(account_key(&a)).exists());
    store.shutdown().await;
}

#[tokio::test]
async fn completion_waits_for_busy_browser_io_and_is_retained() {
    let fixture = Fixture::new();
    let store = fixture.store().await;
    let permits = store
        .0
        .admission
        .clone()
        .acquire_many_owned(IO_WORKERS as u32)
        .await
        .unwrap();
    let writer = tokio::spawn({
        let store = store.clone();
        async move { store.record(&event(1, 10)).await }
    });
    tokio::task::yield_now().await;
    assert!(!writer.is_finished());
    drop(permits);
    writer.await.unwrap().unwrap();
    assert!(
        store
            .get(&access("alice", ""), live(&[&event(1, 10)]))
            .await
            .unwrap()
            .entries[0]
            .unread
    );
    store.shutdown().await;
}
