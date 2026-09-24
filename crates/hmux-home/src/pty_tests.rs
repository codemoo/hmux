use super::*;
use std::{ffi::OsString, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
    time::timeout,
};

static TEST_PTY: Mutex<()> = Mutex::const_new(());
fn sh(script: &str, cols: u16, rows: u16) -> Result<Session, Error> {
    spawn(
        Path::new("/bin/sh"),
        &[OsString::from("-c"), OsString::from(script)],
        cols,
        rows,
    )
}
async fn until<R: AsyncRead + Unpin>(reader: &mut R, marker: &[u8]) -> Vec<u8> {
    timeout(Duration::from_secs(3), async {
        let mut out = Vec::new();
        let mut chunk = [0u8; 128];
        loop {
            let n = reader.read(&mut chunk).await.unwrap();
            assert!(n > 0 && out.len() + n <= 8192);
            out.extend_from_slice(&chunk[..n]);
            if out.windows(marker.len()).any(|part| part == marker) {
                return out;
            }
        }
    })
    .await
    .unwrap()
}
async fn slots(expected: usize) {
    timeout(Duration::from_secs(3), async {
        loop {
            let available = PTY_SLOTS
                .get()
                .map_or(SLOTS, |slot| slot.available_permits());
            if available == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn controlling_tty_korean_input_resize_and_normal_exit() {
    let _serial = TEST_PTY.lock().await;
    let mut session = sh("stty -echo; test -t 0 && printf 'TTY:YES\\n'; stty size; printf 'READY\\n'; IFS= read -r line; printf 'INPUT:%s\\n' \"$line\"; stty size; printf 'DONE\\n'", 80, 24).unwrap();
    assert!(session.pid() > 1);
    assert_eq!(format!("{session:?}"), "Session([redacted])");
    let first = until(&mut session, b"READY\r\n").await;
    let first = String::from_utf8(first).unwrap();
    assert!(
        first.contains("TTY:YES") && first.contains("24 80"),
        "{first:?}"
    );
    session.resize(100, 40).unwrap();
    assert_eq!(session.resize(1, 40), Err(Error::Invalid));
    session
        .write_all("synthetic 한글\n".as_bytes())
        .await
        .unwrap();
    let second = until(&mut session, b"DONE\r\n").await;
    let second = String::from_utf8(second).unwrap();
    assert!(
        second.contains("INPUT:synthetic 한글") && second.contains("40 100"),
        "{second:?}"
    );
    assert_eq!(session.wait().await, Completion::Exited(0));
    slots(SLOTS).await;
}

#[tokio::test(flavor = "current_thread")]
async fn raw_binary_input_and_output_are_unbuffered() {
    let _serial = TEST_PTY.lock().await;
    let mut session = sh(
        "stty raw -echo; printf 'BIN:'; dd bs=1 count=4 2>/dev/null",
        80,
        24,
    )
    .unwrap();
    until(&mut session, b"BIN:").await;
    let bytes = [0u8, 0xff, 0x80, 0x0a];
    session.write_all(&bytes).await.unwrap();
    let mut echoed = [0u8; 4];
    timeout(Duration::from_secs(3), session.read_exact(&mut echoed))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(echoed, bytes);
    assert_eq!(session.wait().await, Completion::Exited(0));
    slots(SLOTS).await;
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_read_closes_and_reaps_only_owned_child() {
    let _serial = TEST_PTY.lock().await;
    let mut session = sh("stty -echo; printf 'READY\\n'; IFS= read -r line", 80, 24).unwrap();
    until(&mut session, b"READY\r\n").await;
    let mut byte = [0u8; 1];
    assert!(timeout(Duration::from_millis(50), session.read(&mut byte))
        .await
        .is_err());
    let completion = timeout(Duration::from_secs(3), session.close())
        .await
        .unwrap();
    assert!(matches!(
        completion,
        Completion::Signaled(_) | Completion::Exited(_)
    ));
    slots(SLOTS).await;
}

#[tokio::test(flavor = "current_thread")]
async fn master_and_waiter_lifetimes_both_hold_admission() {
    let _serial = TEST_PTY.lock().await;
    let session = sh("exit 7", 80, 24).unwrap();
    let (master, child) = session.into_parts();
    assert!(child.pid() > 1);
    assert_eq!(child.wait().await, Completion::Exited(7));
    slots(SLOTS - 1).await;
    drop(master);
    slots(SLOTS).await;
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_owner_and_aborted_waiter_reap_before_slot_reuse() {
    let _serial = TEST_PTY.lock().await;
    let session = sh("stty -echo; printf 'READY\\n'; IFS= read -r line", 80, 24).unwrap();
    drop(session);
    slots(SLOTS).await;

    let session = sh("stty -echo; printf 'READY\\n'; IFS= read -r line", 80, 24).unwrap();
    let (mut master, child) = session.into_parts();
    until(&mut master, b"READY\r\n").await;
    let wait = tokio::spawn(child.wait());
    wait.abort();
    assert!(wait.await.is_err());
    slots(SLOTS - 1).await;
    drop(master);
    slots(SLOTS).await;
}

#[tokio::test(flavor = "current_thread")]
async fn eight_slots_are_global_immediate_and_reclaimed() {
    let _serial = TEST_PTY.lock().await;
    let mut sessions = Vec::new();
    for _ in 0..SLOTS {
        sessions.push(sh("stty -echo; IFS= read -r line", 80, 24).unwrap());
    }
    slots(0).await;
    assert!(matches!(sh("exit 0", 80, 24), Err(Error::Busy)));
    drop(sessions.pop());
    slots(1).await;
    let replacement = sh("exit 0", 80, 24).unwrap();
    assert_eq!(replacement.wait().await, Completion::Exited(0));
    drop(sessions);
    slots(SLOTS).await;
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_size_or_argv_never_spawns_and_spawn_error_releases_slot() {
    let _serial = TEST_PTY.lock().await;
    let program = Path::new("/bin/sh");
    let args = [OsString::from("-c"), OsString::from("exit 0")];
    for (cols, rows) in [(0, 24), (1, 24), (501, 24), (80, 1), (80, 251)] {
        assert!(matches!(
            spawn(program, &args, cols, rows),
            Err(Error::Invalid)
        ));
    }
    assert!(matches!(
        spawn(Path::new("bin/sh"), &args, 80, 24),
        Err(Error::Invalid)
    ));
    assert!(matches!(
        spawn(
            program,
            &[OsString::from("x".repeat(MAX_ARG_BYTES + 1))],
            80,
            24
        ),
        Err(Error::Invalid)
    ));
    assert!(matches!(
        spawn(Path::new("/this-does-not-exist/hmux-pty-test"), &[], 80, 24),
        Err(Error::Spawn)
    ));
    slots(SLOTS).await;
}

#[tokio::test(flavor = "current_thread")]
async fn environment_is_scrubbed_even_when_inherited() {
    const MARKER: &str = "HMUX_PTY_ENV_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pty::tests::environment_is_scrubbed_even_when_inherited",
                "--nocapture",
            ])
            .env(MARKER, "1")
            .env("TMUX", "synthetic-parent-tmux")
            .env("TMUX_PANE", "%999")
            .env("TERM", "synthetic-parent-term")
            .env("COLORTERM", "synthetic-parent-color")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _serial = TEST_PTY.lock().await;
    let mut session = sh("printf 'ENV:%s:%s:%s:%s\\n' \"${TMUX-unset}\" \"${TMUX_PANE-unset}\" \"$TERM\" \"$COLORTERM\"", 80, 24).unwrap();
    let output = until(&mut session, b"truecolor\r\n").await;
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("ENV:unset:unset:xterm-256color:truecolor"));
    assert_eq!(session.wait().await, Completion::Exited(0));
    slots(SLOTS).await;
}

#[tokio::test(flavor = "current_thread")]
async fn owned_halves_allow_concurrent_io_resize_and_keep_lease_until_both_drop() {
    let _serial = TEST_PTY.lock().await;
    let session = sh(
        "stty -echo; printf 'READY\\n'; IFS= read -r line; stty size; printf 'INPUT:%s\\n' \"$line\"",
        80,
        24,
    )
    .unwrap();
    let (master, child) = session.into_parts();
    let (mut read, mut write) = master.into_split();
    until(&mut read, b"READY\r\n").await;
    write.resize(110, 45).unwrap();
    write.write_all(b"split-input\n").await.unwrap();
    let output = String::from_utf8(until(&mut read, b"INPUT:split-input\r\n").await).unwrap();
    assert!(output.contains("45 110"), "{output:?}");
    assert_eq!(child.wait().await, Completion::Exited(0));
    drop(read);
    slots(SLOTS - 1).await;
    drop(write);
    slots(SLOTS).await;
}
