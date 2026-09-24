use super::*;

#[path = "push_delivery_tests.rs"]
mod delivery;

/// Opt-in OS check: normal CI uses synthetic roots; constrained macOS runners
/// can intentionally expose an empty native trust store.
#[test]
#[ignore = "requires access to the host's native TLS trust store"]
fn native_system_trust_initializes_transport() {
    hmux_core::runtime::run_process(async {
        let client = Client::new().await.expect("native TLS initialization");
        let second = Client::new()
            .await
            .expect("cached native TLS initialization");
        assert!(Arc::ptr_eq(&client.0.tls, &second.0.tls));
        assert_eq!(client.shutdown(), 0);
        assert_eq!(second.shutdown(), 0);
    })
    .unwrap();
}

#[test]
fn actual_go_public_address_policy() {
    #[derive(serde::Deserialize)]
    struct Case {
        address: String,
        allowed: bool,
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/push-v1/go-addresses.json"
    ))
    .unwrap();
    assert!(cases.len() > 150);
    for case in cases {
        let actual = case.address.parse::<IpAddr>().is_ok_and(public_address);
        assert_eq!(actual, case.allowed, "{}", case.address);
    }
}

#[tokio::test]
async fn cancellation_and_deadline_close_stalled_tls_and_http_sockets() {
    for stage in ["tls", "headers", "body"] {
        let (cert, key) = cert_for("fcm.googleapis.com", false);
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let client = client(cert.clone(), listener.local_addr().unwrap());
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
        let (ready, started) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 1024];
            if stage == "tls" {
                let _ = ready.send(());
                while socket.read(&mut buffer).await.unwrap_or(0) != 0 {}
                return;
            }
            let mut socket = TlsAcceptor::from(Arc::new(config))
                .accept(socket)
                .await
                .unwrap();
            assert_eq!(socket.get_ref().1.server_name(), Some("fcm.googleapis.com"));
            let mut request = Vec::new();
            while request.len() < 8192 {
                let size = socket.read(&mut buffer).await.unwrap();
                assert_ne!(size, 0);
                request.extend_from_slice(&buffer[..size]);
                if request
                    .windows(4)
                    .position(|p| p == b"\r\n\r\n")
                    .is_some_and(|at| request.len() >= at + 8)
                {
                    break;
                }
            }
            if stage == "body" {
                socket
                    .write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 100\r\n\r\na")
                    .await
                    .unwrap();
            }
            let _ = ready.send(());
            assert_eq!(socket.read(&mut buffer).await.unwrap_or(0), 0);
        });
        let (access, cancel) = access();
        let deadline = Instant::now() + Duration::from_millis(600);
        let send = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .send(prepared(), access, Some(deadline), || async { Ok(()) })
                    .await
            }
        });
        timeout(Duration::from_secs(3), started)
            .await
            .unwrap()
            .unwrap();
        let expected = match stage {
            "tls" => {
                cancel.send(true).unwrap();
                Error::Unauthorized
            }
            "headers" => {
                client.shutdown();
                Error::Cancelled
            }
            _ => Error::Timeout,
        };
        assert_eq!(
            timeout(Duration::from_secs(3), send)
                .await
                .unwrap()
                .unwrap(),
            Err(expected)
        );
        timeout(Duration::from_secs(3), peer)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(client.0.slots.available_permits(), SLOTS);
    }
}

#[tokio::test]
async fn send_time_authorization_and_expiry_prevent_http_post() {
    for mode in ["deny", "revoke", "expired-check", "pending-check"] {
        let (cert, key) = cert_for("fcm.googleapis.com", false);
        let (address, server) = server(cert.clone(), key, Vec::new()).await;
        let client = client(cert, address);
        let (access, cancel) = access();
        let _live_session = cancel.clone();
        let deadline = Instant::now() + Duration::from_millis(400);
        let result = client
            .send(prepared(), access, Some(deadline), move || async move {
                match mode {
                    "deny" => Err(Error::Unauthorized),
                    "revoke" => {
                        cancel.send(true).unwrap();
                        Ok(())
                    }
                    "expired-check" => {
                        // A non-yielding callback can return after timeout's timer
                        // became ready; the explicit pre-POST check must catch it.
                        std::thread::sleep(
                            deadline.saturating_duration_since(Instant::now())
                                + Duration::from_millis(10),
                        );
                        Ok(())
                    }
                    _ => std::future::pending().await,
                }
            })
            .await;
        let expected = if mode == "deny" || mode == "revoke" {
            Error::Unauthorized
        } else {
            Error::Timeout
        };
        assert_eq!(result, Err(expected), "{mode}");
        assert!(
            timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap()
                .is_empty(),
            "{mode} posted"
        );
        assert_eq!(client.0.slots.available_permits(), SLOTS);
    }
}

#[tokio::test]
async fn response_header_limit_and_partial_body_preserve_status_contract() {
    for large_header in [true, false] {
        let (cert, key) = cert_for("fcm.googleapis.com", false);
        let response = if large_header {
            format!(
                "HTTP/1.1 201 Created\r\nX-Oversized: {}\r\n\r\n",
                "x".repeat(9000)
            )
            .into_bytes()
        } else {
            b"HTTP/1.1 410 Gone\r\nContent-Length: 10\r\n\r\nx".to_vec()
        };
        let (address, server) = server(cert.clone(), key, response).await;
        let (access, _cancel) = access();
        let result = client(cert, address)
            .send(prepared(), access, None, || async { Ok(()) })
            .await;
        if large_header {
            assert_eq!(result, Err(Error::Unavailable));
        } else {
            assert_eq!(result, Ok(StatusCode::GONE));
        }
        server.await.unwrap();
    }
}

#[test]
fn blocked_native_dns_does_not_prevent_process_exit_or_unwind() {
    const MODE: &str = "HMUX_PUSH_RUNTIME_EXIT_CHILD";
    if let Ok(mode) = std::env::var(MODE) {
        let panic_mode = mode == "panic";
        let outcome = std::panic::catch_unwind(|| {
            hmux_core::runtime::run_process(async {
                let (cert, _) = cert_for("fcm.googleapis.com", false);
                let (started, wait_started) = oneshot::channel();
                let started = Mutex::new(Some(started));
                let resolver: Arc<Resolver> = Arc::new(move |_| {
                    started.lock().unwrap().take().unwrap().send(()).unwrap();
                    // Only the owned child process runs this injection. Unlike the
                    // in-process tests this OS-like resolver deliberately never returns.
                    loop {
                        std::thread::park();
                    }
                });
                let client = isolated_config(config_from_roots(vec![cert]).unwrap(), resolver);
                let (access, cancel) = access();
                let worker = tokio::spawn({
                    let client = client.clone();
                    async move {
                        client
                            .send(prepared(), access, None, || async { Ok(()) })
                            .await
                    }
                });
                timeout(Duration::from_secs(2), wait_started)
                    .await
                    .unwrap()
                    .unwrap();
                cancel.send(true).unwrap();
                assert_eq!(worker.await.unwrap(), Err(Error::Unauthorized));
                assert_eq!(client.shutdown(), 1);
                assert_eq!(client.0.slots.available_permits(), SLOTS - 1);
                if panic_mode {
                    panic!("synthetic root future panic");
                }
            })
        });
        if panic_mode {
            assert!(outcome.is_err());
        } else {
            outcome.unwrap().unwrap();
        }
        eprintln!("blocked native DNS child completed: {mode}");
        return;
    }
    for mode in ["normal", "panic"] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "push_transport::tests::blocked_native_dns_does_not_prevent_process_exit_or_unwind",
                "--nocapture",
            ])
            .env(MODE, mode)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "blocked DNS process failed to exit ({mode}): {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "child ({mode}): {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains(&format!("blocked native DNS child completed: {mode}")));
    }
}
use bytes::Bytes;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rcgen::{CertificateParams, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Condvar, Mutex,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::watch,
};
use tokio_rustls::TlsAcceptor;

struct OpenGate(Arc<(Mutex<bool>, Condvar)>);
impl Drop for OpenGate {
    fn drop(&mut self) {
        let (lock, cv) = &*self.0;
        *lock.lock().unwrap_or_else(|poison| poison.into_inner()) = true;
        cv.notify_all();
    }
}

fn access() -> (SessionAccess, watch::Sender<bool>) {
    let (sender, cancelled) = watch::channel(false);
    (
        SessionAccess {
            id: "synthetic-login".into(),
            expires_at: DateTime::<Utc>::from(SystemTime::now()) + ChronoDuration::minutes(5),
            csrf: String::new(),
            username: "synthetic".into(),
            profile: String::new(),
            cancelled,
        },
        sender,
    )
}
fn prepared() -> Prepared {
    Prepared {
        endpoint: "https://fcm.googleapis.com/short/path?token=synthetic".into(),
        authorization: "vapid t=synthetic, k=synthetic".into(),
        content_encoding: "aes128gcm",
        content_type: "application/octet-stream",
        ttl: "120",
        urgency: "normal",
        topic: "0123456789abcdefghijklmnopqrstuv".into(),
        body: Bytes::from_static(b"four"),
    }
}
fn cert_for(host: &str, expired: bool) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec![host.to_owned()]).unwrap();
    if expired {
        params.not_before = rcgen::date_time_ymd(2019, 1, 1);
        params.not_after = rcgen::date_time_ymd(2020, 1, 1);
    }
    let cert = params.self_signed(&key).unwrap();
    (
        cert.der().clone(),
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
    )
}
fn client(cert: CertificateDer<'static>, address: SocketAddr) -> Client {
    let tls = config_from_roots(vec![cert]).unwrap();
    let resolver: Arc<Resolver> = Arc::new(|_| Ok(vec![SocketAddr::from(([8, 8, 8, 8], 443))]));
    let mut client = isolated_config(tls, resolver);
    Arc::get_mut(&mut client.0).unwrap().dial_override = Some(address);
    client
}
async fn server(
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
    reply: Vec<u8>,
) -> (SocketAddr, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let handle = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let Ok(mut socket) = acceptor.accept(socket).await else {
            return Vec::new();
        };
        let mut request = Vec::new();
        let mut buf = [0; 1024];
        while request.len() < 8192 {
            let count = socket.read(&mut buf).await.unwrap_or(0);
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buf[..count]);
            if request
                .windows(4)
                .position(|part| part == b"\r\n\r\n")
                .is_some_and(|at| {
                    request.len() >= at + 4 + if request.starts_with(b"GET ") { 0 } else { 4 }
                })
            {
                break;
            }
        }
        let _ = socket.write_all(&reply).await;
        let _ = socket.shutdown().await;
        request
    });
    (address, handle)
}

#[tokio::test]
async fn verified_tls_exact_post_and_completed_response() {
    let (cert, key) = cert_for("fcm.googleapis.com", false);
    let (address, server) = server(
        cert.clone(),
        key,
        b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec(),
    )
    .await;
    let client = client(cert, address);
    let (access, _sender) = access();
    let checked = Arc::new(AtomicUsize::new(0));
    let observed = checked.clone();
    let status = client
        .send(prepared(), access, None, move || async move {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(checked.load(Ordering::SeqCst), 1);
    let request = String::from_utf8(server.await.unwrap())
        .unwrap()
        .to_ascii_lowercase();
    assert!(request.starts_with("post /short/path?token=synthetic http/1.1\r\n"));
    for header in [
        "host: fcm.googleapis.com",
        "authorization: vapid t=synthetic, k=synthetic",
        "content-encoding: aes128gcm",
        "content-type: application/octet-stream",
        "ttl: 120",
        "urgency: normal",
        "topic: 0123456789abcdefghijklmnopqrstuv",
        "content-length: 4",
    ] {
        assert!(request.contains(header), "missing {header}");
    }
    assert!(request.ends_with("\r\n\r\nfour"));
    assert_eq!(client.shutdown(), 0);
}

#[tokio::test]
async fn certificate_rejections_prevent_post() {
    for mode in ["untrusted", "wrong-host", "expired"] {
        let (server_cert, key) = cert_for(
            if mode == "wrong-host" {
                "web.push.apple.com"
            } else {
                "fcm.googleapis.com"
            },
            mode == "expired",
        );
        let (address, server) = server(server_cert.clone(), key, Vec::new()).await;
        let trusted = if mode == "untrusted" {
            cert_for("fcm.googleapis.com", false).0
        } else {
            server_cert
        };
        let client = client(trusted, address);
        let (access, _sender) = access();
        assert_eq!(
            client
                .send(prepared(), access, None, || async { Ok(()) })
                .await,
            Err(Error::Unavailable),
            "{mode}"
        );
        assert!(server.await.unwrap().is_empty(), "{mode} sent HTTP");
    }
}

#[test]
fn all_resolved_addresses_are_admitted_or_rejected_before_dial() {
    let public = SocketAddr::from(([8, 8, 8, 8], 443));
    let private = SocketAddr::from(([127, 0, 0, 1], 443));
    assert_eq!(
        validate_addresses(vec![public, private]),
        Err(Error::Invalid)
    );
    assert_eq!(validate_addresses(vec![]), Err(Error::Invalid));
    assert_eq!(
        validate_addresses(vec![public; MAX_ADDRESSES + 1]),
        Err(Error::Invalid)
    );
    assert_eq!(
        validate_addresses(vec![SocketAddr::from(([8, 8, 8, 8], 80))]),
        Err(Error::Invalid)
    );
    for ip in [
        "::ffff:127.0.0.1",
        "100.64.0.1",
        "192.0.2.1",
        "198.19.0.1",
        "2001:db8::1",
        "2002::1",
        "3fff::1",
    ] {
        assert!(!public_address(ip.parse().unwrap()), "{ip}");
    }
    for ip in ["8.8.8.8", "::ffff:8.8.8.8", "2606:4700:4700::1111"] {
        assert!(public_address(ip.parse().unwrap()), "{ip}");
    }
}

#[tokio::test]
async fn resolver_retains_two_slots_after_callers_drop_and_never_dials() {
    let (cert, _) = cert_for("fcm.googleapis.com", false);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let _release_on_unwind = OpenGate(gate.clone());
    let started = Arc::new(AtomicUsize::new(0));
    let wait_gate = gate.clone();
    let count = started.clone();
    let resolver: Arc<Resolver> = Arc::new(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        let (lock, cv) = &*wait_gate;
        let mut open = lock.lock().unwrap();
        while !*open {
            open = cv.wait(open).unwrap();
        }
        Ok(vec![SocketAddr::from(([8, 8, 8, 8], 443))])
    });
    let mut client = isolated_config(config_from_roots(vec![cert]).unwrap(), resolver);
    Arc::get_mut(&mut client.0).unwrap().dial_override = Some(address);
    let (first_access, first_sender) = access();
    let (second_access, _second_sender) = access();
    let first = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .send(prepared(), first_access, None, || async { Ok(()) })
                .await
        }
    });
    let second = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .send(
                    prepared(),
                    second_access,
                    Some(Instant::now() + Duration::from_millis(150)),
                    || async { Ok(()) },
                )
                .await
        }
    });
    timeout(Duration::from_secs(2), async {
        while started.load(Ordering::SeqCst) != 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let (third_access, _third_sender) = access();
    assert_eq!(
        client
            .send(prepared(), third_access, None, || async { Ok(()) })
            .await,
        Err(Error::Busy)
    );
    first_sender.send(true).unwrap();
    assert_eq!(first.await.unwrap(), Err(Error::Unauthorized));
    assert_eq!(second.await.unwrap(), Err(Error::Timeout));
    assert_eq!(client.outstanding_dns_jobs(), 2);
    let (fourth_access, _fourth_sender) = access();
    assert_eq!(
        client
            .send(prepared(), fourth_access, None, || async { Ok(()) })
            .await,
        Err(Error::Busy)
    );
    assert_eq!(client.shutdown(), 2);
    {
        let (lock, cv) = &*gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    timeout(Duration::from_secs(2), async {
        while client.outstanding_dns_jobs() != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(timeout(Duration::from_millis(150), listener.accept())
        .await
        .is_err());
}

#[tokio::test]
async fn redirect_rejected_and_large_response_is_bounded_discard() {
    let (cert, key) = cert_for("fcm.googleapis.com", false);
    let (address, first_server) = server(
        cert.clone(),
        key,
        b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1/private\r\nContent-Length: 0\r\n\r\n"
            .to_vec(),
    )
    .await;
    let (first_access, _sender) = access();
    assert_eq!(
        client(cert, address)
            .send(prepared(), first_access, None, || async { Ok(()) })
            .await,
        Err(Error::Redirect)
    );
    first_server.await.unwrap();

    let (cert, key) = cert_for("fcm.googleapis.com", false);
    let mut response = b"HTTP/1.1 410 Gone\r\nContent-Length: 8000\r\n\r\n".to_vec();
    response.extend(vec![b'x'; 8000]);
    let (address, server) = server(cert.clone(), key, response).await;
    let (access, _sender) = access();
    assert_eq!(
        client(cert, address)
            .send(prepared(), access, None, || async { Ok(()) })
            .await,
        Ok(StatusCode::GONE)
    );
    server.await.unwrap();
}

// Most synthetic tests are independent owners with isolated admission. The
// cross-owner test below explicitly uses the production process-wide pool.
fn isolated_config(tls: Arc<ClientConfig>, resolver: Arc<Resolver>) -> Client {
    let mut client = Client::with_config(tls, resolver);
    Arc::get_mut(&mut client.0).unwrap().slots = Arc::new(Semaphore::new(SLOTS));
    client
}

#[tokio::test]
async fn recreated_transport_owners_cannot_bypass_process_dns_admission() {
    let (cert, _) = cert_for("fcm.googleapis.com", false);
    let tls = config_from_roots(vec![cert]).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let started = Arc::new(AtomicUsize::new(0));
    let resolver: Arc<Resolver> = {
        let gate = gate.clone();
        let started = started.clone();
        Arc::new(move |_| {
            started.fetch_add(1, Ordering::SeqCst);
            let (lock, cv) = &*gate;
            // Failures in the test must not leave an in-process worker parked.
            let _guard = cv
                .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |open| !*open)
                .unwrap();
            Ok(vec![SocketAddr::from(([8, 8, 8, 8], 443))])
        })
    };
    let mut owners = Vec::new();
    let mut senders = Vec::new();
    let mut sends = Vec::new();
    for _ in 0..2 {
        let mut client = Client::with_config(tls.clone(), resolver.clone());
        Arc::get_mut(&mut client.0).unwrap().dial_override = Some(listener.local_addr().unwrap());
        let (access, sender) = access();
        senders.push(sender);
        sends.push(tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .send(prepared(), access, None, || async { Ok(()) })
                    .await
            }
        }));
        owners.push(client);
    }
    assert!(Arc::ptr_eq(&owners[0].0.slots, &owners[1].0.slots));
    timeout(Duration::from_secs(2), async {
        while started.load(Ordering::SeqCst) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let recreated = Client::with_config(tls, resolver);
    let (access, _sender) = access();
    assert_eq!(
        recreated
            .send(prepared(), access.clone(), None, || async { Ok(()) })
            .await,
        Err(Error::Busy)
    );
    for client in &owners {
        assert_eq!(client.shutdown(), 1);
    }
    for send in sends {
        assert_eq!(send.await.unwrap(), Err(Error::Cancelled));
    }
    assert_eq!(
        recreated
            .send(prepared(), access.clone(), None, || async { Ok(()) })
            .await,
        Err(Error::Busy)
    );
    {
        let (lock, cv) = &*gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    timeout(Duration::from_secs(2), async {
        while recreated.0.slots.available_permits() != SLOTS {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!recreated.0.slots.is_closed());
    assert_eq!(
        owners[0]
            .send(prepared(), access, None, || async { Ok(()) })
            .await,
        Err(Error::Cancelled)
    );
    assert!(timeout(Duration::from_millis(100), listener.accept())
        .await
        .is_err());
}

#[tokio::test]
async fn location_fixed_endpoint_verified_tls_no_credentials_and_bounded_response() {
    for (status, body, expected) in [
        (
            "200 OK",
            br#"{"success":true,"city":"City"}"#.to_vec(),
            None,
        ),
        ("302 Found", Vec::new(), Some(Error::Redirect)),
        ("500 Error", Vec::new(), Some(Error::Unavailable)),
        ("200 OK", vec![b'x'; 8193], Some(Error::Invalid)),
    ] {
        let (cert, key) = cert_for("ipwho.is", false);
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nLocation: https://other.example/\r\n\r\n",
            body.len()
        );
        let (address, server) =
            server(cert.clone(), key, [response.as_bytes(), &body].concat()).await;
        let result = client(cert, address)
            .location("::ffff:1.1.1.1".parse().unwrap())
            .await;
        match expected {
            Some(error) => assert_eq!(result, Err(error)),
            None => assert_eq!(result.unwrap(), body),
        }
        let request = String::from_utf8(server.await.unwrap()).unwrap();
        assert!(request.starts_with("GET /1.1.1.1?fields=success,country,region,city HTTP/1.1\r\n"));
        assert!(request
            .to_ascii_lowercase()
            .contains("\r\nhost: ipwho.is\r\n"));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        assert!(!request.to_ascii_lowercase().contains("cookie:"));
    }
    let (cert, key) = cert_for("wrong.example", false);
    let (address, server) = server(cert.clone(), key, Vec::new()).await;
    assert_eq!(
        client(cert, address)
            .location("1.1.1.1".parse().unwrap())
            .await,
        Err(Error::Unavailable)
    );
    assert!(server.await.unwrap().is_empty());
}

#[tokio::test]
async fn location_rejects_private_inputs_and_cancellation_closes_socket() {
    let (cert, _) = cert_for("ipwho.is", false);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let client = client(cert, listener.local_addr().unwrap());
    for ip in ["127.0.0.1", "::ffff:192.168.1.1", "2001:db8::1"] {
        assert_eq!(
            client.location(ip.parse().unwrap()).await,
            Err(Error::Invalid)
        );
    }
    let work = tokio::spawn({
        let client = client.clone();
        async move { client.location("1.1.1.1".parse().unwrap()).await }
    });
    let (mut socket, _) = timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    client.shutdown();
    assert_eq!(work.await.unwrap(), Err(Error::Cancelled));
    let mut bytes = Vec::new();
    timeout(Duration::from_secs(2), socket.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(client.0.slots.available_permits(), SLOTS);
}
