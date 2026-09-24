use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use hmux_core::command::CommandRunner;
use hmux_home::{catalog::TmuxCatalogReader, config::HomeConfig, peer};
use hmux_protocol::{
    flow, legacy,
    protobuf::{self as pb, types as p, Direction, Negotiated},
    transport,
};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{io::DuplexStream, sync::Mutex, time::timeout};
use tokio_tungstenite::{
    tungstenite::{protocol::Role, Message},
    WebSocketStream,
};
use tokio_util::sync::CancellationToken;

static NEXT: AtomicU64 = AtomicU64::new(0);
static SERIAL: Mutex<()> = Mutex::const_new(());
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-peer-pty-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::write(root.join("inventory.toml"),"schema_version=1\nrevision='test'\n[[profiles]]\nid='shell'\nlabel='Shell'\ndefault_directory='~'\ncommand=['sh']\n").unwrap();
        fs::set_permissions(
            root.join("inventory.toml"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let script = r#"#!/usr/bin/python3
import os,sys,pathlib,time,tty
root=pathlib.Path(__file__).parent
args=sys.argv[1:]
cmd=args[0]
mode=(root/'mode').read_text() if (root/'mode').exists() else ''
sep='|:hmux-sep-v1:|'
if cmd=='list-sessions' and (root/'catalog-fail').exists():
 (root/'catalog-failed').touch()
 sys.exit(7)
if cmd=='list-sessions': print(sep.join(['$7',(root/'catalog-name').read_text() if (root/'catalog-name').exists() else 'synthetic','1700000000','1700000000','0','1','','']))
elif cmd=='list-windows': print(sep.join(['$7','main','1','/synthetic','sh','80','24','123']))
elif cmd=='display-message': print('1700000000')
elif cmd=='new-session':
 name=args[args.index('-s')+1];nonce=args[args.index('-e')+1].split('=',1)[1]
 (root/(name+'.owner')).write_text(nonce)
 (root/'created').touch()
 if mode=='setup-wait':time.sleep(30)
elif cmd=='set-hook':pass
elif cmd=='if-shell':
 if mode=='cleanup-fail':sys.exit(7)
 path=root/(args[3]+'.owner')
 if path.exists() and path.read_text() in args[4]:path.unlink()
 (root/'cleaned').touch()
elif cmd=='list-clients': print('invalid synthetic refresh target')
elif cmd=='attach-session':
 name=args[args.index('-t')+1]
 (root/(name+'.pid')).write_text(str(os.getpid()))
 tty.setraw(0)
 os.write(1,b'READY\n')
 if mode=='input-wait':time.sleep(30)
 pending=b''
 while True:
  data=os.read(0,32768)
  if not data:break
  pending+=data
  while b'\n' in pending:
   line,pending=pending.split(b'\n',1)
   if line==b'BULK':
    data=b'x'*(40*16384)
    while data:data=data[os.write(1,data):]
   elif line==b'SIZE':
    size=os.get_terminal_size(0);os.write(1,('SIZE:%s:%s\n'%(size.columns,size.lines)).encode())
   elif line==b'EXIT':os.write(1,b'BYE\n');sys.exit(0)
   else:os.write(1,b'ECHO:'+line+b'\n')
else:sys.exit(2)
"#;
        fs::write(root.join("tmux"), script).unwrap();
        fs::set_permissions(root.join("tmux"), fs::Permissions::from_mode(0o700)).unwrap();
        Self(root)
    }
    fn mode(&self, value: &str) {
        fs::write(self.0.join("mode"), value).unwrap();
    }
    async fn wait(&self, name: &str) {
        timeout(Duration::from_secs(4), async {
            while !self.0.join(name).exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    fn owners(&self) -> usize {
        fs::read_dir(&self.0)
            .unwrap()
            .flatten()
            .filter(|p| p.path().extension().is_some_and(|ext| ext == "owner"))
            .count()
    }
    fn reaped(&self) {
        for file in fs::read_dir(&self.0).unwrap().flatten() {
            if file.path().extension().is_some_and(|v| v == "pid") {
                let pid: i32 = fs::read_to_string(file.path()).unwrap().parse().unwrap();
                assert!(rustix::process::test_kill_process(
                    rustix::process::Pid::from_raw(pid).unwrap()
                )
                .is_err());
            }
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Peer {
    gateway: WebSocketStream<DuplexStream>,
    protocol: Negotiated,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<Result<(), peer::Error>>,
}
impl Peer {
    async fn start(f: &Fixture, protocol: Negotiated) -> Self {
        Self::start_reported(f, protocol, None).await
    }
    async fn start_reported(
        f: &Fixture,
        protocol: Negotiated,
        reporter: Option<hmux_home::observation::Reporter>,
    ) -> Self {
        let (a, b) = tokio::io::duplex(128 * 1024);
        let home =
            WebSocketStream::from_raw_socket(a, Role::Client, Some(transport::socket_config()))
                .await;
        let gateway =
            WebSocketStream::from_raw_socket(b, Role::Server, Some(transport::socket_config()))
                .await;
        let stop = CancellationToken::new();
        let config = HomeConfig {
            schema_version: 1,
            role: "home".into(),
            inventory_path: f.0.join("inventory.toml"),
            state_dir: f.0.clone(),
        };
        let catalog =
            TmuxCatalogReader::new(f.0.join("tmux"), None, Duration::from_secs(3)).unwrap();
        let task = tokio::spawn(peer::run_connected_with_services(
            transport::start(home, protocol, Direction::ToHome).unwrap(),
            config,
            catalog,
            CommandRunner::new(8).unwrap(),
            peer::Services {
                reporter,
                ..peer::Services::default()
            },
            stop.clone(),
        ));
        let mut value = Self {
            gateway,
            protocol,
            stop,
            task,
        };
        assert!(
            matches!(value.receive().await,p::envelope::Body::Hello(h) if h.capabilities==[flow::CAPABILITY])
        );
        assert!(matches!(value.frame().await, p::envelope::Body::Catalog(_)));
        value
    }
    async fn send(&mut self, body: p::envelope::Body) {
        let env = p::Envelope {
            version: pb::VERSION,
            body: Some(body),
        };
        let message = match self.protocol {
            Negotiated::ProtobufV2 => Message::Binary(pb::encode(&env, Direction::ToHome).unwrap()),
            Negotiated::JsonV1 => Message::Text(
                String::from_utf8(
                    legacy::to_json(env, Direction::ToHome)
                        .unwrap()
                        .encode()
                        .unwrap(),
                )
                .unwrap()
                .into(),
            ),
        };
        self.gateway.send(message).await.unwrap();
    }
    async fn frame(&mut self) -> p::envelope::Body {
        timeout(Duration::from_secs(5), async {
            loop {
                let frame = self.gateway.next().await.unwrap().unwrap();
                let envelope = match frame {
                    Message::Binary(raw) => pb::decode(raw, Direction::ToGateway).unwrap(),
                    Message::Text(raw) => {
                        let message = hmux_protocol::wire::Message::decode(raw.as_bytes()).unwrap();
                        let context = if message.id == "profiles" {
                            hmux_protocol::actions::ResponseContext::Operation(
                                p::Operation::Profiles,
                            )
                        } else {
                            hmux_protocol::actions::ResponseContext::TerminalOpen
                        };
                        legacy::from_json_with_context(message, Direction::ToGateway, Some(context))
                    }
                    .unwrap(),
                    Message::Ping(raw) => {
                        self.gateway.send(Message::Pong(raw)).await.unwrap();
                        continue;
                    }
                    other => panic!("unexpected frame: {other:?}"),
                };
                let body = envelope.body.unwrap();
                return body;
            }
        })
        .await
        .unwrap()
    }
    async fn receive(&mut self) -> p::envelope::Body {
        loop {
            let body = self.frame().await;
            if !matches!(body, p::envelope::Body::Catalog(_)) {
                return body;
            }
        }
    }
    async fn open(&mut self, id: &str, controlled: bool) {
        self.send(p::envelope::Body::TerminalOpen(p::TerminalOpen {
            id: id.into(),
            session: Some(p::Session {
                id: "$7".into(),
                created_at: 1700000000,
            }),
            cols: 80,
            rows: 24,
            capabilities: if controlled {
                vec![flow::CAPABILITY.into()]
            } else {
                vec![]
            },
        }))
        .await;
        assert!(
            matches!(self.receive().await,p::envelope::Body::Response(r) if r.id==id && r.error.is_empty() && matches!(r.result, Some(p::response::Result::Ok(_))))
        );
        self.text(id, b"READY\n", controlled).await;
    }
    async fn input(&mut self, id: &str, data: &[u8]) {
        self.send(p::envelope::Body::TerminalInput(p::Data {
            id: id.into(),
            data: Bytes::copy_from_slice(data),
        }))
        .await;
    }
    async fn ack(&mut self, id: &str, n: usize) {
        self.send(p::envelope::Body::OutputAck(p::Ack {
            id: id.into(),
            received: n as i64,
        }))
        .await;
    }
    async fn text(&mut self, id: &str, marker: &[u8], ack: bool) {
        let mut data = Vec::new();
        loop {
            let p::envelope::Body::TerminalOutput(part) = self.receive().await else {
                panic!("output expected")
            };
            assert_eq!(part.id, id);
            assert!(part.data.len() <= flow::CHUNK);
            if ack {
                self.ack(id, part.data.len()).await;
            }
            data.extend_from_slice(&part.data);
            if data.windows(marker.len()).any(|v| v == marker) {
                return;
            }
            assert!(data.len() < 1024 * 1024);
        }
    }
    async fn profiles(&mut self) {
        self.send(p::envelope::Body::Request(Box::new(p::Request {
            id: "profiles".into(),
            operation: p::Operation::Profiles as i32,
            session: None,
            payload: Some(p::request::Payload::Empty(p::Empty {})),
        })))
        .await;
        assert!(
            matches!(self.receive().await,p::envelope::Body::Response(r) if r.id=="profiles" && r.error.is_empty())
        );
    }
    async fn close(&mut self, id: &str) {
        self.send(p::envelope::Body::Close(p::Reference { id: id.into() }))
            .await;
        assert!(matches!(self.receive().await,p::envelope::Body::TerminalExit(r) if r.id==id));
    }
    async fn shutdown(self) {
        self.stop.cancel();
        assert_eq!(
            timeout(Duration::from_secs(5), self.task)
                .await
                .unwrap()
                .unwrap(),
            Ok(())
        );
    }
}

#[tokio::test]
async fn cleanup_failure_is_reported_without_disconnecting_other_views() {
    // Quarantine intentionally lasts until process exit. Keep this fault in a
    // child so it cannot consume the capacity tested by other cases.
    if std::env::var_os("HMUX_TEST_VIEW_CLEANUP_CHILD").is_none() {
        let output = timeout(
            Duration::from_secs(20),
            tokio::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "cleanup_failure_is_reported_without_disconnecting_other_views",
                    "--nocapture",
                ])
                .env("HMUX_TEST_VIEW_CLEANUP_CHILD", "1")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let f = Fixture::new();
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let reported = events.clone();
        let reporter = std::sync::Arc::new(move |event: hmux_home::observation::Event| {
            reported.lock().unwrap().push(event);
        });
        let mut peer = Peer::start_reported(&f, protocol, Some(reporter)).await;
        peer.open("failed", true).await;
        peer.open("survivor", true).await;
        f.mode("cleanup-fail");
        peer.send(p::envelope::Body::Close(p::Reference {
            id: "failed".into(),
        }))
        .await;
        assert!(
            matches!(peer.receive().await, p::envelope::Body::TerminalExit(r) if r.id == "failed" && r.error == "view-cleanup-failed")
        );
        let diagnostics: Vec<_> = events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.stage == hmux_home::observation::Stage::ViewCleanup)
            .map(ToString::to_string)
            .collect();
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0]
            .starts_with("home stage=view-cleanup operation=none reason=quarantined duration_ms="));
        assert_eq!(f.owners(), 2); // The uncertain view is never blindly removed.
        peer.profiles().await;
        peer.input("survivor", b"ALIVE\n").await;
        peer.text("survivor", b"ECHO:ALIVE\n", true).await;
        f.mode("");
        peer.close("survivor").await;
        peer.open("replacement", true).await;
        peer.close("replacement").await;
        peer.shutdown().await;
        assert_eq!(f.owners(), 1);
        f.reaped();
    }
}

#[tokio::test]
async fn transient_catalog_failure_keeps_live_view_and_peer_until_recovery() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let reported = events.clone();
    let reporter = std::sync::Arc::new(move |event: hmux_home::observation::Event| {
        reported.lock().unwrap().push(event.reason);
    });
    let mut peer = Peer::start_reported(&f, Negotiated::ProtobufV2, Some(reporter)).await;
    peer.open("view", true).await;
    fs::write(f.0.join("catalog-fail"), b"").unwrap();
    timeout(Duration::from_secs(8), async {
        while !f.0.join("catalog-failed").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(events
        .lock()
        .unwrap()
        .contains(&hmux_home::observation::Reason::Published));
    peer.profiles().await;
    peer.input("view", b"DURING\n").await;
    peer.text("view", b"ECHO:DURING\n", true).await;
    fs::remove_file(f.0.join("catalog-fail")).unwrap();
    fs::write(f.0.join("catalog-name"), b"recovered").unwrap();
    timeout(Duration::from_secs(12), async {
        while !events
            .lock()
            .unwrap()
            .contains(&hmux_home::observation::Reason::Recovered)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let p::envelope::Body::Catalog(snapshot) = timeout(Duration::from_secs(12), peer.frame())
        .await
        .unwrap()
    else {
        panic!("recovered catalog expected")
    };
    let catalog = hmux_protocol::snapshots::catalog_from_proto(*snapshot).unwrap();
    assert_eq!(catalog.sessions.unwrap()[0].name, "recovered");
    peer.input("view", b"AFTER\n").await;
    peer.text("view", b"ECHO:AFTER\n", true).await;
    peer.close("view").await;
    peer.shutdown().await;
}

#[tokio::test]
async fn both_codecs_terminal_io_resize_refresh_failure_and_close_are_isolated() {
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let f = Fixture::new();
        let mut peer = Peer::start(&f, protocol).await;
        peer.open("view", true).await;
        peer.input("view", "한글\n".as_bytes()).await;
        peer.text("view", "ECHO:한글\n".as_bytes(), true).await;
        peer.input("view", &[0xff, 0x00, 0x80, b'\n']).await;
        peer.text(
            "view",
            &[b'E', b'C', b'H', b'O', b':', 0xff, 0x00, 0x80, b'\n'],
            true,
        )
        .await;
        peer.input("view", b"BULK\n").await;
        let mut total = 0;
        while total < 40 * flow::CHUNK {
            let p::envelope::Body::TerminalOutput(data) = peer.receive().await else {
                panic!("bulk data expected")
            };
            assert_eq!(data.id, "view");
            assert!(data.data.iter().all(|byte| *byte == b'x'));
            total += data.data.len();
            peer.ack("view", data.data.len()).await;
        }
        assert_eq!(total, 40 * flow::CHUNK);
        peer.send(p::envelope::Body::Resize(p::Resize {
            id: "view".into(),
            cols: 110,
            rows: 45,
        }))
        .await;
        peer.input("view", b"SIZE\n").await;
        peer.text("view", b"SIZE:110:45\n", true).await;
        peer.send(p::envelope::Body::Refresh(p::Reference {
            id: "view".into(),
        }))
        .await;
        assert!(
            matches!(peer.receive().await,p::envelope::Body::RefreshResult(r) if r.error=="Terminal refresh unavailable")
        );
        peer.input("view", b"alive\n").await;
        peer.text("view", b"ECHO:alive\n", true).await;
        peer.close("view").await;
        assert_eq!(f.owners(), 0);
        f.reaped();
        peer.profiles().await;
        peer.open("legacy", false).await;
        peer.input("legacy", b"EXIT\n").await;
        peer.text("legacy", b"BYE\n", false).await;
        assert!(
            matches!(peer.receive().await,p::envelope::Body::TerminalExit(r) if r.id=="legacy")
        );
        peer.shutdown().await;
        assert_eq!(f.owners(), 0);
        f.reaped();
    }
}

#[tokio::test]
async fn full_output_credit_does_not_block_other_requests_or_views() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let mut peer = Peer::start(&f, Negotiated::ProtobufV2).await;
    peer.open("slow", true).await;
    peer.input("slow", b"BULK\n").await;
    let mut sizes = Vec::new();
    for _ in 0..flow::FRAMES {
        let p::envelope::Body::TerminalOutput(data) = peer.receive().await else {
            panic!("bulk output expected")
        };
        assert_eq!(data.id, "slow");
        sizes.push(data.data.len());
    }
    peer.profiles().await;
    peer.open("fast", true).await;
    peer.ack("slow", sizes[0]).await;
    assert!(matches!(peer.receive().await,p::envelope::Body::TerminalOutput(d) if d.id=="slow"));
    peer.ack("slow", sizes[1] + 1).await;
    assert!(matches!(peer.receive().await,p::envelope::Body::TerminalExit(r) if r.id=="slow"));
    peer.input("fast", b"alive\n").await;
    peer.text("fast", b"ECHO:alive\n", true).await;
    peer.close("fast").await;
    peer.profiles().await;
    peer.shutdown().await;
    assert_eq!(f.owners(), 0);
    f.reaped();
}

#[tokio::test]
async fn canceled_setup_and_aborted_peer_keep_owned_cleanup() {
    let _serial = SERIAL.lock().await;
    for abort in [false, true] {
        let f = Fixture::new();
        f.mode("setup-wait");
        let mut peer = Peer::start(&f, Negotiated::ProtobufV2).await;
        peer.send(p::envelope::Body::TerminalOpen(p::TerminalOpen {
            id: "pending".into(),
            session: Some(p::Session {
                id: "$7".into(),
                created_at: 1700000000,
            }),
            cols: 80,
            rows: 24,
            capabilities: vec![],
        }))
        .await;
        f.wait("created").await;
        if abort {
            peer.task.abort();
            let _ = peer.task.await;
            f.wait("cleaned").await;
        } else {
            peer.send(p::envelope::Body::Cancel(p::Reference {
                id: "pending".into(),
            }))
            .await;
            f.wait("cleaned").await;
            peer.profiles().await;
            peer.shutdown().await;
        }
        assert_eq!(f.owners(), 0);
    }
}

#[tokio::test]
async fn blocked_input_queue_overflow_closes_only_its_view() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    f.mode("input-wait");
    let mut peer = Peer::start(&f, Negotiated::ProtobufV2).await;
    peer.open("blocked", true).await;
    for _ in 0..40 {
        peer.input("blocked", &vec![b'x'; 32768]).await;
    }
    assert!(matches!(peer.receive().await,p::envelope::Body::TerminalExit(r) if r.id=="blocked"));
    peer.profiles().await;
    peer.shutdown().await;
    assert_eq!(f.owners(), 0);
    f.reaped();
}

#[tokio::test]
async fn saturated_terminal_startup_does_not_starve_catalog() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    f.mode("setup-wait");
    let mut peer = Peer::start(&f, Negotiated::ProtobufV2).await;
    for index in 0..8 {
        peer.send(p::envelope::Body::TerminalOpen(p::TerminalOpen {
            id: format!("pending-{index}"),
            session: Some(p::Session {
                id: "$7".into(),
                created_at: 1700000000,
            }),
            cols: 80,
            rows: 24,
            capabilities: vec![],
        }))
        .await;
    }
    timeout(Duration::from_secs(4), async {
        while f.owners() != 8 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    fs::write(f.0.join("catalog-name"), "changed-under-load").unwrap();
    tokio::time::sleep(Duration::from_secs(6)).await;
    // The next catalog cycle must be independent of eight blocked tmux creates.
    let p::envelope::Body::Catalog(snapshot) = peer.frame().await else {
        panic!("changed catalog expected");
    };
    let catalog =
        serde_json::to_value(hmux_protocol::snapshots::catalog_from_proto(*snapshot).unwrap())
            .unwrap();
    assert_eq!(catalog["sessions"][0]["name"], "changed-under-load");
    for index in 0..8 {
        peer.send(p::envelope::Body::Cancel(p::Reference {
            id: format!("pending-{index}"),
        }))
        .await;
    }
    timeout(Duration::from_secs(4), async {
        while f.owners() != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // The fake tmux removes its owner marker before its cleanup process exits.
    // Admission correctly remains held through reap/join; observe the protocol
    // until it is reclaimed instead of equating marker removal with completion.
    timeout(Duration::from_secs(4), async {
        loop {
            peer.send(p::envelope::Body::Request(Box::new(p::Request {
                id: "profiles".into(),
                operation: p::Operation::Profiles as i32,
                session: None,
                payload: Some(p::request::Payload::Empty(p::Empty {})),
            })))
            .await;
            let p::envelope::Body::Response(response) = peer.receive().await else {
                panic!("profiles response expected");
            };
            assert_eq!(response.id, "profiles");
            if response.error.is_empty() {
                break;
            }
            assert_eq!(response.error, "Home is busy");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    peer.shutdown().await;
}

#[tokio::test]
async fn abort_with_active_pty_still_reaps_and_cleans_owned_view() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let mut peer = Peer::start(&f, Negotiated::ProtobufV2).await;
    peer.open("active", true).await;
    peer.task.abort();
    let _ = peer.task.await;
    f.wait("cleaned").await;
    timeout(Duration::from_secs(4), async {
        loop {
            let alive = fs::read_dir(&f.0)
                .unwrap()
                .flatten()
                .filter(|file| file.path().extension().is_some_and(|ext| ext == "pid"))
                .any(|file| {
                    let pid: i32 = fs::read_to_string(file.path()).unwrap().parse().unwrap();
                    rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap())
                        .is_ok()
                });
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.owners(), 0);
    f.reaped();
}
