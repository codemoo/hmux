//! Real browser HTTP -> Hub completion -> encrypted, verified TLS POST. All
//! credentials, certificates, subscriptions and Home events are disposable.
use super::*;
use crate::{auth, auth_store::AuthStore, http_auth::Gateway, hub::Hub, push::Push};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures_util::{SinkExt, StreamExt};
use hmux_protocol::{
    protobuf::{self as pb, types as p, Direction, Negotiated},
    transport,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    tungstenite::{protocol::Role, Message},
    WebSocketStream,
};

const PASSWORD: &str = "synthetic-fixture-password";
const ORIGIN: &str = "https://hmux.example";
const RECEIVER_PUBLIC: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const RECEIVER_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
const RECEIVER_AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-e2e-rust-push-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let salt = vec![5; 32];
        let mut credentials = auth::Credentials {
            username: "primary".into(),
            hash: auth::derive_password(PASSWORD, &salt).to_vec(),
            salt,
            totp_secret: data_encoding::BASE32_NOPAD.encode(&[5; 20]),
            last_step: 0,
            totp_disabled: true,
        };
        write_private(&path.join("credentials.json"), &credentials.go_json());
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path.join("credentials.json.users"))
            .unwrap();
        credentials.username = "guest".into();
        write_private(
            &path.join("credentials.json.users/guest.json"),
            &credentials.go_json(),
        );
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn write_private(path: &std::path::Path, raw: &str) {
    fs::write(path, raw).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[derive(Default)]
struct Browser {
    cookie: String,
    csrf: String,
    id: String,
}
struct HttpReply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Value,
}
struct BrowserServer {
    address: SocketAddr,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl BrowserServer {
    async fn request(&self, browser: &Browser, method: &str, path: &str, raw: &str) -> HttpReply {
        let mut stream = TcpStream::connect(self.address).await.unwrap();
        let request = format!("{method} {path} HTTP/1.1\r\nHost: hmux.example\r\nOrigin: {ORIGIN}\r\nCookie: {}\r\nX-CSRF-Token: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{raw}", browser.cookie, browser.csrf, raw.len());
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reply = Vec::new();
        timeout(Duration::from_secs(30), stream.read_to_end(&mut reply))
            .await
            .unwrap()
            .unwrap();
        let raw = String::from_utf8(reply).unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        let headers: BTreeMap<_, _> = head
            .lines()
            .skip(1)
            .map(|line| {
                let (k, v) = line.split_once(':').unwrap();
                (k.to_ascii_lowercase(), v.trim().to_owned())
            })
            .collect();
        assert_eq!(headers["cache-control"], "no-store");
        HttpReply {
            status: head.split_whitespace().nth(1).unwrap().parse().unwrap(),
            headers,
            body: serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.to_owned())),
        }
    }
    async fn login(&self, name: &str) -> Browser {
        let reply = self
            .request(
                &Browser::default(),
                "POST",
                "/api/login",
                &json!({"username":name,"password":PASSWORD}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        let mut browser = Browser {
            cookie: reply.headers["set-cookie"]
                .split(';')
                .next()
                .unwrap()
                .into(),
            ..Default::default()
        };
        let session = self.request(&browser, "GET", "/api/session", "").await;
        browser.csrf = session.body["csrf"].as_str().unwrap().into();
        browser.id = session.body["login_id"].as_str().unwrap().into();
        browser
    }
    async fn post(&self, browser: &Browser, path: &str, value: Value, expected: u16) -> Value {
        let reply = self
            .request(browser, "POST", path, &value.to_string())
            .await;
        assert_eq!(reply.status, expected, "{path}: {}", reply.body);
        reply.body
    }
}

#[derive(Clone)]
struct Captured {
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
type Pause = Arc<Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>>;
fn pause_next(pause: &Pause) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
    let (started, ready) = oneshot::channel();
    let (release, wait) = oneshot::channel();
    assert!(pause.lock().unwrap().replace((started, wait)).is_none());
    (ready, release)
}
async fn capture() -> (
    Client,
    mpsc::Receiver<Captured>,
    CancellationToken,
    tokio::task::JoinHandle<()>,
    Pause,
) {
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
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let stop = CancellationToken::new();
    let stopping = stop.clone();
    let (tx, rx) = mpsc::channel(16);
    let pause: Pause = Arc::new(Mutex::new(None));
    let gate = pause.clone();
    let task = tokio::spawn(async move {
        'connections: loop {
            let accepted = tokio::select! { biased; _=stopping.cancelled()=>break, pair=listener.accept()=>pair.unwrap() };
            let pause = gate.lock().unwrap().take();
            if let Some((started, wait)) = pause {
                let _ = started.send(());
                timeout(Duration::from_secs(4), wait)
                    .await
                    .unwrap()
                    .unwrap();
            }
            let mut socket = acceptor.accept(accepted.0).await.unwrap();
            assert_eq!(socket.get_ref().1.server_name(), Some("fcm.googleapis.com"));
            let mut raw = Vec::new();
            let mut chunk = [0; 1024];
            let (at, length, headers) = loop {
                assert!(raw.len() < 16384);
                let n = timeout(Duration::from_secs(3), socket.read(&mut chunk))
                    .await
                    .unwrap()
                    .unwrap_or_else(|error| {
                        if raw.is_empty()
                            && matches!(
                                error.kind(),
                                std::io::ErrorKind::UnexpectedEof
                                    | std::io::ErrorKind::ConnectionReset
                            )
                        {
                            0
                        } else {
                            panic!("truncated push request: {error}")
                        }
                    });
                if n == 0 && raw.is_empty() {
                    continue 'connections; // authorization changed before POST
                }
                assert_ne!(n, 0);
                raw.extend_from_slice(&chunk[..n]);
                if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = std::str::from_utf8(&raw[..at]).unwrap();
                    let headers: BTreeMap<_, _> = head
                        .lines()
                        .skip(1)
                        .map(|line| {
                            let (k, v) = line.split_once(':').unwrap();
                            (k.to_ascii_lowercase(), v.trim().to_owned())
                        })
                        .collect();
                    let length: usize = headers["content-length"].parse().unwrap();
                    assert_eq!(length, 4096);
                    break (at, length, headers);
                }
            };
            while raw.len() < at + 4 + length {
                let n = timeout(Duration::from_secs(3), socket.read(&mut chunk))
                    .await
                    .unwrap()
                    .unwrap();
                assert_ne!(n, 0);
                raw.extend_from_slice(&chunk[..n]);
            }
            let head = std::str::from_utf8(&raw[..at]).unwrap();
            assert!(head.starts_with("POST "));
            let path = head.split_whitespace().nth(1).unwrap().to_owned();
            let status = if path == "/gone" {
                "410 Gone"
            } else {
                "201 Created"
            };
            socket
                .write_all(
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .unwrap();
            tx.send(Captured {
                path,
                headers,
                body: raw[at + 4..].to_vec(),
            })
            .await
            .unwrap();
        }
    });
    (client, rx, stop, task, pause)
}
fn subscription(path: &str) -> Value {
    json!({"endpoint":format!("https://fcm.googleapis.com{path}"),"expirationTime":null,
        "keys":{"auth":RECEIVER_AUTH,"p256dh":RECEIVER_PUBLIC}})
}
fn decrypt(body: &[u8]) -> Value {
    use aes_gcm::{
        aead::{Aead, KeyInit},
        Aes128Gcm, Nonce,
    };
    use hkdf::Hkdf;
    use p256::{ecdh::diffie_hellman, PublicKey, SecretKey};
    use sha2::Sha256;
    assert_eq!(body.len(), 4096);
    let secret = SecretKey::from_slice(&URL_SAFE_NO_PAD.decode(RECEIVER_PRIVATE).unwrap()).unwrap();
    let sender = PublicKey::from_sec1_bytes(&body[21..86]).unwrap();
    let shared = diffie_hellman(secret.to_nonzero_scalar(), sender.as_affine());
    let mut info = b"WebPush: info\0".to_vec();
    info.extend(URL_SAFE_NO_PAD.decode(RECEIVER_PUBLIC).unwrap());
    info.extend(&body[21..86]);
    let mut ikm = [0; 32];
    Hkdf::<Sha256>::new(
        Some(&URL_SAFE_NO_PAD.decode(RECEIVER_AUTH).unwrap()),
        shared.raw_secret_bytes(),
    )
    .expand(&info, &mut ikm)
    .unwrap();
    let hkdf = Hkdf::<Sha256>::new(Some(&body[..16]), &ikm);
    let mut key = [0; 16];
    let mut nonce = [0; 12];
    hkdf.expand(b"Content-Encoding: aes128gcm\0", &mut key)
        .unwrap();
    hkdf.expand(b"Content-Encoding: nonce\0", &mut nonce)
        .unwrap();
    let plain = Aes128Gcm::new_from_slice(&key)
        .unwrap()
        .decrypt(Nonce::from_slice(&nonce), &body[86..])
        .unwrap();
    let end = plain.iter().rposition(|b| *b != 0).unwrap();
    assert_eq!(plain[end], 2);
    serde_json::from_slice(&plain[..end]).unwrap()
}
async fn received(rx: &mut mpsc::Receiver<Captured>, event: &str, login: &str) -> Captured {
    let request = timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let payload = decrypt(&request.body);
    assert_eq!(payload["event_id"], event);
    assert_eq!(payload["login_id"], login);
    request
}
async fn encode(socket: &mut WebSocketStream<tokio::io::DuplexStream>, body: p::envelope::Body) {
    socket
        .send(Message::Binary(
            pb::encode(
                &p::Envelope {
                    version: pb::VERSION,
                    body: Some(body),
                },
                Direction::ToGateway,
            )
            .unwrap(),
        ))
        .await
        .unwrap();
}
struct FakeHome {
    tx: mpsc::Sender<p::envelope::Body>,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl FakeHome {
    async fn new(hub: &Hub) -> Self {
        let (left, right) = tokio::io::duplex(8192);
        let server =
            WebSocketStream::from_raw_socket(left, Role::Server, Some(transport::socket_config()))
                .await;
        let mut socket =
            WebSocketStream::from_raw_socket(right, Role::Client, Some(transport::socket_config()))
                .await;
        let owner = hub
            .attach(transport::start(server, Negotiated::ProtobufV2, Direction::ToGateway).unwrap())
            .unwrap();
        encode(&mut socket, p::envelope::Body::Hello(p::Hello::default())).await;
        encode(&mut socket,p::envelope::Body::Catalog(Box::new(hmux_protocol::snapshots::catalog_from_json(r#"{"sessions":[{"id":"$1","created_at":42,"name":"raw-name","alias":"배포 작업"},{"id":"$2","created_at":43,"name":"sentinel"}]}"#.as_bytes()).unwrap()))).await;
        timeout(Duration::from_secs(2), async {
            while hub.snapshot().catalog.is_none() {
                tokio::task::yield_now().await
            }
        })
        .await
        .unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! { biased;
                    _=stopping.cancelled()=>break,
                    event=rx.recv()=> {if let Some(event)=event {encode(&mut socket,event).await} else {break}},
                    message=socket.next()=> {match message {
                        Some(Ok(Message::Binary(raw)))=>{
                            let p::envelope::Body::Request(request)=pb::decode(raw,Direction::ToHome).unwrap().body.unwrap() else {panic!("unexpected Home message")};
                            assert_eq!(request.operation,p::Operation::Workspace as i32);
                            encode(&mut socket,p::envelope::Body::Response(p::Response{id:request.id,result:Some(p::response::Result::Workspace(Box::new(p::WorkspaceSnapshot{
                                version:1,initialized:true,revision:1,tabs:vec![
                                    p::Session{id:"$1".into(),created_at:42},
                                    p::Session{id:"$2".into(),created_at:43},
                                ],..Default::default()
                            }))),..Default::default()})).await;
                        },
                        Some(Ok(Message::Ping(_)))=>{socket.flush().await.unwrap()},
                        Some(Ok(Message::Pong(_)))=>{},
                        None|Some(Ok(Message::Close(_)))=>break,
                        other=>panic!("unexpected message {other:?}")
                    }}
                }
            }
            let _ = socket.close(None).await;
            owner.wait().await;
        });
        Self { tx, stop, task }
    }
    async fn completion(&self, n: u8, id: &str, created_at: i64) -> String {
        let event = format!("{n:064x}");
        self.tx
            .send(p::envelope::Body::TaskComplete(p::Completion {
                id: event.clone(),
                session: Some(p::Session {
                    id: id.into(),
                    created_at,
                }),
                completed_at: chrono::DateTime::<chrono::Utc>::from(SystemTime::now()).to_rfc3339(),
            }))
            .await
            .unwrap();
        event
    }
}

#[tokio::test]
async fn browser_oracle_and_completion_delivery_preserve_scope_presence_and_deep_links() {
    let fixture = Fixture::new();
    let auth = loop {
        match AuthStore::open(fixture.0.join("credentials.json")).await {
            Ok(auth) => break Arc::new(auth),
            Err(ref e) if e.to_string() == "authentication startup is busy" => {
                tokio::task::yield_now().await
            }
            Err(e) => panic!("{e:?}"),
        }
    };
    let dir = hmux_core::PrivateDir::open(&fixture.0).unwrap();
    let store = push_state::Store::open(dir, OsStr::new("credentials.json"))
        .await
        .unwrap();
    let workspaces = hmux_core::workspace::Store::new(
        hmux_core::PrivateDir::open(&fixture.0)
            .unwrap()
            .create_private_child(OsStr::new("web-profiles"))
            .unwrap(),
    );
    let (client, mut captures, capture_stop, capture_task, pause) = capture().await;
    let (hub, events) = Hub::new();
    let home = FakeHome::new(&hub).await;
    let push = Push::new(store, client, events);
    let gateway = Arc::new(
        Gateway::new(ORIGIN, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", auth)
            .unwrap()
            .with_home(hub)
            .with_workspaces(workspaces)
            .with_push(push),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let stop = CancellationToken::new();
    let server = BrowserServer {
        address: listener.local_addr().unwrap(),
        task: tokio::spawn(gateway.serve(listener, stop.clone())),
        stop,
    };
    let primary = server.login("primary").await;
    let guest = server.login("guest").await;
    let forged = Browser {
        cookie: primary.cookie.clone(),
        ..Default::default()
    };
    server
        .post(&forged, "/api/push/subscribe", subscription("/csrf"), 403)
        .await;
    assert_eq!(
        server
            .request(&Browser::default(), "GET", "/api/push", "")
            .await
            .status,
        401
    );
    assert_eq!(
        server
            .request(&primary, "GET", "/api/push/test", "")
            .await
            .status,
        405
    );
    // Preserve duplicate JSON fields by sending the Go fixture's raw body.
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("../../../tests/fixtures/push-v1/go-api.json")).unwrap();
    assert_eq!(cases.len(), 41);
    for case in cases {
        let reply = server
            .request(
                &primary,
                "POST",
                case["path"].as_str().unwrap(),
                case["body"].as_str().unwrap(),
            )
            .await;
        assert_eq!(
            u64::from(reply.status),
            case["rust_status"].as_u64().unwrap(),
            "{}: {}",
            case["name"],
            reply.body
        );
    }
    server
        .post(
            &primary,
            "/api/push/subscribe",
            subscription("/primary"),
            200,
        )
        .await;
    server
        .post(&guest, "/api/push/subscribe", subscription("/guest"), 200)
        .await;
    // A subscription alone must not give guest access to primary's open tabs.
    let first = home.completion(1, "$1", 42).await;
    let request = received(&mut captures, &first, &primary.id).await;
    assert_eq!(request.path, "/primary");
    let expected = json!({"type":"codex-complete","tab_name":"배포 작업","session":{"id":"$1","created_at":42},"login_id":primary.id,"event_id":first});
    assert_eq!(decrypt(&request.body), expected);
    if let Ok(path) = std::env::var("HMUX_PUSH_DELIVERY_CAPTURE") {
        let config = server.request(&primary, "GET", "/api/push", "").await.body;
        fs::write(path,serde_json::to_vec(&json!({"endpoint":"https://fcm.googleapis.com/primary","headers":request.headers,
            "body":URL_SAFE_NO_PAD.encode(&request.body),"plaintext":URL_SAFE_NO_PAD.encode(serde_json::to_vec(&expected).unwrap()),
            "login_id":primary.id,"origin":ORIGIN,"now":chrono::DateTime::<chrono::Utc>::from(SystemTime::now()).timestamp(),"subscription":subscription("/primary"),
            "receiver_private":RECEIVER_PRIVATE,"vapid_public":config["public_key"]})).unwrap()).unwrap();
    }
    server
        .post(
            &primary,
            "/api/push/presence",
            json!({"client_id":"browser","session":{"id":"$1","created_at":42}}),
            200,
        )
        .await;
    home.completion(2, "$1", 42).await;
    let sentinel = home.completion(3, "$2", 43).await;
    received(&mut captures, &sentinel, &primary.id).await;
    // The same tmux ID with a different birth timestamp is never a match.
    home.completion(4, "$1", 41).await;
    let sentinel = home.completion(5, "$2", 43).await;
    received(&mut captures, &sentinel, &primary.id).await;
    server
        .post(
            &primary,
            "/api/push/presence",
            json!({"client_id":"browser","session":null}),
            200,
        )
        .await;
    let next = home.completion(6, "$1", 42).await;
    received(&mut captures, &next, &primary.id).await;
    // Delivery was admitted before TLS. A newly visible tab must still suppress
    // it when the pre-POST gate runs after the handshake.
    let (ready, release) = pause_next(&pause);
    home.completion(7, "$1", 42).await;
    timeout(Duration::from_secs(3), ready)
        .await
        .unwrap()
        .unwrap();
    server
        .post(
            &primary,
            "/api/push/presence",
            json!({"client_id":"late","session":{"id":"$1","created_at":42}}),
            200,
        )
        .await;
    release.send(()).unwrap();
    let sentinel = home.completion(8, "$2", 43).await;
    received(&mut captures, &sentinel, &primary.id).await;
    // Endpoint ownership transfers to the subscribing login only.
    server
        .post(&guest, "/api/push/subscribe", subscription("/primary"), 200)
        .await;
    assert_eq!(
        server.request(&primary, "GET", "/api/push", "").await.body["enabled"],
        false
    );
    // A concurrent account-workspace removal during transport preparation must
    // be checked again, even though the Home catalog and generation are stable.
    let workspace = server
        .post(
            &guest,
            "/api/action",
            json!({"operation":"workspace","payload":null}),
            200,
        )
        .await;
    let both = json!([{"id":"$1","created_at":42},{"id":"$2","created_at":43}]);
    let workspace=server.post(&guest,"/api/action",json!({"operation":"workspace","payload":{"change":{
        "operation_id":"push-workspace-add","revision":workspace["revision"],"base":workspace["tabs"],"tabs":both}}}),200).await;
    let (ready, release) = pause_next(&pause);
    home.completion(9, "$1", 42).await;
    timeout(Duration::from_secs(3), ready)
        .await
        .unwrap()
        .unwrap();
    server.post(&guest,"/api/action",json!({"operation":"workspace","payload":{"change":{
        "operation_id":"push-workspace-remove","revision":workspace["revision"],"base":workspace["tabs"],"tabs":[{"id":"$2","created_at":43}]}}}),200).await;
    release.send(()).unwrap();
    let sentinel = home.completion(10, "$2", 43).await;
    received(&mut captures, &sentinel, &guest.id).await;
    server.post(&guest, "/api/push/test", json!({}), 200).await;
    let test = timeout(Duration::from_secs(3), captures.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decrypt(&test.body)["login_id"], guest.id);
    assert_eq!(decrypt(&test.body)["type"], "test");
    server.post(&guest, "/api/push/test", json!({}), 429).await;
    server
        .post(&primary, "/api/push/test", json!({}), 409)
        .await;
    server
        .post(&primary, "/api/push/subscribe", subscription("/gone"), 200)
        .await;
    server
        .post(&primary, "/api/push/test", json!({}), 502)
        .await;
    assert_eq!(captures.recv().await.unwrap().path, "/gone");
    assert_eq!(
        server.request(&primary, "GET", "/api/push", "").await.body["enabled"],
        false
    );
    server.post(&guest, "/api/logout", json!({}), 200).await;
    assert_eq!(
        server.request(&guest, "GET", "/api/push", "").await.status,
        401
    );
    server.stop.cancel();
    timeout(Duration::from_secs(5), server.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    home.stop.cancel();
    home.task.await.unwrap();
    capture_stop.cancel();
    capture_task.await.unwrap();
    assert!(captures.try_recv().is_err());
}

#[path = "session_location_http_tests.rs"]
mod location;
