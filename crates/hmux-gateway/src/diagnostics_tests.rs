use super::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs,
    future::Future,
    os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::AtomicU64,
    task::Context,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
static OPEN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-e2e-diagnostics-{}-{}-{}",
            std::process::id(),
            now().timestamp_nanos_opt().unwrap(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(root)
    }
    async fn open(&self) -> Store {
        let _guard = OPEN.lock().await;
        Store::open(
            PrivateDir::open(&self.0).unwrap(),
            "diagnostics.json".into(),
        )
        .await
        .unwrap()
    }
    fn file(&self) -> PathBuf {
        self.0.join("diagnostics.json")
    }
    fn write(&self, raw: &[u8]) {
        fs::write(self.file(), raw).unwrap();
        fs::set_permissions(self.file(), fs::Permissions::from_mode(0o600)).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn login(account: &str, profile: &str) -> (SessionAccess, watch::Sender<bool>) {
    let (cancelled, receiver) = watch::channel(false);
    (
        SessionAccess {
            id: format!("login-{account}-{profile}"),
            username: account.into(),
            profile: profile.into(),
            csrf: "synthetic-csrf-never-store".into(),
            expires_at: now() + chrono::Duration::days(1),
            cancelled: receiver,
        },
        cancelled,
    )
}
fn batch(at: DateTime<Utc>, start: i64, count: i64) -> Batch {
    let events:Vec<_>=(start..start+count).map(|seq|json!({"sequence":seq,"at":at.timestamp_millis(),"kind":"terminal-failed","reason":"network","online":true,"visible":true})).collect();
    Batch::decode(&serde_json::to_vec(&json!({"version":1,"client":"12345678-1234-4234-8234-123456789abc","build":"app-fixture.js","events":events})).unwrap()).unwrap()
}
fn report(store: &Store, access: &SessionAccess) -> Value {
    serde_json::to_value(store.report(access).unwrap()).unwrap()
}

#[test]
fn actual_go_oracle_matches_decode_transitions_dedup_eviction_and_export() {
    #[derive(Deserialize)]
    struct Oracle {
        base: DateTime<Utc>,
        decode: Vec<Decode>,
        steps: Vec<Step>,
        report: Value,
        disk: Value,
    }
    #[derive(Deserialize)]
    struct Decode {
        json: String,
        valid: bool,
    }
    #[derive(Deserialize)]
    struct Step {
        at: DateTime<Utc>,
        login: String,
        account: String,
        profile: String,
        browser: String,
        batch: Value,
        status: u16,
        count: usize,
        revision: u64,
        digest: String,
    }
    let oracle: Oracle = serde_json::from_str(include_str!(
        "../../../tests/fixtures/diagnostics-v1/go-oracle.json"
    ))
    .unwrap();
    for (i, case) in oracle.decode.iter().enumerate() {
        let valid = Batch::decode(case.json.as_bytes()).is_ok_and(|b| b.valid(oracle.base));
        assert_eq!(valid, case.valid, "decode {i}");
    }
    let mut state = State::new(Vec::new(), false);
    let mut last = oracle.base;
    for (i, step) in oracle.steps.iter().enumerate() {
        let (mut access, _cancel) = login(&step.account, &step.profile);
        access.id = step.login.clone();
        let mut batch = step.batch.clone();
        let template = batch.as_object_mut().unwrap().remove("event").unwrap();
        let sequences = batch.as_object_mut().unwrap().remove("sequences").unwrap();
        batch["events"] = Value::Array(
            sequences
                .as_array()
                .unwrap()
                .iter()
                .map(|sequence| {
                    let mut event = template.clone();
                    event["sequence"] = sequence.clone();
                    event
                })
                .collect(),
        );
        let b = Batch::decode(&serde_json::to_vec(&batch).unwrap()).unwrap();
        assert!(b.valid(step.at));
        let status = match state.append(&access, &step.browser, b, step.at) {
            Ok(()) => 202,
            Err(Error::RateLimited) => 429,
            other => panic!("unexpected status {other:?}"),
        };
        assert_eq!(
            (status, state.records.len(), state.revision),
            (step.status, step.count, step.revision),
            "step {i}"
        );
        let raw = serde_json::to_vec(&serde_json::to_value(&state.records).unwrap()).unwrap();
        let digest = format!("{:x}", Sha256::digest(&raw));
        assert_eq!(digest, step.digest, "record digest at step {i}");
        last = step.at;
    }
    assert_eq!(
        serde_json::to_value(state.report("account-9", "", last).unwrap()).unwrap(),
        oracle.report
    );
    let disk = serde_json::to_vec(&oracle.disk).unwrap();
    let records = model::decode_disk(&disk, last).unwrap();
    assert_eq!(
        serde_json::to_value(records).unwrap(),
        oracle.disk["records"]
    );
}

#[test]
fn bounded_dtos_reject_unknown_positional_duplicate_and_enum_objects() {
    let valid = json!({"version":1,"client":"12345678-1234-4234-8234-123456789abc","build":"unknown","events":[{"sequence":1,"at":now().timestamp_millis(),"kind":"offline","online":null,"reason":null}]});
    let b = Batch::decode(&serde_json::to_vec(&valid).unwrap()).unwrap();
    assert!(b.valid(now()));
    for replacement in [json!([]), json!(null), json!({"offline":null}), json!(3)] {
        let mut input = valid.clone();
        input["events"][0]["kind"] = replacement;
        assert!(!Batch::decode(&serde_json::to_vec(&input).unwrap()).is_ok_and(|b| b.valid(now())));
    }
    let mut unknown = valid.clone();
    unknown["events"][0]["message"] = json!("SECRET");
    assert!(Batch::decode(&serde_json::to_vec(&unknown).unwrap()).is_err());
    let mut positional = valid.clone();
    positional["events"] = json!([[]]);
    assert!(Batch::decode(&serde_json::to_vec(&positional).unwrap()).is_err());
    let mut excessive = valid.clone();
    excessive["events"] = Value::Array(vec![valid["events"][0].clone(); 21]);
    assert!(Batch::decode(&serde_json::to_vec(&excessive).unwrap()).is_err());
    let duplicate = serde_json::to_string(&valid).unwrap().replacen(
        "\"version\":1",
        "\"version\":1,\"version\":1",
        1,
    );
    assert!(Batch::decode(duplicate.as_bytes()).is_err());
    for raw in [
        br#"{"version":1}"#.as_slice(),
        br#"{"version":1,"records":null}"#,
        br#"{"version":1,"records":[]}"#,
    ] {
        assert!(model::decode_disk(raw, now()).unwrap().is_empty());
    }
    assert!(model::decode_disk(br#"{"version":1,"records":[[]]}"#, now()).is_err());
}

#[test]
fn persisted_rows_validate_metadata_and_enforce_real_array_and_owner_caps() {
    let current = now();
    let record = batch(current, 1, 1)
        .records(
            Source {
                account: "primary".into(),
                browser: "Safari on macOS".into(),
                ..Source::default()
            },
            current,
        )
        .next()
        .unwrap();
    let row = serde_json::to_value(record).unwrap();
    let decode = |rows: Value| {
        model::decode_disk(
            &serde_json::to_vec(&json!({"version":1,"records":rows})).unwrap(),
            current,
        )
    };
    assert!(decode(json!([row.clone()])).is_ok());
    for (key, value) in [
        ("account", json!("")),
        ("account", json!(" primary")),
        ("account", json!("x".repeat(81))),
        ("profile", json!("A".repeat(64))),
        ("profile", json!("a".repeat(63))),
        ("client", json!("12345678-1234-1234-8234-123456789abc")),
        ("build", json!("app-../private.js")),
        ("browser", json!("Safari on private-host")),
        ("received_at", json!("0001-01-01T00:00:00Z")),
        (
            "received_at",
            json!(
                (current + chrono::Duration::minutes(5) + chrono::Duration::nanoseconds(1))
                    .to_rfc3339()
            ),
        ),
        ("received_at", json!("invalid")),
    ] {
        let mut invalid = row.clone();
        invalid[key] = value;
        assert!(decode(json!([invalid])).is_err(), "accepted invalid {key}");
    }
    let rows: Vec<_> = (0..=LIMIT)
        .map(|i| {
            let mut row = row.clone();
            row["account"] = json!(format!("owner-{}", i / ACCOUNT_LIMIT));
            row["sequence"] = json!(i + 1);
            row
        })
        .collect();
    assert_eq!(decode(json!(&rows[..LIMIT])).unwrap().len(), LIMIT);
    assert!(decode(json!(rows)).is_err());
    let rows: Vec<_> = (0..=ACCOUNT_LIMIT)
        .map(|i| {
            let mut row = row.clone();
            row["sequence"] = json!(i + 1);
            row
        })
        .collect();
    assert_eq!(
        decode(json!(&rows[..ACCOUNT_LIMIT])).unwrap().len(),
        ACCOUNT_LIMIT
    );
    assert!(decode(json!(rows)).is_err());
}

#[test]
fn reports_count_only_failures_and_normalize_nullable_scalars() {
    let current = now();
    let events: Vec<_> = [
        "terminal-failed",
        "terminal-recovered",
        "api-failed",
        "offline",
        "resume",
        "runtime-error",
        "unhandled-rejection",
    ]
    .into_iter()
    .enumerate()
    .map(|(i, kind)| {
        json!({"sequence":i+1,"at":current.timestamp_millis(),"kind":kind,
            "reason":null,"route":null,"code":null,"attempt":null,"retry_ms":null,
            "duration_ms":null,"line":null,"column":null,
            "online":null,"visible":null,"standalone":null})
    })
    .collect();
    let batch = Batch::decode(
        &serde_json::to_vec(&json!({"version":1,
        "client":"12345678-1234-4234-8234-123456789abc","build":"unknown","events":events}))
        .unwrap(),
    )
    .unwrap();
    assert!(batch.valid(current));
    let mut state = State::new(Vec::new(), false);
    let (access, _stop) = login("primary", "");
    state.append(&access, "Safari", batch, current).unwrap();
    let report = serde_json::to_value(state.report("primary", "", current).unwrap()).unwrap();
    assert_eq!(
        report["counts"],
        json!({"terminal-failed:":1,"api-failed:":1,
        "runtime-error:":1,"unhandled-rejection:":1})
    );
    for event in report["events"].as_array().unwrap() {
        for key in [
            "reason",
            "route",
            "code",
            "attempt",
            "retry_ms",
            "duration_ms",
            "line",
            "column",
        ] {
            assert!(event.get(key).is_none(), "unexpected {key}");
        }
        for key in ["online", "visible", "standalone"] {
            assert_eq!(event[key], false);
        }
    }
}

#[test]
fn rate_buckets_are_bounded_and_receipt_ttl_keeps_nanosecond_precision() {
    let current = DateTime::parse_from_rfc3339("2026-09-24T00:00:00.123456789Z")
        .unwrap()
        .with_timezone(&Utc);
    let mut state = State::new(Vec::new(), false);
    let (mut access, _cancel) = login("primary", "");
    for i in 0..RATE_LIMIT {
        access.id = format!("login-{i}");
        assert_eq!(
            state.append(&access, "Safari", batch(current, 1, 1), current),
            Ok(())
        );
    }
    assert_eq!(state.rates.len(), RATE_LIMIT);
    access.id = "overflow".into();
    assert_eq!(
        state.append(&access, "Safari", batch(current, 1, 1), current),
        Err(Error::RateLimited)
    );
    assert_eq!(
        state.append(
            &access,
            "Safari",
            batch(current, 1, 1),
            current + chrono::Duration::seconds(60)
        ),
        Ok(())
    );
    assert_eq!(state.rates.len(), 1);
    let cutoff = current + chrono::Duration::days(7);
    assert_eq!(
        state
            .report("primary", "", cutoff - chrono::Duration::nanoseconds(1))
            .unwrap()
            .events
            .0
            .len(),
        1
    );
    assert_eq!(
        state.report("primary", "", cutoff).unwrap().events.0.len(),
        0
    );
    assert!(state.revision > state.saved);
    state.revision = u64::MAX;
    assert_eq!(
        state.append(&access, "Safari", batch(cutoff, 2, 1), cutoff),
        Err(Error::Unavailable)
    );
    assert!(state.disabled);
}

#[tokio::test]
async fn account_exports_omit_private_fields_and_shutdown_hands_current_state_to_go() {
    let fixture = Fixture::new();
    let store = fixture.open().await;
    let (a, stop) = login("primary", "");
    store
        .append(&a, "Safari on macOS", batch(now(), 1, 1))
        .unwrap();
    store
        .append(&a, "Safari on macOS", batch(now(), 1, 1))
        .unwrap();
    let value = report(&store, &a);
    assert_eq!(value["events"].as_array().unwrap().len(), 1);
    assert_eq!(value["counts"]["terminal-failed:network"], 1);
    assert_eq!(value["pending_save"], true);
    let raw = serde_json::to_string(&value).unwrap();
    for forbidden in ["account", "profile", a.csrf.as_str(), a.id.as_str()] {
        assert!(!raw.contains(forbidden));
    }
    assert!(value["events"][0].get("route").is_none());
    assert_eq!(value["events"][0]["standalone"], false);
    let (other, _other_stop) = login("primary", &"a".repeat(64));
    assert_eq!(report(&store, &other)["events"], json!([]));
    store.shutdown().await;
    assert_eq!(report(&store, &a)["pending_save"], false);
    assert_eq!(
        store.append(&a, "Safari", batch(now(), 2, 1)),
        Err(Error::Unavailable)
    );
    assert_eq!(
        fs::metadata(fixture.file()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let again = fixture.open().await;
    assert_eq!(report(&again, &a)["events"].as_array().unwrap().len(), 1);
    again.shutdown().await;
    if let Some(path) = std::env::var_os("HMUX_RUST_DIAGNOSTICS_HANDOFF") {
        let path = PathBuf::from(path)
            .canonicalize()
            .unwrap()
            .join("diagnostics.json");
        fs::copy(fixture.file(), &path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    stop.send_replace(true);
    assert!(matches!(store.report(&a), Err(Error::Unauthorized)));
}

#[tokio::test]
#[ignore = "run by make rust-compat after the current Go diagnostic write"]
async fn reload_current_diagnostics_after_go_append() {
    let path = PathBuf::from(std::env::var_os("HMUX_RUST_DIAGNOSTICS_HANDOFF").unwrap())
        .canonicalize()
        .unwrap();
    let store = Store::open(PrivateDir::open(&path).unwrap(), "diagnostics.json".into())
        .await
        .unwrap();
    let (a, _stop) = login("primary", "");
    let report = report(&store, &a);
    assert_eq!(report["storage_ok"], true);
    assert_eq!(
        report["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["sequence"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    store.shutdown().await;
}

#[tokio::test]
async fn full_history_exceeds_wire_limit_and_restores_shared_metadata() {
    let fixture = Fixture::new();
    let store = fixture.open().await;
    let current = now();
    for owner in 0..10 {
        let (mut access, _stop) = login(&format!("owner-{owner}"), "");
        for n in 0..13 {
            access.id = format!("{owner}-{n}");
            store
                .append(&access, "Unknown browser", batch(current, n * 20 + 1, 20))
                .unwrap();
        }
    }
    assert_eq!(store.owner.inner.state.lock().unwrap().records.len(), LIMIT);
    store.shutdown().await;
    let size = fs::metadata(fixture.file()).unwrap().len();
    assert!(size > 16 << 10 && size <= DISK_BYTES as u64);
    let store = fixture.open().await;
    let (a, _stop) = login("owner-9", "");
    assert_eq!(
        report(&store, &a)["events"].as_array().unwrap().len(),
        ACCOUNT_LIMIT
    );
    {
        let state = store.owner.inner.state.lock().unwrap();
        assert_eq!(state.records.len(), LIMIT);
        let count = state
            .records
            .iter()
            .filter(|r| Arc::ptr_eq(&r.source, &state.records.back().unwrap().source))
            .count();
        assert_eq!(count, ACCOUNT_LIMIT);
    }
    store.shutdown().await;
}

#[tokio::test]
async fn corrupt_unsafe_and_changed_originals_are_preserved_with_visible_storage_errors() {
    let (a, _stop) = login("primary", "");
    for raw in [
        b"broken".to_vec(),
        br#"{"version":1,"records":[{"message":"SECRET"}]}"#.to_vec(),
        vec![b' '; DISK_BYTES + 1],
    ] {
        let fixture = Fixture::new();
        fixture.write(&raw);
        let store = fixture.open().await;
        assert_eq!(report(&store, &a)["storage_ok"], false);
        assert_eq!(
            store.append(&a, "Safari", batch(now(), 1, 1)),
            Err(Error::Unavailable)
        );
        store.shutdown().await;
        assert_eq!(fs::read(fixture.file()).unwrap(), raw);
    }
    for kind in ["mode", "symlink", "hardlink"] {
        let fixture = Fixture::new();
        fixture.write(br#"{"version":1,"records":[]}"#);
        match kind {
            "mode" => {
                fs::set_permissions(fixture.file(), fs::Permissions::from_mode(0o644)).unwrap()
            }
            "symlink" => {
                fs::rename(fixture.file(), fixture.0.join("original")).unwrap();
                symlink("original", fixture.file()).unwrap();
            }
            _ => fs::hard_link(fixture.file(), fixture.0.join("original")).unwrap(),
        }
        let before = fs::read(fixture.file()).unwrap();
        let store = fixture.open().await;
        assert_eq!(report(&store, &a)["storage_ok"], false);
        store.shutdown().await;
        assert_eq!(fs::read(fixture.file()).unwrap(), before);
    }
    let fixture = Fixture::new();
    let store = fixture.open().await;
    store.append(&a, "Safari", batch(now(), 1, 1)).unwrap();
    flush(store.owner.inner.clone()).await;
    let original = fs::read(fixture.file()).unwrap();
    fixture.write(br#"{"version":1,"records":[]}"#);
    store.append(&a, "Safari", batch(now(), 2, 1)).unwrap();
    flush(store.owner.inner.clone()).await;
    let state = report(&store, &a);
    assert_eq!(state["storage_ok"], false);
    assert_eq!(state["pending_save"], true);
    assert_eq!(
        fs::read(fixture.file()).unwrap(),
        br#"{"version":1,"records":[]}"#
    );
    fixture.write(&original);
    flush(store.owner.inner.clone()).await;
    assert_eq!(report(&store, &a)["storage_ok"], true);
    assert_eq!(report(&store, &a)["pending_save"], false);
    store.shutdown().await;
}

#[tokio::test]
async fn append_during_snapshot_save_remains_pending_and_shutdown_joins_final_save() {
    let fixture = Fixture::new();
    let store = fixture.open().await;
    let (a, _stop) = login("primary", "");
    store.append(&a, "Safari", batch(now(), 1, 1)).unwrap();
    let mut saving = Box::pin(flush(store.owner.inner.clone()));
    {
        let _disk = store.owner.inner.disk.lock().unwrap();
        let waker = futures_util::task::noop_waker();
        assert!(saving
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        store.append(&a, "Safari", batch(now(), 2, 1)).unwrap();
    }
    saving.await;
    assert_eq!(report(&store, &a)["pending_save"], true);
    let hold = store.owner.inner.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _disk = hold.disk.lock().unwrap();
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    entered_rx.await.unwrap();
    let closing = store.clone();
    let mut closing = tokio::spawn(async move {
        closing.shutdown().await;
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut closing)
            .await
            .is_err()
    );
    assert_eq!(
        store.append(&a, "Safari", batch(now(), 3, 1)),
        Err(Error::Unavailable)
    );
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    tokio::time::timeout(Duration::from_secs(2), closing)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report(&store, &a)["pending_save"], false);
    let again = fixture.open().await;
    assert_eq!(report(&again, &a)["events"].as_array().unwrap().len(), 2);
    again.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn one_periodic_task_saves_and_last_owner_drop_requests_final_flush() {
    let fixture = Fixture::new();
    let store = fixture.open().await;
    let (a, _stop) = login("primary", "");
    store.append(&a, "Safari", batch(now(), 1, 1)).unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(INTERVAL).await;
    for _ in 0..10000 {
        if !report(&store, &a)["pending_save"].as_bool().unwrap() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(report(&store, &a)["pending_save"], false);
    store.append(&a, "Safari", batch(now(), 2, 1)).unwrap();
    let mut done = store.owner.inner.done.subscribe();
    drop(store);
    while !*done.borrow_and_update() {
        done.changed().await.unwrap();
    }
    let again = fixture.open().await;
    assert_eq!(report(&again, &a)["events"].as_array().unwrap().len(), 2);
    again.shutdown().await;
}
