//! End-to-end provider conversation gates over the two Home wire codecs.
//! All process, tmux and transcript data is synthetic and stays in this fixture.
use futures_util::{SinkExt, StreamExt};
use hmux_core::command::CommandRunner;
use hmux_home::{
    catalog::TmuxCatalogReader,
    config::HomeConfig,
    inspection::Inspector,
    peer::{run_connected_with_services, Services},
};
use hmux_protocol::{
    legacy,
    protobuf::{self as pb, types as p, Direction, Negotiated},
    transport, wire,
};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
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
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const SEP: &str = "|:hmux-sep-v1:|";

async fn no_completion_for(
    g: &mut WebSocketStream<DuplexStream>,
    protocol: Negotiated,
    duration: Duration,
) {
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        let Ok(body) = tokio::time::timeout_at(deadline, receive(g, protocol)).await else {
            return;
        };
        assert!(
            matches!(body, p::envelope::Body::Catalog(_)),
            "unexpected history replay"
        );
    }
}

#[tokio::test]
async fn completion_is_sent_in_both_codecs_and_never_replayed_on_reconnect() {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let _serial = SERIAL.lock().await;
    const START: &str = "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n";
    const DONE:&str="{\"type\":\"event_msg\",\"timestamp\":\"2026-09-24T00:00:00.1234Z\",\"payload\":{\"type\":\"task_complete\"}}\n";
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let f = Fixture::new();
        let record = f.codex("completion-private", &format!("{START}{DONE}"));
        f.put("lsof-data", &format!("p90\nn{}\n", record.display()));
        for _ in 0..2 {
            let (stop, owner, mut g) = boot_with_completions(&f, protocol, true).await;
            // Cross a complete catalog/observer cycle before adding new records.
            no_completion_for(&mut g, protocol, Duration::from_secs(6)).await;
            let offset = fs::metadata(&record).unwrap().len() + START.len() as u64;
            let mut file = fs::OpenOptions::new().append(true).open(&record).unwrap();
            file.write_all(format!("{START}{DONE}").as_bytes()).unwrap();
            drop(file);
            let completion = timeout(Duration::from_secs(8), async {
                loop {
                    match receive(&mut g, protocol).await {
                        p::envelope::Body::TaskComplete(event) => break event,
                        p::envelope::Body::Catalog(_) => {}
                        _ => panic!("unexpected completion peer message"),
                    }
                }
            })
            .await
            .unwrap();
            let identity = completion.session.unwrap();
            assert_eq!(identity.id, "$7");
            assert_eq!(identity.created_at, 1700000000);
            assert_eq!(completion.completed_at, "2026-09-24T00:00:00.1234Z");
            let hash = Sha256::digest(format!(
                "hmux-codex-task-complete-v1\0$7\01700000000\0completion-private\0{offset}"
            ));
            assert_eq!(completion.id, format!("{hash:x}"));
            stop.cancel();
            timeout(Duration::from_secs(3), owner)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
    }
}

#[tokio::test]
async fn completion_discovery_cancel_joins_all_inspection_children() {
    let _serial = SERIAL.lock().await;
    let mut f = Fixture::new();
    f.script(
        "fake-ps",
        "#!/bin/sh\nroot=${0%/*}\nprintf '%s\\n' \"$$\" >> \"$root/pids\"\nexec /bin/sleep 20\n",
    );
    let (stop, owner, _g) = boot_with_completions(&f, Negotiated::ProtobufV2, true).await;
    timeout(Duration::from_secs(2), async {
        loop {
            if fs::read_to_string(f.dir.join("pids")).is_ok_and(|v| v.lines().count() == 2) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.cancel();
    timeout(Duration::from_secs(3), owner)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    for line in fs::read_to_string(f.dir.join("pids")).unwrap().lines() {
        let pid = rustix::process::Pid::from_raw(line.parse().unwrap()).unwrap();
        assert!(rustix::process::test_kill_process(pid).is_err());
    }
}
struct Fixture {
    dir: PathBuf,
    tmux: PathBuf,
    ps: PathBuf,
    lsof: PathBuf,
    inventory: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-conversation-peer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let mut f = Self {
            tmux: dir.join("fake-tmux"),
            ps: dir.join("fake-ps"),
            lsof: dir.join("fake-lsof"),
            inventory: dir.join("inventory.toml"),
            dir,
        };
        f.script("fake-tmux", "#!/bin/sh\nroot=${0%/*}\ncase \"$1\" in\nlist-sessions) /bin/cat \"$root/sessions-data\" ;;\nlist-windows) count=0; if [ -f \"$root/windows-count\" ]; then read -r count < \"$root/windows-count\"; fi; count=$((count + 1)); printf '%s\\n' \"$count\" > \"$root/windows-count\"; if [ -f \"$root/swap-pane\" ] && [ \"$count\" -ge 3 ]; then /bin/cat \"$root/windows-replaced-data\"; else /bin/cat \"$root/windows-data\"; fi ;;\n*) exit 99 ;;\nesac\n");
        f.script("fake-ps", "#!/bin/sh\nroot=${0%/*}\nif [ -f \"$root/slow\" ]; then printf '%s\\n' \"$$\" > \"$root/ps-pid\"; exec /bin/sleep 20; fi\n/bin/cat \"$root/ps-data\"\n");
        f.script(
            "fake-lsof",
            "#!/bin/sh\nroot=${0%/*}\n/bin/cat \"$root/lsof-data\"\n",
        );
        f.put("inventory.toml", "schema_version=1\nrevision='synthetic'\n[[profiles]]\nid='shell'\nlabel='Shell'\ndefault_directory='~'\ncommand=['sh']\n");
        f.put(
            "sessions-data",
            &format!("$7{SEP}agent{SEP}1700000000{SEP}1700000200{SEP}0{SEP}1{SEP}{SEP}\n"),
        );
        f.put(
            "windows-data",
            &format!("$7{SEP}main{SEP}1{SEP}/synthetic/work{SEP}zsh{SEP}80{SEP}24{SEP}80\n"),
        );
        f.put("ps-data", "80 1 S 0.0 zsh\n90 80 S+ 0.1 codex\n");
        f.put("lsof-data", "");
        f
    }
    fn script(&mut self, name: &str, body: &str) {
        let path = self.dir.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn put(&self, relative: &str, body: &str) -> PathBuf {
        let path = self.dir.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
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
        TmuxCatalogReader::new(self.tmux.clone(), None, Duration::from_secs(3)).unwrap()
    }
    fn inspector(&self) -> Arc<Inspector> {
        Arc::new(
            Inspector::new(self.dir.clone(), self.ps.clone(), Some(self.lsof.clone())).unwrap(),
        )
    }
    fn codex(&self, id: &str, content: &str) -> PathBuf {
        self.put(&format!(".codex/sessions/2026/09/24/rollout-2026-09-24T00-00-00-{id}.jsonl"),
            &format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"source\":\"cli\"}}}}\n{content}"))
    }
    fn claude(&self) {
        self.put("ps-data", "80 1 S 0.0 zsh\n90 80 S+ 0.1 claude\n");
        self.put(
            ".claude/sessions/90.json",
            "{\"pid\":90,\"sessionId\":\"claude-private-id\",\"status\":\"busy\"}",
        );
        self.put(".claude/projects/project/claude-private-id.jsonl", concat!(
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Public question\"}}\n",
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"thinking\",\"thinking\":\"secret thought\"},{\"type\":\"text\",\"text\":\"Public answer\"}]}}\n",
            "{\"type\":\"assistant\",\"isSidechain\":true,\"message\":{\"role\":\"assistant\",\"content\":\"sidechain secret\"}}\n"));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
async fn connected(protocol: Negotiated) -> (transport::Connection, WebSocketStream<DuplexStream>) {
    let (left, right) = tokio::io::duplex(64 * 1024);
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
    g: &mut WebSocketStream<DuplexStream>,
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
    g.send(frame).await.unwrap();
}
async fn receive(g: &mut WebSocketStream<DuplexStream>, protocol: Negotiated) -> p::envelope::Body {
    let frame = timeout(Duration::from_secs(8), g.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let envelope = match protocol {
        Negotiated::JsonV1 => {
            let message = wire::Message::decode(&frame.into_data()).unwrap();
            let operation = if message.id == "profiles" {
                p::Operation::Profiles
            } else {
                p::Operation::Conversation
            };
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
async fn boot(
    f: &Fixture,
    protocol: Negotiated,
) -> (
    CancellationToken,
    tokio::task::JoinHandle<Result<(), hmux_home::peer::Error>>,
    WebSocketStream<DuplexStream>,
) {
    boot_with_completions(f, protocol, false).await
}
async fn boot_with_completions(
    f: &Fixture,
    protocol: Negotiated,
    completions: bool,
) -> (
    CancellationToken,
    tokio::task::JoinHandle<Result<(), hmux_home::peer::Error>>,
    WebSocketStream<DuplexStream>,
) {
    let (connection, mut g) = connected(protocol).await;
    let stop = CancellationToken::new();
    let owner = tokio::spawn(run_connected_with_services(
        connection,
        f.config(),
        f.reader(),
        CommandRunner::new(2).unwrap(),
        Services {
            completions,
            inspector: Some(f.inspector()),
            ..Services::default()
        },
        stop.clone(),
    ));
    assert!(matches!(
        receive(&mut g, protocol).await,
        p::envelope::Body::Hello(_)
    ));
    assert!(matches!(
        receive(&mut g, protocol).await,
        p::envelope::Body::Catalog(_)
    ));
    (stop, owner, g)
}
fn conversation(id: &str, created_at: i64) -> p::envelope::Body {
    p::envelope::Body::Request(Box::new(p::Request {
        id: id.into(),
        operation: p::Operation::Conversation as i32,
        session: Some(p::Session {
            id: "$7".into(),
            created_at,
        }),
        payload: Some(p::request::Payload::Empty(p::Empty {})),
    }))
}
async fn reply(
    g: &mut WebSocketStream<DuplexStream>,
    protocol: Negotiated,
    id: &str,
) -> p::Response {
    loop {
        match receive(g, protocol).await {
            p::envelope::Body::Response(r) if r.id == id => return r,
            p::envelope::Body::Catalog(_) => {}
            _ => panic!("unexpected peer reply"),
        }
    }
}
async fn close(
    stop: CancellationToken,
    owner: tokio::task::JoinHandle<Result<(), hmux_home::peer::Error>>,
) {
    stop.cancel();
    assert_eq!(
        timeout(Duration::from_secs(5), owner)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
}
fn texts(response: &p::Response) -> Vec<String> {
    let value: serde_json::Value =
        serde_json::from_slice(&hmux_protocol::actions::response_payload(response).unwrap())
            .unwrap();
    value["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["text"].as_str().unwrap().to_owned())
        .collect()
}
fn status(response: &p::Response) -> String {
    serde_json::from_slice::<serde_json::Value>(
        &hmux_protocol::actions::response_payload(response).unwrap(),
    )
    .unwrap()["status"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn no_private_leak(response: &p::Response, root: &Path, record_id: &str) {
    let payload = hmux_protocol::actions::response_payload(response).unwrap();
    let raw = String::from_utf8_lossy(&payload);
    assert!(!raw.contains(root.to_str().unwrap()));
    assert!(!raw.contains(record_id));
}
#[tokio::test]
async fn both_codecs_return_exact_public_codex_and_claude_text() {
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let f = Fixture::new();
        let content = concat!(
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Public question\"}]}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"channel\":\"analysis\",\"content\":[{\"type\":\"output_text\",\"text\":\"secret thought\"}]}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Public answer\"}]}}\n");
        let path = f.codex("codex-private-id", content);
        f.put("lsof-data", &format!("p90\nn{}\n", path.display()));
        let (stop, owner, mut g) = boot(&f, protocol).await;
        send(&mut g, protocol, conversation("codex", 1700000000)).await;
        let r = reply(&mut g, protocol, "codex").await;
        assert_eq!(r.error, "");
        assert_eq!(status(&r), "ready");
        assert_eq!(texts(&r), ["Public question", "Public answer"]);
        no_private_leak(&r, &f.dir, "codex-private-id");
        close(stop, owner).await;

        let f = Fixture::new();
        f.claude();
        let (stop, owner, mut g) = boot(&f, protocol).await;
        send(&mut g, protocol, conversation("claude", 1700000000)).await;
        let r = reply(&mut g, protocol, "claude").await;
        assert_eq!(r.error, "");
        assert_eq!(status(&r), "ready");
        assert_eq!(texts(&r), ["Public question", "Public answer"]);
        no_private_leak(&r, &f.dir, "claude-private-id");
        close(stop, owner).await;
    }
}
#[tokio::test]
async fn stale_identity_and_ambiguous_descriptors_never_select_public_text() {
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let f = Fixture::new();
        let one = f.codex("first-private-id", "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"wrong session\"}]}}\n");
        let two = f.codex("second-private-id", "");
        f.put(
            "lsof-data",
            &format!("p90\nn{}\nn{}\n", one.display(), two.display()),
        );
        let (stop, owner, mut g) = boot(&f, protocol).await;
        send(&mut g, protocol, conversation("ambiguous", 1700000000)).await;
        let r = reply(&mut g, protocol, "ambiguous").await;
        assert_eq!(r.error, "");
        assert_eq!(status(&r), "ambiguous");
        assert!(texts(&r).is_empty());
        no_private_leak(&r, &f.dir, "first-private-id");
        f.put("lsof-data", &format!("p90\nn{}\n", one.display()));
        send(&mut g, protocol, conversation("stale", 1700000001)).await;
        let r = reply(&mut g, protocol, "stale").await;
        assert_eq!(r.error, "");
        assert_eq!(status(&r), "unavailable");
        assert!(texts(&r).is_empty());
        close(stop, owner).await;
    }
}
#[tokio::test]
async fn slow_inspection_keeps_first_catalog_and_profiles_live_and_shutdown_reaps_child() {
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let f = Fixture::new();
        f.put("slow", "yes");
        let (stop, owner, mut g) = boot(&f, protocol).await;
        timeout(Duration::from_secs(2), async {
            while !f.dir.join("ps-pid").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        send(
            &mut g,
            protocol,
            p::envelope::Body::Request(Box::new(p::Request {
                id: "profiles".into(),
                operation: p::Operation::Profiles as i32,
                session: None,
                payload: Some(p::request::Payload::Empty(p::Empty {})),
            })),
        )
        .await;
        let r = reply(&mut g, protocol, "profiles").await;
        assert_eq!(r.error, "");
        assert!(!hmux_protocol::actions::response_payload(&r)
            .unwrap()
            .is_empty());
        let pid: i32 = fs::read_to_string(f.dir.join("ps-pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        close(stop, owner).await;
        assert!(!std::process::Command::new("/bin/kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success());
        fs::remove_file(f.dir.join("slow")).unwrap();
        let (stop, owner, mut g) = boot(&f, protocol).await;
        send(&mut g, protocol, conversation("after-cancel", 1700000000)).await;
        let r = reply(&mut g, protocol, "after-cancel").await;
        assert_eq!(r.error, "");
        close(stop, owner).await;
    }
}

#[tokio::test]
async fn pane_replacement_between_identity_checks_returns_no_text() {
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let f = Fixture::new();
        let path = f.codex("pane-private-id", "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"old pane text\"}]}}\n");
        f.put("lsof-data", &format!("p90\nn{}\n", path.display()));
        f.put("swap-pane", "yes");
        f.put(
            "windows-replaced-data",
            &format!("$7{SEP}main{SEP}1{SEP}/synthetic/work{SEP}zsh{SEP}80{SEP}24{SEP}81\n"),
        );
        let (stop, owner, mut g) = boot(&f, protocol).await;
        send(&mut g, protocol, conversation("pane-swapped", 1700000000)).await;
        let r = reply(&mut g, protocol, "pane-swapped").await;
        assert_eq!(r.error, "");
        assert_eq!(status(&r), "unavailable");
        assert!(texts(&r).is_empty());
        no_private_leak(&r, &f.dir, "pane-private-id");
        close(stop, owner).await;
    }
}

#[tokio::test]
async fn file_replacement_and_shrink_are_rejected_but_append_is_allowed() {
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        for operation in ["replace", "shrink", "append"] {
            let mut f = Fixture::new();
            let content = "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"public\"}]}}\n";
            let record = f.codex("record-id", content);
            f.put("record-path", record.to_str().unwrap());
            f.put("lsof-data", &format!("p90\nn{}\n", record.display()));
            let original = fs::read_to_string(&f.tmux).unwrap();
            f.script(
                "fake-tmux",
                &original.replace(
                    "count=$((count + 1));",
                    "count=$((count + 1)); if [ \"$count\" -eq 3 ]; then \"$root/mutate\"; fi;",
                ),
            );
            let mutation = match operation {
                "replace" => "/bin/cp \"$record\" \"$root/replacement\"; /bin/mv \"$root/replacement\" \"$record\"",
                "shrink" => "/usr/bin/head -n 1 \"$record\" > \"$root/header\"; /bin/cat \"$root/header\" > \"$record\"",
                _ => "printf '{}\\n' >> \"$record\"",
            };
            f.script("mutate", &format!("#!/bin/sh\nroot=${{0%/*}}\nrecord=$(/bin/cat \"$root/record-path\")\n{mutation}\n"));
            let (stop, owner, mut g) = boot(&f, protocol).await;
            send(&mut g, protocol, conversation("check", 1700000000)).await;
            let r = reply(&mut g, protocol, "check").await;
            assert_eq!(r.error, "");
            if operation == "append" {
                assert_eq!(status(&r), "ready");
                assert_eq!(texts(&r), ["public"]);
            } else {
                assert_eq!(status(&r), "ambiguous", "{operation}");
                assert!(texts(&r).is_empty());
            }
            no_private_leak(&r, &f.dir, "record-id");
            close(stop, owner).await;
        }
    }
}

#[tokio::test]
async fn wrapper_descriptors_are_accepted_only_for_one_owner_chain() {
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let mut f = Fixture::new();
        f.put(
            "ps-data",
            "80 1 S 0 zsh\n90 80 S+ 0 codex\n91 90 S 0 node\n92 91 S 0 sh\n",
        );
        let path = f.codex("wrapper-record", "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"wrapper text\"}]}}\n");
        // Unknown PIDs in lsof are ignored, even if the tool returns partial data.
        f.put(
            "lsof-data",
            &format!(
                "p777\nn{}\np90\np91\nn{}\np92\n",
                path.display(),
                path.display()
            ),
        );
        f.script(
            "fake-lsof",
            "#!/bin/sh\nroot=${0%/*}\n/bin/cat \"$root/lsof-data\"\nexit 1\n",
        );
        let (stop, owner, mut g) = boot(&f, protocol).await;
        send(&mut g, protocol, conversation("single", 1700000000)).await;
        let r = reply(&mut g, protocol, "single").await;
        assert_eq!(status(&r), "ready");
        assert_eq!(texts(&r), ["wrapper text"]);
        for absent in ["p91\nn", "p90\np91\nn"] {
            // Missing owner or another wrapper is unknown, not an empty record set.
            f.put("lsof-data", &format!("{absent}{}\n", path.display()));
            send(&mut g, protocol, conversation("missing", 1700000000)).await;
            let r = reply(&mut g, protocol, "missing").await;
            assert_eq!(status(&r), "unavailable");
            assert!(texts(&r).is_empty());
        }
        let second = f.codex("other-record", "");
        f.put(
            "lsof-data",
            &format!(
                "p90\np91\nn{}\np92\nn{}\n",
                path.display(),
                second.display()
            ),
        );
        send(&mut g, protocol, conversation("multiple", 1700000000)).await;
        let r = reply(&mut g, protocol, "multiple").await;
        assert_eq!(status(&r), "ambiguous");
        assert!(texts(&r).is_empty());
        close(stop, owner).await;
    }
}

#[tokio::test]
async fn escaped_public_text_is_trimmed_to_the_encoded_reply_budget() {
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let f = Fixture::new();
        let mut content = String::new();
        for index in 0..4 {
            let message = serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":format!("{index}{}", "\u{0001}".repeat(100_000))}]}});
            content.push_str(&message.to_string());
            content.push('\n');
        }
        let path = f.codex("encoded-budget", &content);
        f.put("lsof-data", &format!("p90\nn{}\n", path.display()));
        let (stop, owner, mut g) = boot(&f, protocol).await;
        send(&mut g, protocol, conversation("limit", 1700000000)).await;
        let r = reply(&mut g, protocol, "limit").await;
        assert_eq!(r.error, "");
        assert!(
            hmux_protocol::actions::response_payload(&r).unwrap().len() <= 2 * 1024 * 1024 - 4096
        );
        let value: serde_json::Value =
            serde_json::from_slice(&hmux_protocol::actions::response_payload(&r).unwrap()).unwrap();
        assert_eq!(value["truncated"], true);
        let text = texts(&r);
        assert_eq!(text.len(), 3);
        assert!(text[0].starts_with('1'));
        assert!(text[2].starts_with('3'));
        close(stop, owner).await;
    }
}

#[tokio::test]
async fn busy_cancel_and_disconnect_keep_inspection_workers_bounded() {
    let _serial = SERIAL.lock().await;
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let mut f = Fixture::new();
        f.script("fake-ps", "#!/bin/sh\nroot=${0%/*}\nprintf '%s\\n' \"$$\" >> \"$root/ps-pids\"\nexec /bin/sleep 20\n");
        let (stop, owner, mut g) = boot(&f, protocol).await;
        timeout(Duration::from_secs(2), async {
            while !f.dir.join("ps-pids").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        send(&mut g, protocol, conversation("pending", 1700000000)).await;
        let pids = timeout(Duration::from_secs(2), async {
            loop {
                let pids = fs::read_to_string(f.dir.join("ps-pids")).unwrap();
                if pids.lines().count() == 2 {
                    break pids;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        send(&mut g, protocol, conversation("busy", 1700000000)).await;
        let r = reply(&mut g, protocol, "busy").await;
        assert!(!r.error.is_empty());
        assert!(r.result.is_none());
        send(
            &mut g,
            protocol,
            p::envelope::Body::Cancel(p::Reference {
                id: "pending".into(),
            }),
        )
        .await;
        let r = reply(&mut g, protocol, "pending").await;
        assert!(!r.error.is_empty());
        assert!(r.result.is_none());
        // The whole peer owner joins both the collector and conversation children.
        close(stop, owner).await;
        for pid in pids.lines().map(|p| p.parse::<i32>().unwrap()) {
            let child = rustix::process::Pid::from_raw(pid).unwrap();
            assert!(rustix::process::test_kill_process(child).is_err());
        }
    }
}
