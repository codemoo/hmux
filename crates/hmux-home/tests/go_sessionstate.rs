//! Opt-in Go↔Rust session-state oracle using a retained external test binary.
//! Historical source/provenance is documented in tests/RUST.md; no live tmux is used.
use hmux_core::PrivateDir;
use hmux_home::sessionstate::{Error, Store};
use hmux_model::{Catalog, Session, SessionIdentity};
use std::{
    ffi::OsStr,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

const CHILD_TIMEOUT: Duration = Duration::from_secs(10);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "hmux-e2e-sessionstate-{}-{nanos}",
                std::process::id()
            ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self(root)
    }
    fn lock(&self, name: &str) -> PathBuf {
        self.0.join("sessions").join(name)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl OwnedChild {
    fn first_line(&mut self) -> String {
        let output = self.0.stdout.take().unwrap();
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let mut reader = BufReader::new(output);
            let result = reader.read_line(&mut line).map(|_| line);
            let _ = send.send(result);
            let _ = io::copy(&mut reader, &mut io::sink());
        });
        receive
            .recv_timeout(CHILD_TIMEOUT)
            .expect("Go oracle reply timeout")
            .unwrap()
    }
    fn wait(&mut self) {
        let start = Instant::now();
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success(), "Go oracle exited with {status}");
                return;
            }
            assert!(start.elapsed() < CHILD_TIMEOUT, "Go oracle timeout");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn spawn(
    helper: &Path,
    fixture: &Fixture,
    mode: &str,
    lock: Option<&str>,
    hold: bool,
) -> OwnedChild {
    let mut command = Command::new(helper);
    command
        .args([
            "-test.run",
            "^TestRustSessionStateOracle$",
            "-test.timeout",
            "15s",
        ])
        .env_clear()
        .env("HOME", &fixture.0)
        .env("TMPDIR", &fixture.0)
        .env("PATH", "/usr/bin:/bin")
        .env("GORACE", "atexit_sleep_ms=0")
        .env("HMUX_GO_SESSIONSTATE_ORACLE", "1")
        .env("HMUX_GO_SESSIONSTATE_DIR", &fixture.0)
        .env("HMUX_GO_SESSIONSTATE_MODE", mode)
        .current_dir(&fixture.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .stdin(if hold { Stdio::piped() } else { Stdio::null() });
    if let Some(lock) = lock {
        command.env("HMUX_GO_SESSIONSTATE_LOCK", lock);
    }
    OwnedChild(command.spawn().unwrap())
}
fn run(helper: &Path, fixture: &Fixture, mode: &str, lock: Option<&str>) -> String {
    let mut child = spawn(helper, fixture, mode, lock, false);
    child.wait();
    let mut output = String::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    output.lines().next().unwrap_or_default().to_owned()
}
fn live() -> Session {
    Session {
        id: "$7".into(),
        name: "native".into(),
        created_at: 1_700_000_000,
        ..Session::default()
    }
}
fn identity(session: &Session) -> SessionIdentity {
    SessionIdentity {
        id: session.id.clone(),
        created_at: session.created_at,
    }
}
fn catalog() -> Catalog {
    Catalog {
        sessions: Some(vec![
            live(),
            Session {
                id: "$8".into(),
                name: "sibling".into(),
                created_at: 1_700_000_001,
                ..Session::default()
            },
        ]),
        ..Catalog::default()
    }
}
fn overlay(store: &Store) -> Vec<Session> {
    let mut value = catalog();
    store.apply(&mut value).unwrap();
    store.apply_visibility(&mut value).unwrap();
    value.sessions.unwrap()
}

#[test]
#[ignore = "requires HMUX_GO_SESSIONSTATE_HELPER, a Go -race test binary"]
fn go_and_rust_share_current_session_state_and_persistent_locks() {
    let helper = PathBuf::from(
        std::env::var_os("HMUX_GO_SESSIONSTATE_HELPER").expect("Go oracle binary required"),
    );
    assert!(helper.is_absolute() && helper.is_file());
    let fixture = Fixture::new();
    let store = Store::new(fixture.0.clone());
    let live = live();
    let expected = identity(&live);
    let deadline = || Instant::now() + Duration::from_secs(20);

    assert_eq!(run(&helper, &fixture, "seed", None), "seeded");
    let sessions_inode = fs::metadata(fixture.lock("sessions.lock")).unwrap().ino();
    let visibility_inode = fs::metadata(fixture.lock("visibility.lock")).unwrap().ino();
    let initial = overlay(&store);
    assert_eq!(initial[0].alias, "go-seed");
    assert_eq!(initial[0].profile, "codex");
    assert_eq!(initial[0].label, "Codex");
    assert_eq!(
        initial[0].tags.as_deref(),
        Some(["ai".into(), "codex".into()].as_slice())
    );
    assert!(initial[0].hidden);
    assert_eq!(initial[1].alias, "sibling-alias");

    // These Go try-lock calls run inside Rust's resolver callbacks, proving
    // that live identity lookup occurs while each transaction lock is held.
    store
        .set_alias_expected(
            &expected,
            "rust-alias",
            CancellationToken::new(),
            deadline(),
            || {
                assert_eq!(
                    run(&helper, &fixture, "try-lock", Some("sessions.lock")),
                    "busy"
                );
                Ok(live.clone())
            },
        )
        .unwrap();
    store
        .set_hidden_expected(
            &expected,
            false,
            CancellationToken::new(),
            deadline(),
            || {
                assert_eq!(
                    run(&helper, &fixture, "try-lock", Some("visibility.lock")),
                    "busy"
                );
                Ok(live.clone())
            },
        )
        .unwrap();
    let changed = overlay(&store);
    assert_eq!(changed[0].alias, "rust-alias");
    assert!(!changed[0].hidden);
    assert_eq!(changed[1].alias, "sibling-alias");

    let stale = SessionIdentity {
        id: live.id.clone(),
        created_at: live.created_at - 1,
    };
    assert_eq!(
        store.set_alias_expected(
            &stale,
            "stale",
            CancellationToken::new(),
            deadline(),
            || Ok(live.clone())
        ),
        Err(Error::Changed)
    );
    assert_eq!(
        store.set_hidden_expected(&stale, true, CancellationToken::new(), deadline(), || Ok(
            live.clone()
        )),
        Err(Error::Changed)
    );

    let dir = PrivateDir::open(&fixture.0.join("sessions")).unwrap();
    for name in ["sessions.lock", "visibility.lock"] {
        // Go-held lock excludes Rust; Rust-held lock excludes Go.
        let mut go = spawn(&helper, &fixture, "hold-lock", Some(name), true);
        assert_eq!(go.first_line(), "locked\n");
        assert!(dir.try_lock(OsStr::new(name)).unwrap().is_none());
        go.0.stdin.take().unwrap().write_all(b"\n").unwrap();
        go.wait();
        let rust = dir.try_lock(OsStr::new(name)).unwrap().unwrap();
        assert_eq!(run(&helper, &fixture, "try-lock", Some(name)), "busy");
        drop(rust);
        assert_eq!(run(&helper, &fixture, "try-lock", Some(name)), "locked");
    }

    assert_eq!(run(&helper, &fixture, "check-update", None), "checked");
    let final_state = overlay(&store);
    assert_eq!(final_state[0].alias, "go-final");
    assert!(final_state[0].hidden);
    assert_eq!(final_state[0].profile, "codex");
    assert_eq!(final_state[1].alias, "sibling-alias");
    assert_eq!(
        fs::metadata(fixture.lock("sessions.lock")).unwrap().ino(),
        sessions_inode
    );
    assert_eq!(
        fs::metadata(fixture.lock("visibility.lock")).unwrap().ino(),
        visibility_inode
    );
}
