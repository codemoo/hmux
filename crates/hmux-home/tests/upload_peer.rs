use bytes::Bytes;
use sha2::{Digest, Sha256};
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
use futures_util::{SinkExt, StreamExt};
use hmux_core::command::CommandRunner;
use hmux_home::{
    catalog::TmuxCatalogReader,
    config::HomeConfig,
    filestage::Store,
    peer::{run_connected_with_uploads, Error},
};
use hmux_protocol::{
    legacy,
    protobuf::{self as pb, types as p, Direction, Negotiated},
    transport, wire,
};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::Path,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{io::DuplexStream, time::timeout};
use tokio_tungstenite::{
    tungstenite::{protocol::Role, Message},
    WebSocketStream,
};
use tokio_util::sync::CancellationToken;

static NEXT: AtomicU64 = AtomicU64::new(0);
const SEP: &str = "|:hmux-sep-v1:|";
struct Fixture {
    dir: PathBuf,
    command: PathBuf,
    inventory: PathBuf,
}
impl Fixture {
    fn new(script: &str) -> Self {
        let dir = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-home-upload-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let command = dir.join("fake-tmux");
        fs::write(&command, script).unwrap();
        fs::set_permissions(&command, fs::Permissions::from_mode(0o700)).unwrap();
        let inventory = dir.join("inventory.toml");
        let fixture = Self {
            dir,
            command,
            inventory,
        };
        fs::write(fixture.dir.join("identity"), "1700000000\n").unwrap();
        fixture.inventory("schema_version = 1\nrevision = \"synthetic-v1\"\n[[profiles]]\nid = \"shell\"\nlabel = \"Shell\"\ndefault_directory = \"~\"\ncommand = [\"sh\"]\n");
        fixture
    }
    fn inventory(&self, data: &str) {
        fs::write(&self.inventory, data).unwrap();
        fs::set_permissions(&self.inventory, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn config(&self) -> HomeConfig {
        HomeConfig {
            schema_version: 1,
            role: "home".into(),
            inventory_path: self.inventory.clone(),
            state_dir: self.dir.clone(),
        }
    }
    fn reader(&self) -> TmuxCatalogReader {
        TmuxCatalogReader::new(self.command.clone(), None, Duration::from_secs(3)).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn tmux_script() -> String {
    format!("#!/bin/sh\ncase \"$1\" in\n list-sessions) printf '%s\\n' '$7{SEP}agent{SEP}1700000000{SEP}1700000200{SEP}0{SEP}1{SEP}{SEP}' ;;\n list-windows) printf '%s\\n' '$7{SEP}main{SEP}1{SEP}/synthetic/work{SEP}zsh{SEP}80{SEP}24{SEP}123' ;;\n display-message) cat \"${{0%/*}}/identity\" ;;\nesac\n")
}
async fn connected(
    protocol: Negotiated,
    capacity: usize,
) -> (transport::Connection, WebSocketStream<DuplexStream>) {
    let (left, right) = tokio::io::duplex(capacity);
    let home =
        WebSocketStream::from_raw_socket(left, Role::Client, Some(transport::socket_config()))
            .await;
    let gateway =
        WebSocketStream::from_raw_socket(right, Role::Server, Some(transport::socket_config()))
            .await;
    (
        transport::start(home, protocol, Direction::ToHome).unwrap(),
        gateway,
    )
}
async fn send(
    gateway: &mut WebSocketStream<DuplexStream>,
    protocol: Negotiated,
    body: p::envelope::Body,
) {
    let envelope = p::Envelope {
        version: pb::VERSION,
        body: Some(body),
    };
    let frame = match protocol {
        Negotiated::JsonV1 => Message::Text(
            legacy::to_json(envelope, Direction::ToHome)
                .unwrap()
                .encode()
                .unwrap()
                .try_into()
                .unwrap(),
        ),
        Negotiated::ProtobufV2 => {
            Message::Binary(pb::encode(&envelope, Direction::ToHome).unwrap())
        }
    };
    gateway.send(frame).await.unwrap();
}
async fn receive(
    gateway: &mut WebSocketStream<DuplexStream>,
    protocol: Negotiated,
) -> p::envelope::Body {
    let frame = timeout(Duration::from_secs(8), gateway.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let envelope = match protocol {
        Negotiated::JsonV1 => {
            let message = wire::Message::decode(&frame.into_data()).unwrap();
            let operation = p::Operation::Profiles;
            legacy::from_json_with_context(
                message,
                Direction::ToGateway,
                Some(hmux_protocol::actions::ResponseContext::Operation(
                    operation,
                )),
            )
        }
        .unwrap(),
        Negotiated::ProtobufV2 => pb::decode(frame.into_data(), Direction::ToGateway).unwrap(),
    };
    envelope.body.unwrap()
}

fn unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
fn root(f: &Fixture) -> PathBuf {
    f.dir.join("hmux/staged-files-v1")
}
fn header(id: u32, sizes: &[usize]) -> p::UploadHeader {
    p::UploadHeader {
        protocol_version: 1,
        request_id: format!("{id:032x}"),
        session: Some(p::Session {
            id: "$7".into(),
            created_at: 1_700_000_000,
        }),
        file_count: sizes.len() as u32,
        total_bytes: sizes.iter().sum::<usize>() as i64,
        files: sizes
            .iter()
            .enumerate()
            .map(|(i, &size)| p::FileHeader {
                index: i as u32,
                size: size as i64,
                extension: if i == 0 { "png".into() } else { String::new() },
            })
            .collect(),
    }
}
async fn start(g: &mut WebSocketStream<DuplexStream>, protocol: Negotiated, h: &p::UploadHeader) {
    send(
        g,
        protocol,
        p::envelope::Body::UploadStart(p::UploadStart {
            id: h.request_id.clone(),
            header: Some(h.clone()),
        }),
    )
    .await;
}
async fn data(
    g: &mut WebSocketStream<DuplexStream>,
    protocol: Negotiated,
    h: &p::UploadHeader,
    data: &[u8],
) {
    send(
        g,
        protocol,
        p::envelope::Body::UploadData(p::Data {
            id: h.request_id.clone(),
            data: Bytes::copy_from_slice(data),
        }),
    )
    .await;
}
async fn finish(g: &mut WebSocketStream<DuplexStream>, protocol: Negotiated, h: &p::UploadHeader) {
    send(
        g,
        protocol,
        p::envelope::Body::UploadFinish(p::Reference {
            id: h.request_id.clone(),
        }),
    )
    .await;
}
async fn event(g: &mut WebSocketStream<DuplexStream>, protocol: Negotiated) -> p::envelope::Body {
    loop {
        match receive(g, protocol).await {
            p::envelope::Body::Catalog(_) => continue,
            body => return body,
        }
    }
}
async fn boot(
    f: &Fixture,
    protocol: Negotiated,
) -> (
    Arc<Store>,
    CancellationToken,
    tokio::task::JoinHandle<Result<(), Error>>,
    WebSocketStream<DuplexStream>,
) {
    let store = Arc::new(Store::open(root(f)).unwrap());
    let (connection, mut gateway) = connected(protocol, 64 * 1024).await;
    let stop = CancellationToken::new();
    let owner = tokio::spawn(run_connected_with_uploads(
        connection,
        f.config(),
        f.reader(),
        CommandRunner::new(2).unwrap(),
        Some(store.clone()),
        stop.clone(),
    ));
    assert!(
        matches!(event(&mut gateway, protocol).await, p::envelope::Body::Hello(h) if h.capabilities.contains(&"web-upload-v1".to_owned()))
    );
    (store, stop, owner, gateway)
}
fn assert_error(body: p::envelope::Body, h: &p::UploadHeader) {
    assert!(
        matches!(body, p::envelope::Body::UploadError(r) if r.id == h.request_id && r.error == "Home upload unavailable" && r.result.is_none())
    );
}
fn assert_empty(path: &Path) {
    let entries: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(entries.len() <= 1 && entries.iter().all(|name| name == ".lock"));
}
async fn close(stop: CancellationToken, owner: tokio::task::JoinHandle<Result<(), Error>>) {
    stop.cancel();
    assert_eq!(
        timeout(Duration::from_secs(5), owner)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
}

#[tokio::test]
async fn both_codecs_stream_binary_files_commit_hashes_and_expire_three_hours_after_completion() {
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let fixture = Fixture::new(&tmux_script());
        let (store, stop, owner, mut gateway) = boot(&fixture, protocol).await;
        let h = header(1, &[200_001, 200_003, 12]);
        let content: Vec<u8> = (0..h.total_bytes).map(|i| (i % 256) as u8).collect();
        start(&mut gateway, protocol, &h).await;
        assert!(
            matches!(event(&mut gateway, protocol).await, p::envelope::Body::UploadReady(r) if r.id == h.request_id)
        );
        let mut received = 0;
        for chunk in content.chunks(wire::MAX_UPLOAD_CHUNK) {
            data(&mut gateway, protocol, &h, chunk).await;
            received += chunk.len() as i64;
            assert!(
                matches!(event(&mut gateway, protocol).await, p::envelope::Body::UploadAck(r) if r.id == h.request_id && r.received == received)
            );
        }
        let before = unix();
        finish(&mut gateway, protocol, &h).await;
        let p::envelope::Body::UploadComplete(reply) = event(&mut gateway, protocol).await else {
            panic!("complete expected");
        };
        assert_eq!(reply.id, h.request_id);
        assert!(reply.error.is_empty());
        let manifest: serde_json::Value =
            serde_json::from_slice(&hmux_protocol::actions::response_payload(&reply).unwrap())
                .unwrap();
        let expiry = manifest["expires_at_unix"].as_i64().unwrap();
        assert!((before + 10800..=unix() + 10800).contains(&expiry));
        assert_eq!(manifest["session"]["id"], "$7");
        assert_eq!(manifest["session"]["created_at"], 1_700_000_000);
        let mut offset = 0;
        let mut paths = Vec::new();
        for (i, f) in manifest["files"].as_array().unwrap().iter().enumerate() {
            let path = PathBuf::from(f["path"].as_str().unwrap());
            let length = h.files[i].size as usize;
            assert!(path.starts_with(root(&fixture)));
            assert_eq!(fs::read(&path).unwrap(), content[offset..offset + length]);
            assert_eq!(
                f["sha256"],
                format!("{:x}", Sha256::digest(&content[offset..offset + length]))
            );
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            offset += length;
            paths.push(path);
        }
        close(stop, owner).await;
        store
            .sweep(
                expiry - 1,
                CancellationToken::new(),
                std::time::Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
        assert!(paths.iter().all(|p| p.exists()));
        store
            .sweep(
                expiry,
                CancellationToken::new(),
                std::time::Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
        assert_empty(&root(&fixture));
    }
}

#[tokio::test]
async fn identity_replacement_incomplete_extra_and_cancelled_uploads_leave_no_files_or_link_damage()
{
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let fixture = Fixture::new(&tmux_script());
        let (_, stop, owner, mut gateway) = boot(&fixture, protocol).await;
        for case in 0..5 {
            let h = header(10 + case, &[2]);
            fs::write(
                fixture.dir.join("identity"),
                if case == 0 {
                    "1700000001\n"
                } else {
                    "1700000000\n"
                },
            )
            .unwrap();
            start(&mut gateway, protocol, &h).await;
            if case != 0 {
                assert!(matches!(
                    event(&mut gateway, protocol).await,
                    p::envelope::Body::UploadReady(_)
                ));
                if case == 4 {
                    send(
                        &mut gateway,
                        protocol,
                        p::envelope::Body::UploadCancel(p::Reference {
                            id: h.request_id.clone(),
                        }),
                    )
                    .await;
                } else {
                    let bytes = match case {
                        1 => &b"ok"[..],
                        2 => &b"o"[..],
                        _ => &b"bad"[..],
                    };
                    data(&mut gateway, protocol, &h, bytes).await;
                    if case != 3 {
                        assert!(matches!(
                            event(&mut gateway, protocol).await,
                            p::envelope::Body::UploadAck(_)
                        ));
                        if case == 1 {
                            fs::write(fixture.dir.join("identity"), "1700000001\n").unwrap();
                        }
                        finish(&mut gateway, protocol, &h).await;
                    }
                }
            }
            assert_error(event(&mut gateway, protocol).await, &h);
            assert_empty(&root(&fixture));
            data(&mut gateway, protocol, &h, b"late").await;
            send(
                &mut gateway,
                protocol,
                p::envelope::Body::Request(Box::new(p::Request {
                    id: format!("profiles-{case}"),
                    operation: p::Operation::Profiles as i32,
                    session: None,
                    payload: Some(p::request::Payload::Empty(p::Empty {})),
                })),
            )
            .await;
            assert!(
                matches!(event(&mut gateway, protocol).await, p::envelope::Body::Response(r) if r.error.is_empty())
            );
        }
        close(stop, owner).await;
    }
}

#[tokio::test]
async fn two_upload_admission_and_one_frame_queue_keep_catalog_and_profiles_responsive() {
    use rustix::fs::{flock, FlockOperation};
    let _serial = SERIAL.lock().await;
    let fixture = Fixture::new(&tmux_script());
    let protocol = Negotiated::ProtobufV2;
    let (_, stop, owner, mut gateway) = boot(&fixture, protocol).await;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root(&fixture).join(".lock"))
        .unwrap();
    fs::set_permissions(
        root(&fixture).join(".lock"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    flock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap();
    let first = header(21, &[4]);
    let second = header(22, &[4]);
    let third = header(23, &[4]);
    start(&mut gateway, protocol, &first).await;
    start(&mut gateway, protocol, &second).await;
    start(&mut gateway, protocol, &third).await;
    assert_error(event(&mut gateway, protocol).await, &third);
    send(
        &mut gateway,
        protocol,
        p::envelope::Body::Request(Box::new(p::Request {
            id: "while-disk-blocked".into(),
            operation: p::Operation::Profiles as i32,
            session: None,
            payload: Some(p::request::Payload::Empty(p::Empty {})),
        })),
    )
    .await;
    assert!(
        matches!(timeout(Duration::from_secs(2), event(&mut gateway, protocol)).await.unwrap(), p::envelope::Body::Response(r) if r.error.is_empty())
    );
    // No upload is ready while flock is occupied. The second input exceeds the
    // sole queue slot and cancels only the first upload, including its worker.
    data(&mut gateway, protocol, &first, b"a").await;
    data(&mut gateway, protocol, &first, b"b").await;
    assert_error(event(&mut gateway, protocol).await, &first);
    send(
        &mut gateway,
        protocol,
        p::envelope::Body::UploadCancel(p::Reference {
            id: second.request_id.clone(),
        }),
    )
    .await;
    assert_error(event(&mut gateway, protocol).await, &second);
    drop(lock);
    close(stop, owner).await;
    assert_empty(&root(&fixture));
}

#[tokio::test]
async fn disconnect_and_caller_abort_join_owned_staging_cleanup() {
    let _serial = SERIAL.lock().await;
    for abort in [false, true] {
        let fixture = Fixture::new(&tmux_script());
        let protocol = Negotiated::ProtobufV2;
        let (store, stop, owner, mut gateway) = boot(&fixture, protocol).await;
        let h = header(30, &[10]);
        start(&mut gateway, protocol, &h).await;
        assert!(matches!(
            event(&mut gateway, protocol).await,
            p::envelope::Body::UploadReady(_)
        ));
        data(&mut gateway, protocol, &h, b"partial").await;
        assert!(matches!(
            event(&mut gateway, protocol).await,
            p::envelope::Body::UploadAck(_)
        ));
        if abort {
            owner.abort();
            assert!(owner.await.unwrap_err().is_cancelled());
            timeout(Duration::from_secs(5), async {
                while fs::read_dir(root(&fixture)).unwrap().count() != 1 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
        } else {
            close(stop, owner).await;
        }
        assert_empty(&root(&fixture));
        // Cleanup released the actual flock, not only the async request map.
        let fresh = store
            .begin(
                header(31, &[1]),
                unix(),
                CancellationToken::new(),
                std::time::Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
        drop(fresh);
        assert_empty(&root(&fixture));
    }
}
