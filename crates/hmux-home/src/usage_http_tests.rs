use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn now() -> DateTime<Utc> {
    "2026-09-24T00:00:00Z".parse().unwrap()
}
async fn response(raw: Vec<u8>) -> Result<Vec<u8>, Failure> {
    let (client, mut server) = tokio::io::duplex(4096);
    let task = tokio::spawn(async move {
        let mut request = Vec::new();
        loop {
            request.push(server.read_u8().await.unwrap());
            if request.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        assert!(request.starts_with(b"GET /api/oauth/usage HTTP/1.1\r\n"));
        // Oversized bodies may be rejected while still being sent.
        let _ = server.write_all(&raw).await;
    });
    let result = exchange(
        client,
        request(Provider::Claude, "synthetic-token", "").unwrap(),
        now(),
    )
    .await;
    task.await.unwrap();
    result
}

#[test]
fn fixed_requests_and_secret_headers() {
    for provider in [Provider::Claude, Provider::Codex] {
        let req = request(provider, "synthetic-token", "synthetic-account").unwrap();
        assert_eq!(req.uri(), endpoint(provider).1);
        assert_eq!(req.headers()[header::HOST], endpoint(provider).0);
        assert!(req.headers()[header::AUTHORIZATION].is_sensitive());
        assert!(!format!("{req:?}").contains("synthetic-token"));
        assert_eq!(req.headers()[header::CONNECTION], "close");
        assert!(!req.headers().contains_key(header::ACCEPT_ENCODING));
        if provider == Provider::Codex {
            assert!(req.headers()["chatgpt-account-id"].is_sensitive());
        }
    }
    for token in ["", "bad\r\nheader", "한글", "token with spaces"] {
        assert_eq!(
            request(Provider::Claude, token, "").unwrap_err(),
            Failure::CredentialMalformed
        );
    }
    assert!(request(Provider::Codex, "ok", "x\r\n").is_err());
    assert!(request(Provider::Codex, &"x".repeat(16385), "").is_err());
}

#[tokio::test]
async fn statuses_body_limits_chunking_and_redirects() {
    assert_eq!(
        response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_vec())
            .await
            .unwrap(),
        b"{}"
    );
    assert_eq!(
        response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\n1\r\nb\r\n0\r\n\r\n"
                .to_vec()
        )
        .await
        .unwrap(),
        b"ab"
    );
    for (status, expected) in [
        (401, Failure::Unauthorized),
        (403, Failure::Unauthorized),
        (408, Failure::Server),
        (425, Failure::Server),
        (503, Failure::Server),
        (400, Failure::Contract),
        (302, Failure::Contract),
    ] {
        let raw = format!("HTTP/1.1 {status} Test\r\nLocation: https://untrusted.invalid/\r\nContent-Length: 0\r\n\r\n");
        assert_eq!(response(raw.into_bytes()).await, Err(expected));
    }
    assert_eq!(
        response(b"HTTP/1.1 429 Test\r\nRetry-After: 120\r\nContent-Length: 0\r\n\r\n".to_vec())
            .await,
        Err(Failure::RateLimited {
            retry_after: Some(ChronoDuration::seconds(120))
        })
    );
    for count in [MAX_BODY, MAX_BODY + 1] {
        let mut raw = format!("HTTP/1.1 200 OK\r\nContent-Length: {count}\r\n\r\n").into_bytes();
        raw.resize(raw.len() + count, b'x');
        assert_eq!(
            response(raw).await.map(|v| v.len()),
            if count == MAX_BODY {
                Ok(count)
            } else {
                Err(Failure::Contract)
            }
        );
    }
    assert_eq!(
        response(
            b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 0\r\n\r\n".to_vec()
        )
        .await,
        Err(Failure::Contract)
    );
    assert_eq!(
        response(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\nx".to_vec()).await,
        Err(Failure::Network)
    );
}

#[test]
fn retry_header_compatibility() {
    assert_eq!(retry_after(" 0 ", now()), Some(ChronoDuration::zero()));
    assert_eq!(
        retry_after("99999999", now()),
        Some(ChronoDuration::hours(24))
    );
    assert_eq!(
        retry_after("Thu, 24 Sep 2026 00:01:00 GMT", now()),
        Some(ChronoDuration::seconds(60))
    );
    assert_eq!(
        retry_after("Wed, 23 Sep 2026 00:00:00 GMT", now()),
        Some(ChronoDuration::zero())
    );
    for raw in ["", "-1", "1.5", "nonsense"] {
        assert_eq!(retry_after(raw, now()), None);
    }
}

#[tokio::test]
async fn cancellation_and_timeout_close_owned_stream() {
    for cancelled in [false, true] {
        let (client, mut server) = tokio::io::duplex(4096);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            let mut request = Vec::new();
            loop {
                request.push(server.read_u8().await.unwrap());
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            server
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nx")
                .await
                .unwrap();
            if cancelled {
                task_cancel.cancel();
            }
            let mut rest = Vec::new();
            server.read_to_end(&mut rest).await.unwrap();
            assert!(rest.is_empty());
        });
        let result = bounded(
            exchange(client, request(Provider::Codex, "fake", "").unwrap(), now()),
            &cancel,
            Instant::now() + Duration::from_millis(50),
        )
        .await;
        assert_eq!(result, Err(Failure::Network));
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }
}
