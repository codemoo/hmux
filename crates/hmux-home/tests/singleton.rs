use hmux_home::singleton::{ConnectorLock, Error};
use std::{
    fs,
    os::unix::fs::{symlink, DirBuilderExt, MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-home-lock-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn creates_nested_state_and_holds_persistent_inode_until_drop() {
    let fixture = Fixture::new();
    let state = fixture.0.join("nested/state");
    let first = ConnectorLock::acquire(&state).unwrap();
    for path in [fixture.0.join("nested"), state.clone()] {
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o700);
    }
    let path = state.join("home-connector.lock");
    let inode = fs::metadata(&path).unwrap().ino();
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    fs::write(&path, b"preserve fixture contents").unwrap();
    // Opening/closing the same inode must not release flock ownership.
    drop(fs::File::open(&path).unwrap());
    assert_eq!(
        ConnectorLock::acquire(&state).err(),
        Some(Error::AlreadyRunning)
    );
    drop(first);
    let second = ConnectorLock::acquire(&state).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(fs::read(&path).unwrap(), b"preserve fixture contents");
    drop(second);
    assert_eq!(fs::metadata(path).unwrap().ino(), inode);
}

#[test]
fn unsafe_ancestors_and_nonclean_paths_are_rejected_before_creation() {
    let fixture = Fixture::new();
    for suffix in [
        "double//child",
        "dot/./child",
        "parent/../child",
        "trailing/",
    ] {
        let raw = PathBuf::from(format!("{}/{}", fixture.0.display(), suffix));
        assert_eq!(
            ConnectorLock::acquire(&raw).err(),
            Some(Error::StateDirectory)
        );
    }
    assert!(fs::read_dir(&fixture.0).unwrap().next().is_none());
    assert_eq!(
        ConnectorLock::acquire(std::path::Path::new("relative")).err(),
        Some(Error::StateDirectory)
    );
    let unsafe_dir = fixture.0.join("writable");
    fs::create_dir(&unsafe_dir).unwrap();
    fs::set_permissions(&unsafe_dir, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        ConnectorLock::acquire(&unsafe_dir.join("state")).err(),
        Some(Error::StateDirectory)
    );
    assert!(!unsafe_dir.join("state").exists());
    let target = fixture.0.join("target");
    fs::create_dir(&target).unwrap();
    let alias = fixture.0.join("alias");
    symlink(&target, &alias).unwrap();
    assert_eq!(
        ConnectorLock::acquire(&alias.join("state")).err(),
        Some(Error::StateDirectory)
    );
    assert!(!target.join("state").exists());
    // Readable but not writable by others is Go-compatible; do not chmod it.
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    drop(ConnectorLock::acquire(&target).unwrap());
    assert_eq!(fs::metadata(target).unwrap().mode() & 0o777, 0o755);
}

#[test]
fn unsafe_lock_files_fail_without_overwriting_contents() {
    let fixture = Fixture::new();
    let path = fixture.0.join("home-connector.lock");
    let target = fixture.0.join("target");
    fs::write(&target, b"keep").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&target, &path).unwrap();
    assert_eq!(
        ConnectorLock::acquire(&fixture.0).err(),
        Some(Error::LockFile)
    );
    fs::remove_file(&path).unwrap();
    fs::hard_link(&target, &path).unwrap();
    assert_eq!(
        ConnectorLock::acquire(&fixture.0).err(),
        Some(Error::LockFile)
    );
    fs::remove_file(&path).unwrap();
    fs::write(&path, b"public").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        ConnectorLock::acquire(&fixture.0).err(),
        Some(Error::LockFile)
    );
    assert_eq!(fs::read(&path).unwrap(), b"public");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert_eq!(
        ConnectorLock::acquire(&fixture.0).err(),
        Some(Error::LockFile)
    );
    fs::remove_dir(&path).unwrap();
    assert!(std::process::Command::new("/usr/bin/mkfifo")
        .args(["-m", "600"])
        .arg(&path)
        .env_clear()
        .status()
        .unwrap()
        .success());
    assert_eq!(
        ConnectorLock::acquire(&fixture.0).err(),
        Some(Error::LockFile)
    );
    assert_eq!(fs::read(target).unwrap(), b"keep");
}

#[test]
fn simultaneous_threads_admit_exactly_one_owner() {
    let fixture = Fixture::new();
    let start = std::sync::Barrier::new(12);
    let acquired = std::sync::Barrier::new(12);
    let wins = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..12 {
            scope.spawn(|| {
                start.wait();
                let held = ConnectorLock::acquire(&fixture.0);
                if held.is_ok() {
                    wins.fetch_add(1, Ordering::SeqCst);
                }
                acquired.wait();
                if let Err(error) = &held {
                    assert_eq!(*error, Error::AlreadyRunning);
                }
                drop(held);
            });
        }
    });
    assert_eq!(wins.load(Ordering::SeqCst), 1);
    drop(ConnectorLock::acquire(&fixture.0).unwrap());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires built actual Go Home lock oracle via make rust-home-lock-compat"]
async fn actual_go_home_lock_is_exclusive_in_both_directions_and_releases_on_exit() {
    use std::process::Stdio;
    use tokio::{
        io::AsyncReadExt,
        process::Command,
        time::{timeout, Duration},
    };
    let fixture = Fixture::new();
    let oracle =
        std::env::var_os("HMUX_GO_HOME_LOCK_ORACLE").expect("Go Home lock oracle required");
    let command = |action: &str| {
        let mut command = Command::new(&oracle);
        command
            .arg(action)
            .arg(&fixture.0)
            .env_clear()
            .env("HOME", &fixture.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command
    };
    let mut go = command("hold").spawn().unwrap();
    let mut response = [0; 7];
    timeout(
        Duration::from_secs(5),
        go.stdout.take().unwrap().read_exact(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(&response, b"locked\n");
    let path = fixture.0.join("home-connector.lock");
    let inode = fs::metadata(&path).unwrap().ino();
    assert_eq!(
        ConnectorLock::acquire(&fixture.0).err(),
        Some(Error::AlreadyRunning)
    );
    drop(go.stdin.take());
    assert!(timeout(Duration::from_secs(5), go.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    let rust = ConnectorLock::acquire(&fixture.0).unwrap();
    let result = timeout(Duration::from_secs(5), command("try").output())
        .await
        .unwrap()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"busy\n");
    drop(rust);
    let result = timeout(Duration::from_secs(5), command("try").output())
        .await
        .unwrap()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"locked\n");
    let mut go = command("hold").spawn().unwrap();
    timeout(
        Duration::from_secs(5),
        go.stdout.take().unwrap().read_exact(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(&response, b"locked\n");
    go.start_kill().unwrap();
    timeout(Duration::from_secs(5), go.wait())
        .await
        .unwrap()
        .unwrap();
    drop(ConnectorLock::acquire(&fixture.0).unwrap());
    assert_eq!(fs::metadata(path).unwrap().ino(), inode);
}
