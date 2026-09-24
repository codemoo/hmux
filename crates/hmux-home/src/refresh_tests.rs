use super::*;
use crate::catalog::TmuxSocket;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-refresh-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let tmux = root.join("tmux");
        fs::write(
            &tmux,
            r#"#!/usr/bin/python3
import sys,pathlib
root=pathlib.Path(__file__).parent
args=sys.argv[1:]
if args[:2]==['-S',str(root/'socket')]: args=args[2:]
else: sys.exit(2)
mode=(root/'mode').read_text() if (root/'mode').exists() else ''
if args[0]=='list-clients':
 count=int((root/'count').read_text()) if (root/'count').exists() else 0
 (root/'count').write_text(str(count+1))
 if args[1:]!=['-t','hmux-app-view-42-abcd','-F','#{client_pid} #{client_tty} #{pane_id} #{pane_tty} #{pane_pid} #{session_name} #{HMUX_VIEW_OWNER} #{@hmux_app_view}']: sys.exit(2)
 owner='wrong' if mode=='owner' else 'abcd'
 pane='%2' if mode=='stale' and count>0 else '%1'
 print(f'4242 /dev/ttys001 {pane} /dev/ttys001 33 hmux-app-view-42-abcd {owner} 1')
elif args==['refresh-client','-t','/dev/ttys001']:
 (root/'refreshed').touch()
else: sys.exit(2)
"#,
        )
        .unwrap();
        let ps = root.join("ps");
        fs::write(
            &ps,
            r#"#!/usr/bin/python3
import sys,pathlib,time
root=pathlib.Path(__file__).parent
if sys.argv[1:]!=['-p','33','-o','pid=,tpgid=,tty=']: sys.exit(2)
(root/'ps-called').touch()
if (root/'mode').exists() and (root/'mode').read_text()=='wait-ps': time.sleep(10)
print('33 44 s001')
"#,
        )
        .unwrap();
        for script in [&tmux, &ps] {
            fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(root)
    }
    fn target(&self) -> Target {
        Target::new(
            self.0.join("tmux"),
            Some(TmuxSocket::Path(self.0.join("socket"))),
        )
        .unwrap()
    }
    fn mode(&self, value: &str) {
        fs::write(self.0.join("mode"), value).unwrap();
    }
    async fn wait(&self, file: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !self.0.join(file).exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn parsers_reject_unowned_ambiguous_and_unsafe_targets() {
    let valid = "4242 /dev/ttys001 %1 /dev/ttys001 33 hmux-app-view-42-abcd abcd 1\n";
    let row = client_row(valid.as_bytes(), 4242, "hmux-app-view-42-abcd", "abcd").unwrap();
    assert_eq!(foreground_group(b"33 44 s001\n", &row), Ok(44));
    assert_eq!(foreground_group(b"33 44 ttys001\n", &row), Ok(44));
    for bad in [
        valid.replace("abcd", "xxxx"),
        valid.replace(" 1\n", " 0\n"),
        valid.replace("/dev/ttys001", "/tmp/not-a-tty"),
        valid.replace("%1", "-1"),
        format!("{valid}{valid}"),
    ] {
        assert_eq!(
            client_row(bad.as_bytes(), 4242, "hmux-app-view-42-abcd", "abcd"),
            Err(Error::Unavailable)
        );
    }
    assert_eq!(
        client_row(valid.as_bytes(), 1, "hmux-app-view-42-abcd", "abcd"),
        Err(Error::Unavailable)
    );
    for bad in [
        b"33 1 s001\n".as_slice(),
        b"33 44 pts/0\n",
        b"34 44 s001\n",
        b"33 44 s001 extra\n",
    ] {
        assert_eq!(foreground_group(bad, &row), Err(Error::Unavailable));
    }
    assert_eq!(valid_pid("2147483648"), None);
    assert!(!valid_tty("/dev/pts/../0"));
}

#[tokio::test]
async fn synthetic_refresh_signals_only_verified_group_and_client() {
    let f = Fixture::new();
    let target = f.target();
    let runner = CommandRunner::new(2).unwrap();
    let signaled = Arc::new(AtomicUsize::new(0));
    let seen = signaled.clone();
    let closing = CancellationToken::new();
    let stop = CancellationToken::new();
    run_with(
        &target,
        "hmux-app-view-42-abcd",
        "abcd",
        &closing,
        &runner,
        4242,
        &stop,
        &f.0.join("ps"),
        move |group| {
            assert_eq!(group, 44);
            seen.fetch_add(1, Ordering::Relaxed);
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(signaled.load(Ordering::Relaxed), 1);
    assert!(f.0.join("refreshed").exists());
    assert_eq!(fs::read_to_string(f.0.join("count")).unwrap(), "2");
}

#[tokio::test]
async fn wrong_owner_and_stale_recheck_never_signal() {
    for (mode, want) in [("owner", Error::Unavailable), ("stale", Error::Changed)] {
        let f = Fixture::new();
        f.mode(mode);
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let error = run_with(
            &f.target(),
            "hmux-app-view-42-abcd",
            "abcd",
            &CancellationToken::new(),
            &CommandRunner::new(2).unwrap(),
            4242,
            &CancellationToken::new(),
            &f.0.join("ps"),
            move |_| {
                seen.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .await;
        assert_eq!(error, Err(want));
        assert_eq!(count.load(Ordering::Relaxed), 0);
        assert!(!f.0.join("refreshed").exists());
    }
}

#[tokio::test]
async fn failed_signal_does_not_refresh_client() {
    let f = Fixture::new();
    let result = run_with(
        &f.target(),
        "hmux-app-view-42-abcd",
        "abcd",
        &CancellationToken::new(),
        &CommandRunner::new(2).unwrap(),
        4242,
        &CancellationToken::new(),
        &f.0.join("ps"),
        |_| Err(()),
    )
    .await;
    assert_eq!(result, Err(Error::Signal));
    assert!(!f.0.join("refreshed").exists());
}

#[tokio::test]
async fn cancellation_reaps_ps_and_never_signals() {
    let f = Fixture::new();
    f.mode("wait-ps");
    let stop = CancellationToken::new();
    let closing = CancellationToken::new();
    let runner = CommandRunner::new(2).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let target = f.target();
    let ps = f.0.join("ps");
    let work = run_with(
        &target,
        "hmux-app-view-42-abcd",
        "abcd",
        &closing,
        &runner,
        4242,
        &stop,
        &ps,
        move |_| {
            seen.fetch_add(1, Ordering::Relaxed);
            Ok(())
        },
    );
    tokio::pin!(work);
    tokio::select! {
        _ = f.wait("ps-called") => stop.cancel(),
        _ = &mut work => panic!("refresh ended before ps cancellation"),
    }
    assert_eq!(work.await, Err(Error::Cancelled));
    assert_eq!(runner.available_slots(), 2);
    assert_eq!(count.load(Ordering::Relaxed), 0);
    assert!(!f.0.join("refreshed").exists());
    closing.cancel();
    assert_eq!(
        run_with(
            &f.target(),
            "hmux-app-view-42-abcd",
            "abcd",
            &closing,
            &runner,
            4242,
            &CancellationToken::new(),
            &f.0.join("ps"),
            |_| Ok(())
        )
        .await,
        Err(Error::Closed)
    );
}
