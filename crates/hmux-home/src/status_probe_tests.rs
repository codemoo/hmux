use super::*;
use std::{
    fs,
    os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
const ID: &str = "01234567-89ab-7cde-8fab-0123456789ab";
#[test]
fn status_labels_uuid_wrapping_and_conflicts() {
    assert_eq!(
        session_id(&format!("│ Session: {ID} │")).unwrap(),
        Some(ID.into())
    );
    assert_eq!(
        session_id("│ Session: 01234567-89ab-7cde- │\n│ 8fab-0123456789ab │").unwrap(),
        Some(ID.into())
    );
    assert_eq!(
        session_id(&format!("Session ID: {ID}")).unwrap(),
        Some(ID.into())
    );
    assert_eq!(session_id(&format!("unlabeled {ID}")).unwrap(), None);
    assert!(session_id(&format!(
        "Session: {ID}\nSession: 01234567-89ab-7cde-8fab-0123456789ac"
    ))
    .is_err());
    assert!(session_id("Session: invalid").is_err());
    assert!(!status_panel(&format!("Session: {ID}")));
    assert!(status_panel(&format!(
        "│ Model: test │\n│ Directory: /synthetic │\n│ Session: {ID} │"
    )));
    assert!(session_id("Session: 01234567-89ab-7cde-8fab-0123456789ab trailing").is_err());
}
#[test]
fn prompt_and_identity_guard_fail_closed() {
    let identity = SessionIdentity {
        id: "$7".into(),
        created_at: 42,
    };
    let raw = b"$7|42|%80|80|2|2|80|24|0|0|1|bash\n";
    let pane = Pane::parse(raw, &identity, 80).unwrap();
    let screen = "old output\n\n» Ask Codex to do anything\n\n  GPT-6-Astra\n";
    assert!(empty_prompt(screen, &pane));
    assert!(!empty_prompt(
        &screen.replace("Ask Codex to do anything", "draft text"),
        &pane
    ));
    assert!(!empty_prompt(
        &screen.replace("old output", "• Working (3s • esc to interrupt)"),
        &pane
    ));
    assert!(!empty_prompt(
        &screen.replace("old output", "Would you like to run this command?"),
        &pane
    ));
    assert!(!empty_prompt(
        &screen.replace("\n\n  GPT", "\ncontinued draft\n  GPT"),
        &pane
    ));
    for (from, to) in [
        ("|42|", "|43|"),
        ("|%80|", "|%80;bad|"),
        ("|2|2|", "|80|2|"),
        ("|0|0|1|", "|1|0|1|"),
        ("|0|0|1|", "|0|1|1|"),
        ("|0|0|1|", "|0|0|2|"),
        ("|bash", "|bash,evil"),
    ] {
        let modified = std::str::from_utf8(raw).unwrap().replace(from, to);
        assert!(
            Pane::parse(modified.as_bytes(), &identity, 80).is_err(),
            "{modified}"
        );
    }
    let normal = Pane::parse(b"$7|42|%80|80|2|2|80|24|0|0|0|bash\n", &identity, 80).unwrap();
    assert!(empty_prompt(screen, &normal));
    let moved = Pane::parse(b"$7|42|%80|80|10|2|80|24|0|0|0|bash\n", &identity, 80).unwrap();
    assert!(!empty_prompt(screen, &moved));
    assert!(pane.guard().contains("#{==:#{session_created},42}"));
    assert!(pane.guard().contains("#{==:#{pane_id},%80}"));
}
static NEXT: AtomicU64 = AtomicU64::new(0);
static SERIAL: Mutex<()> = Mutex::new(());
struct Fixture {
    dir: PathBuf,
    identity: SessionIdentity,
    reader: TmuxCatalogReader,
    inspector: Inspector,
    runtime: tokio::runtime::Runtime,
    stop: CancellationToken,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-status-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let identity = SessionIdentity {
            id: format!("${}", 1000 + NEXT.fetch_add(1, Ordering::Relaxed)),
            created_at: 42,
        };
        let write = |name: &str, s: &str, mode| {
            let p = dir.join(name);
            fs::write(&p, s).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap()
        };
        write("ps","#!/bin/sh\nroot=${0%/*}\nif [ \"$1\" = -p ]; then if [ \"$4\" = stat= ]; then printf '%s\\n' 'S+'; else /bin/cat \"$root/stamp\"; fi; else printf '%s\\n' '80 1 S 0.0 bash' '90 80 S+ 0.0 codex'; fi\n",0o700);
        write("stamp", "start-one\n", 0o600);
        write("lsof", "#!/bin/sh\nexit 0\n", 0o700);
        write(
            "tmux",
            r#"#!/bin/sh
root=${0%/*}
case "$1" in
list-sessions) /bin/cat "$root/sessions" ;;
list-windows) /bin/cat "$root/windows" ;;
display-message) if [ -f "$root/submitted" ] && [ -f "$root/metadata-after" ]; then /bin/cat "$root/metadata-after"; elif [ -f "$root/sent" ] && [ ! -f "$root/submitted" ]; then /bin/cat "$root/metadata-typed"; else /bin/cat "$root/metadata"; fi ;;
capture-pane) if [ -f "$root/submitted" ]; then /bin/cat "$root/after"; elif [ -f "$root/sent" ]; then /bin/cat "$root/typed"; else /bin/cat "$root/before"; fi ;;
if-shell) printf '%s\n' "$@" >> "$root/sent"; case "$6" in *' Enter') printf '' > "$root/submitted" ;; esac; if [ -f "$root/change-stamp" ]; then printf '%s\n' 'start-two' > "$root/stamp"; fi ;;
*) exit 99 ;;
esac
"#,
            0o700,
        );
        write("sessions",&format!("{}|:hmux-sep-v1:|fixture|:hmux-sep-v1:|42|:hmux-sep-v1:|43|:hmux-sep-v1:|0|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|\n",identity.id),0o600);
        write("windows",&format!("{}|:hmux-sep-v1:|main|:hmux-sep-v1:|1|:hmux-sep-v1:|/synthetic|:hmux-sep-v1:|bash|:hmux-sep-v1:|80|:hmux-sep-v1:|24|:hmux-sep-v1:|80\n",identity.id),0o600);
        write(
            "metadata",
            &format!("{}|42|%80|80|2|2|80|24|0|0|1|bash\n", identity.id),
            0o600,
        );
        write(
            "metadata-typed",
            &format!("{}|42|%80|80|9|2|80|24|0|0|1|bash\n", identity.id),
            0o600,
        );
        write("typed", "output\n\n» /status\n\nGPT-6-Astra\n", 0o600);
        write(
            "before",
            "output\n\n» Ask Codex to do anything\n\nGPT-6-Astra\n",
            0o600,
        );
        write(
            "after",
            &format!("│ Model: GPT-6-Astra │\n│ Directory: /synthetic │\n│ Session: {ID} │\n"),
            0o600,
        );
        let path = dir.join(format!(
            ".codex/sessions/2026/10/03/rollout-fixture-{ID}.jsonl"
        ));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path,format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ID}\",\"source\":\"cli\"}}}}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            reader: TmuxCatalogReader::new(dir.join("tmux"), None, Duration::from_secs(2)).unwrap(),
            inspector: Inspector::new(dir.clone(), dir.join("ps"), Some(dir.join("lsof"))).unwrap(),
            identity,
            dir,
            path,
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
            stop: CancellationToken::new(),
        }
    }
    fn run(&self) -> Result<(), Error> {
        Probe {
            reporter: None,
            identity: &self.identity,
            pane: 80,
            base: &Binding::unavailable(Provider::Codex, 90),
            reader: &self.reader,
            inspector: &self.inspector,
            state_dir: &self.dir,
            stop: &self.stop,
            deadline: Instant::now() + Duration::from_secs(5),
            runtime: self.runtime.handle(),
        }
        .run()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.dir).unwrap()
    }
}
#[test]
fn probe_sends_once_links_exact_id_and_cools_down_failed_attempts() {
    let _serial = SERIAL.lock().unwrap();
    let f = Fixture::new();
    assert!(f.run().is_ok());
    let link = conversation_link::load(&f.dir, &f.identity)
        .unwrap()
        .unwrap();
    assert!(link.notification_owner().is_none());
    assert!(link
        .resolve(
            "start-one",
            &f.stop,
            Instant::now() + Duration::from_secs(2)
        )
        .is_ok());
    let sent = fs::read_to_string(f.dir.join("sent")).unwrap();
    assert!(sent.contains("-H 2f 73 74 61 74 75 73"));
    assert!(!sent.contains("73 0d"));
    assert!(sent.contains("send-keys -t %80 Enter"));
    assert!(!sent.contains("C-c"));
    fs::remove_file(f.dir.join("sent")).unwrap();
    assert!(f.run().is_err());
    assert!(!f.dir.join("sent").exists());
    let f = Fixture::new();
    fs::write(f.dir.join("change-stamp"), "").unwrap();
    assert!(f.run().is_err());
    assert!(conversation_link::load(&f.dir, &f.identity)
        .unwrap()
        .is_none());
    fs::remove_file(f.dir.join("sent")).unwrap();
    assert!(f.run().is_err());
    assert!(!f.dir.join("sent").exists());
}
#[test]
fn busy_drafts_stale_status_queued_input_and_cancel_do_not_send() {
    let _serial = SERIAL.lock().unwrap();
    for screen in [
        "output\n\n» draft\n\n",
        "• Working (2s • esc to interrupt)\n\n» Ask Codex to do anything\n\n",
        "Session: 01234567-89ab-7cde-8fab-0123456789ab\n\n» Ask Codex to do anything\n\n",
    ] {
        let f = Fixture::new();
        fs::write(f.dir.join("before"), screen).unwrap();
        assert!(f.run().is_err());
        assert!(!f.dir.join("sent").exists());
    }
    let f = Fixture::new();
    let ticket = input_gate::Ticket::new(input_gate::state(&f.identity));
    assert!(f.run().is_err());
    assert!(!f.dir.join("sent").exists());
    drop(ticket);
    f.stop.cancel();
    assert!(f.run().is_err());
    assert!(!f.dir.join("sent").exists());
}
#[test]
fn changed_composer_or_new_input_never_submits_enter() {
    let _serial = SERIAL.lock().unwrap();
    let f = Fixture::new();
    fs::write(f.dir.join("typed"), "output\n\n» /status user draft\n\n").unwrap();
    assert!(f.run().is_err());
    assert!(!fs::read_to_string(f.dir.join("sent"))
        .unwrap()
        .contains(" Enter"));
    let f = Fixture::new();
    std::thread::scope(|scope| {
        let probe = scope.spawn(|| f.run());
        let until = Instant::now() + Duration::from_secs(3);
        while !f.dir.join("sent").exists() {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(2));
        }
        let ticket = input_gate::Ticket::new(input_gate::state(&f.identity));
        assert!(probe.join().unwrap().is_err());
        drop(ticket);
    });
    assert!(!fs::read_to_string(f.dir.join("sent"))
        .unwrap()
        .contains(" Enter"));
    assert!(conversation_link::load(&f.dir, &f.identity)
        .unwrap()
        .is_none());
}

#[test]
fn normal_screen_and_post_status_cursor_change_can_recover() {
    let _serial = SERIAL.lock().unwrap();
    let f = Fixture::new();
    let before = format!("{}|42|%80|80|2|2|80|24|0|0|0|bash\n", f.identity.id);
    let after = format!("{}|42|%80|80|30|5|80|24|0|0|0|bash\n", f.identity.id);
    fs::write(f.dir.join("metadata"), before).unwrap();
    fs::write(f.dir.join("metadata-after"), after).unwrap();
    fs::write(
        f.dir.join("metadata-typed"),
        format!("{}|42|%80|80|9|2|80|24|0|0|0|bash\n", f.identity.id),
    )
    .unwrap();
    assert!(f.run().is_ok());
    assert!(conversation_link::load(&f.dir, &f.identity)
        .unwrap()
        .is_some());
}

#[test]
fn partial_status_render_waits_for_complete_uuid() {
    let _serial = SERIAL.lock().unwrap();
    let f = Fixture::new();
    fs::write(
        f.dir.join("partial"),
        "Model: test\nDirectory: /synthetic\nSession: 01234567-89ab-\n",
    )
    .unwrap();
    let script = fs::read_to_string(f.dir.join("tmux")).unwrap().replace(
        "/bin/cat \"$root/after\"",
        "if [ -f \"$root/partial-seen\" ]; then /bin/cat \"$root/after\"; else printf '' > \"$root/partial-seen\"; /bin/cat \"$root/partial\"; fi",
    );
    fs::write(f.dir.join("tmux"), script).unwrap();
    assert!(f.run().is_ok());
    assert!(f.dir.join("partial-seen").exists());
    assert!(conversation_link::load(&f.dir, &f.identity)
        .unwrap()
        .is_some());
}

#[test]
fn large_screen_capture_and_input_during_status_poll() {
    let _serial = SERIAL.lock().unwrap();
    let f = Fixture::new();
    let mut before = fs::read_to_string(f.dir.join("before")).unwrap();
    before.push_str(&format!("{}\n", "output ".repeat(10)).repeat(80));
    assert!(before.len() > 4096);
    fs::write(f.dir.join("before"), before).unwrap();
    fs::write(
        f.dir.join("metadata"),
        format!("{}|42|%80|80|2|2|80|100|0|0|1|bash\n", f.identity.id),
    )
    .unwrap();
    fs::write(
        f.dir.join("metadata-typed"),
        format!("{}|42|%80|80|9|2|80|100|0|0|1|bash\n", f.identity.id),
    )
    .unwrap();
    assert!(f.run().is_ok());

    let f = Fixture::new();
    // Delay status output after the fixed send. A writer on another HMux view
    // must be able to acquire the gate while polling, and its input cancels repair.
    let script = fs::read_to_string(f.dir.join("tmux")).unwrap().replace(
        "/bin/cat \"$root/after\"",
        "printf '' > \"$root/polling\"; /bin/sleep 0.4; /bin/cat \"$root/after\"",
    );
    fs::write(f.dir.join("tmux"), script).unwrap();
    std::thread::scope(|scope| {
        let probe = scope.spawn(|| f.run());
        let until = Instant::now() + Duration::from_secs(3);
        while !f.dir.join("polling").exists() {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        let input = input_gate::state(&f.identity);
        let gate = input
            .gate
            .try_lock()
            .expect("status poll must not block input");
        let ticket = input_gate::Ticket::new(input.clone());
        drop(gate);
        assert!(probe.join().unwrap().is_err());
        drop(ticket);
    });
    assert!(conversation_link::load(&f.dir, &f.identity)
        .unwrap()
        .is_none());
}

#[test]
fn resize_revealing_an_old_status_panel_never_sends_or_links() {
    let _serial = SERIAL.lock().unwrap();
    let f = Fixture::new();
    let narrow = fs::read_to_string(f.dir.join("metadata"))
        .unwrap()
        .replace("|80|24|", "|62|24|");
    fs::write(f.dir.join("metadata-narrow"), narrow).unwrap();
    let wide = format!(
        "Model: old\nDirectory: /synthetic\nSession: {ID}\n\n» Ask Codex to do anything\n\n"
    );
    fs::write(f.dir.join("before-wide"), wide).unwrap();
    let metadata = fs::read_to_string(f.dir.join("metadata"))
        .unwrap()
        .replace("|2|2|80|", "|2|4|80|");
    fs::write(f.dir.join("metadata-wide"), metadata).unwrap();
    fs::write(f.dir.join("tmux"), r#"#!/bin/sh
root=${0%/*}
case "$1" in
list-sessions) /bin/cat "$root/sessions" ;;
list-windows) /bin/cat "$root/windows" ;;
display-message)
 case "$5" in *window_id*) printf '%s\n' '@1|1|62|24|latest' ;;
 *) if [ -f "$root/widened" ]; then /bin/cat "$root/metadata-wide"; else /bin/cat "$root/metadata-narrow"; fi ;; esac ;;
show-options) exit 0 ;;
capture-pane) if [ -f "$root/widened" ]; then /bin/cat "$root/before-wide"; else /bin/cat "$root/before"; fi ;;
if-shell) case "$6" in *send-keys*) printf '' > "$root/sent" ;; *resize-window*) printf '' > "$root/widened" ;; esac ;;
*) exit 99 ;;
esac
"#).unwrap();
    assert!(f.run().is_err());
    assert!(f.dir.join("widened").exists());
    assert!(!f.dir.join("sent").exists());
    assert!(conversation_link::load(&f.dir, &f.identity)
        .unwrap()
        .is_none());
}

#[test]
fn exact_record_lookup_rejects_duplicates_headers_symlinks_and_cancel() {
    let _serial = SERIAL.lock().unwrap();
    let f = Fixture::new();
    let root = f.inspector.sessions_root();
    let deadline = Instant::now() + Duration::from_secs(5);
    assert_eq!(
        records::find_codex_thread(&root, ID, &f.stop, deadline).unwrap(),
        f.path
    );
    let second = f.path.with_file_name(format!("rollout-second-{ID}.jsonl"));
    fs::copy(&f.path, &second).unwrap();
    assert!(records::find_codex_thread(&root, ID, &f.stop, deadline).is_err());
    fs::remove_file(&second).unwrap();
    let old = f.path.with_extension("old");
    fs::rename(&f.path, &old).unwrap();
    symlink(&old, &f.path).unwrap();
    assert!(records::find_codex_thread(&root, ID, &f.stop, deadline).is_err());
    fs::remove_file(&f.path).unwrap();
    fs::write(
        &f.path,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"other\",\"source\":\"cli\"}}\n",
    )
    .unwrap();
    assert!(records::find_codex_thread(&root, ID, &f.stop, deadline).is_err());
    f.stop.cancel();
    assert!(records::find_codex_thread(&root, ID, &f.stop, deadline).is_err());
}

#[test]
#[ignore = "requires HMUX_TEST_TMUX and HMUX_TEST_PYTHON; isolated tmux with synthetic provider"]
fn isolated_real_tmux_status_send_and_exact_link() {
    let _serial = SERIAL.lock().unwrap();
    let mut f = Fixture::new();
    let executable = PathBuf::from(std::env::var_os("HMUX_TEST_TMUX").unwrap());
    let python = PathBuf::from(std::env::var_os("HMUX_TEST_PYTHON").unwrap());
    assert!(executable.is_absolute() && python.is_absolute());
    struct Server {
        executable: PathBuf,
        socket: PathBuf,
    }
    impl Server {
        fn run(&self, args: &[&str]) -> std::process::Output {
            std::process::Command::new(&self.executable)
                .arg("-S")
                .arg(&self.socket)
                .args(["-f", "/dev/null"])
                .args(args)
                .env_clear()
                .env("HOME", self.socket.parent().unwrap())
                .env("PATH", "/usr/bin:/bin")
                .env("SHELL", "/bin/sh")
                .env("TERM", "xterm-256color")
                .output()
                .unwrap()
        }
        fn text(&self, args: &[&str]) -> String {
            let out = self.run(args);
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.run(&["kill-server"]);
        }
    }
    let server = Server {
        executable: executable.clone(),
        socket: f.dir.join("isolated.sock"),
    };
    let script = f.dir.join("tui.py");
    fs::write(&script, format!(r#"import os,sys,tty,time
from pathlib import Path
tty.setraw(0)
sys.stdout.write('\x1b[?1049h\x1b[2J\x1b[3;1H» Ask Codex to do anything\x1b[3;3H');sys.stdout.flush()
data=b''
while not data.endswith(b'\r'):
    data+=os.read(0,1)
    if data==b'/status':
        sys.stdout.write('\x1b[3;1H\x1b[2K» /status\x1b[3;10H');sys.stdout.flush()
Path(__file__).with_name('received').write_bytes(data)
sys.stdout.write('\x1b[2J\x1b[HModel: synthetic\r\nDirectory: /synthetic\r\nSession: {ID}\r\n\r\n» Ask Codex to do anything\x1b[5;3H');sys.stdout.flush()
while True:
    data+=os.read(0,1)
    Path(__file__).with_name('received').write_bytes(data)
"#)).unwrap();
    server.text(&[
        "new-session",
        "-d",
        "-x",
        "62",
        "-y",
        "30",
        "-s",
        "hmux-e2e-status",
        python.to_str().unwrap(),
        "-u",
        script.to_str().unwrap(),
    ]);
    let lifetime = server.text(&[
        "display-message",
        "-p",
        "-t",
        "hmux-e2e-status",
        "#{session_id}|#{session_created}|#{pane_pid}",
    ]);
    let fields: Vec<_> = lifetime.trim().split('|').collect();
    f.identity = SessionIdentity {
        id: fields[0].into(),
        created_at: fields[1].parse().unwrap(),
    };
    let pane_pid: i32 = fields[2].parse().unwrap();
    // Only the provider process graph is synthetic; metadata, capture and guarded
    // key delivery use the actual disposable tmux server and raw-mode TUI.
    fs::write(f.dir.join("ps"), format!("#!/bin/sh\nroot=${{0%/*}}\nif [ \"$1\" = -p ]; then if [ \"$4\" = stat= ]; then printf '%s\\n' 'S+'; else /bin/cat \"$root/stamp\"; fi; else printf '%s\\n' '{pane_pid} 1 S 0.0 python' '90 {pane_pid} S+ 0.0 codex'; fi\n")).unwrap();
    f.reader = TmuxCatalogReader::new(
        executable,
        Some(crate::catalog::TmuxSocket::Path(server.socket.clone())),
        Duration::from_secs(2),
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let raw = server.text(&["display-message", "-p", "-t", &f.identity.id, FORMAT]);
        let screen = server.text(&["capture-pane", "-p", "-t", &f.identity.id]);
        if Pane::parse(raw.as_bytes(), &f.identity, pane_pid)
            .is_ok_and(|p| empty_prompt(&screen, &p))
        {
            break;
        }
        assert!(Instant::now() < until, "synthetic TUI did not become ready");
        std::thread::sleep(Duration::from_millis(30));
    }
    Probe {
        reporter: None,
        identity: &f.identity,
        pane: pane_pid,
        base: &Binding::unavailable(Provider::Codex, 90),
        reader: &f.reader,
        inspector: &f.inspector,
        state_dir: &f.dir,
        stop: &f.stop,
        deadline: Instant::now() + Duration::from_secs(5),
        runtime: f.runtime.handle(),
    }
    .run()
    .unwrap();
    assert_eq!(fs::read(f.dir.join("received")).unwrap(), b"/status\r");
    assert_eq!(
        server
            .text(&[
                "display-message",
                "-p",
                "-t",
                &f.identity.id,
                "#{window_width}"
            ])
            .trim(),
        "62"
    );
    assert!(server
        .text(&[
            "show-options",
            "-wqv",
            "-t",
            &f.identity.id,
            "@hmux_status_probe"
        ])
        .trim()
        .is_empty());
    assert!(server
        .text(&["show-options", "-wqv", "-t", &f.identity.id, "window-size"])
        .trim()
        .is_empty());
    let link = conversation_link::load(&f.dir, &f.identity)
        .unwrap()
        .unwrap();
    assert!(link.notification_owner().is_none());
    assert_eq!(
        link.resolve(
            "start-one",
            &f.stop,
            Instant::now() + Duration::from_secs(2)
        )
        .unwrap()
        .record_id,
        ID
    );
    // Cleanup runs independently of request cancellation; explicit manual and
    // automatic policies retain their local/inherited option semantics.
    let base = Binding::unavailable(Provider::Codex, 90);
    let cleanup_stop = CancellationToken::new();
    let probe = Probe {
        reporter: None,
        identity: &f.identity,
        pane: pane_pid,
        base: &base,
        reader: &f.reader,
        inspector: &f.inspector,
        state_dir: &f.dir,
        stop: &cleanup_stop,
        deadline: Instant::now() + Duration::from_secs(5),
        runtime: f.runtime.handle(),
    };
    let target = f.reader.terminal_target().unwrap();
    for policy in ["manual", "latest"] {
        server.text(&[
            "set-option",
            "-w",
            "-t",
            &f.identity.id,
            "window-size",
            policy,
        ]);
        let original = probe.metadata(&target, &f.identity.id).unwrap();
        let lease = size::Width::widen(probe, &target, &original).unwrap();
        assert_eq!(
            probe.metadata(&target, &f.identity.id).unwrap().fields[6],
            "80"
        );
        drop(lease);
        assert_eq!(
            probe.metadata(&target, &f.identity.id).unwrap().fields[6],
            "62"
        );
        assert_eq!(
            server
                .text(&["show-options", "-wqv", "-t", &f.identity.id, "window-size"])
                .trim(),
            policy
        );
    }
    let original = probe.metadata(&target, &f.identity.id).unwrap();
    let window = server
        .text(&[
            "display-message",
            "-p",
            "-t",
            &f.identity.id,
            "#{window_id}",
        ])
        .trim()
        .to_owned();
    let selector = format!("{}:{}.{}", f.identity.id, window, original.id);
    let lease = size::Width::widen(probe, &target, &original).unwrap();
    server.text(&[
        "new-window",
        "-t",
        &f.identity.id,
        "-n",
        "hmux-e2e-other",
        "/bin/sh",
    ]);
    drop(lease);
    assert_eq!(
        server
            .text(&["display-message", "-p", "-t", &selector, "#{window_width}"])
            .trim(),
        "62"
    );
    assert!(server
        .text(&["show-options", "-wqv", "-t", &window, "@hmux_status_probe"])
        .trim()
        .is_empty());
    server.text(&["select-window", "-t", &window]);
    let original = probe.metadata(&target, &f.identity.id).unwrap();
    let lease = size::Width::widen(probe, &target, &original).unwrap();
    // An observable external resize wins; cleanup only removes its own marker.
    server.text(&["resize-window", "-t", &f.identity.id, "-x", "90"]);
    drop(lease);
    assert_eq!(
        probe.metadata(&target, &f.identity.id).unwrap().fields[6],
        "90"
    );
    assert_eq!(
        server
            .text(&["show-options", "-wqv", "-t", &f.identity.id, "window-size"])
            .trim(),
        "manual"
    );
    server.text(&["resize-window", "-t", &f.identity.id, "-x", "62"]);
    server.text(&["set-option", "-wu", "-t", &f.identity.id, "window-size"]);
    let original = probe.metadata(&target, &f.identity.id).unwrap();
    let lease = size::Width::widen(probe, &target, &original).unwrap();
    cleanup_stop.cancel();
    drop(lease);
    assert_eq!(
        server
            .text(&[
                "display-message",
                "-p",
                "-t",
                &f.identity.id,
                "#{window_width}"
            ])
            .trim(),
        "62"
    );
    assert!(server
        .text(&["show-options", "-wqv", "-t", &f.identity.id, "window-size"])
        .trim()
        .is_empty());
    assert!(server
        .text(&[
            "show-options",
            "-wqv",
            "-t",
            &f.identity.id,
            "@hmux_status_probe"
        ])
        .trim()
        .is_empty());
    // An expired lifetime guard cannot deliver another key to the same pane.
    let raw = server.text(&["display-message", "-p", "-t", &f.identity.id, FORMAT]);
    let pane = Pane::parse(raw.as_bytes(), &f.identity, pane_pid).unwrap();
    let original_guard = pane.guard();
    let guard = original_guard.replace(
        &format!("session_created}},{}}}", f.identity.created_at),
        "session_created},1}",
    );
    assert_ne!(guard, original_guard);
    server.text(&[
        "if-shell",
        "-F",
        "-t",
        &f.identity.id,
        &guard,
        &format!("send-keys -t {} -H 78", pane.id),
    ]);
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(fs::read(f.dir.join("received")).unwrap(), b"/status\r");
}
