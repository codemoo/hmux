//! Actual HTTP requests against synthetic private credentials; no live account
//! state, browser storage, tmux or provider processes are used.
use hmux_gateway::{
    auth::{self, Credentials},
    auth_store::AuthStore,
    http_auth::Gateway,
};
use serde_json::{json, Value};
use std::{
    fs, io,
    net::SocketAddr,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const PASSWORD: &str = "synthetic-fixture-password";
const CONNECTOR: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
#[path = "support/action.rs"]
mod action;
#[path = "support/browser_terminal.rs"]
mod browser_terminal;
#[path = "support/browser_upload.rs"]
mod browser_upload;
#[path = "support/diagnostics.rs"]
mod diagnostics;
#[path = "support/static_assets.rs"]
mod static_assets;
#[path = "support/usage_preferences.rs"]
mod usage_preferences;
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
// The application opens one store at startup. Parallel synthetic servers must
// not accidentally exhaust the production process-wide startup admission.
static AUTH_START: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
async fn open_auth(path: &std::path::Path) -> AuthStore {
    let _startup = AUTH_START.lock().await;
    AuthStore::open(path).await.unwrap()
}

struct Fixture {
    root: PathBuf,
    credentials: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-e2e-rust-http-auth-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let credentials = root.join("credentials.json");
        let salt = vec![5; 32];
        let primary = Credentials {
            username: "primary".into(),
            hash: auth::derive_password(PASSWORD, &salt).to_vec(),
            salt,
            totp_secret: data_encoding::BASE32_NOPAD.encode(&[5; 20]),
            last_step: 0,
            totp_disabled: true,
        };
        write_private(&credentials, primary.go_json());
        let users = root.join("credentials.json.users");
        fs::DirBuilder::new().mode(0o700).create(&users).unwrap();
        let guest = Credentials {
            username: "guest".into(),
            ..primary
        };
        write_private(&users.join("guest.json"), guest.go_json());
        Self { root, credentials }
    }
    async fn start(&self) -> Server {
        self.start_with_home(None).await
    }
    async fn start_with_home(&self, home: Option<hmux_gateway::hub::Hub>) -> Server {
        self.start_with_services(home, false).await
    }
    async fn start_with_services(
        &self,
        home: Option<hmux_gateway::hub::Hub>,
        preferences: bool,
    ) -> Server {
        self.start_with_options(home, preferences, false).await
    }
    async fn start_with_options(
        &self,
        home: Option<hmux_gateway::hub::Hub>,
        preferences: bool,
        workspaces: bool,
    ) -> Server {
        self.start_all(home, preferences, workspaces, false).await
    }
    async fn start_all(
        &self,
        home: Option<hmux_gateway::hub::Hub>,
        preferences: bool,
        workspaces: bool,
        diagnostics: bool,
    ) -> Server {
        let auth = Arc::new(open_auth(&self.credentials).await);
        let mut gateway = Gateway::new("https://hmux.example", CONNECTOR, auth).unwrap();
        if diagnostics {
            let _startup = AUTH_START.lock().await;
            let store = hmux_gateway::diagnostics::Store::open(
                hmux_core::PrivateDir::open(&self.root).unwrap(),
                "credentials.json.diagnostics.json".into(),
            )
            .await
            .unwrap();
            gateway = gateway.with_diagnostics(store);
        }
        if workspaces {
            let dir = hmux_core::PrivateDir::open(&self.root)
                .unwrap()
                .create_private_child(std::ffi::OsStr::new("web-profiles"))
                .unwrap();
            gateway = gateway.with_workspaces(hmux_core::workspace::Store::new(dir));
        }
        if preferences {
            let dir = hmux_core::PrivateDir::open(&self.root)
                .unwrap()
                .create_private_child(std::ffi::OsStr::new("credentials.json.usage-preferences"))
                .unwrap();
            gateway = gateway.with_preferences(hmux_gateway::usage_preferences::Store::new(dir));
        }
        if let Some(home) = home {
            gateway = gateway.with_home(home);
        }
        let gateway = Arc::new(gateway);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(gateway.serve(listener, shutdown.clone()));
        Server {
            address,
            shutdown,
            task,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn write_private(path: &std::path::Path, raw: String) {
    fs::write(path, raw).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[tokio::test]
async fn opt_in_home_route_joins_generations_and_supports_both_protocols() {
    use futures_util::{SinkExt, StreamExt};
    use hmux_gateway::hub::{Error as HubError, Hub};
    use hmux_protocol::{
        legacy,
        protobuf::{self as pb, types as p, Direction, Negotiated},
        transport, wire,
    };
    use tokio_tungstenite::{
        client_async_with_config,
        tungstenite::{client::IntoClientRequest, Message},
    };

    fn encode(body: p::envelope::Body, protocol: Negotiated) -> Message {
        let envelope = p::Envelope {
            version: pb::VERSION,
            body: Some(body),
        };
        match protocol {
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
        }
    }
    async fn connect(
        server: &Server,
        protocol: Negotiated,
    ) -> tokio_tungstenite::WebSocketStream<TcpStream> {
        let mut request = "ws://hmux.example/connect".into_client_request().unwrap();
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {CONNECTOR}").parse().unwrap(),
        );
        if protocol == Negotiated::ProtobufV2 {
            request
                .headers_mut()
                .insert("sec-websocket-protocol", pb::SUBPROTOCOL.parse().unwrap());
        }
        let socket = TcpStream::connect(server.address).await.unwrap();
        let (client, response) =
            client_async_with_config(request, socket, Some(transport::socket_config()))
                .await
                .unwrap();
        assert_eq!(
            pb::negotiate(
                response
                    .headers()
                    .get("sec-websocket-protocol")
                    .map(|v| v.to_str().unwrap())
            )
            .unwrap(),
            protocol
        );
        client
    }
    async fn wait_for(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !condition() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    let fixture = Fixture::new();
    let (hub, _completions) = Hub::new();
    let server = fixture.start_with_home(Some(hub.clone())).await;
    let rejected = server.request("GET", "/connect", &[], None).await;
    assert_eq!(rejected.code, 403);
    let rejected = server
        .request(
            "GET",
            "/connect",
            &[
                ("Authorization", &format!("Bearer {CONNECTOR}")),
                ("Origin", "https://hmux.example"),
            ],
            None,
        )
        .await;
    assert_eq!(rejected.code, 403);

    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let mut client = connect(&server, protocol).await;
        client
            .send(encode(
                p::envelope::Body::Hello(p::Hello {
                    capabilities: vec!["terminal-output-flow-v1".into()],
                }),
                protocol,
            ))
            .await
            .unwrap();
        client
            .send(encode(
                p::envelope::Body::Catalog(Box::new(
                    hmux_protocol::snapshots::catalog_from_json(br#"{"sessions":[]}"#).unwrap(),
                )),
                protocol,
            ))
            .await
            .unwrap();
        wait_for(|| hub.snapshot().online).await;
        let generation = hub.snapshot().generation.unwrap();
        let mut duplicate = connect(&server, protocol).await;
        let closed = tokio::time::timeout(Duration::from_secs(2), duplicate.next())
            .await
            .unwrap();
        assert!(
            matches!(closed, Some(Ok(Message::Close(Some(frame)))) if u16::from(frame.code) == 1008)
        );
        assert_eq!(hub.snapshot().generation, Some(generation));

        let pending = tokio::spawn({
            let hub = hub.clone();
            async move {
                hub.request(
                    generation,
                    p::Request {
                        operation: p::Operation::Profiles as i32,
                        payload: Some(p::request::Payload::Empty(p::Empty {})),
                        ..Default::default()
                    },
                )
                .await
            }
        });
        let frame = tokio::time::timeout(Duration::from_secs(2), client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(frame.is_binary(), protocol == Negotiated::ProtobufV2);
        let raw = frame.into_data();
        let envelope = match protocol {
            Negotiated::ProtobufV2 => pb::decode(raw, Direction::ToHome).unwrap(),
            Negotiated::JsonV1 => {
                legacy::from_json(wire::Message::decode(&raw).unwrap(), Direction::ToHome).unwrap()
            }
        };
        let Some(p::envelope::Body::Request(request)) = envelope.body else {
            panic!("expected request")
        };
        client
            .send(encode(
                p::envelope::Body::Response(p::Response {
                    id: request.id,
                    result: Some(p::response::Result::Profiles(p::ProfilesResult {
                        items: Vec::new(),
                    })),
                    error: String::new(),
                }),
                protocol,
            ))
            .await
            .unwrap();
        assert!(matches!(
            pending.await.unwrap().unwrap().result.as_ref(),
            Some(p::response::Result::Profiles(_))
        ));
        if protocol == Negotiated::JsonV1 {
            drop(client);
            wait_for(|| !hub.snapshot().connected).await;
            assert!(hub.snapshot().catalog.is_none());
            assert!(matches!(
                hub.request(generation, p::Request::default()).await,
                Err(HubError::Stale)
            ));
        } else {
            server.shutdown.cancel();
            server.task.await.unwrap().unwrap();
            assert!(!hub.snapshot().connected);
            assert_eq!(hub.retained_payload_bytes(), 0);
            let closed = tokio::time::timeout(Duration::from_secs(2), client.next())
                .await
                .unwrap();
            assert!(
                closed.is_none() || matches!(closed, Some(Err(_)) | Some(Ok(Message::Close(_))))
            );
            return;
        }
    }
}
struct Server {
    address: SocketAddr,
    shutdown: CancellationToken,
    task: JoinHandle<io::Result<()>>,
}
impl Server {
    async fn stop(self) {
        self.shutdown.cancel();
        self.task.await.unwrap().unwrap();
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> Reply {
        let mut stream = TcpStream::connect(self.address).await.unwrap();
        let body = body.map(|v| v.to_string()).unwrap_or_default();
        let mut raw = format!("{method} {path} HTTP/1.1\r\nHost: hmux.example\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
        if !body.is_empty() {
            raw.push_str("Content-Type: application/json\r\n");
        }
        for (key, value) in headers {
            raw.push_str(&format!("{key}: {value}\r\n"));
        }
        raw.push_str("\r\n");
        raw.push_str(&body);
        stream.write_all(raw.as_bytes()).await.unwrap();
        let mut reply = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(30), stream.read_to_end(&mut reply))
            .await
            .unwrap();
        let raw = String::from_utf8(reply).unwrap();
        let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
        let code = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        let headers: Vec<_> = headers
            .lines()
            .skip(1)
            .map(|line| {
                let (name, value) = line.split_once(':').unwrap();
                (name.to_ascii_lowercase(), value.trim().to_owned())
            })
            .collect();
        // An early rejection can close with unread inbound bytes on macOS.
        // Accept a reset only after a complete, explicitly framed response;
        // truncated replies and other transport failures must still fail.
        if let Err(error) = read {
            assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
            let length = headers
                .iter()
                .find(|(name, _)| name == "content-length")
                .expect("reset without response length")
                .1
                .parse::<usize>()
                .unwrap();
            assert_eq!(body.len(), length, "truncated response before reset");
        }
        assert!(headers
            .iter()
            .any(|(name, value)| name == "cache-control" && value == "no-store"));
        Reply {
            code,
            headers,
            body: body.to_owned(),
        }
    }
    async fn login(&self, username: &str) -> String {
        let reply = self
            .request(
                "POST",
                "/api/login",
                &[("Origin", "https://hmux.example")],
                Some(json!({"username":username,"password":PASSWORD})),
            )
            .await;
        assert_eq!(reply.code, 200, "{}", reply.body);
        let cookie = reply
            .headers
            .iter()
            .find(|(n, _)| n == "set-cookie")
            .unwrap()
            .1
            .clone();
        assert!(cookie.contains("HttpOnly; Secure; SameSite=Strict"));
        cookie.split(';').next().unwrap().to_owned()
    }
    async fn session(&self, cookie: &str) -> Reply {
        self.request("GET", "/api/session", &[("Cookie", cookie)], None)
            .await
    }
}
struct Reply {
    code: u16,
    headers: Vec<(String, String)>,
    body: String,
}
impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }
}

fn current_code() -> String {
    use hmac::{Hmac, Mac};
    let step = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        / 30;
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(&[5; 20]).unwrap();
    mac.update(&step.to_be_bytes());
    let hash = mac.finalize().into_bytes();
    let offset = (hash[19] & 15) as usize;
    let value = u32::from_be_bytes(hash[offset..offset + 4].try_into().unwrap()) & 0x7fffffff;
    format!("{:06}", value % 1_000_000)
}

#[tokio::test]
async fn auth_http_restart_revocation_cookie_and_csrf_contracts() {
    let fixture = Fixture::new();
    let server = fixture.start().await;
    for path in [
        "/api/session",
        "/api/state",
        "/api/push",
        "/api/diagnostics",
        "/api/account/usage",
    ] {
        assert_eq!(server.request("GET", path, &[], None).await.code, 401);
    }
    assert_eq!(
        server
            .request(
                "POST",
                "/api/login",
                &[],
                Some(json!({"username":"primary","password":PASSWORD}))
            )
            .await
            .code,
        403
    );
    let primary = server.login("primary").await;
    let guest = server.login("guest").await;
    let second = server.login("primary").await;
    let access = server.session(&primary).await.json();
    let guest_access = server.session(&guest).await.json();
    assert_eq!(access["profile"], "");
    assert_eq!(guest_access["profile"], auth::account_profile("guest"));
    let csrf = access["csrf"].as_str().unwrap();
    let origin = ("Origin", "https://hmux.example");
    let cookie = ("Cookie", primary.as_str());
    assert_eq!(
        server
            .request("POST", "/api/logout", &[origin, cookie], None)
            .await
            .code,
        403
    );
    assert_eq!(
        server
            .request(
                "POST",
                "/api/logout",
                &[origin, cookie, ("X-CSRF-Token", "wrong")],
                None
            )
            .await
            .code,
        403
    );
    assert_eq!(
        server
            .request(
                "POST",
                "/api/logout",
                &[
                    origin,
                    cookie,
                    ("X-CSRF-Token", csrf),
                    ("X-CSRF-Token", csrf)
                ],
                None
            )
            .await
            .code,
        403
    );
    assert_eq!(
        server
            .request("GET", "/api/session", &[cookie, cookie], None)
            .await
            .code,
        401
    );
    assert_eq!(
        server
            .request("GET", "/api/terminal", &[cookie], None)
            .await
            .code,
        403
    );
    assert_eq!(
        server
            .request("GET", "/api/terminal", &[cookie, origin], None)
            .await
            .code,
        503
    );
    assert_eq!(
        server
            .request("GET", "/api/account/security", &[cookie], None)
            .await
            .json()["totp_enabled"],
        false
    );
    assert_eq!(
        server
            .request(
                "POST",
                "/api/account/security",
                &[origin, cookie, ("X-CSRF-Token", csrf)],
                Some(json!({"password":PASSWORD}))
            )
            .await
            .code,
        400
    );
    let rows = server
        .request("GET", "/api/sessions", &[cookie], None)
        .await
        .json();
    assert_eq!(rows["sessions"].as_array().unwrap().len(), 2);
    assert!(rows["sessions"][0]["current"].as_bool().unwrap());
    assert_eq!(
        server
            .request(
                "POST",
                "/api/sessions/revoke",
                &[origin, cookie, ("X-CSRF-Token", csrf)],
                Some(json!({"id":guest_access["login_id"]}))
            )
            .await
            .code,
        404
    );
    let second_id = server.session(&second).await.json()["login_id"].clone();
    assert_eq!(
        server
            .request(
                "POST",
                "/api/sessions/revoke",
                &[origin, cookie, ("X-CSRF-Token", csrf)],
                Some(json!({"id":second_id}))
            )
            .await
            .code,
        200
    );
    assert_eq!(server.session(&second).await.code, 401);
    assert_eq!(
        server
            .request(
                "POST",
                "/api/account/security",
                &[origin, cookie, ("X-CSRF-Token", csrf)],
                Some(json!({"totp_enabled":true,"password":PASSWORD,"code":current_code()}))
            )
            .await
            .code,
        200
    );
    let challenge = server
        .request(
            "POST",
            "/api/login",
            &[origin],
            Some(json!({"username":"primary","password":PASSWORD})),
        )
        .await;
    assert_eq!(challenge.json()["totp_required"], true);
    assert!(!challenge
        .headers
        .iter()
        .any(|(name, _)| name == "set-cookie"));
    server.stop().await;
    let server = fixture.start().await;
    assert_eq!(server.session(&primary).await.code, 200);
    assert_eq!(server.session(&guest).await.code, 200);
    assert_eq!(
        server
            .request("GET", "/api/account/security", &[cookie], None)
            .await
            .json()["totp_enabled"],
        true
    );
    assert_eq!(server.session(&second).await.code, 401);
    assert_eq!(
        server
            .request(
                "POST",
                "/api/logout",
                &[origin, cookie, ("X-CSRF-Token", csrf)],
                None
            )
            .await
            .code,
        200
    );
    assert_eq!(server.session(&primary).await.code, 401);
    assert_eq!(
        server
            .request(
                "POST",
                "/api/account/security",
                &[origin, cookie, ("X-CSRF-Token", csrf)],
                Some(json!({"totp_enabled":false,"password":PASSWORD,"code":"000000"}))
            )
            .await
            .code,
        401
    );
    assert_eq!(server.session(&guest).await.code, 200);
    if let Some(output) = std::env::var_os("HMUX_RUST_AUTH_HANDOFF") {
        let output = PathBuf::from(output).canonicalize().unwrap();
        fs::set_permissions(&output, fs::Permissions::from_mode(0o700)).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(output.join("credentials.json.users"))
            .unwrap();
        for name in [
            "credentials.json",
            "credentials.json.sessions",
            "credentials.json.users/guest.json",
        ] {
            write_private(
                &output.join(name),
                fs::read_to_string(fixture.root.join(name)).unwrap(),
            );
        }
        write_private(&output.join("synthetic-handoff.json"), json!({"primary":primary.split('=').nth(1).unwrap(), "revoked":second.split('=').nth(1).unwrap(), "guest":guest.split('=').nth(1).unwrap()}).to_string());
    }
    let sessions_path = fixture.root.join("credentials.json.sessions");
    fs::set_permissions(&sessions_path, fs::Permissions::from_mode(0o644)).unwrap();
    let guest_csrf = guest_access["csrf"].as_str().unwrap();
    assert_eq!(
        server
            .request(
                "POST",
                "/api/logout",
                &[origin, ("Cookie", &guest), ("X-CSRF-Token", guest_csrf)],
                None
            )
            .await
            .code,
        503
    );
    assert_eq!(server.session(&guest).await.code, 503);
    server.stop().await;
    let persisted = fs::read_to_string(fixture.root.join("credentials.json.sessions")).unwrap();
    for value in [&primary, &second, &guest] {
        assert!(!persisted.contains(value.split('=').nth(1).unwrap()));
    }
}

#[tokio::test]
#[ignore = "run by make rust-compat after the Go current-state rollback writer"]
async fn reload_current_state_after_go_logout() {
    let output = PathBuf::from(
        std::env::var_os("HMUX_RUST_AUTH_HANDOFF").expect("synthetic handoff directory required"),
    );
    let tokens: Value =
        serde_json::from_slice(&fs::read(output.join("synthetic-handoff.json")).unwrap()).unwrap();
    let store = AuthStore::open(output.canonicalize().unwrap().join("credentials.json"))
        .await
        .unwrap();
    for key in ["primary", "revoked", "guest"] {
        assert!(store
            .access(
                tokens[key].as_str().unwrap(),
                false,
                SystemTime::now().into()
            )
            .await
            .unwrap()
            .is_none());
    }
}
