//! Shared Home usage reaches the real gateway Hub across both codecs and reconnects.
//! All provider inputs and tmux commands are synthetic and local.
use base64::Engine as _;
use hmux_core::command::CommandRunner;
use hmux_gateway::hub::Hub;
use hmux_home::{
    catalog::TmuxCatalogReader,
    config::HomeConfig,
    dial,
    peer::{self, Services},
    usage, usage_config,
};
use hmux_protocol::{
    protobuf::{Direction, Negotiated},
    transport,
};
use hmux_usage::{transport as usage_wire, Snapshot};
use std::{
    ffi::OsStr, fs, io::Write, os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
    time::timeout,
};
use tokio_tungstenite::{tungstenite::protocol::Role, WebSocketStream};
use tokio_util::sync::CancellationToken;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("hmux-e2e-shared-usage-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self(root)
    }
    fn write(&self, path: &str, contents: &str, mode: u32) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn line(tokens: i64) -> String {
    format!("{{\"type\":\"event_msg\",\"timestamp\":\"{}\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"last_token_usage\":{{\"input_tokens\":{tokens},\"cached_input_tokens\":0,\"output_tokens\":0}}}}}}}}\n", chrono::Utc::now().to_rfc3339())
}
fn decoded(hub: &Hub) -> Option<(Snapshot, Snapshot)> {
    let snapshot = hub.snapshot();
    Some((
        usage_wire::decode(snapshot.claude_usage.as_ref()?).ok()?,
        usage_wire::decode(snapshot.codex_usage.as_ref()?).ok()?,
    ))
}
async fn until(mut condition: impl FnMut() -> bool) {
    timeout(Duration::from_secs(9), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn shared_sources_survive_reconnect_and_slow_lb_does_not_delay_catalog() {
    let fixture = Fixture::new();
    let now = chrono::Utc::now().to_rfc3339();
    fixture.write(".codex/sessions/rollout.jsonl", &line(7), 0o600);
    fs::create_dir_all(fixture.0.join(".claude/projects")).unwrap();
    fixture.write(".config/token-usage/codex-lb-accounts.json", &format!(r#"{{"schemaVersion":1,"accountsUpdatedAt":"{now}","accounts":[{{"number":1,"alias":"Synthetic Pro","status":"active","planType":"pro","sevenDayPct":40}}]}}"#), 0o600);
    fixture.write(".local/bin/cswap", &format!("#!/bin/sh\n[ \"$1\" = list ] && [ \"$2\" = --json ] || exit 3\nprintf x >> \"$HOME/cswap-calls\"\nprintf '%s' '{{\"schemaVersion\":1,\"activeAccountNumber\":1,\"accounts\":[{{\"number\":1,\"alias\":\"Synthetic Claude\",\"active\":true,\"usageStatus\":\"ok\",\"usage\":{{\"sevenDay\":{{\"pct\":20}}}},\"usageFetchedAt\":\"{now}\"}}]}}'\n"), 0o700);
    fixture.write(
        "inventory.toml",
        "schema_version=1\nrevision='synthetic'\n",
        0o600,
    );
    fixture.write(
        "fake-tmux",
        "#!/bin/sh\ncase \"$1\" in\nlist-sessions|list-windows) exit 0 ;;\n*) exit 1 ;;\nesac\n",
        0o700,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requested = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let stop = CancellationToken::new();
    let _cleanup = stop.clone().drop_guard();
    let server = {
        let requested = requested.clone();
        let release = release.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            while !raw.ends_with(b"\r\n\r\n") {
                raw.push(socket.read_u8().await.unwrap());
                assert!(raw.len() < 8192);
            }
            let raw = String::from_utf8(raw).unwrap();
            assert!(raw.starts_with("GET /v1/usage HTTP/1.1"));
            assert!(raw
                .to_ascii_lowercase()
                .contains("authorization: bearer synthetic-key"));
            requested.notify_one();
            tokio::select! { _ = release.notified() => {}, _ = stop.cancelled() => return }
            let body = r#"{"account_pool_usage":{"primary":30,"secondary":40}}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            drop(socket);
            tokio::select! { _ = stop.cancelled() => {}, _ = listener.accept() => panic!("reconnect restarted quota source") }
        })
    };
    let options =
        usage_config::Options::from_env(&fixture.0, OsStr::new("/usr/bin:/bin"), |key| match key {
            "TOKEN_USAGE_CODEX_LB_URL" => Some(format!("http://{address}").into()),
            "TOKEN_USAGE_CODEX_LB_API_KEY" => Some("synthetic-key".into()),
            "TOKEN_USAGE_DISABLE_CLAUDE_SWAP_SESSIONS" => Some("1".into()),
            _ => None,
        })
        .unwrap();
    // This test binary has one test; override trust inputs before any loader
    // starts. Never depend on the user's platform keychain or certificate paths.
    let cert = rcgen::generate_simple_self_signed(vec!["usage-test.invalid".into()]).unwrap();
    fixture.write(
        "trust/ca.pem",
        &format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            base64::engine::general_purpose::STANDARD.encode(cert.cert.der())
        ),
        0o600,
    );
    fs::create_dir(fixture.0.join("empty-trust")).unwrap();
    let previous_file = std::env::var_os("SSL_CERT_FILE");
    let previous_dir = std::env::var_os("SSL_CERT_DIR");
    std::env::set_var("SSL_CERT_FILE", fixture.0.join("trust/ca.pem"));
    std::env::set_var("SSL_CERT_DIR", fixture.0.join("empty-trust"));
    let client = dial::Client::new().await;
    for (name, value) in [
        ("SSL_CERT_FILE", previous_file),
        ("SSL_CERT_DIR", previous_dir),
    ] {
        if let Some(value) = value {
            std::env::set_var(name, value);
        } else {
            std::env::remove_var(name);
        }
    }
    let collector = usage::Collector::start(options, client.unwrap(), &stop);
    timeout(Duration::from_secs(3), requested.notified())
        .await
        .unwrap();
    let (hub, _completion) = Hub::new();
    for (index, protocol) in [Negotiated::JsonV1, Negotiated::ProtobufV2]
        .into_iter()
        .enumerate()
    {
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
        let gateway = hub
            .attach(transport::start(gateway, protocol, Direction::ToGateway).unwrap())
            .unwrap();
        let link_stop = stop.child_token();
        let owner = tokio::spawn(peer::run_connected_with_services(
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
            Services {
                usage: Some(collector.subscribe()),
                ..Default::default()
            },
            link_stop.clone(),
        ));
        // The pending LB endpoint has not answered, but catalog is already usable.
        timeout(Duration::from_secs(2), async {
            while !hub.snapshot().online {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if index == 0 {
            release.notify_one();
        }
        until(|| {
            decoded(&hub).is_some_and(|(claude, codex)| {
                codex.status.quota_source == "codex_lb"
                    && codex.today_total_tokens == 7
                    && codex.accounts.len() == 1
                    && !claude.sources["cswap"].accounts.is_empty()
            })
        })
        .await;
        let (claude, codex) = decoded(&hub).unwrap();
        assert_eq!(
            claude.sources["cswap"].accounts[0].display_name,
            "Synthetic Claude"
        );
        assert_eq!(codex.accounts[0].plan_type, "pro");
        assert!(codex.accounts[0].five_hour.is_none());
        assert_eq!(codex.sources["cli"].today_total_tokens, 7);
        if index == 1 {
            // Existing file offsets survived the previous link. Only appended work is added.
            fs::OpenOptions::new()
                .append(true)
                .open(fixture.0.join(".codex/sessions/rollout.jsonl"))
                .unwrap()
                .write_all(line(11).as_bytes())
                .unwrap();
            until(|| decoded(&hub).is_some_and(|(_, codex)| codex.today_total_tokens == 18)).await;
        }
        link_stop.cancel();
        assert_eq!(
            timeout(Duration::from_secs(3), owner)
                .await
                .unwrap()
                .unwrap(),
            Ok(())
        );
        timeout(Duration::from_secs(3), gateway.wait())
            .await
            .unwrap();
        assert!(!hub.snapshot().connected);
    }
    assert_eq!(
        fs::read_to_string(fixture.0.join("cswap-calls")).unwrap(),
        "x"
    );
    stop.cancel();
    assert_eq!(
        timeout(Duration::from_secs(5), collector.shutdown())
            .await
            .unwrap(),
        Ok(())
    );
    timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}
