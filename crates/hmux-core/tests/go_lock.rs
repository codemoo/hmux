//! Invoked by make rust-compat with the current Go flock helper. Never touches
//! live Home locks; both processes use this test's exclusive temporary directory.
use hmux_core::PrivateDir;
use std::{
    ffi::OsStr,
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
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
    fn wait(&mut self) {
        let start = Instant::now();
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "Go helper timeout"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn reply(&mut self) -> String {
        let output = self.0.stdout.take().unwrap();
        let (send, recv) = mpsc::channel();
        std::thread::spawn(move || {
            let mut text = String::new();
            let result = BufReader::new(output.take(64))
                .read_line(&mut text)
                .map(|_| text);
            let _ = send.send(result);
        });
        recv.recv_timeout(Duration::from_secs(5))
            .expect("Go helper reply timeout")
            .unwrap()
    }
}

#[test]
#[ignore = "requires HMUX_GO_FLOCK_HELPER; make rust-compat builds and runs it"]
fn go_and_rust_exclude_each_other_on_the_same_persistent_inode() {
    let helper = std::env::var_os("HMUX_GO_FLOCK_HELPER").expect("Go helper path required");
    let root = std::env::temp_dir().canonicalize().unwrap();
    let path = root.join(format!("hmux-cross-lock-{}", std::process::id()));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    let fixture = Fixture(path);
    let lock_path = fixture.0.join("home-connector.lock");
    let directory = PrivateDir::open(&fixture.0).unwrap();
    let spawn = |mode: &str| {
        OwnedChild(
            Command::new(&helper)
                .arg(mode)
                .arg(&lock_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    };

    let mut go = spawn("hold");
    assert_eq!(go.reply(), "locked\n");
    let inode = fs::metadata(&lock_path).unwrap().ino();
    assert!(directory
        .try_lock(OsStr::new("home-connector.lock"))
        .unwrap()
        .is_none());
    go.0.stdin.take().unwrap().write_all(b"\n").unwrap();
    go.wait();

    let rust = directory
        .try_lock(OsStr::new("home-connector.lock"))
        .unwrap()
        .unwrap();
    let mut go = spawn("try");
    assert_eq!(go.reply(), "busy\n");
    go.wait();
    drop(rust);
    let mut go = spawn("try");
    assert_eq!(go.reply(), "locked\n");
    go.wait();
    assert_eq!(fs::metadata(&lock_path).unwrap().ino(), inode);
}
