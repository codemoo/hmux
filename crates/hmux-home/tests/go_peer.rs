//! Actual Go connector endpoint/hub against Rust Home catalog, upload and terminal owners.
//! This is not the production WSS/reconnect wrapper or a live tmux test.
use hmux_core::command::CommandRunner;
use hmux_home::{
    catalog::TmuxCatalogReader, config::HomeConfig, filestage::Store,
    peer::run_connected_with_uploads, upgrade::upgrade,
};
use hmux_protocol::protobuf;
use std::{
    fs, net::SocketAddr, os::unix::fs::PermissionsExt, path::PathBuf, process::Stdio, sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpStream,
    process::Command,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("hmux-e2e-rust-home-go-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self(root)
    }
    fn file(&self, name: &str, text: &str, mode: u32) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
#[ignore = "requires the isolated Go gateway test executable through the optional external baseline suite (tests/RUST.md)"]
async fn actual_go_gateway_accepts_rust_home_catalog_upload_terminal_and_disconnect() {
    let helper =
        PathBuf::from(std::env::var_os("HMUX_GO_HOME_PEER_HELPER").expect("Go helper required"));
    assert!(helper.is_absolute());
    let fixture = Fixture::new();
    let tmux = fixture.file("fake-tmux", r#"#!/bin/sh
case "$1" in
list-sessions) printf '%s\n' '$7|:hmux-sep-v1:|agent|:hmux-sep-v1:|1700000000|:hmux-sep-v1:|1700000200|:hmux-sep-v1:|0|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|' ;;
list-windows) printf '%s\n' '$7|:hmux-sep-v1:|main|:hmux-sep-v1:|1|:hmux-sep-v1:|/synthetic/work|:hmux-sep-v1:|zsh|:hmux-sep-v1:|80|:hmux-sep-v1:|24|:hmux-sep-v1:|123' ;;
display-message) printf '1700000000\n' ;;
new-session|set-hook|if-shell) exit 0 ;;
attach-session)
 stty -echo
 printf 'RUST_READY\n'
 while IFS= read -r line; do
  case "$line" in
   SIZE) stty size ;;
   *) printf 'RUST_INPUT:%s\n' "$line" ;;
  esac
 done ;;
*) exit 1 ;;
esac
"#, 0o700);
    let inventory_path = fixture.file("inventory.toml", "schema_version = 1\nrevision = 'synthetic'\n[[profiles]]\nid = 'shell'\nlabel = 'Shell'\ndefault_directory = '~'\ncommand = ['sh']\n[[profiles]]\nid = 'codex'\nlabel = 'Codex'\ndefault_directory = '~'\ncommand = ['codex']\n", 0o600);
    let mut child = Command::new(helper)
        .args([
            "-test.run",
            "^TestRustHomePeerGoGateway$",
            "-test.timeout",
            "30s",
        ])
        .env_clear()
        .env("HOME", &fixture.0)
        .env("TMPDIR", &fixture.0)
        .env("PATH", "/usr/bin:/bin")
        .env("HMUX_RUST_HOME_PEER_ORACLE", "1")
        .env("GORACE", "atexit_sleep_ms=0")
        .current_dir(&fixture.0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let ready = timeout(Duration::from_secs(8), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let address: SocketAddr = ready
        .strip_prefix("HMUX_GO_HOME_READY ")
        .expect("Go readiness")
        .parse()
        .unwrap();
    assert!(address.ip().is_loopback() && address.port() != 0);
    let socket = timeout(Duration::from_secs(3), TcpStream::connect(address))
        .await
        .unwrap()
        .unwrap();
    let stop = CancellationToken::new();
    let _stop_on_drop = stop.clone().drop_guard();
    // The actual Go endpoint receives the v2 offer, validates authentication and
    // selects no protocol. The candidate accepts that 101 as v1 on this socket.
    // This fixture deliberately uses loopback plaintext; production TLS is an
    // outer connector responsibility and is not validated by this test.
    let connection = upgrade(
        socket,
        "hmux.example",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        &stop,
        None,
    )
    .await
    .unwrap();
    assert_eq!(connection.protocol(), protobuf::Negotiated::JsonV1);
    let store = Arc::new(Store::open(fixture.0.join("hmux/staged-files-v1")).unwrap());
    let home = tokio::spawn(run_connected_with_uploads(
        connection,
        HomeConfig {
            schema_version: 1,
            role: "home".into(),
            inventory_path,
            state_dir: fixture.0.clone(),
        },
        TmuxCatalogReader::new(tmux, None, Duration::from_secs(3)).unwrap(),
        CommandRunner::new(2).unwrap(),
        Some(store),
        stop.clone(),
    ));
    let checked = timeout(Duration::from_secs(15), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(checked, "HMUX_GO_HOME_CHECKED");
    stop.cancel();
    assert_eq!(
        timeout(Duration::from_secs(5), home)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
    assert!(timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
}
