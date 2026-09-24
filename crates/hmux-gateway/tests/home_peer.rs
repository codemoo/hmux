//! Cross-owner integration with both real Rust owners and synthetic tmux only.
use bytes::Bytes;
use hmux_core::command::CommandRunner;
use hmux_gateway::hub::{Error, Hub, ViewEvent, ViewLease};
use hmux_home::{catalog::TmuxCatalogReader, config::HomeConfig, peer::run_connected};
use hmux_protocol::{
    protobuf::{types as p, Direction, Negotiated},
    transport,
};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};
use tokio::time::timeout;
use tokio_tungstenite::{tungstenite::protocol::Role, WebSocketStream};
use tokio_util::sync::CancellationToken;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("hmux-e2e-rust-both-peers-{}", std::process::id()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Self(path);
        fixture.file("inventory.toml", "schema_version = 1\nrevision = 'synthetic'\n[[profiles]]\nid = 'shell'\nlabel = 'Shell'\ndefault_directory = '~'\ncommand = ['sh']\n", 0o600);
        fixture.file("fake-tmux", r#"#!/bin/sh
case "$1" in
list-sessions) printf '%s\n' '$7|:hmux-sep-v1:|synthetic|:hmux-sep-v1:|1700000000|:hmux-sep-v1:|1700000200|:hmux-sep-v1:|0|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|' ;;
list-windows) : ;;
display-message) printf '1700000000\n' ;;
new-session|set-hook|if-shell) : ;;
attach-session)
 stty -echo
 printf 'RUST_READY\n'
 while IFS= read -r line; do
  case "$line" in
   SIZE) stty size ;;
   EXIT) printf 'BYE\n';exit 0 ;;
   *) printf 'RUST_INPUT:%s\n' "$line" ;;
  esac
 done ;;
*) exit 1 ;;
esac
"#,0o700);
        fixture
    }
    fn file(&self, name: &str, contents: &str, mode: u32) {
        let path = self.0.join(name);
        fs::write(&path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn request(operation: p::Operation) -> p::Request {
    p::Request {
        id: String::new(),
        operation: operation as i32,
        session: None,
        payload: Some(if operation == p::Operation::Workspace {
            p::request::Payload::Workspace(p::WorkspaceRequest { change: None })
        } else {
            p::request::Payload::Empty(p::Empty {})
        }),
    }
}

#[tokio::test]
async fn rust_owners_exchange_both_codecs_and_reject_previous_generation() {
    let fixture = Fixture::new();
    let (hub, _completions) = Hub::new();
    let mut previous = None;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let (home, gateway) = tokio::io::duplex(64 * 1024);
        let home =
            WebSocketStream::from_raw_socket(home, Role::Client, Some(transport::socket_config()))
                .await;
        let gateway = WebSocketStream::from_raw_socket(
            gateway,
            Role::Server,
            Some(transport::socket_config()),
        )
        .await;
        let connected = hub
            .attach(transport::start(gateway, protocol, Direction::ToGateway).unwrap())
            .unwrap();
        let generation = connected.generation();
        let stop = CancellationToken::new();
        let _stop_on_drop = stop.clone().drop_guard();
        let peer = tokio::spawn(run_connected(
            transport::start(home, protocol, Direction::ToHome).unwrap(),
            HomeConfig {
                schema_version: 1,
                role: "home".into(),
                inventory_path: fixture.0.join("inventory.toml"),
                state_dir: fixture.0.clone(),
            },
            TmuxCatalogReader::new(fixture.0.join("fake-tmux"), None, Duration::from_secs(2))
                .unwrap(),
            CommandRunner::new(2).unwrap(),
            stop.clone(),
        ));
        timeout(Duration::from_secs(3), async {
            while !hub.snapshot().online {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        {
            let snapshot = hub.snapshot();
            assert_eq!(snapshot.generation, Some(generation));
            assert!(snapshot.output_flow && !snapshot.upload);
            let value: serde_json::Value =
                serde_json::from_slice(snapshot.catalog.as_ref().unwrap()).unwrap();
            assert_eq!(value["sessions"][0]["id"], "$7");
            assert_eq!(value["sessions"][0]["created_at"], 1_700_000_000);
        }
        if let Some(old) = previous {
            assert_ne!(old, generation);
            assert!(matches!(
                hub.request(old, request(p::Operation::Profiles)).await,
                Err(Error::Stale)
            ));
        }
        {
            let profiles = timeout(
                Duration::from_secs(3),
                hub.request(generation, request(p::Operation::Profiles)),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(profiles.error.is_empty());
            let Some(p::response::Result::Profiles(profiles)) = &profiles.result else {
                panic!("profiles result expected")
            };
            assert!(
                profiles.items
                    == vec![p::Profile {
                        id: "shell".into(),
                        label: "Shell".into()
                    }]
            );
            let unsupported = timeout(
                Duration::from_secs(3),
                hub.request(generation, request(p::Operation::Workspace)),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(unsupported.error, "Home operation unavailable");
            assert!(unsupported.result.is_none());
        }
        let mut view = timeout(
            Duration::from_secs(4),
            hub.open_view(
                generation,
                p::TerminalOpen {
                    id: String::new(),
                    session: Some(p::Session {
                        id: "$7".into(),
                        created_at: 1700000000,
                    }),
                    cols: 80,
                    rows: 24,
                    capabilities: vec![],
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(view.output_flow());
        read_text(&mut view, b"RUST_READY").await;
        view.input(Bytes::from_static("synthetic 한글\n".as_bytes()))
            .await
            .unwrap();
        read_text(&mut view, "RUST_INPUT:synthetic 한글".as_bytes()).await;
        view.resize(100, 40).await.unwrap();
        view.input(Bytes::from_static(b"SIZE\n")).await.unwrap();
        read_text(&mut view, b"40 100").await;
        view.refresh().await.unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(3), view.receive())
                .await
                .unwrap()
                .unwrap(),
            ViewEvent::RefreshResult { ok: false }
        ));
        view.input(Bytes::from_static(b"EXIT\n")).await.unwrap();
        read_text(&mut view, b"BYE").await;
        assert!(matches!(
            timeout(Duration::from_secs(3), view.receive())
                .await
                .unwrap()
                .unwrap(),
            ViewEvent::Exit { .. }
        ));
        drop(view);
        stop.cancel();
        assert_eq!(
            timeout(Duration::from_secs(4), peer)
                .await
                .unwrap()
                .unwrap(),
            Ok(())
        );
        timeout(Duration::from_secs(3), connected.wait())
            .await
            .unwrap();
        assert!(!hub.snapshot().connected);
        assert!(hub.snapshot().catalog.is_none());
        assert_eq!(hub.retained_payload_bytes(), 0);
        previous = Some(generation);
    }
}

async fn read_text(view: &mut ViewLease, marker: &[u8]) {
    timeout(Duration::from_secs(3), async {
        let mut output = Vec::new();
        loop {
            let ViewEvent::Data(data) = view.receive().await.unwrap() else {
                panic!("terminal data expected")
            };
            output.extend_from_slice(&data);
            view.acknowledge(data.len() as i64).await.unwrap();
            if output.windows(marker.len()).any(|part| part == marker) {
                break;
            }
            assert!(output.len() < 64 * 1024);
        }
    })
    .await
    .unwrap();
}
