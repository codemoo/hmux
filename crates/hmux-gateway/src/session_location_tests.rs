use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn go_address_boundaries_and_display_label_rules() {
    #[derive(Deserialize)]
    struct Case {
        address: String,
        allowed: bool,
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/session-location-v1/go-addresses.json"
    ))
    .unwrap();
    assert!(cases.len() > 200);
    for case in cases {
        assert_eq!(
            case.address.parse().is_ok_and(public_ip),
            case.allowed,
            "{}",
            case.address
        );
    }
    assert_eq!(
        label(br#"{"success":true,"city":" City ","region":"City","country":"Country"}"#),
        "City, Country"
    );
    assert_eq!(
        label(br#"{"success":true,"city":"bad\nlabel","region":"Fine","country":"Fine"}"#),
        "Fine"
    );
    assert_eq!(
        label(br#"{"success":true,"city":"A","region":"B","country":"A"}"#),
        "A, B, A"
    );
    for raw in [
        br#"{"success":false}"#.to_vec(),
        b"bad-json".to_vec(),
        vec![b' '; 8193],
    ] {
        assert!(label(&raw).is_empty());
    }
    assert_eq!(
        label(
            serde_json::json!({"success":true,"city":"가".repeat(54),"country":"Valid"})
                .to_string()
                .as_bytes()
        ),
        "Valid"
    );
}

#[tokio::test(start_paused = true)]
async fn cache_ttls_limit_canonicalization_and_private_exclusion() {
    let calls = Arc::new(AtomicUsize::new(0));
    let fetch: Arc<Fetch> = Arc::new({
        let calls = calls.clone();
        move |ip| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                assert!(public_ip(ip));
                if ip.to_string() == "8.8.8.8" {
                    return Err(Error::Unavailable);
                }
                Ok(br#"{"success":true,"city":"City","country":"Country"}"#.to_vec())
            })
        }
    });
    let locator = Locator::with_fetch(fetch);
    for ip in ["127.0.0.1", "10.0.0.1", "2001:db8::1", "::ffff:192.168.1.2"] {
        assert_eq!(locator.lookup(ip).await, INTERNAL);
    }
    assert_eq!(locator.lookup("invalid/path").await, "");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(locator.lookup("::ffff:1.1.1.1").await, "City, Country");
    assert_eq!(locator.lookup("1.1.1.1").await, "City, Country");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(locator.lookup("8.8.8.8").await, "");
    assert_eq!(locator.lookup("8.8.8.8").await, "");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    tokio::time::advance(Duration::from_secs(3601)).await;
    locator.lookup("1.1.1.1").await;
    locator.lookup("8.8.8.8").await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    tokio::time::advance(Duration::from_secs(24 * 3600)).await;
    locator.lookup("1.1.1.1").await;
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    for index in 0..300 {
        locator
            .lookup(&format!("11.0.{}.{}", index / 256, index % 256))
            .await;
    }
    assert_eq!(locator.0.state.lock().unwrap().cache.len(), 256);
}

#[tokio::test]
async fn coalescing_admission_and_cancelled_owner_retry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let gates = Arc::new(tokio::sync::Semaphore::new(0));
    let locator = Locator::with_fetch(Arc::new({
        let calls = calls.clone();
        let gates = gates.clone();
        move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            let gates = gates.clone();
            Box::pin(async move {
                let _permit = gates.acquire().await.unwrap();
                Ok(br#"{"success":true,"city":"Recovered"}"#.to_vec())
            })
        }
    }));
    let owner = tokio::spawn({
        let locator = locator.clone();
        async move { locator.lookup("1.1.1.1").await }
    });
    while calls.load(Ordering::SeqCst) < 1 {
        tokio::task::yield_now().await;
    }
    let waiter = tokio::spawn({
        let locator = locator.clone();
        async move { locator.lookup("1.1.1.1").await }
    });
    let other = tokio::spawn({
        let locator = locator.clone();
        async move { locator.lookup("8.8.8.8").await }
    });
    while calls.load(Ordering::SeqCst) < 2 {
        tokio::task::yield_now().await;
    }
    assert_eq!(locator.lookup("9.9.9.9").await, "");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    owner.abort();
    let _ = owner.await;
    while calls.load(Ordering::SeqCst) < 3 {
        tokio::task::yield_now().await;
    }
    gates.add_permits(1);
    assert_eq!(waiter.await.unwrap(), "Recovered");
    assert_eq!(other.await.unwrap(), "Recovered");
    assert_eq!(locator.lookup("1.1.1.1").await, "Recovered");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(locator.0.state.lock().unwrap().pending.is_empty());
}

#[tokio::test(start_paused = true)]
async fn timeout_shutdown_and_enrichment_leave_no_pending_work() {
    let calls = Arc::new(AtomicUsize::new(0));
    let locator = Locator::with_fetch(Arc::new({
        let calls = calls.clone();
        move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        }
    }));
    let now = chrono::Utc::now();
    let mut sessions: Vec<_> = (1..=20)
        .map(|n| SessionInfo {
            id: n.to_string(),
            ip: format!("11.0.0.{n}"),
            browser: String::new(),
            location: String::new(),
            created_at: now,
            last_seen_at: now,
            expires_at: now,
            current: false,
        })
        .collect();
    sessions[9].ip = "192.168.1.1".into();
    sessions[19].ip = "2001:db8::1".into();
    let start = Instant::now();
    locator.enrich(&mut sessions).await;
    assert_eq!(sessions[9].location, INTERNAL);
    assert_eq!(sessions[19].location, INTERNAL);
    assert!(Instant::now() - start <= LOOKUP_TIMEOUT + Duration::from_millis(1));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(locator.0.state.lock().unwrap().pending.is_empty());
    assert!(locator.0.state.lock().unwrap().cache.is_empty());
    assert_eq!(locator.lookup("11.0.0.1").await, "");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    locator.shutdown();
    assert_eq!(locator.lookup("11.0.0.1").await, "");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}
