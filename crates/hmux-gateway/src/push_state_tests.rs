use super::*;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::AtomicU64,
};
use tokio::sync::watch;

static NEXT: AtomicU64 = AtomicU64::new(0);
static OPEN_TEST: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
const CREDENTIALS: &str = "synthetic-credentials.json";
async fn exclusive() -> tokio::sync::MutexGuard<'static, ()> {
    OPEN_TEST
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

struct Fixture {
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap();
        for _ in 0..16 {
            let path = root.join(format!(
                "hmux-push-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            if fs::DirBuilder::new().mode(0o700).create(&path).is_ok() {
                return Self { path };
            }
        }
        panic!("could not create synthetic fixture");
    }
    fn dir(&self) -> PrivateDir {
        PrivateDir::open(&self.path).unwrap()
    }
    fn file(&self) -> PathBuf {
        self.path.join(format!("{CREDENTIALS}.push.json"))
    }
    fn lock(&self) -> PathBuf {
        self.path.join(format!("{CREDENTIALS}.push.json.lock"))
    }
    async fn open(&self) -> Result<Store, Error> {
        Store::open(self.dir(), OsStr::new(CREDENTIALS)).await
    }
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.file()).unwrap()).unwrap()
    }
    fn write_json(&self, value: &serde_json::Value) {
        self.write_raw(&serde_json::to_vec(value).unwrap());
    }
    fn write_raw(&self, raw: &[u8]) {
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(self.file())
            .unwrap()
            .write_all(raw)
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn id(n: u16) -> String {
    let mut bytes = [0u8; 32];
    bytes[..2].copy_from_slice(&n.to_be_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

#[tokio::test]
async fn final_subscription_check_is_exact_nonblocking_and_fails_closed() {
    let _exclusive = exclusive().await;
    let fixture = Fixture::new();
    let store = fixture.open().await.unwrap();
    let (access, _sender) = access(51);
    let sub = sub("https://fcm.googleapis.com/exact");
    store
        .subscribe(&access, sub.clone(), |_| Ok(true))
        .await
        .unwrap();
    assert_eq!(store.current_subscription(&access.id, &sub), Ok(true));
    let mut changed = sub.clone();
    changed.keys.auth = URL_SAFE_NO_PAD.encode([3u8; 16]);
    assert_eq!(store.current_subscription(&access.id, &changed), Ok(false));
    {
        let mut state = store.inner.state.lock().unwrap();
        assert_eq!(
            store.current_subscription(&access.id, &sub),
            Err(Error::Busy)
        );
        state.failed = true;
    }
    assert_eq!(
        store.current_subscription(&access.id, &sub),
        Err(Error::Unavailable)
    );
    store.inner.state.lock().unwrap().failed = false;
    store.shutdown().await;
    assert_eq!(
        store.current_subscription(&access.id, &sub),
        Err(Error::Unavailable)
    );
}
fn sub(endpoint: &str) -> Subscription {
    let key = SecretKey::from_slice(&[1u8; 32]).unwrap();
    Subscription {
        endpoint: endpoint.into(),
        keys: Keys {
            auth: URL_SAFE_NO_PAD.encode([2u8; 16]),
            p256dh: URL_SAFE_NO_PAD.encode(key.public_key().to_encoded_point(false).as_bytes()),
        },
    }
}
fn access(n: u16) -> (SessionAccess, watch::Sender<bool>) {
    let (sender, cancelled) = watch::channel(false);
    (
        SessionAccess {
            id: id(n),
            expires_at: now() + chrono::Duration::hours(1),
            csrf: String::new(),
            username: "synthetic".into(),
            profile: String::new(),
            cancelled,
        },
        sender,
    )
}
fn always_live(_: &str) -> Result<bool, Error> {
    Ok(true)
}

#[tokio::test]
async fn create_reopen_private_mode_and_consistent_keys() {
    let _exclusive = exclusive().await;
    let fixture = Fixture::new();
    let store = fixture.open().await.unwrap();
    let (login, _hold) = access(1);
    let config = store.public_config(&login).await.unwrap();
    assert!(!config.enabled);
    assert_eq!(config.login_id, login.id);
    assert!(config.endpoint.is_empty());
    assert_eq!(
        fs::metadata(fixture.file()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(fixture.lock()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    validate_keys(
        &config.public_key,
        fixture.json()["private_key"].as_str().unwrap(),
    )
    .unwrap();
    store
        .subscribe(&login, sub("https://fcm.googleapis.com/send"), always_live)
        .await
        .unwrap();
    assert!(store.public_config(&login).await.unwrap().enabled);
    store.shutdown().await;
    let reopened = fixture.open().await.unwrap();
    let reread = reopened.public_config(&login).await.unwrap();
    assert_eq!(reread.public_key, config.public_key);
    assert_eq!(reread.endpoint, "https://fcm.googleapis.com/send");
    assert_eq!(reopened.snapshot().await.unwrap().len(), 1);
    reopened.shutdown().await;
}

#[test]
fn keys_ids_and_endpoint_validation() {
    assert!(valid_id(&id(1)));
    assert!(!valid_id("bad"));
    assert!(!valid_id(&format!("{}=", id(1))));
    let base = sub("https://web.push.apple.com/a?b=c");
    assert!(validate_subscription(&base).is_ok());
    for endpoint in [
        "http://fcm.googleapis.com/x",
        "https://evil.example/x",
        "https://fcm.googleapis.com:443/x",
        "https://user@fcm.googleapis.com/x",
        "https://fcm.googleapis.com/x#frag",
        "https://fcm.googleapis.com\\@evil.example/x",
        "https://fcm.googleapis.com.evil.example/x",
        "https://fcm.googleapis.com/x\r\ny",
        "https://fcm.googleapis.com/x y",
        "https://fcm.googleapis.com/x%GG",
        "https://fcm.googleapis.com/x%",
    ] {
        assert!(
            validate_subscription(&sub(endpoint)).is_err(),
            "{endpoint:?}"
        );
    }
    assert!(validate_subscription(&sub(&format!(
        "https://fcm.googleapis.com/{}",
        "a".repeat(2049)
    )))
    .is_err());
    let mut bad = base.clone();
    bad.keys.auth = URL_SAFE_NO_PAD.encode([2u8; 15]);
    assert!(validate_subscription(&bad).is_err());
    bad = base.clone();
    bad.keys.p256dh = URL_SAFE_NO_PAD.encode([2u8; 65]);
    assert!(validate_subscription(&bad).is_err());
    bad = base;
    bad.keys.p256dh = URL_SAFE_NO_PAD.encode([2u8; 33]);
    assert!(validate_subscription(&bad).is_err());
}

#[tokio::test]
async fn malformed_state_preserved_and_rejected() {
    let _exclusive = exclusive().await;
    let fixture = Fixture::new();
    let initial = fixture.open().await.unwrap();
    initial.shutdown().await;
    let good = fixture.json();
    let mut cases = Vec::new();
    let mut v = good.clone();
    v["version"] = 2.into();
    cases.push(serde_json::to_vec(&v).unwrap());
    let mut v = good.clone();
    v["extra"] = true.into();
    cases.push(serde_json::to_vec(&v).unwrap());
    let mut v = good.clone();
    v["public_key"] = "bad".into();
    cases.push(serde_json::to_vec(&v).unwrap());
    let mut v = good.clone();
    v["private_key"] = "bad".into();
    cases.push(serde_json::to_vec(&v).unwrap());
    let mut v = good.clone();
    v["subscriptions"][id(1)] = serde_json::to_value(sub("https://evil.example/x")).unwrap();
    cases.push(serde_json::to_vec(&v).unwrap());
    let mut v = good.clone();
    v["subscriptions"]["bad"] = serde_json::to_value(sub("https://fcm.googleapis.com/x")).unwrap();
    cases.push(serde_json::to_vec(&v).unwrap());
    let mut v = good.clone();
    v["subscriptions"] = serde_json::json!([]);
    cases.push(serde_json::to_vec(&v).unwrap());
    let mut v = good.clone();
    v["subscriptions"][id(1)] = serde_json::json!(["https://fcm.googleapis.com/x", {}]);
    cases.push(serde_json::to_vec(&v).unwrap());
    let mut v = good.clone();
    v["subscriptions"][id(1)] =
        serde_json::json!({"endpoint":"https://fcm.googleapis.com/x","keys":[]});
    cases.push(serde_json::to_vec(&v).unwrap());
    cases.push(b"[1,2,3,4]".to_vec());
    let duplicate = format!("{{\"version\":1,\"version\":1,\"public_key\":{},\"private_key\":{},\"subscriptions\":{{}}}}", good["public_key"], good["private_key"]);
    cases.push(duplicate.into_bytes());
    let duplicate_map = format!("{{\"version\":1,\"public_key\":{},\"private_key\":{},\"subscriptions\":{{\"{}\":{},\"{}\":{}}}}}", good["public_key"], good["private_key"], id(1), serde_json::to_string(&sub("https://fcm.googleapis.com/x")).unwrap(), id(1), serde_json::to_string(&sub("https://fcm.googleapis.com/x")).unwrap());
    cases.push(duplicate_map.into_bytes());
    cases.push(vec![b'x'; DISK_BYTES + 1]);
    for raw in cases {
        fs::write(fixture.file(), &raw).unwrap();
        assert!(matches!(fixture.open().await, Err(Error::Unavailable)));
        assert_eq!(fs::read(fixture.file()).unwrap(), raw);
    }
    fixture.write_json(&good);
    // Go accepts a missing or null map as empty; keep this bounded compatibility.
    let mut nullable = good.clone();
    nullable["subscriptions"] = serde_json::Value::Null;
    fixture.write_json(&nullable);
    let reopened = fixture.open().await.unwrap();
    assert!(reopened.snapshot().await.unwrap().is_empty());
    reopened.shutdown().await;

    // JSON escapes may spell the same canonical login ID.
    let escaped = format!(
        "{{\"version\":1,\"public_key\":{},\"private_key\":{},\"subscriptions\":{{\"\\u0041{}\":{}}}}}",
        good["public_key"], good["private_key"], &id(1)[1..],
        serde_json::to_string(&sub("https://fcm.googleapis.com/x")).unwrap()
    );
    fs::write(fixture.file(), escaped).unwrap();
    let escaped_store = fixture.open().await.unwrap();
    assert!(escaped_store
        .snapshot()
        .await
        .unwrap()
        .iter()
        .any(|(key, _)| key == &id(1)));
    escaped_store.shutdown().await;
}

#[tokio::test]
async fn lifetime_lock_conflict_and_shutdown_with_live_clones() {
    let _exclusive = exclusive().await;
    let fixture = Fixture::new();
    let store = fixture.open().await.unwrap();
    let clone = store.clone();
    assert!(matches!(fixture.open().await, Err(Error::Unavailable)));
    store.shutdown().await;
    assert!(matches!(clone.snapshot().await, Err(Error::Unavailable)));
    assert!(fixture.lock().exists());
    let reopened = fixture.open().await.unwrap();
    reopened.shutdown().await;
}

#[tokio::test]
async fn unsafe_files_and_external_changes_preserve_originals() {
    let _exclusive = exclusive().await;
    let fixture = Fixture::new();
    let store = fixture.open().await.unwrap();
    let (login, _hold) = access(1);
    let original = fs::read(fixture.file()).unwrap();
    fs::write(fixture.file(), b"different but private").unwrap();
    assert_eq!(
        store
            .subscribe(&login, sub("https://fcm.googleapis.com/x"), always_live)
            .await,
        Err(Error::Unavailable)
    );
    assert_eq!(fs::read(fixture.file()).unwrap(), b"different but private");
    store.shutdown().await;
    assert!(matches!(fixture.open().await, Err(Error::Unavailable)));
    fs::write(fixture.file(), original).unwrap();
    fs::set_permissions(fixture.file(), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(fixture.open().await, Err(Error::Unavailable)));
    assert_eq!(
        fs::metadata(fixture.file()).unwrap().permissions().mode() & 0o777,
        0o644
    );
    fs::remove_file(fixture.file()).unwrap();
    fs::write(fixture.path.join("other"), b"do not follow").unwrap();
    std::os::unix::fs::symlink(fixture.path.join("other"), fixture.file()).unwrap();
    assert!(matches!(fixture.open().await, Err(Error::Unavailable)));
    assert_eq!(
        fs::read(fixture.path.join("other")).unwrap(),
        b"do not follow"
    );
    fs::remove_file(fixture.file()).unwrap();
    fs::write(fixture.file(), b"linked private file").unwrap();
    fs::hard_link(fixture.file(), fixture.path.join("alias")).unwrap();
    assert!(matches!(fixture.open().await, Err(Error::Unavailable)));
    assert_eq!(fs::read(fixture.file()).unwrap(), b"linked private file");
    fs::remove_file(fixture.path.join("alias")).unwrap();
    fs::remove_file(fixture.lock()).unwrap();
    std::os::unix::fs::symlink(fixture.path.join("other"), fixture.lock()).unwrap();
    assert!(matches!(fixture.open().await, Err(Error::Unavailable)));
    assert_eq!(
        fs::read(fixture.path.join("other")).unwrap(),
        b"do not follow"
    );
}

#[tokio::test]
async fn transfer_prune_stale_guard_and_limit() {
    let _exclusive = exclusive().await;
    let fixture = Fixture::new();
    let store = fixture.open().await.unwrap();
    let (first, _one) = access(1);
    let (second, _two) = access(2);
    let endpoint = "https://fcm.googleapis.com/device";
    store
        .subscribe(&first, sub(endpoint), always_live)
        .await
        .unwrap();
    store
        .subscribe(&second, sub(endpoint), always_live)
        .await
        .unwrap();
    assert_eq!(store.snapshot().await.unwrap().len(), 1);
    assert!(!store.public_config(&first).await.unwrap().enabled);
    store
        .remove(&second.id, Some("https://fcm.googleapis.com/old"))
        .await
        .unwrap();
    assert!(store.public_config(&second).await.unwrap().enabled);
    store
        .subscribe(
            &second,
            sub("https://fcm.googleapis.com/replacement"),
            always_live,
        )
        .await
        .unwrap();
    store.remove(&second.id, Some(endpoint)).await.unwrap();
    assert!(store.public_config(&second).await.unwrap().enabled);
    let old_id = second.id.clone();
    store
        .subscribe(&first, sub("https://fcm.googleapis.com/other"), move |id| {
            Ok(id != old_id)
        })
        .await
        .unwrap();
    assert_eq!(store.snapshot().await.unwrap().len(), 1);
    let before = fs::read(fixture.file()).unwrap();
    assert_eq!(
        store
            .subscribe(&second, sub(endpoint), |_| Err(Error::Busy))
            .await,
        Err(Error::Busy)
    );
    assert_eq!(fs::read(fixture.file()).unwrap(), before);
    store.shutdown().await;

    let mut state = fixture.json();
    for n in 0..MAX_SUBSCRIPTIONS as u16 {
        state["subscriptions"][id(n)] =
            serde_json::to_value(sub(&format!("https://fcm.googleapis.com/{n}"))).unwrap();
    }
    fixture.write_json(&state);
    let full = fixture.open().await.unwrap();
    assert_eq!(full.snapshot().await.unwrap().len(), 256);
    let (extra, _hold) = access(500);
    let raw = fs::read(fixture.file()).unwrap();
    assert_eq!(
        full.subscribe(&extra, sub(endpoint), always_live).await,
        Err(Error::Busy)
    );
    assert_eq!(fs::read(fixture.file()).unwrap(), raw);
    full.shutdown().await;
    state["subscriptions"][id(500)] = serde_json::to_value(sub(endpoint)).unwrap();
    fixture.write_json(&state);
    assert!(matches!(fixture.open().await, Err(Error::Unavailable)));
}
#[tokio::test]
async fn cancelled_queue_keeps_slot_until_completion_and_shutdown_joins() {
    let _exclusive = exclusive().await;
    let fixture = Fixture::new();
    let store = fixture.open().await.unwrap();
    let (first, _one) = access(1);
    let (second, _two) = access(2);
    let raw = fs::read(fixture.file()).unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let first_visit = std::sync::Arc::new(AtomicBool::new(false));
    let first_store = store.clone();
    let first_task = tokio::spawn(async move {
        first_store
            .subscribe(&first, sub("https://fcm.googleapis.com/one"), move |_| {
                if !first_visit.swap(true, Ordering::AcqRel) {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                }
                Ok(true)
            })
            .await
    });
    tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
        .await
        .unwrap();
    let second_store = store.clone();
    let second_task = tokio::spawn(async move {
        second_store
            .subscribe(&second, sub("https://fcm.googleapis.com/two"), always_live)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while store.inner.slots.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    second_task.abort();
    let (third, _three) = access(3);
    assert_eq!(store.public_config(&third).await.err(), Some(Error::Busy));
    let shutdown_store = store.clone();
    let shutdown_task = tokio::spawn(async move { shutdown_store.shutdown().await });
    tokio::task::yield_now().await;
    assert!(!shutdown_task.is_finished());
    release_tx.send(()).unwrap();
    assert_eq!(first_task.await.unwrap(), Err(Error::Unavailable));
    tokio::time::timeout(Duration::from_secs(2), shutdown_task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(fixture.file()).unwrap(), raw);
    assert!(matches!(store.snapshot().await, Err(Error::Unavailable)));
    let reopened = fixture.open().await.unwrap();
    reopened.shutdown().await;
}

#[tokio::test]
async fn panicking_lookup_cannot_keep_lock_after_shutdown() {
    let _exclusive = exclusive().await;
    let fixture = Fixture::new();
    let store = fixture.open().await.unwrap();
    let clone = store.clone();
    let (login, _hold) = access(1);
    assert_eq!(
        store
            .subscribe(&login, sub("https://fcm.googleapis.com/x"), |_| panic!(
                "synthetic lookup panic"
            ))
            .await,
        Err(Error::Unavailable)
    );
    store.shutdown().await;
    assert!(matches!(clone.snapshot().await, Err(Error::Unavailable)));
    let reopened = fixture.open().await.unwrap();
    assert!(reopened.snapshot().await.unwrap().is_empty());
    reopened.shutdown().await;
}

#[tokio::test]
async fn revocation_during_precommit_lookup_preserves_state() {
    let _exclusive = exclusive().await;
    let fixture = Fixture::new();
    let store = fixture.open().await.unwrap();
    let (login, sender) = access(1);
    let raw = fs::read(fixture.file()).unwrap();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let result = store
        .subscribe(&login, sub("https://fcm.googleapis.com/x"), move |_| {
            if calls.fetch_add(1, Ordering::AcqRel) == 2 {
                sender.send(true).unwrap();
            }
            Ok(true)
        })
        .await;
    assert_eq!(result, Err(Error::Unauthorized));
    assert_eq!(fs::read(fixture.file()).unwrap(), raw);
    store.shutdown().await;
}

#[tokio::test]
async fn actual_go_push_state_oracle() {
    #[derive(Deserialize)]
    struct Case {
        name: String,
        input: String,
        go_valid: bool,
        rust_valid: bool,
        #[serde(default)]
        delta: String,
    }
    #[derive(Deserialize)]
    struct Oracle {
        subscription: Subscription,
        endpoints: Vec<Case>,
        keys: Vec<Case>,
        ids: Vec<Case>,
        states: Vec<Case>,
    }
    let oracle: Oracle = serde_json::from_str(include_str!(
        "../../../tests/fixtures/push-v1/go-oracle.json"
    ))
    .unwrap();
    for cases in [&oracle.endpoints, &oracle.keys, &oracle.ids, &oracle.states] {
        for case in cases {
            if case.delta.is_empty() {
                assert_eq!(case.rust_valid, case.go_valid, "{}", case.name);
            } else {
                assert!(!case.rust_valid, "delta must fail closed: {}", case.name);
            }
        }
    }
    for case in &oracle.endpoints {
        let mut subscription = oracle.subscription.clone();
        subscription.endpoint.clone_from(&case.input);
        assert_eq!(
            validate_subscription(&subscription).is_ok(),
            case.rust_valid,
            "endpoint: {}",
            case.name
        );
    }
    for case in &oracle.keys {
        let subscription: Subscription = serde_json::from_str(&case.input).unwrap();
        assert_eq!(
            validate_subscription(&subscription).is_ok(),
            case.rust_valid,
            "keys: {}",
            case.name
        );
    }
    for case in &oracle.ids {
        assert_eq!(valid_id(&case.input), case.rust_valid, "id: {}", case.name);
    }
    let _exclusive = exclusive().await;
    let fixture = Fixture::new();
    for case in &oracle.states {
        fixture.write_raw(case.input.as_bytes());
        let result = fixture.open().await;
        assert_eq!(result.is_ok(), case.rust_valid, "state: {}", case.name);
        if let Ok(store) = result {
            store.shutdown().await;
        }
        assert_eq!(
            fs::read(fixture.file()).unwrap(),
            case.input.as_bytes(),
            "open must preserve input: {}",
            case.name
        );
    }
}

#[tokio::test]
async fn delivery_preparation_uses_stored_key_and_exact_live_subscription() {
    let _exclusive = exclusive().await;
    let fixture = Fixture::new();
    let store = fixture.open().await.unwrap();
    let (login, sender) = access(1);
    let expected = sub("https://web.push.apple.com/synthetic");
    store
        .subscribe(&login, expected.clone(), always_live)
        .await
        .unwrap();
    let raw = fs::read(fixture.file()).unwrap();
    let prepared = store
        .prepare_for_delivery(&login, &expected, "https://hmux.example", b"{}")
        .await
        .unwrap();
    assert_eq!(prepared.body.len(), 4096);
    assert_eq!(prepared.endpoint, expected.endpoint);
    let public = store.public_config(&login).await.unwrap().public_key;
    assert!(prepared.authorization.ends_with(&format!(", k={public}")));
    let jwt = prepared
        .authorization
        .strip_prefix("vapid t=")
        .unwrap()
        .split(", k=")
        .next()
        .unwrap();
    let parts: Vec<_> = jwt.split('.').collect();
    let signature =
        p256::ecdsa::Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
    let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(&URL_SAFE_NO_PAD.decode(&public).unwrap())
        .unwrap();
    use p256::ecdsa::signature::Verifier;
    key.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .unwrap();
    assert_eq!(fs::read(fixture.file()).unwrap(), raw);
    let mut replacement = expected.clone();
    replacement.keys.auth = URL_SAFE_NO_PAD.encode([3; 16]);
    store
        .subscribe(&login, replacement.clone(), always_live)
        .await
        .unwrap();
    assert_eq!(
        store
            .prepare_for_delivery(&login, &expected, "https://hmux.example", b"{}")
            .await
            .err(),
        Some(Error::Unauthorized)
    );
    let (other, _other_sender) = access(2);
    assert_eq!(
        store
            .prepare_for_delivery(&other, &replacement, "https://hmux.example", b"{}")
            .await
            .err(),
        Some(Error::Unauthorized)
    );
    assert_eq!(
        store
            .prepare_for_delivery(&login, &replacement, "https://hmux.example", &vec![0; 3994])
            .await
            .err(),
        Some(Error::Invalid)
    );
    sender.send_replace(true);
    assert_eq!(
        store
            .prepare_for_delivery(&login, &replacement, "https://hmux.example", b"{}")
            .await
            .err(),
        Some(Error::Unauthorized)
    );
    store.shutdown().await;
}

#[tokio::test]
async fn queued_delivery_preparation_observes_revocation() {
    let _exclusive = exclusive().await;
    let fixture = Fixture::new();
    let store = fixture.open().await.unwrap();
    let (login, sender) = access(1);
    let expected = sub("https://web.push.apple.com/synthetic");
    store
        .subscribe(&login, expected.clone(), always_live)
        .await
        .unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let blocker = store.clone();
    let first = tokio::spawn(async move {
        blocker
            .transact(move |_, _| {
                let _ = entered_tx.send(());
                release_rx.recv().unwrap();
                Ok(())
            })
            .await
    });
    entered_rx.await.unwrap();
    let queued = store.clone();
    let second = tokio::spawn(async move {
        queued
            .prepare_for_delivery(&login, &expected, "https://hmux.example", b"{}")
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while store.inner.slots.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    sender.send_replace(true);
    release_tx.send(()).unwrap();
    first.await.unwrap().unwrap();
    assert_eq!(second.await.unwrap().err(), Some(Error::Unauthorized));
    store.shutdown().await;
}

#[tokio::test]
#[ignore = "the optional external baseline suite (tests/RUST.md) provides the actual Go gateway test helper"]
async fn current_push_state_survives_go_handoff_and_lock_conflict() {
    use hmux_core::command::{CommandRunner, CommandSpec};
    let _exclusive = exclusive().await;
    let fixture = Fixture::new();
    let store = fixture.open().await.unwrap();
    let (mut first, _one) = access(1);
    let (mut second, _two) = access(2);
    first.id = URL_SAFE_NO_PAD.encode([1u8; 32]);
    second.id = URL_SAFE_NO_PAD.encode([2u8; 32]);
    store
        .subscribe(&first, sub("https://web.push.apple.com/first"), always_live)
        .await
        .unwrap();
    store
        .subscribe(
            &second,
            sub("https://fcm.googleapis.com/second"),
            always_live,
        )
        .await
        .unwrap();
    let public = store.public_config(&first).await.unwrap().public_key;
    fs::write(fixture.path.join("expected-public.txt"), &public).unwrap();
    let before = fs::read(fixture.file()).unwrap();
    let helper = std::env::var_os("HMUX_GO_PUSH_HELPER")
        .expect("tests/RUST.md describes the external legacy helper");
    let runner = CommandRunner::new(1).unwrap();
    let command = || {
        CommandSpec::new(helper.clone(), 8192, Duration::from_secs(10))
            .arg("-test.run=^TestRustPushStateHandoff$")
            .arg("-test.timeout=8s")
            .env("HMUX_RUST_PUSH_HANDOFF", fixture.path.as_os_str())
    };
    let locked = runner
        .run(command().env("HMUX_RUST_PUSH_EXPECT_LOCKED", "1"))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&locked.stdout).contains("Go respected Rust lifetime lock"));
    assert_eq!(fs::read(fixture.file()).unwrap(), before);
    store.shutdown().await;
    let handoff = runner
        .run(command().env("HMUX_RUST_PUSH_EXPECT_LOCKED", ""))
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&handoff.stdout).contains("Go removal persisted"));
    let current = fs::read(fixture.file()).unwrap();
    assert_ne!(current, before);
    let reopened = fixture.open().await.unwrap();
    let config = reopened.public_config(&first).await.unwrap();
    assert_eq!(config.public_key, public);
    assert!(!config.enabled);
    assert!(reopened.public_config(&second).await.unwrap().enabled);
    assert_eq!(reopened.snapshot().await.unwrap().len(), 1);
    assert_eq!(fs::read(fixture.file()).unwrap(), current);
    reopened.shutdown().await;
}
