use super::*;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use hmux_gateway::{browser_upload, hub::Hub};
use hmux_protocol::{
    legacy,
    protobuf::{self as pb, types as p, Direction, Negotiated},
    transport, wire,
};
use sha2::{Digest, Sha256};
use tokio_tungstenite::{
    client_async_with_config,
    tungstenite::{client::IntoClientRequest, Message},
    WebSocketStream,
};
type Socket = WebSocketStream<TcpStream>;

async fn next<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
) -> Message {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Ping(_) | Message::Pong(_) => socket.flush().await.unwrap(),
                other => return other,
            }
        }
    })
    .await
    .unwrap()
}
async fn json_reply(socket: &mut Socket) -> Value {
    let Message::Text(text) = next(socket).await else {
        panic!("expected text reply")
    };
    serde_json::from_str(&text).unwrap()
}
async fn browser(server: &Server, cookie: &str) -> Socket {
    let mut request = "ws://hmux.example/api/upload"
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "https://hmux.example".parse().unwrap());
    request
        .headers_mut()
        .insert("cookie", cookie.parse().unwrap());
    let (socket, response) = client_async_with_config(
        request,
        TcpStream::connect(server.address).await.unwrap(),
        Some(transport::socket_config()),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), 101);
    assert!(!response.headers().contains_key("sec-websocket-extensions"));
    socket
}
struct Home {
    socket: Socket,
    protocol: Negotiated,
}
impl Home {
    async fn new(server: &Server, hub: &Hub, protocol: Negotiated) -> Self {
        let mut req = "ws://hmux.example/connect".into_client_request().unwrap();
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {CONNECTOR}").parse().unwrap(),
        );
        if protocol == Negotiated::ProtobufV2 {
            req.headers_mut()
                .insert("sec-websocket-protocol", pb::SUBPROTOCOL.parse().unwrap());
        }
        let (socket, _) = client_async_with_config(
            req,
            TcpStream::connect(server.address).await.unwrap(),
            Some(transport::socket_config()),
        )
        .await
        .unwrap();
        let mut home = Self { socket, protocol };
        home.send(p::envelope::Body::Hello(p::Hello {
            capabilities: vec!["web-upload-v1".into()],
        }))
        .await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while !hub.snapshot().upload {
                tokio::task::yield_now().await
            }
        })
        .await
        .unwrap();
        home
    }
    async fn send(&mut self, body: p::envelope::Body) {
        let envelope = p::Envelope {
            version: pb::VERSION,
            body: Some(body),
        };
        let frame = match self.protocol {
            Negotiated::ProtobufV2 => {
                Message::Binary(pb::encode(&envelope, Direction::ToGateway).unwrap())
            }
            Negotiated::JsonV1 => Message::Text(
                String::from_utf8(
                    legacy::to_json(envelope, Direction::ToGateway)
                        .unwrap()
                        .encode()
                        .unwrap(),
                )
                .unwrap()
                .into(),
            ),
        };
        self.socket.send(frame).await.unwrap();
    }
    async fn receive(&mut self) -> p::envelope::Body {
        let raw = next(&mut self.socket).await.into_data();
        match self.protocol {
            Negotiated::ProtobufV2 => pb::decode(raw, Direction::ToHome).unwrap(),
            Negotiated::JsonV1 => {
                legacy::from_json(wire::Message::decode(&raw).unwrap(), Direction::ToHome).unwrap()
            }
        }
        .body
        .unwrap()
    }
    async fn cancelled(&mut self, id: &str) {
        let p::envelope::Body::UploadCancel(cancel) = self.receive().await else {
            panic!("missing upload cancel")
        };
        assert_eq!(cancel.id, id);
    }
}
fn start(csrf: &str, sizes: &[usize]) -> Value {
    json!({"type":"start","csrf":csrf,"session":{"id":"$7","created_at":42},
        "files":sizes.iter().enumerate().map(|(i,n)|json!({"size":n,"extension":if i==0 {"txt"}else{"bin"}})).collect::<Vec<_>>()})
}
async fn begin(
    server: &Server,
    cookie: &str,
    csrf: &str,
    sizes: &[usize],
    home: &mut Home,
) -> (Socket, p::UploadHeader) {
    let mut browser = browser(server, cookie).await;
    browser
        .send(Message::Text(start(csrf, sizes).to_string().into()))
        .await
        .unwrap();
    let p::envelope::Body::UploadStart(start) = home.receive().await else {
        panic!("missing upload start")
    };
    let header = start.header.unwrap();
    assert_eq!(header.request_id, start.id);
    assert_eq!(header.request_id.len(), 32);
    home.send(p::envelope::Body::UploadReady(p::Reference {
        id: start.id,
    }))
    .await;
    assert_eq!(json_reply(&mut browser).await, json!({"type":"ready"}));
    (browser, header)
}
async fn send_chunk(browser: &mut Socket, home: &mut Home, id: &str, raw: Bytes, received: i64) {
    browser.send(Message::Binary(raw.clone())).await.unwrap();
    let p::envelope::Body::UploadData(data) = home.receive().await else {
        panic!("missing data")
    };
    assert_eq!(data.data, raw);
    assert_eq!(data.id, id);
    // No browser ACK is allowed until Home reports this precise byte count.
    assert!(
        tokio::time::timeout(Duration::from_millis(15), browser.next())
            .await
            .is_err()
    );
    home.send(p::envelope::Body::UploadAck(p::Ack {
        id: id.into(),
        received,
    }))
    .await;
    assert_eq!(
        json_reply(browser).await,
        json!({"type":"ack","received":received})
    );
}
fn stage(header: &p::UploadHeader, raw: &[u8]) -> Value {
    let expiry = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 10800;
    let stage_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let mut offset = 0;
    let files:Vec<_> = header.files.iter().map(|file| {
        let size = file.size as usize;
        let hash = format!("{:x}", Sha256::digest(&raw[offset..offset+size]));
        offset += size;
        json!({"index":file.index,"size":size,"sha256":hash,"path":format!("/tmp/hmux/staged-files-v1/{expiry}-{stage_id}/file-{:04}.{}",file.index+1,file.extension)})
    }).collect();
    let session = header.session.as_ref().unwrap();
    json!({"protocol_version":1,"request_id":header.request_id,"stage_id":stage_id,
        "session":{"id":session.id,"created_at":session.created_at},"expires_at_unix":expiry,"files":files})
}
async fn finish(
    browser: &mut Socket,
    home: &mut Home,
    header: &p::UploadHeader,
    stage: Value,
) -> Value {
    browser
        .send(Message::Text(r#"{"type":"finish"}"#.into()))
        .await
        .unwrap();
    let p::envelope::Body::UploadFinish(finish) = home.receive().await else {
        panic!("missing finish")
    };
    assert_eq!(finish.id, header.request_id);
    home.send(p::envelope::Body::UploadComplete(p::Response {
        id: finish.id,
        error: String::new(),
        result: hmux_protocol::actions::response_from_json(
            stage.to_string().as_bytes(),
            hmux_protocol::actions::ResponseContext::Upload,
        )
        .unwrap(),
    }))
    .await;
    json_reply(browser).await
}
#[tokio::test]
async fn upload_streams_across_files_and_validates_completion_with_both_home_protocols() {
    let fixture = Fixture::new();
    let (hub, _events) = Hub::new();
    let server = fixture.start_with_home(Some(hub.clone())).await;
    let cookie = server.login("primary").await;
    let csrf = server.session(&cookie).await.json()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let mut home = Home::new(&server, &hub, protocol).await;
        for corrupt in [false, true] {
            let raw: Vec<u8> = (0..browser_upload::CHUNK_BYTES + 7)
                .map(|i| (i % 256) as u8)
                .collect();
            let (mut browser, header) =
                begin(&server, &cookie, &csrf, &[3, raw.len() - 3], &mut home).await;
            let chunk = Bytes::copy_from_slice(&raw[..browser_upload::CHUNK_BYTES]);
            send_chunk(
                &mut browser,
                &mut home,
                &header.request_id,
                chunk,
                browser_upload::CHUNK_BYTES as i64,
            )
            .await;
            send_chunk(
                &mut browser,
                &mut home,
                &header.request_id,
                Bytes::copy_from_slice(&raw[browser_upload::CHUNK_BYTES..]),
                raw.len() as i64,
            )
            .await;
            let mut output = stage(&header, &raw);
            if corrupt {
                output["session"]["created_at"] = json!(43);
            }
            let result = finish(&mut browser, &mut home, &header, output.clone()).await;
            assert_eq!(
                result,
                if corrupt {
                    json!({"type":"error","error":"Home upload unavailable"})
                } else {
                    json!({"type":"complete","stage":output})
                }
            );
            assert!(matches!(next(&mut browser).await, Message::Close(_)));
        }
        home.socket.close(None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while hub.snapshot().connected {
                tokio::task::yield_now().await
            }
        })
        .await
        .unwrap();
    }
    server.stop().await;
}
#[tokio::test]
async fn upload_auth_origin_csrf_and_malformed_start_never_reach_home() {
    let fixture = Fixture::new();
    let (hub, _events) = Hub::new();
    let server = fixture.start_with_home(Some(hub.clone())).await;
    let cookie = server.login("primary").await;
    let csrf = server.session(&cookie).await.json()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        server
            .request(
                "GET",
                "/api/upload",
                &[("Origin", "https://hmux.example")],
                None
            )
            .await
            .code,
        401
    );
    assert_eq!(
        server
            .request("GET", "/api/upload", &[("Cookie", &cookie)], None)
            .await
            .code,
        403
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/api/upload",
                &[("Cookie", &cookie), ("Origin", "https://elsewhere.example")],
                None
            )
            .await
            .code,
        403
    );
    let mut home = Home::new(&server, &hub, Negotiated::ProtobufV2).await;
    let good = start(&csrf, &[1]);
    let mut unknown = good.clone();
    unknown["name"] = json!("private");
    let mut size = good.clone();
    size["files"][0]["size"] = json!(0);
    for raw in [start("wrong", &[1]), unknown, size, json!(["start"])] {
        let mut browser = browser(&server, &cookie).await;
        browser
            .send(Message::Text(raw.to_string().into()))
            .await
            .unwrap();
        assert_eq!(
            json_reply(&mut browser).await,
            json!({"type":"error","error":"Invalid upload request"})
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(30), home.socket.next())
            .await
            .is_err()
    );
    server.stop().await;
}
#[tokio::test]
async fn upload_limits_are_per_login_and_global_and_logout_cancels_only_owner() {
    let fixture = Fixture::new();
    let (hub, _events) = Hub::new();
    let server = fixture.start_with_home(Some(hub.clone())).await;
    let mut home = Home::new(&server, &hub, Negotiated::ProtobufV2).await;
    let a = server.login("primary").await;
    let b = server.login("guest").await;
    let c = server.login("primary").await;
    let ac = server.session(&a).await.json()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    let bc = server.session(&b).await.json()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    let cc = server.session(&c).await.json()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    let (mut one, first) = begin(&server, &a, &ac, &[1], &mut home).await;
    let mut duplicate = browser(&server, &a).await;
    duplicate
        .send(Message::Text(start(&ac, &[1]).to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        json_reply(&mut duplicate).await["error"],
        "Upload limit reached"
    );
    let (mut two, second) = begin(&server, &b, &bc, &[1], &mut home).await;
    let mut third = browser(&server, &c).await;
    third
        .send(Message::Text(start(&cc, &[1]).to_string().into()))
        .await
        .unwrap();
    assert_eq!(
        json_reply(&mut third).await["error"],
        "Upload limit reached"
    );
    assert_eq!(
        server
            .request(
                "POST",
                "/api/logout",
                &[
                    ("Origin", "https://hmux.example"),
                    ("Cookie", &a),
                    ("X-CSRF-Token", &ac)
                ],
                None
            )
            .await
            .code,
        200
    );
    home.cancelled(&first.request_id).await;
    assert!(matches!(next(&mut one).await, Message::Close(_)));
    send_chunk(
        &mut two,
        &mut home,
        &second.request_id,
        Bytes::from_static(b"x"),
        1,
    )
    .await;
    let result = finish(&mut two, &mut home, &second, stage(&second, b"x")).await;
    assert_eq!(result["type"], "complete");
    assert!(matches!(next(&mut two).await, Message::Close(_)));
    let (mut replacement, header) = begin(&server, &c, &cc, &[1], &mut home).await;
    replacement.close(None).await.unwrap();
    home.cancelled(&header.request_id).await;
    server.stop().await;
}
#[tokio::test]
async fn upload_bad_data_ack_abandoned_start_and_home_replacement_release_leases() {
    let fixture = Fixture::new();
    let (hub, _events) = Hub::new();
    let server = fixture.start_with_home(Some(hub.clone())).await;
    let cookie = server.login("primary").await;
    let csrf = server.session(&cookie).await.json()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut home = Home::new(&server, &hub, Negotiated::ProtobufV2).await;
    for frame in [
        Message::Text(r#"{"type":"finish"}"#.into()),
        Message::Binary(Bytes::new()),
        Message::Binary(Bytes::from_static(b"extra")),
    ] {
        let (mut browser, header) = begin(&server, &cookie, &csrf, &[1], &mut home).await;
        browser.send(frame).await.unwrap();
        assert_eq!(
            json_reply(&mut browser).await["error"],
            "Upload data rejected"
        );
        home.cancelled(&header.request_id).await;
        assert!(matches!(next(&mut browser).await, Message::Close(_)));
    }
    let (mut browser, header) = begin(&server, &cookie, &csrf, &[2], &mut home).await;
    browser
        .send(Message::Binary(Bytes::from_static(b"x")))
        .await
        .unwrap();
    assert!(matches!(
        home.receive().await,
        p::envelope::Body::UploadData(_)
    ));
    home.send(p::envelope::Body::UploadAck(p::Ack {
        id: header.request_id.clone(),
        received: 2,
    }))
    .await;
    assert_eq!(
        json_reply(&mut browser).await["error"],
        "Home upload unavailable"
    );
    home.cancelled(&header.request_id).await;
    assert!(matches!(next(&mut browser).await, Message::Close(_)));
    let mut abandoned = browser_upload_socket(&server, &cookie).await;
    abandoned
        .send(Message::Text(start(&csrf, &[1]).to_string().into()))
        .await
        .unwrap();
    let p::envelope::Body::UploadStart(pending) = home.receive().await else {
        panic!("missing pending start")
    };
    drop(abandoned);
    home.cancelled(&pending.id).await;
    let (mut live, _) = begin(&server, &cookie, &csrf, &[1], &mut home).await;
    home.socket.close(None).await.unwrap();
    assert_eq!(
        json_reply(&mut live).await["error"],
        "Home upload unavailable"
    );
    assert!(matches!(next(&mut live).await, Message::Close(_)));
    tokio::time::timeout(Duration::from_secs(2), async {
        while hub.snapshot().connected {
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    let mut home = Home::new(&server, &hub, Negotiated::ProtobufV2).await;
    let (mut fresh, header) = begin(&server, &cookie, &csrf, &[1], &mut home).await;
    fresh.close(None).await.unwrap();
    home.cancelled(&header.request_id).await;
    server.stop().await;
}
// Avoid shadowing the browser helper in tests with a local socket of that name.
async fn browser_upload_socket(server: &Server, cookie: &str) -> Socket {
    browser(server, cookie).await
}

#[tokio::test]
#[ignore = "the optional external baseline suite (tests/RUST.md) supplies the actual Go Home upload test executable"]
async fn upload_actual_go_home_commits_binary_files_and_three_hour_expiry() {
    use std::process::Stdio;
    let fixture = Fixture::new();
    let (hub, _events) = Hub::new();
    let server = fixture.start_with_home(Some(hub.clone())).await;
    let cookie = server.login("primary").await;
    let csrf = server.session(&cookie).await.json()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    let helper = std::env::var_os("HMUX_GO_UPLOAD_HELPER")
        .expect("tests/RUST.md describes the external legacy helper");
    let log = fs::File::create(fixture.root.join("go-upload.log")).unwrap();
    let mut child = tokio::process::Command::new(helper)
        .arg("-test.run=^TestRustGatewayActualGoUploadHome$")
        .arg("-test.timeout=25s")
        .env("HMUX_RUST_UPLOAD_ADDRESS", server.address.to_string())
        .env("HMUX_RUST_UPLOAD_DIRECTORY", &fixture.root)
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !hub.snapshot().upload {
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    let mut browser = browser(&server, &cookie).await;
    browser
        .send(Message::Text(start(&csrf, &[3, 4]).to_string().into()))
        .await
        .unwrap();
    assert_eq!(json_reply(&mut browser).await["type"], "ready");
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for (chunk, n) in [(vec![0, 1, 2, 3], 4), (vec![4, 255, 6], 7)] {
        browser.send(Message::Binary(chunk.into())).await.unwrap();
        assert_eq!(
            json_reply(&mut browser).await,
            json!({"type":"ack","received":n})
        );
    }
    browser
        .send(Message::Text(r#"{"type":"finish"}"#.into()))
        .await
        .unwrap();
    let result = json_reply(&mut browser).await;
    assert_eq!(result["type"], "complete");
    let expiry = result["stage"]["expires_at_unix"].as_u64().unwrap();
    assert!((started + 10800..=started + 10810).contains(&expiry));
    for (i, want) in [vec![0, 1, 2], vec![3, 4, 255, 6]].into_iter().enumerate() {
        let row = &result["stage"]["files"][i];
        let path = PathBuf::from(row["path"].as_str().unwrap());
        assert!(path.starts_with(fixture.root.join("hmux/staged-files-v1")));
        assert_eq!(fs::read(&path).unwrap(), want);
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(row["sha256"], format!("{:x}", Sha256::digest(&want)));
    }
    server.stop().await;
    let status = tokio::time::timeout(Duration::from_secs(4), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        status.success(),
        "{}",
        fs::read_to_string(fixture.root.join("go-upload.log")).unwrap()
    );
}

#[tokio::test]
async fn upload_start_timeout_expiry_and_shutdown_join_blocked_browser_writes() {
    use hmux_gateway::auth_store::LoginRequest;
    use tokio_tungstenite::tungstenite::protocol::Role;
    let fixture = Fixture::new();
    let auth = Arc::new(open_auth(&fixture.credentials).await);
    let token = auth
        .login(LoginRequest {
            username: "primary".into(),
            password: PASSWORD.into(),
            code: String::new(),
            source: "synthetic".into(),
            ip: "127.0.0.1".into(),
            browser: "Test".into(),
            now: SystemTime::now().into(),
        })
        .await
        .token
        .unwrap();
    let (hub, _events) = Hub::new();
    let mut home = super::browser_terminal::Home::new(&hub).await;
    home.send(p::envelope::Body::Hello(p::Hello {
        capabilities: vec!["web-upload-v1".into()],
    }))
    .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while !hub.snapshot().upload {
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    let limiter = browser_upload::Limiter::default();
    for mode in ["first-timeout", "expiry", "pending-home", "blocked-ready"] {
        let mut access = auth
            .access(&token, false, SystemTime::now().into())
            .await
            .unwrap()
            .unwrap();
        let csrf = access.csrf.clone();
        if mode == "expiry" {
            access.expires_at = chrono::DateTime::<chrono::Utc>::from(SystemTime::now())
                + chrono::Duration::milliseconds(30);
        }
        let (left, right) = tokio::io::duplex(1);
        let mut browser =
            WebSocketStream::from_raw_socket(right, Role::Client, Some(transport::socket_config()))
                .await;
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(browser_upload::serve(
            left,
            hub.clone(),
            auth.clone(),
            token.clone(),
            access,
            limiter.clone(),
            shutdown.clone(),
        ));
        if mode == "first-timeout" {
            tokio::time::pause();
            // The client never reads the error: the two-second write budget
            // still releases the raw socket and its early admission slot.
            // Drive both sequential deadlines with automatic clock advancement;
            // a manual jump can defer starting the second timer until after it.
            tokio::time::timeout(Duration::from_secs(14), task)
                .await
                .unwrap()
                .unwrap();
            tokio::time::resume();
            continue;
        }
        if mode == "expiry" {
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            continue;
        }
        browser
            .send(Message::Text(start(&csrf, &[1]).to_string().into()))
            .await
            .unwrap();
        let p::envelope::Body::UploadStart(started) = home.receive().await else {
            panic!("missing start")
        };
        if mode == "blocked-ready" {
            home.send(p::envelope::Body::UploadReady(p::Reference {
                id: started.id.clone(),
            }))
            .await;
            // A one-byte duplex cannot fit the ready response, and the browser
            // intentionally never polls it. Shutdown must interrupt this wait.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        shutdown.cancel();
        let p::envelope::Body::UploadCancel(cancel) = home.receive().await else {
            panic!("missing cancel")
        };
        assert_eq!(cancel.id, started.id);
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
    }
    home.stop().await;
}
