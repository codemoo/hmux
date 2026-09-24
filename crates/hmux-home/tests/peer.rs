use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use hmux_core::command::CommandRunner;
use hmux_home::{
    catalog::TmuxCatalogReader,
    config::HomeConfig,
    peer::{run_connected, Error},
};
use hmux_protocol::{
    legacy,
    protobuf::{self as pb, types as p, Direction, Negotiated},
    transport, wire,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{
    io::DuplexStream,
    time::{timeout, Instant},
};
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
        let dir = std::env::temp_dir().join(format!(
            "hmux-home-peer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let command = dir.join("fake-tmux");
        fs::write(&command, script).unwrap();
        fs::set_permissions(&command, fs::Permissions::from_mode(0o700)).unwrap();
        let inventory = dir.join("inventory.toml");
        let fixture = Self {
            dir,
            command,
            inventory,
        };
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
    format!("#!/bin/sh\ncase \"$1\" in\n list-sessions) printf '%s\\n' '$7{SEP}agent{SEP}1700000000{SEP}1700000200{SEP}0{SEP}1{SEP}{SEP}' ;;\n list-windows) printf '%s\\n' '$7{SEP}main{SEP}1{SEP}/synthetic/work{SEP}zsh{SEP}80{SEP}24{SEP}123' ;;\nesac\n")
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
        Negotiated::JsonV1 => legacy::from_json_with_context(
            wire::Message::decode(&frame.into_data()).unwrap(),
            Direction::ToGateway,
            Some(hmux_protocol::actions::ResponseContext::Operation(
                p::Operation::Profiles,
            )),
        )
        .unwrap(),
        Negotiated::ProtobufV2 => pb::decode(frame.into_data(), Direction::ToGateway).unwrap(),
    };
    envelope.body.unwrap()
}
fn request(id: &str, operation: p::Operation) -> p::envelope::Body {
    p::envelope::Body::Request(Box::new(p::Request {
        id: id.into(),
        operation: operation as i32,
        session: None,
        payload: Some(
            hmux_protocol::actions::request_from_json(
                operation,
                if operation == p::Operation::Create {
                    b"{}"
                } else {
                    b""
                },
            )
            .unwrap(),
        ),
    }))
}
fn assert_error(body: p::envelope::Body, id: &str, expected: &str) {
    let p::envelope::Body::Response(reply) = body else {
        panic!("response expected")
    };
    assert_eq!(reply.id, id);
    assert_eq!(reply.error, expected);
    assert!(reply.result.is_none());
}
fn assert_upload_error(body: p::envelope::Body, id: &str) {
    let p::envelope::Body::UploadError(reply) = body else {
        panic!("upload error expected")
    };
    assert_eq!(reply.id, id);
    assert_eq!(reply.error, "Home upload unavailable");
    assert!(reply.result.is_none());
}

#[tokio::test]
async fn both_versions_publish_catalog_profiles_and_terminal_flow_capability() {
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let fixture = Fixture::new(&tmux_script());
        let (connection, mut gateway) = connected(protocol, 64 * 1024).await;
        let stop = CancellationToken::new();
        let owner = tokio::spawn(run_connected(
            connection,
            fixture.config(),
            fixture.reader(),
            CommandRunner::new(2).unwrap(),
            stop.clone(),
        ));
        assert!(
            matches!(receive(&mut gateway, protocol).await, p::envelope::Body::Hello(p::Hello { capabilities }) if capabilities == [hmux_protocol::flow::CAPABILITY])
        );
        let p::envelope::Body::Catalog(snapshot) = receive(&mut gateway, protocol).await else {
            panic!("catalog expected")
        };
        let catalog =
            serde_json::to_value(hmux_protocol::snapshots::catalog_from_proto(*snapshot).unwrap())
                .unwrap();
        assert_eq!(catalog["protocol_version"], 1);
        assert_eq!(catalog["sessions"][0]["id"], "$7");
        assert_eq!(catalog["sessions"][0]["current_path"], "/synthetic/work");
        send(
            &mut gateway,
            protocol,
            request("profiles-1", p::Operation::Profiles),
        )
        .await;
        let p::envelope::Body::Response(reply) = receive(&mut gateway, protocol).await else {
            panic!("profiles response expected")
        };
        assert_eq!(reply.id, "profiles-1");
        assert_eq!(reply.error, "");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &hmux_protocol::actions::response_payload(&reply).unwrap()
            )
            .unwrap(),
            serde_json::json!([{"id":"shell","label":"Shell"}])
        );
        fixture.inventory("not valid inventory");
        send(
            &mut gateway,
            protocol,
            request("profiles-2", p::Operation::Profiles),
        )
        .await;
        assert_error(
            receive(&mut gateway, protocol).await,
            "profiles-2",
            "Home operation unavailable",
        );
        stop.cancel();
        assert_eq!(
            timeout(Duration::from_secs(3), owner)
                .await
                .unwrap()
                .unwrap(),
            Ok(())
        );
    }
}

#[tokio::test]
async fn unsupported_actions_upload_and_invalid_terminal_identity_fail_explicitly() {
    let fixture = Fixture::new(&tmux_script());
    let protocol = Negotiated::ProtobufV2;
    let (connection, mut gateway) = connected(protocol, 64 * 1024).await;
    let stop = CancellationToken::new();
    let owner = tokio::spawn(run_connected(
        connection,
        fixture.config(),
        fixture.reader(),
        CommandRunner::new(2).unwrap(),
        stop.clone(),
    ));
    receive(&mut gateway, protocol).await;
    receive(&mut gateway, protocol).await;
    send(
        &mut gateway,
        protocol,
        request("create", p::Operation::Create),
    )
    .await;
    assert_error(
        receive(&mut gateway, protocol).await,
        "create",
        "Home operation unavailable",
    );
    send(
        &mut gateway,
        protocol,
        p::envelope::Body::TerminalOpen(p::TerminalOpen {
            id: "open".into(),
            session: Some(p::Session {
                id: "$7".into(),
                created_at: 1_700_000_000,
            }),
            cols: 80,
            rows: 24,
            capabilities: Vec::new(),
        }),
    )
    .await;
    assert_error(
        receive(&mut gateway, protocol).await,
        "open",
        "Home operation unavailable",
    );
    send(
        &mut gateway,
        protocol,
        p::envelope::Body::UploadStart(p::UploadStart {
            id: "0123456789abcdef0123456789abcdef".into(),
            header: Some(p::UploadHeader {
                protocol_version: 1,
                request_id: "0123456789abcdef0123456789abcdef".into(),
                session: Some(p::Session {
                    id: "$7".into(),
                    created_at: 1_700_000_000,
                }),
                file_count: 1,
                total_bytes: 1,
                files: vec![p::FileHeader {
                    index: 0,
                    size: 1,
                    extension: "txt".into(),
                }],
            }),
        }),
    )
    .await;
    assert_upload_error(
        receive(&mut gateway, protocol).await,
        "0123456789abcdef0123456789abcdef",
    );
    send(
        &mut gateway,
        protocol,
        p::envelope::Body::TerminalInput(p::Data {
            id: "open".into(),
            data: Bytes::from_static(b"x"),
        }),
    )
    .await;
    // Late frames for a closed view are ignored without harming the link.
    send(
        &mut gateway,
        protocol,
        request("after-late-input", p::Operation::Profiles),
    )
    .await;
    assert!(
        matches!(receive(&mut gateway,protocol).await,p::envelope::Body::Response(v) if v.id=="after-late-input" && v.error.is_empty())
    );
    stop.cancel();
    assert_eq!(
        timeout(Duration::from_secs(3), owner)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
}

#[tokio::test]
async fn malformed_v1_operation_closes_only_the_connected_peer() {
    let fixture = Fixture::new(&tmux_script());
    let (connection, mut gateway) = connected(Negotiated::JsonV1, 64 * 1024).await;
    let owner = tokio::spawn(run_connected(
        connection,
        fixture.config(),
        fixture.reader(),
        CommandRunner::new(2).unwrap(),
        CancellationToken::new(),
    ));
    receive(&mut gateway, Negotiated::JsonV1).await;
    gateway
        .send(Message::Text(
            r#"{"type":"request","id":"x","operation":"unknown"}"#.into(),
        ))
        .await
        .unwrap();
    assert_eq!(
        timeout(Duration::from_secs(4), owner)
            .await
            .unwrap()
            .unwrap(),
        Err(Error::Protocol)
    );
}

#[tokio::test]
async fn shutdown_joins_inflight_catalog_command_and_transport() {
    let fixture = Fixture::new("#!/bin/sh\ncase \"$1\" in list-sessions) exec sleep 20; printf '%s\\n' '$7|:hmux-sep-v1:|slow|:hmux-sep-v1:|1700000000|:hmux-sep-v1:|1700000200|:hmux-sep-v1:|0|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|' ;; list-windows) : ;; esac\n");
    let (connection, mut gateway) = connected(Negotiated::JsonV1, 64 * 1024).await;
    let stop = CancellationToken::new();
    let owner = tokio::spawn(run_connected(
        connection,
        fixture.config(),
        fixture.reader(),
        CommandRunner::new(2).unwrap(),
        stop.clone(),
    ));
    receive(&mut gateway, Negotiated::JsonV1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let start = Instant::now();
    stop.cancel();
    assert_eq!(
        timeout(Duration::from_secs(4), owner)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn stalled_socket_shutdown_joins_transport_without_waiting_for_a_reader() {
    let fixture = Fixture::new(&tmux_script());
    let (connection, _gateway) = connected(Negotiated::JsonV1, 16).await;
    let stop = CancellationToken::new();
    let owner = tokio::spawn(run_connected(
        connection,
        fixture.config(),
        fixture.reader(),
        CommandRunner::new(2).unwrap(),
        stop.clone(),
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    stop.cancel();
    assert_eq!(
        timeout(Duration::from_secs(2), owner)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
}

#[tokio::test]
async fn oversized_profile_projection_fails_closed() {
    let fixture = Fixture::new(&tmux_script());
    let mut inventory = String::from("schema_version = 1\nrevision = \"synthetic-v1\"\n");
    for n in 0..16_000 {
        inventory.push_str(&format!("[[profiles]]\nid = \"p{n}\"\nlabel = \"{}\"\ndefault_directory = \"~\"\ncommand = [\"sh\"]\n", "x".repeat(256)));
    }
    assert!(inventory.len() < 16 << 20);
    fixture.inventory(&inventory);
    let protocol = Negotiated::ProtobufV2;
    let (connection, mut gateway) = connected(protocol, 64 * 1024).await;
    let stop = CancellationToken::new();
    let owner = tokio::spawn(run_connected(
        connection,
        fixture.config(),
        fixture.reader(),
        CommandRunner::new(2).unwrap(),
        stop.clone(),
    ));
    receive(&mut gateway, protocol).await;
    receive(&mut gateway, protocol).await;
    send(
        &mut gateway,
        protocol,
        request("large", p::Operation::Profiles),
    )
    .await;
    assert_error(
        timeout(Duration::from_secs(15), receive(&mut gateway, protocol))
            .await
            .unwrap(),
        "large",
        "Home operation unavailable",
    );
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
async fn wrong_direction_v2_frame_is_rejected() {
    let fixture = Fixture::new(&tmux_script());
    let protocol = Negotiated::ProtobufV2;
    let (connection, mut gateway) = connected(protocol, 64 * 1024).await;
    let owner = tokio::spawn(run_connected(
        connection,
        fixture.config(),
        fixture.reader(),
        CommandRunner::new(2).unwrap(),
        CancellationToken::new(),
    ));
    receive(&mut gateway, protocol).await;
    let invalid = p::Envelope {
        version: pb::VERSION,
        body: Some(p::envelope::Body::Hello(p::Hello {
            capabilities: vec![],
        })),
    };
    gateway
        .send(Message::Binary(
            pb::encode(&invalid, Direction::ToGateway).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(
        timeout(Duration::from_secs(4), owner)
            .await
            .unwrap()
            .unwrap(),
        Err(Error::Protocol)
    );
}

#[tokio::test]
async fn duplicate_inflight_profile_id_closes_peer_and_joins_worker() {
    let fixture = Fixture::new(&tmux_script());
    let mut inventory = String::from("schema_version = 1\nrevision = \"synthetic-v1\"\n");
    for n in 0..12_000 {
        inventory.push_str(&format!("[[profiles]]\nid = \"p{n}\"\nlabel = \"{}\"\ndefault_directory = \"~\"\ncommand = [\"sh\"]\n", "x".repeat(128)));
    }
    fixture.inventory(&inventory);
    let protocol = Negotiated::ProtobufV2;
    let (connection, mut gateway) = connected(protocol, 64 * 1024).await;
    let owner = tokio::spawn(run_connected(
        connection,
        fixture.config(),
        fixture.reader(),
        CommandRunner::new(2).unwrap(),
        CancellationToken::new(),
    ));
    receive(&mut gateway, protocol).await;
    receive(&mut gateway, protocol).await;
    send(
        &mut gateway,
        protocol,
        request("same-id", p::Operation::Profiles),
    )
    .await;
    send(
        &mut gateway,
        protocol,
        request("same-id", p::Operation::Profiles),
    )
    .await;
    assert_eq!(
        timeout(Duration::from_secs(8), owner)
            .await
            .unwrap()
            .unwrap(),
        Err(Error::Protocol)
    );
}

#[tokio::test]
async fn unchanged_catalog_suppresses_early_polls_but_renews_freshness() {
    let fixture = Fixture::new(&tmux_script());
    let protocol = Negotiated::JsonV1;
    let (connection, mut gateway) = connected(protocol, 64 * 1024).await;
    let stop = CancellationToken::new();
    let owner = tokio::spawn(run_connected(
        connection,
        fixture.config(),
        fixture.reader(),
        CommandRunner::new(2).unwrap(),
        stop.clone(),
    ));
    receive(&mut gateway, protocol).await;
    let first = receive(&mut gateway, protocol).await;
    assert!(timeout(Duration::from_secs(7), gateway.next())
        .await
        .is_err());
    let next = timeout(Duration::from_secs(13), async {
        loop {
            let frame = gateway.next().await.unwrap().unwrap();
            if let Message::Ping(data) = frame {
                gateway.send(Message::Pong(data)).await.unwrap();
            } else {
                break frame;
            }
        }
    })
    .await
    .unwrap();
    let next = legacy::from_json(
        wire::Message::decode(&next.into_data()).unwrap(),
        Direction::ToGateway,
    )
    .unwrap()
    .body
    .unwrap();
    assert!(matches!(
        (first, next),
        (p::envelope::Body::Catalog(_), p::envelope::Body::Catalog(_))
    ));
    stop.cancel();
    assert_eq!(
        timeout(Duration::from_secs(3), owner)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
}

#[tokio::test]
async fn canceled_profile_request_never_emits_a_late_success() {
    let fixture = Fixture::new(&tmux_script());
    let mut inventory = String::from("schema_version = 1\nrevision = \"synthetic-v1\"\n");
    for n in 0..10_000 {
        inventory.push_str(&format!("[[profiles]]\nid = \"p{n}\"\nlabel = \"{}\"\ndefault_directory = \"~\"\ncommand = [\"sh\"]\n", "x".repeat(128)));
    }
    fixture.inventory(&inventory);
    let protocol = Negotiated::ProtobufV2;
    let (connection, mut gateway) = connected(protocol, 64 * 1024).await;
    let stop = CancellationToken::new();
    let owner = tokio::spawn(run_connected(
        connection,
        fixture.config(),
        fixture.reader(),
        CommandRunner::new(2).unwrap(),
        stop.clone(),
    ));
    receive(&mut gateway, protocol).await;
    receive(&mut gateway, protocol).await;
    send(
        &mut gateway,
        protocol,
        request("cancelled", p::Operation::Profiles),
    )
    .await;
    send(
        &mut gateway,
        protocol,
        p::envelope::Body::Cancel(p::Reference {
            id: "cancelled".into(),
        }),
    )
    .await;
    assert_error(
        receive(&mut gateway, protocol).await,
        "cancelled",
        "Home operation unavailable",
    );
    assert!(timeout(Duration::from_millis(200), gateway.next())
        .await
        .is_err());
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
async fn dropping_before_first_poll_does_not_start_catalog_work() {
    let fixture = Fixture::new("#!/bin/sh\nprintf '%s\\n' called >> \"${0%/*}/calls\"\n");
    let (connection, mut gateway) = connected(Negotiated::JsonV1, 64 * 1024).await;
    let sender = connection.sender.clone();
    let owner = run_connected(
        connection,
        fixture.config(),
        fixture.reader(),
        CommandRunner::new(2).unwrap(),
        CancellationToken::new(),
    );
    drop(owner);
    assert!(sender.is_closed());
    let closed = timeout(Duration::from_secs(2), gateway.next())
        .await
        .unwrap();
    assert!(closed.is_none() || closed.unwrap().is_err());
    assert!(!fixture.dir.join("calls").exists());
}

#[tokio::test]
async fn abort_during_catalog_sleep_never_starts_another_poll() {
    let script = tmux_script().replacen(
        "#!/bin/sh\n",
        "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"${0%/*}/calls\"\n",
        1,
    );
    let fixture = Fixture::new(&script);
    let (connection, mut gateway) = connected(Negotiated::JsonV1, 64 * 1024).await;
    let runner = CommandRunner::new(2).unwrap();
    let owner = tokio::spawn(run_connected(
        connection,
        fixture.config(),
        fixture.reader(),
        runner.clone(),
        CancellationToken::new(),
    ));
    receive(&mut gateway, Negotiated::JsonV1).await;
    receive(&mut gateway, Negotiated::JsonV1).await;
    let calls = fs::read_to_string(fixture.dir.join("calls")).unwrap();
    assert_eq!(calls, "list-sessions\nlist-windows\n");
    owner.abort();
    assert!(owner.await.unwrap_err().is_cancelled());
    let closed = timeout(Duration::from_secs(2), gateway.next())
        .await
        .unwrap();
    assert!(closed.is_none() || closed.unwrap().is_err());
    tokio::time::sleep(Duration::from_millis(5300)).await;
    assert_eq!(
        fs::read_to_string(fixture.dir.join("calls")).unwrap(),
        calls
    );
    assert_eq!(runner.available_slots(), 2);
}

#[tokio::test]
async fn abort_during_command_keeps_cleanup_owner_until_child_is_reaped() {
    let script = tmux_script()
        .replacen(
            "#!/bin/sh\n",
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"${0%/*}/calls\"\n",
            1,
        )
        .replace(
            "list-sessions) printf",
            "list-sessions) exec sleep 20; printf",
        );
    let fixture = Fixture::new(&script);
    let (connection, mut gateway) = connected(Negotiated::JsonV1, 64 * 1024).await;
    let runner = CommandRunner::new(2).unwrap();
    let owner = tokio::spawn(run_connected(
        connection,
        fixture.config(),
        fixture.reader(),
        runner.clone(),
        CancellationToken::new(),
    ));
    receive(&mut gateway, Negotiated::JsonV1).await;
    timeout(Duration::from_secs(2), async {
        while !fixture.dir.join("calls").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(runner.available_slots(), 1);
    owner.abort();
    assert!(owner.await.unwrap_err().is_cancelled());
    let closed = timeout(Duration::from_secs(2), gateway.next())
        .await
        .unwrap();
    assert!(closed.is_none() || closed.unwrap().is_err());
    timeout(Duration::from_secs(4), async {
        loop {
            if fs::read_to_string(fixture.dir.join("calls")).unwrap() == "list-sessions\n"
                && runner.available_slots() == 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(runner.available_slots(), 2);
}

#[tokio::test]
async fn missing_tmux_server_publishes_empty_catalog_in_both_protocols() {
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let fixture = Fixture::new(
            "#!/bin/sh\nprintf '%s\\n' 'no server running on /synthetic/socket' >&2\nexit 1\n",
        );
        let (connection, mut gateway) = connected(protocol, 64 * 1024).await;
        let stop = CancellationToken::new();
        let owner = tokio::spawn(run_connected(
            connection,
            fixture.config(),
            fixture.reader(),
            CommandRunner::new(2).unwrap(),
            stop.clone(),
        ));
        receive(&mut gateway, protocol).await;
        let p::envelope::Body::Catalog(snapshot) = receive(&mut gateway, protocol).await else {
            panic!("catalog expected")
        };
        let value =
            serde_json::to_value(hmux_protocol::snapshots::catalog_from_proto(*snapshot).unwrap())
                .unwrap();
        assert_eq!(value["sessions"], serde_json::json!([]));
        stop.cancel();
        assert_eq!(
            timeout(Duration::from_secs(3), owner)
                .await
                .unwrap()
                .unwrap(),
            Ok(())
        );
    }
}
