use super::*;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use hmux_gateway::hub::{HomeConnection, Hub};
use hmux_protocol::{
    flow,
    protobuf::{self as pb, types as p, Direction, Negotiated},
    transport,
};
use tokio::io::DuplexStream;
use tokio_tungstenite::{
    client_async_with_config,
    tungstenite::{client::IntoClientRequest, protocol::Role, Message},
    WebSocketStream,
};

pub(super) struct Home {
    owner: HomeConnection,
    socket: WebSocketStream<DuplexStream>,
}
impl Home {
    pub(super) async fn new(hub: &Hub) -> Self {
        let (left, right) = tokio::io::duplex(4096);
        let server =
            WebSocketStream::from_raw_socket(left, Role::Server, Some(transport::socket_config()))
                .await;
        let socket =
            WebSocketStream::from_raw_socket(right, Role::Client, Some(transport::socket_config()))
                .await;
        let owner = hub
            .attach(transport::start(server, Negotiated::ProtobufV2, Direction::ToGateway).unwrap())
            .unwrap();
        let mut home = Self { owner, socket };
        home.send(p::envelope::Body::Hello(p::Hello {
            capabilities: vec![flow::CAPABILITY.into()],
        }))
        .await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while !hub.snapshot().output_flow {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        home
    }
    pub(super) async fn send(&mut self, body: p::envelope::Body) {
        let raw = pb::encode(
            &p::Envelope {
                version: pb::VERSION,
                body: Some(body),
            },
            Direction::ToGateway,
        )
        .unwrap();
        self.socket.send(Message::Binary(raw)).await.unwrap();
    }
    pub(super) async fn receive(&mut self) -> p::envelope::Body {
        let raw = next(&mut self.socket).await.into_data();
        pb::decode(raw, Direction::ToHome).unwrap().body.unwrap()
    }
    pub(super) async fn stop(mut self) {
        let _ = self.socket.close(None).await;
        self.owner.wait().await;
    }
}
async fn next<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
) -> Message {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Ping(_) | Message::Pong(_) => {
                    socket.flush().await.unwrap();
                }
                Message::Text(text) if text.as_str() == r#"{"type":"heartbeat"}"# => {}
                other => return other,
            }
        }
    })
    .await
    .unwrap()
}
async fn connect(server: &Server, cookie: &str) -> WebSocketStream<TcpStream> {
    let mut request = "ws://hmux.example/api/terminal"
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
        Some(hmux_gateway::browser_terminal::socket_config()),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), 101);
    socket
}
async fn open(
    server: &Server,
    cookie: &str,
    home: &mut Home,
    flow: bool,
) -> (WebSocketStream<TcpStream>, String) {
    let mut browser = connect(server, cookie).await;
    let raw = json!({"type":"open","id":"forged","future":{"nested":[1,true]},"session":{"id":"$1","created_at":42,"future":true},"cols":80,"rows":24,
        "capabilities":if flow {vec![flow::CAPABILITY]} else {Vec::<&str>::new()}});
    browser
        .send(Message::Text(raw.to_string().into()))
        .await
        .unwrap();
    let p::envelope::Body::TerminalOpen(open) = home.receive().await else {
        panic!("expected open")
    };
    assert_ne!(open.id, "forged");
    assert_eq!(open.session.as_ref().unwrap().id, "$1");
    assert_eq!(open.session.as_ref().unwrap().created_at, 42);
    home.send(p::envelope::Body::Response(p::Response {
        id: open.id.clone(),
        result: Some(p::response::Result::Ok(p::Empty {})),
        ..Default::default()
    }))
    .await;
    let ready: Value = serde_json::from_slice(&next(&mut browser).await.into_data()).unwrap();
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["heartbeat"], true);
    assert_eq!(ready["output_flow"].as_bool().unwrap_or(false), flow);
    (browser, open.id)
}
async fn closed(browser: &mut WebSocketStream<TcpStream>, code: u16) {
    let Message::Close(Some(close)) = next(browser).await else {
        panic!("missing close frame")
    };
    assert_eq!(u16::from(close.code), code);
}
async fn view_closed(home: &mut Home, id: &str) {
    let p::envelope::Body::Close(close) = home.receive().await else {
        panic!("missing view cleanup")
    };
    assert_eq!(close.id, id);
}

#[tokio::test]
async fn abandoned_open_cleans_up_promptly_and_home_loss_ends_its_generation() {
    let fixture = Fixture::new();
    let (hub, _events) = Hub::new();
    let mut home = Home::new(&hub).await;
    let server = fixture.start_with_home(Some(hub.clone())).await;
    let cookie = server.login("primary").await;
    let mut abandoned = connect(&server, &cookie).await;
    abandoned
        .send(Message::Binary(
            br#"{"type":"open","session":{"id":"$1","created_at":42},"cols":80,"rows":24}"#
                .as_slice()
                .into(),
        ))
        .await
        .unwrap();
    let p::envelope::Body::TerminalOpen(pending) = home.receive().await else {
        panic!("missing pending open")
    };
    // Home deliberately never replies. Socket death must drop the pending
    // lease now, instead of waiting for the twenty-second open deadline.
    drop(abandoned);
    view_closed(&mut home, &pending.id).await;
    let (mut browser, _id) = open(&server, &cookie, &mut home, true).await;
    home.stop().await;
    closed(&mut browser, 4001).await;
    assert!(!hub.snapshot().connected);
    // A new connection generation can open views without any old lease.
    let mut replacement = Home::new(&hub).await;
    let (mut browser, id) = open(&server, &cookie, &mut replacement, true).await;
    browser.close(None).await.unwrap();
    view_closed(&mut replacement, &id).await;
    server.stop().await;
    replacement.stop().await;
}

#[tokio::test]
async fn expiry_revocation_and_shutdown_join_pending_open_or_blocked_output() {
    use hmux_gateway::{auth_store::LoginRequest, browser_terminal};
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
    let mut home = Home::new(&hub).await;
    for ending in ["expiry", "shutdown", "revoke"] {
        let expiring = ending == "expiry";
        let (left, right) = tokio::io::duplex(128);
        let socket = WebSocketStream::from_raw_socket(
            left,
            Role::Server,
            Some(browser_terminal::socket_config()),
        )
        .await;
        let mut browser = WebSocketStream::from_raw_socket(
            right,
            Role::Client,
            Some(browser_terminal::socket_config()),
        )
        .await;
        let connection = transport::start_frames(socket, browser_terminal::FRAME_BYTES).unwrap();
        let mut access = auth
            .access(&token, false, SystemTime::now().into())
            .await
            .unwrap()
            .unwrap();
        if expiring {
            // The owner must use the exact expiry, including while Home's open
            // reply is pending, instead of the five-second periodic access check.
            access.expires_at = chrono::DateTime::<chrono::Utc>::from(SystemTime::now())
                + chrono::Duration::milliseconds(500);
        }
        let shutdown = CancellationToken::new();
        let owner = tokio::spawn(browser_terminal::serve(
            connection,
            hub.clone(),
            auth.clone(),
            token.clone(),
            access,
            shutdown.clone(),
        ));
        browser
            .send(Message::Text(
                r#"{"type":"open","session":{"id":"$1","created_at":42},"cols":80,"rows":24}"#
                    .into(),
            ))
            .await
            .unwrap();
        let p::envelope::Body::TerminalOpen(open) = home.receive().await else {
            panic!("missing open")
        };
        if expiring {
            let Message::Close(Some(close)) = next(&mut browser).await else {
                panic!("missing expiry close")
            };
            assert_eq!(u16::from(close.code), 1008);
        } else {
            home.send(p::envelope::Body::Response(p::Response {
                id: open.id.clone(),
                result: Some(p::response::Result::Ok(p::Empty {})),
                ..Default::default()
            }))
            .await;
            assert!(matches!(next(&mut browser).await, Message::Text(_)));
            home.send(p::envelope::Body::TerminalOutput(p::Data {
                id: open.id.clone(),
                data: vec![b'x'; 16384].into(),
            }))
            .await;
            // Observe the frame's first byte but never drain the tiny duplex.
            // This proves shutdown interrupts an actual blocked output write.
            assert_eq!(browser.get_mut().read_u8().await.unwrap(), 0x82);
            if ending == "shutdown" {
                shutdown.cancel();
            } else {
                auth.logout(&token).await.unwrap();
            }
        }
        tokio::time::timeout(Duration::from_secs(4), owner)
            .await
            .expect("browser cleanup exceeded HTTP shutdown budget")
            .unwrap();
        view_closed(&mut home, &open.id).await;
        assert!(hub.snapshot().connected);
    }
    home.stop().await;
}

#[tokio::test]
async fn binary_io_ack_resize_refresh_and_legacy_pacing_preserve_browser_contract() {
    let fixture = Fixture::new();
    let (hub, _events) = Hub::new();
    let mut home = Home::new(&hub).await;
    let server = fixture.start_with_home(Some(hub.clone())).await;
    let cookie = server.login("primary").await;
    for enabled in [true, false] {
        let (mut browser, id) = open(&server, &cookie, &mut home, enabled).await;
        browser
            .send(Message::Binary(vec![0, 255, 9].into()))
            .await
            .unwrap();
        let p::envelope::Body::TerminalInput(input) = home.receive().await else {
            panic!("missing input")
        };
        assert_eq!(input.id, id);
        assert_eq!(input.data.as_ref(), [0, 255, 9]);
        home.send(p::envelope::Body::TerminalOutput(p::Data {
            id: id.clone(),
            data: Bytes::from_static(b"abcd"),
        }))
        .await;
        assert!(matches!(next(&mut browser).await,Message::Binary(data) if data.as_ref()==b"abcd"));
        if enabled {
            browser
                .send(Message::Text(
                    r#"{"type":"output-ack","id":"forged","received":4,"future":true}"#.into(),
                ))
                .await
                .unwrap();
        }
        let p::envelope::Body::OutputAck(ack) = home.receive().await else {
            panic!("missing credit")
        };
        assert_eq!(ack.id, id);
        assert_eq!(ack.received, 4);
        browser
            .send(Message::Text(
                r#"{"type":"resize","cols":100,"rows":30,"future":[1]}"#.into(),
            ))
            .await
            .unwrap();
        let p::envelope::Body::Resize(resize) = home.receive().await else {
            panic!("missing resize")
        };
        assert_eq!(resize.id, id);
        assert_eq!((resize.cols, resize.rows), (100, 30));
        browser
            .send(Message::Text(r#"{"type":"refresh"}"#.into()))
            .await
            .unwrap();
        let p::envelope::Body::Refresh(refresh) = home.receive().await else {
            panic!("missing refresh")
        };
        assert_eq!(refresh.id, id);
        home.send(p::envelope::Body::RefreshResult(p::Response {
            id: id.clone(),
            ..Default::default()
        }))
        .await;
        assert_eq!(
            serde_json::from_slice::<Value>(&next(&mut browser).await.into_data()).unwrap(),
            json!({"type":"refresh-result","ok":true})
        );
        home.send(p::envelope::Body::TerminalExit(p::Response {
            id: id.clone(),
            ..Default::default()
        }))
        .await;
        closed(&mut browser, 4003).await;
        view_closed(&mut home, &id).await;
        assert!(hub.snapshot().connected);
    }
    server.stop().await;
    home.stop().await;
}

#[tokio::test]
async fn invalid_ack_and_oversized_input_close_only_the_offending_view() {
    let fixture = Fixture::new();
    let (hub, _events) = Hub::new();
    let mut home = Home::new(&hub).await;
    let server = fixture.start_with_home(Some(hub.clone())).await;
    let cookie = server.login("primary").await;
    for bad_ack in [true, false] {
        let (mut browser, id) = open(&server, &cookie, &mut home, true).await;
        if bad_ack {
            home.send(p::envelope::Body::TerminalOutput(p::Data {
                id: id.clone(),
                data: Bytes::from_static(b"abc"),
            }))
            .await;
            let _ = next(&mut browser).await;
            browser
                .send(Message::Text(
                    r#"{"type":"output-ack","received":2}"#.into(),
                ))
                .await
                .unwrap();
        } else {
            browser
                .send(Message::Binary(vec![b'x'; 32769].into()))
                .await
                .unwrap();
        }
        closed(&mut browser, 1002).await;
        view_closed(&mut home, &id).await;
        assert!(hub.snapshot().connected);
    }
    let mut invalid = connect(&server, &cookie).await;
    invalid
        .send(Message::Text(
            r#"{"type":"open","session":{"id":"$1","created_at":0},"cols":80,"rows":24}"#.into(),
        ))
        .await
        .unwrap();
    closed(&mut invalid, 1008).await;
    server.stop().await;
    home.stop().await;
}

#[tokio::test]
async fn account_logout_revokes_only_its_browser_and_gateway_shutdown_joins_views() {
    let fixture = Fixture::new();
    let (hub, _events) = Hub::new();
    let mut home = Home::new(&hub).await;
    let server = fixture.start_with_home(Some(hub.clone())).await;
    let primary = server.login("primary").await;
    let guest = server.login("guest").await;
    let (mut primary_browser, primary_id) = open(&server, &primary, &mut home, true).await;
    let (mut guest_browser, guest_id) = open(&server, &guest, &mut home, true).await;
    let csrf = server.session(&primary).await.json()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        server
            .request(
                "POST",
                "/api/logout",
                &[
                    ("Origin", "https://hmux.example"),
                    ("Cookie", &primary),
                    ("X-CSRF-Token", &csrf)
                ],
                None
            )
            .await
            .code,
        200
    );
    closed(&mut primary_browser, 1008).await;
    view_closed(&mut home, &primary_id).await;
    guest_browser
        .send(Message::Binary(Bytes::from_static(b"alive")))
        .await
        .unwrap();
    let p::envelope::Body::TerminalInput(input) = home.receive().await else {
        panic!("guest revoked with primary")
    };
    assert_eq!(input.id, guest_id);
    server.stop().await;
    closed(&mut guest_browser, 1001).await;
    view_closed(&mut home, &guest_id).await;
    assert!(hub.snapshot().connected);
    home.stop().await;
}
