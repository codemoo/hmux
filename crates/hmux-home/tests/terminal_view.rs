//! Opt-in real PTY/tmux handoff. Every session, socket and process is disposable.
use hmux_core::command::CommandRunner;
use hmux_home::{catalog::TmuxSocket, pty, view};
use hmux_model::SessionIdentity;
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    process::{Command, Output},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

struct Server {
    root: PathBuf,
    tmux: PathBuf,
    socket: PathBuf,
}
impl Server {
    fn new() -> Self {
        let tmux = PathBuf::from(
            std::env::var_os("HMUX_TEST_TMUX").expect("explicit executable required"),
        );
        assert!(tmux.is_absolute());
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let nonce = nonce.iter().map(|v| format!("{v:02x}")).collect::<String>();
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("hmux-e2e-pty-{}-{nonce}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self {
            socket: root.join("socket"),
            root,
            tmux,
        }
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(&self.tmux)
            .arg("-S")
            .arg(&self.socket)
            .args(["-f", "/dev/null"])
            .args(args)
            .env_clear()
            .env("HOME", &self.root)
            .env("PATH", "/usr/bin:/bin")
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .output()
            .unwrap()
    }
    fn value(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(output.status.success(), "isolated tmux command failed");
        String::from_utf8(output.stdout).unwrap().trim().into()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.run(&["kill-server"]);
        let _ = fs::remove_dir_all(&self.root);
    }
}
async fn until(session: &mut pty::Session, marker: &[u8]) {
    timeout(Duration::from_secs(5), async {
        let mut seen = Vec::new();
        let mut bytes = [0u8; 4096];
        loop {
            let n = session.read(&mut bytes).await.unwrap();
            assert!(n > 0);
            seen.extend_from_slice(&bytes[..n]);
            if seen.windows(marker.len()).any(|part| part == marker) {
                return;
            }
            assert!(seen.len() < 256 * 1024, "bounded synthetic terminal output");
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires HMUX_TEST_TMUX; isolated real PTY/tmux handoff only"]
async fn attached_view_io_resize_close_preserve_original_and_other_client() {
    let server = Server::new();
    assert!(server.run(&["new-session","-d","-s","hmux-e2e-original","stty -echo; printf 'HMUX_READY\\n'; while IFS= read -r line; do printf 'HMUX_INPUT:%s\\n' \"$line\"; done"]).status.success());
    let identity = server.value(&[
        "display-message",
        "-p",
        "-t",
        "hmux-e2e-original",
        "#{session_id} #{session_created}",
    ]);
    let fields = identity.split_whitespace().collect::<Vec<_>>();
    let original = SessionIdentity {
        id: fields[0].into(),
        created_at: fields[1].parse().unwrap(),
    };
    let provider_pid = server.value(&["display-message", "-p", "-t", &original.id, "#{pane_pid}"]);
    // This original client belongs to this test, never to a user's session.
    let args = [
        OsString::from("-S"),
        server.socket.clone().into_os_string(),
        OsString::from("attach-session"),
        OsString::from("-t"),
        OsString::from(&original.id),
    ];
    let mut other = pty::spawn(&server.tmux, &args, 80, 24).unwrap();
    until(&mut other, b"HMUX_READY").await;
    let view = view::open(
        view::Target::new(
            server.tmux.clone(),
            Some(TmuxSocket::Path(server.socket.clone())),
        )
        .unwrap(),
        CommandRunner::new(2).unwrap(),
        original.clone(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (exe, args) = view.attach_command();
    let view_name = args.last().unwrap().to_str().unwrap().to_owned();
    let mut terminal = pty::spawn(&exe, &args, 80, 24).unwrap();
    until(&mut terminal, b"HMUX_READY").await;
    assert_eq!(
        server.value(&[
            "display-message",
            "-p",
            "-t",
            &original.id,
            "#{session_attached}"
        ]),
        "1"
    );
    terminal
        .write_all("synthetic-한글\n".as_bytes())
        .await
        .unwrap();
    until(&mut terminal, "HMUX_INPUT:synthetic-한글".as_bytes()).await;
    terminal.resize(100, 40).unwrap();
    timeout(Duration::from_secs(3), async {
        while server.value(&[
            "list-clients",
            "-t",
            &view_name,
            "-F",
            "#{client_width} #{client_height}",
        ]) != "100 40"
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    hmux_home::refresh::run(
        &view,
        &CommandRunner::new(2).unwrap(),
        terminal.pid(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    timeout(Duration::from_secs(3), terminal.close())
        .await
        .unwrap();
    // The detach hook can win this race; explicit close must still be safe.
    view.close().await.unwrap();
    assert!(!server
        .run(&["has-session", "-t", &view_name])
        .status
        .success());
    assert_eq!(
        server.value(&[
            "display-message",
            "-p",
            "-t",
            &original.id,
            "#{session_attached}"
        ]),
        "1"
    );
    assert_eq!(
        server.value(&["display-message", "-p", "-t", &original.id, "#{pane_pid}"]),
        provider_pid
    );
    other.write_all(b"still-running\n").await.unwrap();
    until(&mut other, b"HMUX_INPUT:still-running").await;
    timeout(Duration::from_secs(3), other.close())
        .await
        .unwrap();
    assert!(server
        .run(&["has-session", "-t", &original.id])
        .status
        .success());
}
