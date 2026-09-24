use super::*;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::{sync::Mutex, time::timeout};

static NEXT: AtomicU64 = AtomicU64::new(0);
static SERIAL: Mutex<()> = Mutex::const_new(());
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-view-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let script = root.join("tmux");
        fs::write(
            &script,
            r#"#!/usr/bin/python3
import sys,json,pathlib,time
root=pathlib.Path(__file__).parent
args=sys.argv[1:]
with (root/'calls').open('a') as f: f.write(json.dumps(args)+'\n')
if args[0] in ['-S','-L']: args=args[2:]
mode=(root/'mode').read_text() if (root/'mode').exists() else ''
command=args[0]
if command=='display-message':
 n=int((root/'checks').read_text()) if (root/'checks').exists() else 0
 (root/'checks').write_text(str(n+1))
 print(999 if mode=='changed' and n>0 else 123)
elif command=='new-session':
 (root/'started').touch()
 if mode=='collision': sys.exit(1)
 name=args[args.index('-s')+1]
 nonce=args[args.index('-e')+1].split('=',1)[1]
 (root/'owner').write_text(nonce)
 (root/'name').write_text(name)
 if mode=='create-wait': time.sleep(30)
 if mode=='partial': sys.exit(1)
 (root/'marker').touch()
elif command=='set-hook':
 (root/'hook').write_text(args[-1])
elif command=='if-shell':
 (root/'cleanup').touch()
 if mode=='cleanup-wait':
  while not (root/'release').exists(): time.sleep(0.01)
 if mode=='missing':
  print("can't find session: "+args[3],file=sys.stderr); sys.exit(1)
 if mode=='cleanup-fail':
  print('permission denied',file=sys.stderr); sys.exit(1)
 if (root/'owner').exists() and (root/'owner').read_text() in args[4]:
  (root/'owner').unlink()
else: sys.exit(2)
"#,
        )
        .unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
        Self(root)
    }
    fn target(&self) -> Target {
        Target::new(
            self.0.join("tmux"),
            Some(TmuxSocket::Path(self.0.join("socket"))),
        )
        .unwrap()
    }
    fn mode(&self, mode: &str) {
        fs::write(self.0.join("mode"), mode).unwrap();
    }
    fn calls(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.0.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    async fn wait(&self, file: &str) {
        timeout(Duration::from_secs(4), async {
            while !self.0.join(file).exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn open(&self, stop: CancellationToken) -> Result<OwnedView, Error> {
        open(
            self.target(),
            CommandRunner::new(1).unwrap(),
            session(),
            stop,
        )
        .await
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn session() -> SessionIdentity {
    SessionIdentity {
        id: "$1".into(),
        created_at: 123,
    }
}
async fn slots_returned() {
    timeout(Duration::from_secs(4), async {
        while VIEW_SLOTS.get().unwrap().available_permits() != hmux_protocol::wire::MAX_TERMINALS {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn identities_are_fresh_and_conditions_require_exact_ownership() {
    let first = Identity::new().unwrap();
    let second = Identity::new().unwrap();
    assert_ne!(first.name, second.name);
    assert_eq!(first.nonce.len(), 24);
    assert!(first.name.starts_with("hmux-app-view-"));
    assert!(first
        .name
        .bytes()
        .all(|v| v.is_ascii_alphanumeric() || v == b'-'));
    let condition = first.condition(true);
    for field in [
        &first.name,
        &first.nonce,
        "#{session_name}",
        "#{HMUX_VIEW_OWNER}",
        "#{session_attached}",
    ] {
        assert!(condition.contains(field));
    }
    assert!(!first.condition(false).contains("session_attached"));
}

#[tokio::test]
async fn setup_attach_close_and_drop_preserve_original() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let view = f.open(CancellationToken::new()).await.unwrap();
    let (exe, args) = view.attach_command();
    assert_eq!(exe, f.0.join("tmux"));
    assert_eq!(args[0], "-S");
    assert_eq!(args[1], f.0.join("socket"));
    assert_eq!(args[2], "attach-session");
    assert!(!args.iter().any(|v| v == "-d"));
    assert_eq!(format!("{view:?}"), "OwnedView([redacted])");
    assert!(fs::read_to_string(f.0.join("hook"))
        .unwrap()
        .contains("session_attached"));
    view.close().await.unwrap();
    slots_returned().await;
    assert!(!f.0.join("owner").exists());
    let calls = f.calls();
    assert_eq!(
        calls.iter().map(|v| v[2].as_str()).collect::<Vec<_>>(),
        [
            "display-message",
            "new-session",
            "display-message",
            "set-hook",
            "if-shell"
        ]
    );
    for args in calls {
        if args[2] == "if-shell" {
            assert!(!args.last().unwrap().contains("$1"));
        }
    }
    let f = Fixture::new();
    drop(f.open(CancellationToken::new()).await.unwrap());
    f.wait("cleanup").await;
    slots_returned().await;
    assert!(!f.0.join("owner").exists());
}

#[tokio::test]
async fn setup_failures_cleanup_only_owned_views_and_missing_is_idempotent() {
    let _serial = SERIAL.lock().await;
    for (mode, expected) in [
        ("changed", Error::Changed),
        ("partial", Error::Command),
        ("collision", Error::Command),
    ] {
        let f = Fixture::new();
        f.mode(mode);
        assert_eq!(
            f.open(CancellationToken::new()).await.unwrap_err(),
            expected
        );
        assert!(f.0.join("cleanup").exists());
        if mode == "partial" {
            assert!(!f.0.join("marker").exists());
        }
        assert!(!f.0.join("owner").exists());
        slots_returned().await;
    }
    let f = Fixture::new();
    let view = f.open(CancellationToken::new()).await.unwrap();
    f.mode("missing");
    view.close().await.unwrap();
    slots_returned().await;
    let f = Fixture::new();
    let view = f.open(CancellationToken::new()).await.unwrap();
    fs::write(f.0.join("owner"), "foreign-owner").unwrap();
    view.close().await.unwrap();
    slots_returned().await;
    assert_eq!(
        fs::read_to_string(f.0.join("owner")).unwrap(),
        "foreign-owner"
    );
}

#[tokio::test]
async fn abort_during_creation_waits_for_cleanup_before_releasing_slot() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    f.mode("create-wait");
    let work = tokio::spawn(open(
        f.target(),
        CommandRunner::new(1).unwrap(),
        session(),
        CancellationToken::new(),
    ));
    f.wait("started").await;
    f.mode("cleanup-wait");
    work.abort();
    let _ = work.await;
    f.wait("cleanup").await;
    assert_eq!(
        VIEW_SLOTS.get().unwrap().available_permits(),
        hmux_protocol::wire::MAX_TERMINALS - 1
    );
    fs::write(f.0.join("release"), "").unwrap();
    slots_returned().await;
    assert!(!f.0.join("owner").exists());
}

#[tokio::test]
async fn finite_admission_and_shutdown_keep_cleanup_owned() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let slots = VIEW_SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(hmux_protocol::wire::MAX_TERMINALS)))
        .clone();
    let permit = slots
        .clone()
        .try_acquire_many_owned(hmux_protocol::wire::MAX_TERMINALS as u32)
        .unwrap();
    assert_eq!(
        f.open(CancellationToken::new()).await.unwrap_err(),
        Error::Busy
    );
    assert!(f.calls().is_empty());
    drop(permit);
    let stop = CancellationToken::new();
    let view = f.open(stop.clone()).await.unwrap();
    f.mode("cleanup-wait");
    stop.cancel();
    f.wait("cleanup").await;
    assert_eq!(
        slots.available_permits(),
        hmux_protocol::wire::MAX_TERMINALS - 1
    );
    fs::write(f.0.join("release"), "").unwrap();
    view.close().await.unwrap();
    slots_returned().await;
    let stop = CancellationToken::new();
    stop.cancel();
    assert_eq!(f.open(stop).await.unwrap_err(), Error::Cancelled);
    let mut invalid = session();
    invalid.id = "-x".into();
    assert_eq!(
        open(
            f.target(),
            CommandRunner::new(1).unwrap(),
            invalid,
            CancellationToken::new()
        )
        .await
        .unwrap_err(),
        Error::Invalid
    );
}

#[tokio::test]
#[ignore = "requires HMUX_TEST_TMUX; creates only an isolated disposable tmux server"]
async fn isolated_real_tmux_ownership_and_original_survival() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let executable =
        PathBuf::from(std::env::var_os("HMUX_TEST_TMUX").expect("explicit tmux executable"));
    assert!(executable.is_absolute());
    let socket = f.0.join("isolated.sock");
    struct Server {
        executable: PathBuf,
        socket: PathBuf,
    }
    impl Server {
        fn run(&self, args: &[&str]) -> std::process::Output {
            std::process::Command::new(&self.executable)
                .args(["-S"])
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
    }
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.run(&["kill-server"]);
        }
    }
    let server = Server {
        executable: executable.clone(),
        socket: socket.clone(),
    };
    assert!(server
        .run(&[
            "new-session",
            "-d",
            "-s",
            "hmux-e2e-original",
            "/bin/sleep",
            "60"
        ])
        .status
        .success());
    let result = server.run(&[
        "display-message",
        "-p",
        "-t",
        "hmux-e2e-original",
        "#{session_id} #{session_created}",
    ]);
    assert!(result.status.success());
    let row = String::from_utf8(result.stdout).unwrap();
    let fields = row.split_whitespace().collect::<Vec<_>>();
    let original = SessionIdentity {
        id: fields[0].into(),
        created_at: fields[1].parse().unwrap(),
    };
    let target = Target::new(executable, Some(TmuxSocket::Path(socket))).unwrap();
    let runner = CommandRunner::new(2).unwrap();
    let view = open(
        target.clone(),
        runner.clone(),
        original.clone(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let identity = view.identity.clone();
    assert_eq!(
        server
            .run(&[
                "display-message",
                "-p",
                "-t",
                &identity.name,
                "#{@hmux_app_view}"
            ])
            .stdout,
        b"1\n"
    );
    let other = Identity {
        name: identity.name.clone(),
        nonce: "foreign-owner".into(),
    };
    assert!(target
        .command(
            &runner,
            other.create(&original),
            &CancellationToken::new(),
            Instant::now() + SETUP_TIMEOUT
        )
        .await
        .is_err());
    assert_eq!(
        server
            .run(&[
                "display-message",
                "-p",
                "-t",
                &identity.name,
                "#{HMUX_VIEW_OWNER}"
            ])
            .stdout,
        format!("{}\n", identity.nonce).as_bytes()
    );
    target.cleanup(&runner, &other).await.unwrap();
    assert!(server
        .run(&["has-session", "-t", &identity.name])
        .status
        .success());
    view.close().await.unwrap();
    slots_returned().await;
    assert!(!server
        .run(&["has-session", "-t", &identity.name])
        .status
        .success());
    // Already removed by a detach hook or another authorized close is successful.
    target.cleanup(&runner, &identity).await.unwrap();
    assert!(server
        .run(&["has-session", "-t", &original.id])
        .status
        .success());
    let partial = Identity::new().unwrap();
    let mut creation = partial.create(&original);
    creation.truncate(creation.iter().position(|v| v == ";").unwrap());
    target
        .command(
            &runner,
            creation,
            &CancellationToken::new(),
            Instant::now() + SETUP_TIMEOUT,
        )
        .await
        .unwrap();
    assert_eq!(
        server
            .run(&[
                "display-message",
                "-p",
                "-t",
                &partial.name,
                "#{@hmux_app_view}"
            ])
            .stdout,
        b"\n"
    );
    target.cleanup(&runner, &partial).await.unwrap();
    assert!(!server
        .run(&["has-session", "-t", &partial.name])
        .status
        .success());
    assert!(server
        .run(&["has-session", "-t", &original.id])
        .status
        .success());
    let view = open(target, runner, original.clone(), CancellationToken::new())
        .await
        .unwrap();
    let name = view.identity.name.clone();
    drop(view);
    slots_returned().await;
    assert!(!server.run(&["has-session", "-t", &name]).status.success());
    assert!(server
        .run(&["has-session", "-t", &original.id])
        .status
        .success());
}

#[tokio::test]
async fn failed_cleanup_quarantines_admission_without_a_retry_task() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    f.mode("cleanup-fail");
    let slots = Arc::new(Semaphore::new(1));
    let permit = slots.clone().try_acquire_owned().unwrap();
    let stop = CancellationToken::new();
    let (ready_tx, ready_rx) = oneshot::channel();
    let (done_tx, done_rx) = oneshot::channel();
    let task = tokio::spawn(owner(
        f.target(),
        CommandRunner::new(1).unwrap(),
        session(),
        Identity::new().unwrap(),
        stop.clone(),
        permit,
        ready_tx,
        done_tx,
    ));
    assert_eq!(ready_rx.await.unwrap(), Ok(()));
    stop.cancel();
    assert_eq!(done_rx.await.unwrap(), Err(Error::Cleanup));
    task.await.unwrap();
    assert_eq!(slots.available_permits(), 0);
    assert!(f.0.join("owner").exists());
}
