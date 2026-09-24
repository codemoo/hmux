//! Actual candidate process, synthetic private config/CA/tmux, loopback WSS only.
//! Native CPU/memory/GPU/disk collectors can run; no real provider records or
//! original tmux sessions are used. This is not a metrics accuracy benchmark.
use base64::{engine::general_purpose::STANDARD, Engine};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use hmux_core::command::with_child_spawn;
use hmux_home::singleton::ConnectorLock;
use hmux_protocol::{
    flow, legacy,
    protobuf::{self as pb, types as p, Direction, Negotiated},
    wire,
};
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use rustls::{
    pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
    ServerConfig,
};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    time::timeout,
};
use tokio_rustls::{server::TlsStream, TlsAcceptor};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        handshake::server::{Request, Response},
        Message,
    },
    WebSocketStream,
};

const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
type Socket = WebSocketStream<TlsStream<TcpStream>>;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("hmux-e2e-home-runtime-{} 한글", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str, text: &str, mode: u32) {
        let path = self.0.join(name);
        fs::write(&path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    fn command(&self, address: std::net::SocketAddr) -> Command {
        let binary = std::env::var_os("HMUX_RUST_HOME_BIN").expect("make rust-home-candidate-e2e");
        assert!(PathBuf::from(&binary).is_absolute());
        let mut command = Command::new(binary);
        if std::env::var_os("HMUX_RUST_HOME_PRODUCTION").is_some() {
            command
                .arg("connect")
                .arg("--log-file")
                .arg(self.0.join("service.log"));
        } else {
            command.arg("--experimental-home");
            command.arg("--staging-root").arg(self.staging_root());
        }
        command
            .arg("--url")
            .arg(format!("wss://{address}/connect"))
            .arg("--token-file")
            .arg(self.0.join("connector.token"))
            .arg("--config")
            .arg(self.0.join("home.toml"))
            .env_clear()
            .env("HOME", &self.0)
            .env(
                "PATH",
                std::env::join_paths([
                    self.0.clone(),
                    PathBuf::from("/usr/bin"),
                    PathBuf::from("/bin"),
                ])
                .unwrap(),
            )
            .env("SSL_CERT_FILE", self.0.join("ca.pem"))
            .current_dir(&self.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }
    fn staging_root(&self) -> PathBuf {
        if std::env::var_os("HMUX_RUST_HOME_PRODUCTION").is_some() {
            let cache = if cfg!(target_os = "macos") {
                "Library/Caches"
            } else {
                ".cache"
            };
            self.0.join(cache).join("hmux/staged-files-v1")
        } else {
            self.0.join("hmux/staged-files-v1")
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn certificate(fixture: &Fixture) -> TlsAcceptor {
    let ca_key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = params.self_signed(&ca_key).unwrap();
    fixture.file(
        "ca.pem",
        &format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            STANDARD.encode(ca.der())
        ),
        0o600,
    );
    let key = KeyPair::generate().unwrap();
    let cert = CertificateParams::new(vec!["127.0.0.1".into()])
        .unwrap()
        .signed_by(&key, &ca, &ca_key)
        .unwrap();
    TlsAcceptor::from(Arc::new(
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .unwrap(),
    ))
}
async fn accept(listener: &TcpListener, tls: &TlsAcceptor, protocol: Negotiated) -> Socket {
    timeout(Duration::from_secs(10), async {
        let (socket, _) = listener.accept().await.unwrap();
        let tls = tls.accept(socket).await.unwrap();
        accept_hdr_async(tls, move |request: &Request, mut response: Response| {
            assert_eq!(request.uri().path(), "/connect");
            assert_eq!(
                request.headers()["authorization"],
                format!("Bearer {TOKEN}")
            );
            assert_eq!(request.headers()["sec-websocket-protocol"], pb::SUBPROTOCOL);
            assert!(!request.headers().contains_key("proxy-authorization"));
            if protocol == Negotiated::ProtobufV2 {
                response
                    .headers_mut()
                    .insert("sec-websocket-protocol", pb::SUBPROTOCOL.parse().unwrap());
            }
            Ok(response)
        })
        .await
        .unwrap()
    })
    .await
    .expect("candidate WSS connection")
}
async fn frame(socket: &mut Socket, protocol: Negotiated) -> p::envelope::Body {
    timeout(Duration::from_secs(5), async {
        loop {
            let body = match socket.next().await.unwrap().unwrap() {
                Message::Binary(raw) if protocol == Negotiated::ProtobufV2 => {
                    pb::decode(raw, Direction::ToGateway).unwrap()
                }
                Message::Text(raw) if protocol == Negotiated::JsonV1 => {
                    let message = wire::Message::decode(raw.as_bytes()).unwrap();
                    let context = if message.id == "profiles" {
                        hmux_protocol::actions::ResponseContext::Operation(p::Operation::Profiles)
                    } else {
                        hmux_protocol::actions::ResponseContext::TerminalOpen
                    };
                    legacy::from_json_with_context(message, Direction::ToGateway, Some(context))
                }
                .unwrap(),
                Message::Ping(raw) => {
                    socket.send(Message::Pong(raw)).await.unwrap();
                    continue;
                }
                _ => panic!("unexpected WSS frame"),
            };
            match body.body.unwrap() {
                // Shared collectors publish independently of request replies.
                p::envelope::Body::Usage(usage) => {
                    let snapshot = hmux_protocol::snapshots::usage_from_proto(*usage).unwrap();
                    assert_eq!(snapshot.schema, 1);
                    assert!(!snapshot.generated_at_utc.is_empty());
                }
                p::envelope::Body::UsageUnavailable(_) => {}
                body => return body,
            }
        }
    })
    .await
    .unwrap()
}
async fn send(socket: &mut Socket, protocol: Negotiated, body: p::envelope::Body) {
    let envelope = p::Envelope {
        version: pb::VERSION,
        body: Some(body),
    };
    let message = match protocol {
        Negotiated::ProtobufV2 => {
            Message::Binary(pb::encode(&envelope, Direction::ToHome).unwrap())
        }
        Negotiated::JsonV1 => Message::Text(
            String::from_utf8(
                legacy::to_json(envelope, Direction::ToHome)
                    .unwrap()
                    .encode()
                    .unwrap(),
            )
            .unwrap()
            .into(),
        ),
    };
    socket.send(message).await.unwrap();
}
async fn finish(child: Child) -> std::process::Output {
    timeout(Duration::from_secs(8), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}
async fn ready(socket: &mut Socket, protocol: Negotiated) {
    assert!(
        matches!(frame(socket, protocol).await, p::envelope::Body::Hello(h) if h.capabilities == [flow::CAPABILITY, "web-upload-v1"])
    );
    assert!(matches!(
        frame(socket, protocol).await,
        p::envelope::Body::Catalog(_)
    ));
}

#[tokio::test]
#[ignore = "requires separately built candidate through make rust-home-candidate-e2e"]
async fn candidate_wss_both_codecs_reconnect_signal_cleanup_and_private_inputs() {
    let fixture = Fixture::new();
    fixture.file("connector.token", TOKEN, 0o600);
    fixture.file(
        "home.toml",
        &format!(
            "schema_version=1\nstate_dir='{}'\ninventory_path='{}'\n",
            fixture.0.join("state").display(),
            fixture.0.join("inventory.toml").display()
        ),
        0o600,
    );
    fixture.file("inventory.toml", "schema_version=1\nrevision='synthetic'\n[[profiles]]\nid='shell'\nlabel='Shell'\ndefault_directory='~'\ncommand=['sh']\n", 0o600);
    fixture.file("tmux", r#"#!/bin/sh
case "$1" in
list-sessions) printf '%s\n' '$7|:hmux-sep-v1:|synthetic|:hmux-sep-v1:|1700000000|:hmux-sep-v1:|1700000200|:hmux-sep-v1:|0|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|' ;;
list-windows) : ;;
list-panes) printf '%s\n' '$7|:hmux-recovery-v1:|@1|:hmux-recovery-v1:|0|:hmux-recovery-v1:|shell|:hmux-recovery-v1:|b1e2,80x24,0,0,0|:hmux-recovery-v1:|1|:hmux-recovery-v1:|%0|:hmux-recovery-v1:|0|:hmux-recovery-v1:|1|:hmux-recovery-v1:|/synthetic|:hmux-recovery-v1:|123' ;;
display-message) printf '1700000000\n' ;;
new-session|set-hook) : ;;
if-shell) printf 'cleaned\n' >> "${0%/*}/cleanup" ;;
attach-session)
 stty -echo
 printf 'READY\n'
 while IFS= read -r line; do printf 'INPUT:%s\n' "$line"; done ;;
*) exit 1 ;;
esac
"#, 0o700);
    fixture.file("ps", "#!/bin/sh\nexit 0\n", 0o700);
    let tls = certificate(&fixture);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let mut command = fixture.command(address);
        let child = with_child_spawn(|| command.spawn()).unwrap();
        let mut socket = accept(&listener, &tls, protocol).await;
        ready(&mut socket, protocol).await;
        assert!(ConnectorLock::acquire(&fixture.0.join("state")).is_err());
        let duplicate = with_child_spawn(|| fixture.command(address).spawn()).unwrap();
        let duplicate = finish(duplicate).await;
        assert!(!duplicate.status.success());
        assert!(!String::from_utf8_lossy(&duplicate.stderr).contains(TOKEN));

        // The same process must relinquish its old socket and reconnect after
        // the existing three-second backoff, while retaining singleton ownership.
        drop(socket);
        let mut socket = accept(&listener, &tls, protocol).await;
        ready(&mut socket, protocol).await;
        send(
            &mut socket,
            protocol,
            p::envelope::Body::Request(Box::new(p::Request {
                id: "profiles".into(),
                operation: p::Operation::Profiles as i32,
                session: None,
                payload: Some(p::request::Payload::Empty(p::Empty {})),
            })),
        )
        .await;
        assert!(
            matches!(frame(&mut socket, protocol).await, p::envelope::Body::Response(r) if r.id == "profiles" && r.error.is_empty())
        );

        let upload_id = "0123456789abcdef0123456789abcdef".to_owned();
        let upload_header = p::UploadHeader {
            protocol_version: 1,
            request_id: upload_id.clone(),
            session: Some(p::Session {
                id: "$7".into(),
                created_at: 1700000000,
            }),
            file_count: 1,
            total_bytes: 4,
            files: vec![p::FileHeader {
                index: 0,
                size: 4,
                extension: "bin".into(),
            }],
        };
        send(
            &mut socket,
            protocol,
            p::envelope::Body::UploadStart(p::UploadStart {
                id: upload_id.clone(),
                header: Some(upload_header.clone()),
            }),
        )
        .await;
        assert!(matches!(
            frame(&mut socket, protocol).await,
            p::envelope::Body::UploadReady(_)
        ));
        send(
            &mut socket,
            protocol,
            p::envelope::Body::UploadData(p::Data {
                id: upload_id.clone(),
                data: Bytes::from_static(b"\x00\xffok"),
            }),
        )
        .await;
        assert!(
            matches!(frame(&mut socket, protocol).await, p::envelope::Body::UploadAck(a) if a.received == 4)
        );
        send(
            &mut socket,
            protocol,
            p::envelope::Body::UploadFinish(p::Reference {
                id: upload_id.clone(),
            }),
        )
        .await;
        let p::envelope::Body::UploadComplete(completed) = frame(&mut socket, protocol).await
        else {
            panic!("upload completion required");
        };
        let manifest: serde_json::Value =
            serde_json::from_slice(&hmux_protocol::actions::response_payload(&completed).unwrap())
                .unwrap();
        let committed = PathBuf::from(manifest["files"][0]["path"].as_str().unwrap());
        assert_eq!(fs::read(&committed).unwrap(), b"\x00\xffok");
        // Leave another upload incomplete through process SIGTERM below.
        let mut partial = upload_header;
        partial.request_id = "1123456789abcdef0123456789abcdef".into();
        let partial_id = partial.request_id.clone();
        send(
            &mut socket,
            protocol,
            p::envelope::Body::UploadStart(p::UploadStart {
                id: partial_id.clone(),
                header: Some(partial),
            }),
        )
        .await;
        assert!(matches!(
            frame(&mut socket, protocol).await,
            p::envelope::Body::UploadReady(_)
        ));
        send(
            &mut socket,
            protocol,
            p::envelope::Body::UploadData(p::Data {
                id: partial_id,
                data: Bytes::from_static(b"x"),
            }),
        )
        .await;
        assert!(
            matches!(frame(&mut socket, protocol).await, p::envelope::Body::UploadAck(a) if a.received == 1)
        );
        send(
            &mut socket,
            protocol,
            p::envelope::Body::TerminalOpen(p::TerminalOpen {
                id: "view".into(),
                session: Some(p::Session {
                    id: "$7".into(),
                    created_at: 1700000000,
                }),
                cols: 80,
                rows: 24,
                capabilities: vec![flow::CAPABILITY.into()],
            }),
        )
        .await;
        assert!(
            matches!(frame(&mut socket, protocol).await, p::envelope::Body::Response(r) if r.id == "view" && r.error.is_empty())
        );
        let mut output = Vec::new();
        while !String::from_utf8_lossy(&output).contains("READY") {
            let p::envelope::Body::TerminalOutput(part) = frame(&mut socket, protocol).await else {
                panic!("terminal output required")
            };
            output.extend_from_slice(&part.data);
            send(
                &mut socket,
                protocol,
                p::envelope::Body::OutputAck(p::Ack {
                    id: part.id,
                    received: part.data.len() as i64,
                }),
            )
            .await;
        }
        let cleanup_before = fs::read(fixture.0.join("cleanup"))
            .unwrap_or_default()
            .len();
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(child.id().unwrap() as i32).unwrap(),
            rustix::process::Signal::TERM,
        )
        .unwrap();
        timeout(Duration::from_secs(8), async {
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Message::Close(_)) {
                    let _ = socket.flush().await;
                    break;
                }
            }
        })
        .await
        .unwrap();
        let result = finish(child).await;
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!String::from_utf8_lossy(&result.stderr).contains(TOKEN));
        if std::env::var_os("HMUX_RUST_HOME_PRODUCTION").is_some() {
            let path = fixture.0.join("service.log");
            let log = fs::read_to_string(&path).unwrap();
            assert!(log.contains("Starting Home connector"));
            assert!(!log.contains(TOKEN));
            assert!(log.len() <= 1 << 20);
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(fs::read(fixture.0.join("cleanup")).unwrap().len() > cleanup_before);
        assert!(committed.is_file());
        assert!(fs::read_dir(fixture.staging_root())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".incoming-")));
        drop(ConnectorLock::acquire(&fixture.0.join("state")).unwrap());
    }
    fixture.file("connector.token", "secret-MUST-NOT-LEAK", 0o600);
    let result = finish(with_child_spawn(|| fixture.command(address).spawn()).unwrap()).await;
    assert!(!result.status.success());
    assert!(!String::from_utf8_lossy(&result.stderr).contains("MUST-NOT-LEAK"));
    assert!(timeout(Duration::from_millis(100), listener.accept())
        .await
        .is_err());
}
