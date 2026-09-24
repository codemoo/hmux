use super::*;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
static NETWORK_TEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn now() -> DateTime<Utc> {
    "2026-09-24T05:00:00Z".parse().unwrap()
}
fn quota() -> Vec<u8> {
    br#"{"upstream_limits":[{"limit_type":"credits","limit_window":"5hr","max_value":100,"current_value":25,"remaining_value":75,"source":"aggregate"}],"account_pool_usage":{"primary":80}}"#.to_vec()
}
struct Reply {
    status: u16,
    body: Vec<u8>,
    delay: Duration,
}
impl Reply {
    fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            body: body.into(),
            delay: Duration::ZERO,
        }
    }
    fn status(status: u16) -> Self {
        Self {
            status,
            body: Vec::new(),
            delay: Duration::ZERO,
        }
    }
    fn hold() -> Self {
        Self {
            status: 200,
            body: quota(),
            delay: Duration::from_secs(30),
        }
    }
}
struct Server {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server(replies: Vec<Reply>) -> Server {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    let task = tokio::spawn(async move {
        let mut replies = VecDeque::from(replies);
        while let Some(reply) = replies.pop_front() {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
                let Ok(n) = stream.read(&mut chunk).await else {
                    break;
                };
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..n]);
            }
            observed
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&request).into_owned());
            tokio::time::sleep(reply.delay).await;
            let head = format!(
                "HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                reply.status,
                reply.body.len()
            );
            if stream.write_all(head.as_bytes()).await.is_err() {
                continue;
            }
            let _ = stream.write_all(&reply.body).await;
        }
    });
    Server {
        address,
        requests,
        task,
    }
}
async fn make_owner(server: &Server) -> Owner {
    let endpoint = Endpoint::parse(&format!("http://{}", server.address)).unwrap();
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(rustls::RootCertStore::empty())
    .with_no_client_auth();
    let dial = crate::dial::Client::with_config(
        Arc::new(tls),
        Arc::new(|_, _| Err(crate::dial::Error::Resolve)),
    );
    Owner::new(endpoint, "synthetic-key".into(), Client::new(dial)).unwrap()
}
fn count(server: &Server) -> usize {
    server.requests.lock().unwrap().len()
}

#[test]
fn endpoint_allowlist_normalization_and_redaction() {
    let default = Endpoint::parse("").unwrap();
    assert_eq!(default.authority, "127.0.0.1:2455");
    assert_eq!(default.path, "/v1/usage");
    assert_eq!(
        Endpoint::parse("http://localhost:3214/prefix/v1/?ignored=yes#fragment")
            .unwrap()
            .path,
        "/prefix/v1/usage"
    );
    assert_eq!(
        Endpoint::parse("https://example.test/base/v1")
            .unwrap()
            .path,
        "/base/v1/usage"
    );
    assert!(Endpoint::parse("http://[::1]:3214")
        .unwrap()
        .loopback
        .unwrap()
        .ip()
        .is_loopback());
    for invalid in [
        "http://example.test",
        "http://127.0.0.2:3000",
        "http://user@localhost:3000",
        "ftp://localhost",
        "http://localhost:0",
        "http://localhost:abc",
        "https://user@example.test",
    ] {
        assert_eq!(
            Endpoint::parse(invalid).err(),
            Some(Failure::Contract),
            "{invalid}"
        );
    }
    assert!(!format!("{default:?}").contains("127.0.0.1"));
}

#[tokio::test]
async fn bearer_request_pool_normalization_cache_sticky_backoff_and_expiry() {
    let _lock = NETWORK_TEST.lock().await;
    let server = server(vec![
        Reply::ok(quota()),
        Reply::status(503),
        Reply::ok(b"{".to_vec()),
        Reply::status(503),
    ])
    .await;
    let mut owner = make_owner(&server).await;
    let cancel = CancellationToken::new();
    let first = owner.refresh(now(), &cancel).await;
    assert_eq!(first.status.state, "ok");
    assert!(first.rolling_5h_observed);
    assert!(!first.weekly_observed);
    assert!((first.rolling_5h.used_pct - 0.2).abs() < 1e-9);
    assert_eq!(count(&server), 1);
    let request = server.requests.lock().unwrap()[0].clone();
    assert!(request.starts_with("GET /v1/usage HTTP/1.1\r\n"));
    assert!(request
        .to_ascii_lowercase()
        .contains("authorization: bearer synthetic-key\r\n"));
    assert!(!request.to_ascii_lowercase().contains("accept-encoding:"));
    let cached = owner.refresh(now() + Span::seconds(10), &cancel).await;
    assert_eq!(cached.seq, 2);
    assert!(!cached.status.stale);
    assert_eq!(count(&server), 1);
    let stale = owner.refresh(now() + Span::seconds(61), &cancel).await;
    assert_eq!(stale.status.state, "ok");
    assert!(stale.status.stale);
    assert_eq!(
        stale.status.quota_observed_at,
        first.status.quota_observed_at
    );
    assert_eq!(count(&server), 2);
    assert!(
        owner
            .refresh(now() + Span::seconds(62), &cancel)
            .await
            .status
            .stale
    );
    assert_eq!(count(&server), 2);
    let malformed = owner.refresh(now() + Span::seconds(92), &cancel).await;
    assert_eq!(malformed.status.state, "ok");
    assert!(malformed.status.stale);
    assert_eq!(count(&server), 3);
    let expired = owner.refresh(now() + Span::minutes(11), &cancel).await;
    assert_eq!(expired.status.state, "networkError");
    assert_eq!(expired.status.quota_source, "codex_lb");
    assert_eq!(count(&server), 4);
}

#[tokio::test]
async fn empty_pool_degrades_immediately_but_invalid_quota_is_sticky() {
    let _lock = NETWORK_TEST.lock().await;
    let empty_pool=br#"{"upstream_limits":[{"limit_type":"credits","limit_window":"5h","max_value":100,"current_value":25,"remaining_value":75,"source":"aggregate"}],"account_pool_usage":{}}"#;
    let bad_quota=br#"{"upstream_limits":[{"limit_type":"credits","limit_window":"5h","max_value":100,"current_value":125,"remaining_value":0,"source":"aggregate"}]}"#;
    let server = server(vec![
        Reply::ok(quota()),
        Reply::ok(empty_pool.to_vec()),
        Reply::ok(bad_quota.to_vec()),
    ])
    .await;
    let mut owner = make_owner(&server).await;
    let cancel = CancellationToken::new();
    assert_eq!(owner.refresh(now(), &cancel).await.status.state, "ok");
    let empty = owner.refresh(now() + Span::seconds(61), &cancel).await;
    assert_eq!(empty.status.state, "quotaEndpointChanged");
    assert!(!empty.rolling_5h_observed);
    assert_eq!(count(&server), 2);
    assert!(
        owner
            .refresh(now() + Span::seconds(62), &cancel)
            .await
            .status
            .stale
    );
    let invalid = owner.refresh(now() + Span::seconds(92), &cancel).await;
    assert_eq!(invalid.status.state, "ok");
    assert!(invalid.status.stale);
    assert_eq!(count(&server), 3);
}

#[tokio::test]
async fn body_cap_cancel_and_deadline_are_bounded() {
    let _lock = NETWORK_TEST.lock().await;
    let huge = server(vec![Reply::ok(vec![b'x'; (1 << 20) + 1])]).await;
    let mut owner = make_owner(&huge).await;
    assert_eq!(
        owner
            .refresh(now(), &CancellationToken::new())
            .await
            .status
            .state,
        "networkError"
    );
    let slow = server(vec![Reply::hold()]).await;
    let mut owner = make_owner(&slow).await;
    let started = std::time::Instant::now();
    let timed = owner.refresh(now(), &CancellationToken::new()).await;
    assert_eq!(timed.status.state, "networkError");
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert!(started.elapsed() < Duration::from_secs(8));
    let slow = server(vec![Reply::hold()]).await;
    let mut owner = make_owner(&slow).await;
    let cancel = CancellationToken::new();
    let stopper = cancel.clone();
    let calls = slow.requests.clone();
    let task = tokio::spawn(async move {
        for _ in 0..100 {
            if !calls.lock().unwrap().is_empty() {
                stopper.cancel();
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    let started = std::time::Instant::now();
    assert_eq!(
        owner.refresh(now(), &cancel).await.status.state,
        "networkError"
    );
    task.await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
}
