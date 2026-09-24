static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
use futures_util::{SinkExt, StreamExt};
use hmux_core::command::CommandRunner;
use hmux_home::{
    catalog::TmuxCatalogReader,
    config::HomeConfig,
    peer::{run_connected_with_services, Error, Services},
    sessions::Context,
};
use hmux_protocol::{
    actions::{self, ResponseContext},
    legacy,
    protobuf::{self as pb, types as p, Direction, Negotiated},
    transport, wire,
};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{io::DuplexStream, time::timeout};
use tokio_tungstenite::{
    tungstenite::{protocol::Role, Message},
    WebSocketStream,
};
use tokio_util::sync::CancellationToken;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    dir: PathBuf,
    command: PathBuf,
    inventory: PathBuf,
}
impl Fixture {
    fn new(script: &str) -> Self {
        let dir = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-e2e-session-peer-{}-{}",
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
    r#"#!/bin/sh
root=${0%/*}
printf '%s\n' "$1" >> "$root/calls"
case "$1" in
list-sessions) if [ -f "$root/identities" ]; then /bin/cat "$root/identities"; fi ;;
list-windows) : ;;
new-session)
 shift
 while [ "$#" -gt 0 ]; do
  case "$1" in -s) name=$2; shift 2 ;; -c) directory=$2; shift 2 ;; -F) shift 2 ;; -d|-P) shift ;; *) break ;; esac
 done
 printf '%s\n' "$@" > "$root/launch-args"
 printf '%s\n' "$directory" > "$root/directory"
 count=0; if [ -f "$root/count" ]; then read -r count < "$root/count"; fi
 count=$((count + 1)); printf '%s\n' "$count" > "$root/count"
 printf '$%s|:hmux-sep-v1:|%s|:hmux-sep-v1:|1700000000|:hmux-sep-v1:|1700000000|:hmux-sep-v1:|0|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|\n' "$count" "$name" >> "$root/identities"
 if [ -f "$root/slow" ]; then exec /bin/sleep 20; fi
 if [ -f "$root/bad-identity" ]; then printf 'invalid\n'; else printf '$%s 1700000000\n' "$count"; fi ;;
*) exit 99 ;;
esac
"#.to_owned()
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
    context: Option<ResponseContext>,
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
            context,
        )
        .unwrap(),
        Negotiated::ProtobufV2 => pb::decode(frame.into_data(), Direction::ToGateway).unwrap(),
    };
    envelope.body.unwrap()
}

async fn boot(
    f: &Fixture,
    protocol: Negotiated,
) -> (
    CancellationToken,
    tokio::task::JoinHandle<Result<(), Error>>,
    WebSocketStream<DuplexStream>,
) {
    f.inventory(&format!("schema_version=1\nrevision='synthetic'\n[[profiles]]\nid='codex'\nlabel='Codex'\ndefault_directory='{}'\ncommand=['codex','literal; $(false)']\ntags=['test']\n",f.dir.join("work").display()));
    fs::write(f.dir.join("codex"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(f.dir.join("codex"), fs::Permissions::from_mode(0o700)).unwrap();
    let context = Context::new(
        f.dir.clone(),
        f.dir.clone().into_os_string(),
        "/bin/sh".into(),
    )
    .unwrap();
    let mut provider_env = hmux_home::providers::ProviderEnv::for_home(
        f.dir.clone(),
        f.dir.clone().into_os_string(),
        f.inventory.clone(),
        f.command.clone(),
    );
    provider_env.system_dirs = Some(Vec::new());
    let provider_service =
        hmux_home::providers::ProviderService::new(provider_env, CommandRunner::new(3).unwrap())
            .unwrap();
    let (connection, mut gateway) = connected(protocol, 64 * 1024).await;
    let stop = CancellationToken::new();
    let owner = tokio::spawn(run_connected_with_services(
        connection,
        f.config(),
        f.reader(),
        CommandRunner::new(2).unwrap(),
        Services {
            providers: Some(Arc::new(provider_service)),
            inspector: None,
            uploads: None,
            sessions: Some(Arc::new(context)),
            workspace: Some(hmux_home::workspace::Workspace::open(&f.dir).unwrap()),
            ..Services::default()
        },
        stop.clone(),
    ));
    assert!(matches!(
        receive(&mut gateway, protocol, None).await,
        p::envelope::Body::Hello(_)
    ));
    assert!(matches!(
        receive(&mut gateway, protocol, None).await,
        p::envelope::Body::Catalog(_)
    ));
    (stop, owner, gateway)
}
fn request(
    id: &str,
    operation: p::Operation,
    payload: serde_json::Value,
    session: Option<p::Session>,
) -> p::envelope::Body {
    p::envelope::Body::Request(Box::new(p::Request {
        id: id.into(),
        operation: operation as i32,
        payload: Some(
            actions::request_from_json(operation, &serde_json::to_vec(&payload).unwrap()).unwrap(),
        ),
        session,
    }))
}
async fn response(
    g: &mut WebSocketStream<DuplexStream>,
    protocol: Negotiated,
    operation: p::Operation,
) -> p::Response {
    loop {
        match receive(g, protocol, Some(ResponseContext::Operation(operation))).await {
            p::envelope::Body::Catalog(_) => {}
            p::envelope::Body::Response(r) => return r,
            _ => panic!("response expected"),
        }
    }
}
fn identity() -> Option<p::Session> {
    Some(p::Session {
        id: "$1".into(),
        created_at: 1700000000,
    })
}
fn metadata(f: &Fixture, name: &str) -> serde_json::Value {
    serde_json::from_slice(&fs::read(f.dir.join("sessions").join(name)).unwrap()).unwrap()
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
async fn both_codecs_create_unique_children_persist_metadata_and_check_fresh_alias_visibility() {
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let f = Fixture::new(&tmux_script());
        let (stop, owner, mut g) = boot(&f, protocol).await;
        for n in 1..=2 {
            send(
                &mut g,
                protocol,
                request(
                    &format!("create{n}"),
                    p::Operation::Create,
                    serde_json::json!({"profile":"codex","name":"한글 ../ project"}),
                    None,
                ),
            )
            .await;
            let r = response(&mut g, protocol, p::Operation::Create).await;
            assert_eq!(r.error, "");
            let v: serde_json::Value =
                serde_json::from_slice(&actions::response_payload(&r).unwrap()).unwrap();
            assert_eq!(
                v,
                serde_json::json!({"id":format!("${n}"),"created_at":1700000000,"reused":false})
            );
        }
        let children: Vec<_> = fs::read_dir(f.dir.join("work"))
            .unwrap()
            .map(|e| e.unwrap())
            .collect();
        assert_eq!(children.len(), 2);
        assert!(children.iter().all(|e| e
            .file_name()
            .to_string_lossy()
            .starts_with("한글-project")
            && e.metadata().unwrap().permissions().mode() & 0o777 == 0o700));
        let args = fs::read_to_string(f.dir.join("launch-args")).unwrap();
        assert!(args.contains("trap ':' INT QUIT"));
        assert!(args.contains("literal; $(false)\n"));
        let m = metadata(&f, "sessions.json");
        assert_eq!(m["sessions"]["$1"]["profile"], "codex");
        assert_eq!(m["sessions"]["$1"]["tags"], serde_json::json!(["test"]));
        send(
            &mut g,
            protocol,
            request(
                "alias",
                p::Operation::Alias,
                serde_json::json!({"alias":"  별칭  "}),
                identity(),
            ),
        )
        .await;
        assert_eq!(
            response(&mut g, protocol, p::Operation::Alias).await.error,
            ""
        );
        send(
            &mut g,
            protocol,
            request(
                "hide",
                p::Operation::Hidden,
                serde_json::json!({"hidden":true}),
                identity(),
            ),
        )
        .await;
        assert_eq!(
            response(&mut g, protocol, p::Operation::Hidden).await.error,
            ""
        );
        assert_eq!(
            metadata(&f, "sessions.json")["sessions"]["$1"]["alias"],
            "별칭"
        );
        assert_eq!(
            metadata(&f, "session-visibility.json")["hidden"]["$1"]["id"],
            "$1"
        );
        timeout(Duration::from_secs(8), async {
            loop {
                if let p::envelope::Body::Catalog(raw) = receive(&mut g, protocol, None).await {
                    let c: hmux_model::Catalog =
                        hmux_protocol::snapshots::catalog_from_proto(*raw).unwrap();
                    if c.sessions.unwrap().iter().any(|s| {
                        s.id == "$1" && s.alias == "별칭" && s.hidden && s.profile == "codex"
                    }) {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        let original = fs::read(f.dir.join("sessions/session-visibility.json")).unwrap();
        let replaced = fs::read_to_string(f.dir.join("identities"))
            .unwrap()
            .replace("1700000000", "1700000001");
        fs::write(f.dir.join("identities"), replaced).unwrap();
        send(
            &mut g,
            protocol,
            request(
                "stale-unhide",
                p::Operation::Hidden,
                serde_json::json!({"hidden":false}),
                identity(),
            ),
        )
        .await;
        assert!(!response(&mut g, protocol, p::Operation::Hidden)
            .await
            .error
            .is_empty());
        assert_eq!(
            fs::read(f.dir.join("sessions/session-visibility.json")).unwrap(),
            original
        );
        close(stop, owner).await;
        let calls = fs::read_to_string(f.dir.join("calls")).unwrap();
        assert!(!calls.contains("kill-session"));
        assert!(!calls.contains("display-message"));
    }
}

#[tokio::test]
async fn failed_create_keeps_new_session_and_directory_and_invalid_requests_have_no_side_effects() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(&tmux_script());
    let protocol = Negotiated::ProtobufV2;
    let (stop, owner, mut g) = boot(&f, protocol).await;
    for (i, value) in [
        serde_json::json!({"profile":"unknown"}),
        serde_json::json!({"profile":"codex","name":"bad\nname"}),
    ]
    .into_iter()
    .enumerate()
    {
        send(
            &mut g,
            protocol,
            request(&format!("bad{i}"), p::Operation::Create, value, None),
        )
        .await;
        assert!(!response(&mut g, protocol, p::Operation::Create)
            .await
            .error
            .is_empty());
    }
    for value in [
        serde_json::json!(["codex", "name"]),
        serde_json::json!({"profile":"codex","other":true}),
    ] {
        assert!(actions::request_from_json(
            p::Operation::Create,
            &serde_json::to_vec(&value).unwrap()
        )
        .is_err());
    }
    assert!(!f.dir.join("work").exists());
    assert!(!f.dir.join("count").exists());
    fs::write(f.dir.join("bad-identity"), "").unwrap();
    send(
        &mut g,
        protocol,
        request(
            "bad-result",
            p::Operation::Create,
            serde_json::json!({"profile":"codex","name":"owned"}),
            None,
        ),
    )
    .await;
    assert_eq!(
        response(&mut g, protocol, p::Operation::Create).await.error,
        "Session created; metadata unavailable"
    );
    assert_eq!(fs::read_dir(f.dir.join("work")).unwrap().count(), 1);
    assert!(fs::read_to_string(f.dir.join("identities"))
        .unwrap()
        .contains("$1"));
    fs::remove_file(f.dir.join("bad-identity")).unwrap();
    let state = hmux_core::PrivateDir::open(&f.dir)
        .unwrap()
        .create_private_child(std::ffi::OsStr::new("sessions"))
        .unwrap();
    let held = state
        .try_lock(std::ffi::OsStr::new("sessions.lock"))
        .unwrap()
        .unwrap();
    send(
        &mut g,
        protocol,
        request(
            "metadata-busy",
            p::Operation::Create,
            serde_json::json!({"profile":"codex","name":"owned"}),
            None,
        ),
    )
    .await;
    assert_eq!(
        response(&mut g, protocol, p::Operation::Create).await.error,
        "Session created; metadata unavailable"
    );
    drop(held);
    assert_eq!(fs::read_dir(f.dir.join("work")).unwrap().count(), 2);
    assert!(fs::read_to_string(f.dir.join("identities"))
        .unwrap()
        .contains("$2"));
    assert!(!f.dir.join("sessions/sessions.json").exists());
    close(stop, owner).await;
    assert!(!fs::read_to_string(f.dir.join("calls"))
        .unwrap()
        .contains("kill-session"));
}

#[tokio::test]
async fn cancel_busy_create_reaps_only_query_child_and_preserves_independent_profiles() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new(&tmux_script());
    let protocol = Negotiated::JsonV1;
    let (stop, owner, mut g) = boot(&f, protocol).await;
    fs::write(f.dir.join("slow"), "").unwrap();
    send(
        &mut g,
        protocol,
        request(
            "slow",
            p::Operation::Create,
            serde_json::json!({"profile":"codex"}),
            None,
        ),
    )
    .await;
    timeout(Duration::from_secs(3), async {
        while !f.dir.join("count").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    send(
        &mut g,
        protocol,
        request(
            "busy",
            p::Operation::Create,
            serde_json::json!({"profile":"codex"}),
            None,
        ),
    )
    .await;
    let busy = response(&mut g, protocol, p::Operation::Create).await;
    assert_eq!(busy.id, "busy");
    assert_eq!(busy.error, "Home is busy");
    send(
        &mut g,
        protocol,
        request(
            "profiles",
            p::Operation::Profiles,
            serde_json::json!({}),
            None,
        ),
    )
    .await;
    assert_eq!(
        response(&mut g, protocol, p::Operation::Profiles)
            .await
            .error,
        ""
    );
    send(
        &mut g,
        protocol,
        p::envelope::Body::Cancel(p::Reference { id: "slow".into() }),
    )
    .await;
    assert!(!timeout(
        Duration::from_secs(3),
        response(&mut g, protocol, p::Operation::Create)
    )
    .await
    .unwrap()
    .error
    .is_empty());
    assert_eq!(fs::read_dir(f.dir.join("work")).unwrap().count(), 1);
    close(stop, owner).await;
    assert!(!fs::read_to_string(f.dir.join("calls"))
        .unwrap()
        .contains("kill-session"));
}

#[tokio::test]
#[ignore = "requires HMUX_TEST_TMUX; creates only an isolated hmux-e2e server and fake providers"]
async fn real_tmux_provider_exit_and_ctrl_c_leave_owned_interactive_shells() {
    let _serial = SERIAL.lock().await;
    let tmux = PathBuf::from(std::env::var_os("HMUX_TEST_TMUX").expect("explicit tmux required"));
    assert!(tmux.is_absolute());
    let f = Fixture::new("");
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\"'\"'"));
    let wrapper=format!("#!/bin/sh\nexec /usr/bin/env -i HOME={} PATH=/usr/bin:/bin SHELL=/bin/sh {} -S {} -f /dev/null \"$@\"\n",quote(f.dir.to_str().unwrap()),quote(tmux.to_str().unwrap()),quote(f.dir.join("owned.socket").to_str().unwrap()));
    fs::write(&f.command, wrapper).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::process::Command::new(&self.0)
                .arg("kill-server")
                .output();
        }
    }
    let cleanup = Cleanup(f.command.clone());
    let protocol = Negotiated::ProtobufV2;
    let (stop, owner, mut g) = boot(&f, protocol).await;
    let mut inventory = "schema_version=1\nrevision='synthetic'\n".to_owned();
    for provider in ["codex", "claude"] {
        inventory.push_str(&format!("[[profiles]]\nid='{provider}'\nlabel='{provider}'\ndefault_directory='{}'\ncommand=['{provider}','literal; $(false)']\n",f.dir.join("work").display()));
    }
    f.inventory(&inventory);
    let command = |args: &[&str]| {
        let output = std::process::Command::new(&f.command)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let mut created_ids = Vec::new();
    for provider in ["codex", "claude"] {
        for status in ["0", "7", "130", "interrupt", "handle-interrupt"] {
            let body=match status {
                "interrupt"=>"exec /bin/sleep 300".to_owned(),
                "handle-interrupt"=>"trap 'printf signal > interrupted' INT\nwhile [ ! -f exit-provider ]; do /bin/sleep 0.05; done\nexit 0".to_owned(),
                value=>format!("exit {value}"),
            };
            fs::write(f.dir.join(provider),format!("#!/bin/sh\n[ \"$1\" = 'literal; $(false)' ] || exit 99\npwd > provider-cwd\n{body}\n")).unwrap();
            fs::set_permissions(f.dir.join(provider), fs::Permissions::from_mode(0o700)).unwrap();
            let name = format!("hmux-e2e-{provider}-{status}");
            send(
                &mut g,
                protocol,
                request(
                    &name,
                    p::Operation::Create,
                    serde_json::json!({"profile":provider,"name":name}),
                    None,
                ),
            )
            .await;
            let r = response(&mut g, protocol, p::Operation::Create).await;
            assert_eq!(r.error, "");
            let result: serde_json::Value =
                serde_json::from_slice(&actions::response_payload(&r).unwrap()).unwrap();
            let id = result["id"].as_str().unwrap().to_owned();
            let target = format!("{id}:");
            let cwd = PathBuf::from(
                command(&[
                    "display-message",
                    "-p",
                    "-t",
                    &target,
                    "#{pane_current_path}",
                ])
                .trim(),
            );
            assert_eq!(cwd.parent(), Some(f.dir.join("work").as_path()));
            timeout(Duration::from_secs(3), async {
                while !cwd.join("provider-cwd").is_file() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            if status.contains("interrupt") {
                command(&["send-keys", "-t", &target, "C-c"]);
            }
            if status == "handle-interrupt" {
                timeout(Duration::from_secs(3), async {
                    while !cwd.join("interrupted").is_file() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                // A provider that handles Ctrl+C must remain the foreground
                // owner; queued shell input may run only after its real exit.
                command(&[
                    "send-keys",
                    "-t",
                    &target,
                    "printf shell-ready > shell-ready",
                    "Enter",
                ]);
                tokio::time::sleep(Duration::from_millis(100)).await;
                assert!(!cwd.join("shell-ready").exists());
                fs::write(cwd.join("exit-provider"), "").unwrap();
            } else {
                command(&[
                    "send-keys",
                    "-t",
                    &target,
                    "printf shell-ready > shell-ready",
                    "Enter",
                ]);
            }
            timeout(Duration::from_secs(4), async {
                // Redirection creates the file before printf writes its data.
                // Wait for the completed signal rather than racing that window.
                while !fs::read(cwd.join("shell-ready"))
                    .is_ok_and(|contents| contents == b"shell-ready")
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(fs::read(cwd.join("shell-ready")).unwrap(), b"shell-ready");
            created_ids.push(id);
        }
    }
    close(stop, owner).await;
    for id in created_ids {
        command(&["has-session", "-t", &id]);
    }
    drop(cleanup); // Only this explicitly isolated server is removed.
}

#[tokio::test]
async fn shared_workspace_both_codecs_replay_reconnect_and_recycled_identity() {
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let f = Fixture::new(&tmux_script());
        fs::write(f.dir.join("identities"), "$1|:hmux-sep-v1:|synthetic|:hmux-sep-v1:|1700000000|:hmux-sep-v1:|1700000000|:hmux-sep-v1:|0|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|\n").unwrap();
        let (stop, owner, mut g) = boot(&f, protocol).await;
        let change = serde_json::json!({"change":{"operation_id":"synthetic-change-01","revision":0,"base":[],"tabs":[{"id":"$1","created_at":1700000000}]}});
        for id in ["first", "replay"] {
            send(
                &mut g,
                protocol,
                request(id, p::Operation::Workspace, change.clone(), None),
            )
            .await;
            let r = response(&mut g, protocol, p::Operation::Workspace).await;
            assert_eq!(r.error, "");
            let snapshot: hmux_model::workspace::Snapshot =
                serde_json::from_slice(&actions::response_payload(&r).unwrap()).unwrap();
            assert_eq!(snapshot.revision, 1);
            assert_eq!(snapshot.tabs.len(), 1);
            assert!(snapshot.conflict.is_empty());
        }
        close(stop, owner).await;
        let (stop, owner, mut g) = boot(&f, protocol).await;
        send(
            &mut g,
            protocol,
            request(
                "reload",
                p::Operation::Workspace,
                serde_json::json!({}),
                None,
            ),
        )
        .await;
        let r = response(&mut g, protocol, p::Operation::Workspace).await;
        let snapshot: hmux_model::workspace::Snapshot =
            serde_json::from_slice(&actions::response_payload(&r).unwrap()).unwrap();
        assert_eq!(snapshot.revision, 1);
        assert_eq!(snapshot.tabs[0].created_at, 1700000000);
        let wrong = serde_json::json!({"change":{"operation_id":"synthetic-change-02","revision":1,"base":[],"tabs":[{"id":"$1","created_at":1700000001}]}});
        send(
            &mut g,
            protocol,
            request("recycled", p::Operation::Workspace, wrong, None),
        )
        .await;
        let r = response(&mut g, protocol, p::Operation::Workspace).await;
        assert_eq!(r.error, "");
        let snapshot: hmux_model::workspace::Snapshot =
            serde_json::from_slice(&actions::response_payload(&r).unwrap()).unwrap();
        assert_eq!(snapshot.conflict, "workspace_conflict");
        assert_eq!(snapshot.tabs[0].created_at, 1700000000);
        close(stop, owner).await;
        let calls = fs::read_to_string(f.dir.join("calls")).unwrap();
        assert!(!calls.contains("kill-") && !calls.contains("new-session"));
    }
}

#[tokio::test]
async fn provider_actions_both_codecs_keep_keys_private_and_configured_workspace() {
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let f = Fixture::new(&tmux_script());
        fs::write(f.dir.join("claude"), "#!/bin/sh\nprintf '2.0.0\\n'\n").unwrap();
        fs::set_permissions(f.dir.join("claude"), fs::Permissions::from_mode(0o700)).unwrap();
        let (stop, owner, mut g) = boot(&f, protocol).await;
        send(
            &mut g,
            protocol,
            request(
                "providers",
                p::Operation::Providers,
                serde_json::json!({}),
                None,
            ),
        )
        .await;
        let r = response(&mut g, protocol, p::Operation::Providers).await;
        assert_eq!(r.error, "");
        let statuses: serde_json::Value =
            serde_json::from_slice(&actions::response_payload(&r).unwrap()).unwrap();
        assert_eq!(statuses["providers"].as_array().unwrap().len(), 3);
        let key = "synthetic-key-not-a-real-credential";
        send(
            &mut g,
            protocol,
            request(
                "key",
                p::Operation::ProviderKey,
                serde_json::json!({"provider":"claude","key":key}),
                None,
            ),
        )
        .await;
        let r = response(&mut g, protocol, p::Operation::ProviderKey).await;
        assert_eq!(r.error, "");
        assert!(!String::from_utf8_lossy(&actions::response_payload(&r).unwrap()).contains(key));
        let reply: serde_json::Value =
            serde_json::from_slice(&actions::response_payload(&r).unwrap()).unwrap();
        assert!(
            reply.get("error").is_none(),
            "{}",
            String::from_utf8_lossy(&actions::response_payload(&r).unwrap())
        );
        let inventory = hmux_home::config::load_inventory(&f.inventory).unwrap();
        let profile = inventory
            .profiles
            .unwrap()
            .into_iter()
            .find(|p| p.id == "claude")
            .unwrap();
        assert_eq!(
            profile.default_directory,
            f.dir.join("work").to_str().unwrap()
        );
        let settings: serde_json::Value =
            serde_json::from_slice(&fs::read(f.dir.join(".claude/settings.json")).unwrap())
                .unwrap();
        assert_eq!(settings["env"]["ANTHROPIC_API_KEY"], key);
        let invalid = serde_json::json!({"provider":"claude","key":key,"unexpected":true});
        assert!(actions::request_from_json(
            p::Operation::ProviderKey,
            &serde_json::to_vec(&invalid).unwrap()
        )
        .is_err());
        if protocol == Negotiated::JsonV1 {
            let message = serde_json::json!({"type":"request","id":"bad","operation":"provider-key","payload":invalid});
            g.send(Message::Text(message.to_string().into()))
                .await
                .unwrap();
            assert!(!response(&mut g, protocol, p::Operation::ProviderKey)
                .await
                .error
                .is_empty());
        }
        close(stop, owner).await;
    }
}
