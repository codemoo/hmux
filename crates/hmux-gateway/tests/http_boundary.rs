use bytes::Bytes;
use hmux_gateway::http_boundary::*;
use http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use http_body_util::{BodyExt, Full};
use serde::Deserialize;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const ORIGIN: &str = "https://hmux.example";

fn policy() -> Policy {
    Policy::new(ORIGIN, TOKEN).unwrap()
}
fn request(path: &str, method: Method) -> Request<()> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("host", "hmux.example")
        .body(())
        .unwrap()
}

#[test]
fn exact_origins_hosts_and_loopback_only() {
    for bad in [
        "http://hmux.example",
        "https://",
        "https://hmux.example/",
        "https://hmux.example?x",
        "https://user@hmux.example",
        "https://hmux.example#x",
        "https://hmux.example:999999",
        "https://hmux.example\n",
    ] {
        assert!(Policy::new(bad, TOKEN).is_err(), "{bad:?}");
    }
    assert!(Policy::new("https://[::1]:8080", TOKEN).is_ok());
    assert!(Policy::new(ORIGIN, "bad-token").is_err());
    for good in [
        "127.0.0.1:0",
        "127.1.2.3:8080",
        "[::1]:8080",
        "[::ffff:127.0.0.1]:8080",
    ] {
        assert!(loopback_address(good).is_ok());
    }
    for bad in [
        "localhost:8080",
        "0.0.0.0:8080",
        "192.0.2.1:8080",
        "[::]:8080",
    ] {
        assert!(loopback_address(bad).is_err());
    }
    let mut req = request("/api/session", Method::GET);
    assert!(policy().preflight(&req).is_ok());
    req.headers_mut()
        .append("host", HeaderValue::from_static("hmux.example"));
    assert_eq!(
        policy().preflight(&req),
        Err(StatusCode::MISDIRECTED_REQUEST)
    );
    assert_eq!(
        policy().preflight(&request("https://foreign.example/api/session", Method::GET)),
        Err(StatusCode::MISDIRECTED_REQUEST)
    );
}

#[test]
fn connector_is_never_a_browser_upgrade() {
    let mut req = request("/connect", Method::GET);
    assert_eq!(policy().preflight(&req), Err(StatusCode::FORBIDDEN));
    req.headers_mut()
        .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
    assert!(policy().preflight(&req).is_ok());
    req.headers_mut().insert("origin", ORIGIN.parse().unwrap());
    assert_eq!(policy().preflight(&req), Err(StatusCode::FORBIDDEN));
    req.headers_mut()
        .insert("origin", HeaderValue::from_static(""));
    assert!(policy().preflight(&req).is_ok());
    req.headers_mut()
        .append("origin", HeaderValue::from_static(""));
    assert_eq!(policy().preflight(&req), Err(StatusCode::FORBIDDEN));
    req.headers_mut().remove("origin");
    req.headers_mut()
        .append("authorization", format!("Bearer {TOKEN}").parse().unwrap());
    assert_eq!(policy().preflight(&req), Err(StatusCode::FORBIDDEN));
}

#[test]
fn terminal_get_and_mutations_require_exact_origin() {
    for (path, method) in [
        ("/api/terminal", Method::GET),
        ("/api/login", Method::POST),
        ("/api/sessions/revoke", Method::POST),
        ("/api/session", Method::HEAD),
    ] {
        let mut req = request(path, method);
        assert_eq!(policy().preflight(&req), Err(StatusCode::FORBIDDEN));
        req.headers_mut().insert("origin", ORIGIN.parse().unwrap());
        assert!(policy().preflight(&req).is_ok());
        req.headers_mut().append("origin", ORIGIN.parse().unwrap());
        assert_eq!(policy().preflight(&req), Err(StatusCode::FORBIDDEN));
    }
    assert_eq!(
        policy().preflight(&request("/", Method::POST)),
        Err(StatusCode::METHOD_NOT_ALLOWED)
    );
}

#[test]
fn canonical_cookie_csrf_and_browser_metadata() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "cookie",
        format!("other=a; __Host-hmux={TOKEN}").parse().unwrap(),
    );
    assert_eq!(session_token(&headers), Some(TOKEN));
    headers.append("cookie", format!("__Host-hmux={TOKEN}").parse().unwrap());
    assert_eq!(session_token(&headers), None);
    headers.clear();
    assert!(!valid_csrf(&headers, "csrf"));
    headers.insert("x-csrf-token", HeaderValue::from_static("csrf"));
    assert!(valid_csrf(&headers, "csrf"));
    headers.append("x-csrf-token", HeaderValue::from_static("csrf"));
    assert!(!valid_csrf(&headers, "csrf"));
    let mut reply = json(&true);
    set_cookie(&mut reply, Some(TOKEN)).unwrap();
    assert_eq!(
        reply.headers()[header::SET_COOKIE],
        format!("__Host-hmux={TOKEN}; Path=/; Max-Age=604800; HttpOnly; Secure; SameSite=Strict")
    );
    set_cookie(&mut reply, None).unwrap();
    assert!(reply.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    assert_eq!(
        browser_label("Mozilla/5.0 (iPhone) CriOS/150 Safari/500"),
        "Chrome on iOS"
    );
    assert_eq!(
        browser_label(&format!("{}Safari/500", "한".repeat(171))),
        "Unknown browser"
    );
}

#[test]
fn proxy_source_cannot_be_spoofed_or_rotated_by_ipv6_suffix() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-real-ip",
        HeaderValue::from_static("2001:db8:1234:5678:1234:5678:1:2"),
    );
    headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.1"));
    let external = "192.0.2.1:1234".parse().unwrap();
    assert_eq!(login_ip(external, &headers).to_string(), "192.0.2.1");
    let local = "127.0.0.1:1234".parse().unwrap();
    assert_eq!(
        login_source(login_ip(local, &headers)),
        "2001:db8:1234:5678::"
    );
    headers.append("x-real-ip", HeaderValue::from_static("203.0.113.2"));
    assert_eq!(login_ip(local, &headers).to_string(), "127.0.0.1");
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Login {
    username: String,
}

#[tokio::test]
async fn json_is_bounded_object_only_and_rejects_trailing_unknown_fields() {
    for raw in [
        r#"["test"]"#,
        r#"null"#,
        r#"{} {}"#,
        r#"{"username":"test","password":"extra"}"#,
        r#"{"username":"test","username":"duplicate"}"#,
    ] {
        let req = Request::builder()
            .header("content-type", "application/json")
            .body(Full::new(Bytes::copy_from_slice(raw.as_bytes())))
            .unwrap();
        assert_eq!(
            decode_json::<_, Login>(req).await,
            Err(DecodeError::Json),
            "{raw}"
        );
    }
    let raw = format!("{{\"username\":\"{}\"}}", "a".repeat(MAX_JSON_BYTES));
    let req = Request::builder()
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(raw)))
        .unwrap();
    assert_eq!(decode_json::<_, Login>(req).await, Err(DecodeError::Size));
    let req = Request::builder()
        .header("content-type", "application/json; charset=utf-8")
        .body(Full::new(Bytes::from_static(br#"{"username":"test"}"#)))
        .unwrap();
    assert_eq!(decode_json::<_, Login>(req).await.unwrap().username, "test");
    let reply = json(&"a".repeat(MAX_REPLY_BYTES));
    assert_eq!(reply.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(reply.into_body().collect().await.unwrap().to_bytes().len() < 100);
}

async fn exchange(address: std::net::SocketAddr, raw: &str) -> String {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(raw.as_bytes()).await.unwrap();
    let mut reply = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut reply))
        .await
        .unwrap();
    // macOS can reset after sending a 431 when unread request bytes remain.
    if let Err(error) = result {
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
    }
    String::from_utf8(reply).unwrap()
}

#[tokio::test]
async fn actual_socket_rejects_before_handler_and_adds_headers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = CancellationToken::new();
    let server = tokio::spawn(serve(
        listener,
        Arc::new(policy()),
        move |_request, _context| {
            counter.fetch_add(1, Ordering::Relaxed);
            async { json(&true) }
        },
        stop.clone(),
    ));
    let reply = exchange(
        address,
        "GET /api/session HTTP/1.1\r\nHost: wrong.example\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(reply.starts_with("HTTP/1.1 421"));
    assert!(reply.contains("cache-control: no-store\r\n"));
    assert!(reply.contains("content-security-policy:"));
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let reply = exchange(
        address,
        "GET / HTTP/1.1\r\nHost: hmux.example\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(reply.starts_with("HTTP/1.1 200"));
    assert!(reply.contains("cache-control: no-store\r\n"));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let oversized = format!(
        "GET / HTTP/1.1\r\nHost: hmux.example\r\nX-Large: {}\r\nConnection: close\r\n\r\n",
        "a".repeat(9000)
    );
    let reply = exchange(address, &oversized).await;
    assert!(reply.starts_with("HTTP/1.1 431"));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    stop.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn slow_chunked_body_exceeding_limit_is_not_delivered_to_route() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = CancellationToken::new();
    let server = tokio::spawn(serve(
        listener,
        Arc::new(policy()),
        |request, _| async {
            match decode_json::<_, Login>(request).await {
                Err(DecodeError::Size) => error(StatusCode::PAYLOAD_TOO_LARGE),
                _ => json(&false),
            }
        },
        stop.clone(),
    ));
    let mut raw = "POST /api/login HTTP/1.1\r\nHost: hmux.example\r\nOrigin: https://hmux.example\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_owned();
    for _ in 0..3 {
        raw.push_str(&format!("2000\r\n{}\r\n", "a".repeat(8192)));
    }
    raw.push_str("0\r\n\r\n");
    let reply = exchange(address, &raw).await;
    assert!(reply.starts_with("HTTP/1.1 413"));
    stop.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn upgrades_keep_connection_admission_and_are_joined_on_shutdown() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = CancellationToken::new();
    let entered = Arc::new(AtomicUsize::new(0));
    let exiting = Arc::new(AtomicUsize::new(0));
    struct Exited(Arc<AtomicUsize>);
    impl Drop for Exited {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let started = entered.clone();
    let completed = exiting.clone();
    let server = tokio::spawn(serve(
        listener,
        Arc::new(policy()),
        move |mut request, context| {
            let started = started.clone();
            let completed = completed.clone();
            async move {
                context
                    .spawn_upgrade(&mut request, move |mut stream| async move {
                        let _done = Exited(completed);
                        started.fetch_add(1, Ordering::SeqCst);
                        let mut byte = [0u8; 1];
                        let _ = stream.read(&mut byte).await;
                    })
                    .unwrap();
                let mut reply = error(StatusCode::SWITCHING_PROTOCOLS);
                reply
                    .headers_mut()
                    .insert("connection", HeaderValue::from_static("upgrade"));
                reply
                    .headers_mut()
                    .insert("upgrade", HeaderValue::from_static("test"));
                *reply.body_mut() = Body::new(Bytes::new());
                reply
            }
        },
        stop.clone(),
    ));
    let mut peers = Vec::new();
    for _ in 0..MAX_CONNECTIONS {
        let mut peer = TcpStream::connect(address).await.unwrap();
        peer.write_all(
            b"GET / HTTP/1.1\r\nHost: hmux.example\r\nConnection: Upgrade\r\nUpgrade: test\r\n\r\n",
        )
        .await
        .unwrap();
        let mut response = Vec::new();
        while !response.ends_with(b"\r\n\r\n") {
            response.push(peer.read_u8().await.unwrap());
        }
        assert!(response.starts_with(b"HTTP/1.1 101"));
        peers.push(peer);
    }
    // All original HTTP tasks have finished, but upgraded peers still own slots.
    let mut extra = TcpStream::connect(address).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async move {
        let mut buf = [0; 1];
        assert!(matches!(extra.read(&mut buf).await, Ok(0) | Err(_)));
    })
    .await
    .unwrap();
    assert_eq!(entered.load(Ordering::SeqCst), MAX_CONNECTIONS);
    stop.cancel();
    server.await.unwrap().unwrap();
    assert_eq!(exiting.load(Ordering::SeqCst), MAX_CONNECTIONS);
    for mut peer in peers {
        assert_eq!(peer.read(&mut [0; 1]).await.unwrap(), 0);
    }
}

#[tokio::test]
async fn gateway_shutdown_waits_for_cooperative_upgrade_cleanup() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = CancellationToken::new();
    let cleanup = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let ready = Arc::new(tokio::sync::Notify::new());
    let handler_cleanup = cleanup.clone();
    let handler_release = release.clone();
    let handler_ready = ready.clone();
    let server = tokio::spawn(serve(
        listener,
        Arc::new(policy()),
        move |mut request, context| {
            let cleanup = handler_cleanup.clone();
            let release = handler_release.clone();
            let ready = handler_ready.clone();
            async move {
                context
                    .spawn_upgrade_graceful(&mut request, move |socket, shutdown| async move {
                        ready.notify_one();
                        shutdown.cancelled().await;
                        cleanup.notify_one();
                        release.notified().await;
                        drop(socket);
                    })
                    .unwrap();
                let mut reply = error(StatusCode::SWITCHING_PROTOCOLS);
                reply
                    .headers_mut()
                    .insert("connection", HeaderValue::from_static("upgrade"));
                reply
                    .headers_mut()
                    .insert("upgrade", HeaderValue::from_static("test"));
                *reply.body_mut() = Body::new(Bytes::new());
                reply
            }
        },
        stop.clone(),
    ));
    let mut peer = TcpStream::connect(address).await.unwrap();
    peer.write_all(
        b"GET / HTTP/1.1\r\nHost: hmux.example\r\nConnection: Upgrade\r\nUpgrade: test\r\n\r\n",
    )
    .await
    .unwrap();
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        headers.push(peer.read_u8().await.unwrap());
    }
    assert!(headers.starts_with(b"HTTP/1.1 101"));
    tokio::time::timeout(Duration::from_secs(2), ready.notified())
        .await
        .unwrap();
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(2), cleanup.notified())
        .await
        .unwrap();
    assert!(!server.is_finished());
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(peer.read(&mut [0u8; 1]).await.unwrap(), 0);
}
