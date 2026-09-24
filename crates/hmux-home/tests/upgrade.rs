use hmux_home::upgrade::{upgrade, Error};
use hmux_protocol::{
    protobuf::{self, Negotiated},
    transport,
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    time::{timeout, Instant},
};
use tokio_util::sync::CancellationToken;

async fn read_request(stream: &mut DuplexStream) -> String {
    let mut raw = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let count = stream.read(&mut buf).await.unwrap();
        if count == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..count]);
        assert!(raw.len() <= 16 * 1024, "request exceeded test cap");
        if raw.windows(4).any(|part| part == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(raw).unwrap()
}
fn request_key(raw: &str) -> String {
    raw.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("sec-websocket-key")
                .then(|| value.trim().to_owned())
        })
        .unwrap()
}
fn response(request: &str, selected: Option<&str>, extra: &str) -> Vec<u8> {
    let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(
        request_key(request).as_bytes(),
    );
    let mut text = format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n");
    if let Some(protocol) = selected {
        text.push_str(&format!("Sec-WebSocket-Protocol: {protocol}\r\n"));
    }
    text.push_str(extra);
    text.push_str("\r\n");
    text.into_bytes()
}
async fn server(
    raw_reply: impl FnOnce(&str) -> Vec<u8> + Send + 'static,
) -> (DuplexStream, tokio::task::JoinHandle<String>) {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move {
        let request = read_request(&mut server).await;
        let reply = raw_reply(&request);
        server.write_all(&reply).await.unwrap();
        let mut remainder = [0u8; 128];
        let _ = timeout(Duration::from_secs(2), server.read(&mut remainder)).await;
        request
    });
    (client, task)
}
async fn close(connection: transport::Connection) {
    let transport::Connection {
        sender,
        reader,
        task,
    } = connection;
    sender.close();
    drop(reader);
    timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn selected_v2_and_valid_unselected_v1_upgrade_once() {
    for selected in [Some(protobuf::SUBPROTOCOL), None] {
        let (stream, server) = server(move |request| response(request, selected, "")).await;
        let connection = upgrade(
            stream,
            "gateway.example:443",
            "synthetic-token_9",
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            connection.protocol(),
            if selected.is_some() {
                Negotiated::ProtobufV2
            } else {
                Negotiated::JsonV1
            }
        );
        close(connection).await;
        let request = server.await.unwrap();
        assert!(request.starts_with("GET /connect HTTP/1.1\r\n"));
        let lower = request.to_ascii_lowercase();
        for header in [
            "host: gateway.example:443",
            "authorization: bearer synthetic-token_9",
            "connection: upgrade",
            "upgrade: websocket",
            "sec-websocket-version: 13",
            "sec-websocket-protocol: hmux-home.pb.v2.controls1",
        ] {
            assert!(lower.contains(header), "missing {header}");
        }
        assert_eq!(lower.matches("sec-websocket-protocol:").count(), 1);
    }
}

#[tokio::test]
async fn buffered_websocket_bytes_survive_hyper_upgrade() {
    let raw = br#"{"type":"request","id":"one","operation":"profiles"}"#;
    let (stream, server) = server(move |request| {
        let mut reply = response(request, None, "");
        reply.extend_from_slice(&[0x81, raw.len() as u8]);
        reply.extend_from_slice(raw);
        reply
    })
    .await;
    let mut connection = upgrade(
        stream,
        "gateway.example",
        "synthetic",
        &CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(connection.protocol(), Negotiated::JsonV1);
    let message = timeout(Duration::from_secs(2), connection.reader.receive())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(message, transport::Incoming::Json(m) if m.kind == "request" && m.id == "one" && m.operation == "profiles")
    );
    close(connection).await;
    server.await.unwrap();
}

#[tokio::test]
async fn non101_auth_redirect_and_invalid_accept_never_retry() {
    for status in [401u16, 302, 200, 599, 699] {
        let expected = (100..=599).contains(&status).then_some(status);
        let (stream, server) = server(move |_| {
            format!("HTTP/1.1 {status} Private-response\r\nContent-Length: 0\r\n\r\n").into_bytes()
        })
        .await;
        assert!(matches!(
            upgrade(
                stream,
                "gateway.example",
                "synthetic",
                &CancellationToken::new(),
                None
            )
            .await,
            Err(Error::Rejected { http_status }) if http_status == expected
        ));
        let request = server.await.unwrap();
        assert_eq!(request.matches("GET /connect").count(), 1);
    }
    let (stream, server) = server(|_| b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: wrong\r\n\r\n".to_vec()).await;
    assert!(matches!(
        upgrade(
            stream,
            "gateway.example",
            "synthetic",
            &CancellationToken::new(),
            None
        )
        .await,
        Err(Error::InvalidResponse)
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn duplicate_and_unknown_upgrade_headers_are_rejected() {
    let extras = [
        "Sec-WebSocket-Protocol: hmux-home.pb.v2.controls1\r\nSec-WebSocket-Protocol: hmux-home.pb.v2.controls1\r\n",
        "Sec-WebSocket-Accept: duplicate\r\n",
        "Sec-WebSocket-Protocol: other.v2\r\n",
        "Sec-WebSocket-Protocol: \r\n",
        "Sec-WebSocket-Extensions: permessage-deflate\r\n",
        "Upgrade: websocket\r\n",
        "Content-Length: 0\r\n",
    ];
    for extra in extras {
        let (stream, server) = server(move |request| response(request, None, extra)).await;
        assert!(
            matches!(
                upgrade(
                    stream,
                    "gateway.example",
                    "synthetic",
                    &CancellationToken::new(),
                    None
                )
                .await,
                Err(Error::InvalidResponse)
            ),
            "accepted {extra}"
        );
        server.await.unwrap();
    }
    let (stream, server) = server(|request| response(request, Some("other.v2"), "")).await;
    assert!(matches!(
        upgrade(
            stream,
            "gateway.example",
            "synthetic",
            &CancellationToken::new(),
            None
        )
        .await,
        Err(Error::InvalidResponse)
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn header_budget_and_invalid_inputs_fail_closed() {
    let (stream, server) =
        server(|request| response(request, None, &format!("X-Fill: {}\r\n", "x".repeat(9000))))
            .await;
    assert!(matches!(
        upgrade(
            stream,
            "gateway.example",
            "synthetic",
            &CancellationToken::new(),
            None
        )
        .await,
        Err(Error::Transport) | Err(Error::InvalidResponse)
    ));
    server.await.unwrap();
    for (authority, token) in [
        ("gateway.example@evil", "synthetic"),
        ("gateway.example:abc", "synthetic"),
        ("gateway.example:99999", "synthetic"),
        ("gateway.example:0", "synthetic"),
        ("gateway.example:", "synthetic"),
        ("[::1]:abc", "synthetic"),
        ("[::1]:99999", "synthetic"),
        ("[::1]garbage", "synthetic"),
        ("::1", "synthetic"),
        ("gateway.example", "bad\r\ntoken"),
        ("gateway.example", ""),
        ("gateway.example", "bad=token"),
    ] {
        let (stream, _peer) = tokio::io::duplex(64);
        assert!(matches!(
            upgrade(stream, authority, token, &CancellationToken::new(), None).await,
            Err(Error::InvalidInput)
        ));
    }
}

#[tokio::test]
async fn timeout_cancel_and_abort_drop_the_handshake_socket() {
    // No response: the absolute parent deadline stops the handshake.
    let (stream, mut peer) = tokio::io::duplex(4096);
    let task = tokio::spawn(async move {
        upgrade(
            stream,
            "gateway.example",
            "synthetic",
            &CancellationToken::new(),
            Some(Instant::now() + Duration::from_millis(80)),
        )
        .await
    });
    read_request(&mut peer).await;
    assert!(matches!(task.await.unwrap(), Err(Error::Timeout)));
    let mut byte = [0u8; 1];
    assert_eq!(
        timeout(Duration::from_secs(1), peer.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );

    let (stream, mut peer) = tokio::io::duplex(4096);
    let token = CancellationToken::new();
    let control = token.clone();
    let task = tokio::spawn(async move {
        upgrade(stream, "gateway.example", "synthetic", &control, None).await
    });
    read_request(&mut peer).await;
    token.cancel();
    assert!(matches!(task.await.unwrap(), Err(Error::Cancelled)));
    assert_eq!(
        timeout(Duration::from_secs(1), peer.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );

    let (stream, mut peer) = tokio::io::duplex(4096);
    let task = tokio::spawn(async move {
        upgrade(
            stream,
            "gateway.example",
            "synthetic",
            &CancellationToken::new(),
            None,
        )
        .await
    });
    read_request(&mut peer).await;
    task.abort();
    assert!(task.await.is_err());
    assert_eq!(
        timeout(Duration::from_secs(1), peer.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn wrong_connection_tokens_are_rejected() {
    for field in [
        "keep-alive",
        "Upgrade, bad token",
        "Upgrade, Upgrade",
        "Upgrade,",
    ] {
        let (stream, server) = server(move |request| {
            let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(
                request_key(request).as_bytes(),
            );
            format!("HTTP/1.1 101 Switching Protocols\r\nConnection: {field}\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").into_bytes()
        }).await;
        assert!(
            matches!(
                upgrade(
                    stream,
                    "gateway.example",
                    "synthetic",
                    &CancellationToken::new(),
                    None
                )
                .await,
                Err(Error::InvalidResponse)
            ),
            "accepted {field}"
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn bracketed_ipv6_authority_is_sent_unchanged() {
    let (stream, server) = server(|request| response(request, None, "")).await;
    let connection = upgrade(
        stream,
        "[::1]:443",
        "synthetic",
        &CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    close(connection).await;
    let request = server.await.unwrap();
    assert!(request.to_ascii_lowercase().contains("host: [::1]:443\r\n"));
}
