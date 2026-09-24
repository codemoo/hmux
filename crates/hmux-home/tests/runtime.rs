use hmux_home::{
    connector,
    runtime::{Error, HomeRuntime, Options},
    singleton,
};
use std::{
    ffi::{OsStr, OsString},
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio_util::sync::CancellationToken;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-runtime-{}-{} 한글",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let fixture = Self(root);
        fixture.file(
            "connector.token",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n",
            0o600,
        );
        fixture.file(
            "home.toml",
            &format!(
                "schema_version=1\nstate_dir='{}'\ninventory_path='{}'\n",
                fixture.0.join("state").display(),
                fixture.0.join("inventory.toml").display()
            ),
            0o600,
        );
        fixture.file("tmux", "#!/bin/sh\nexit 93\n", 0o700);
        fixture
    }
    fn file(&self, name: &str, contents: &str, mode: u32) {
        let path = self.0.join(name);
        fs::write(&path, contents).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }
    fn args(&self) -> Vec<OsString> {
        vec![
            "--experimental-home".into(),
            "--url".into(),
            "wss://gateway.test/connect".into(),
            "--token-file".into(),
            self.0.join("connector.token").into(),
            "--config".into(),
            self.0.join("home.toml").into(),
        ]
    }
    fn prepare(&self) -> Result<HomeRuntime, Error> {
        HomeRuntime::prepare(
            Options::parse(self.args()).unwrap(),
            &self.0,
            self.0.as_os_str(),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn installed_options_preserve_default_config_and_host_staging_location() {
    let fixture = Fixture::new();
    let options = Options::connect(
        "wss://gateway.test/connect".into(),
        fixture.0.join("connector.token"),
        None,
        &fixture.0,
        None,
    )
    .unwrap();
    let runtime = HomeRuntime::prepare(options, &fixture.0, fixture.0.as_os_str()).unwrap();
    let cache = if cfg!(target_os = "macos") {
        "Library/Caches"
    } else {
        ".cache"
    };
    assert!(fixture.0.join(cache).join("hmux/staged-files-v1").is_dir());
    assert!(fixture.0.join(".local/state/hmux").is_dir());
    drop(runtime);
    let config_root = fixture.0.join(".config/hmux");
    fs::create_dir_all(&config_root).unwrap();
    fs::copy(fixture.0.join("home.toml"), config_root.join("client.toml")).unwrap();
    let options = Options::connect(
        "wss://gateway.test/connect".into(),
        fixture.0.join("connector.token"),
        None,
        &fixture.0,
        None,
    )
    .unwrap();
    let runtime = HomeRuntime::prepare(options, &fixture.0, fixture.0.as_os_str()).unwrap();
    assert!(fixture.0.join("state").is_dir());
    drop(runtime);
    let options = Options::connect(
        "wss://gateway.test/connect".into(),
        fixture.0.join("connector.token"),
        Some(fixture.0.join("missing.toml")),
        &fixture.0,
        None,
    )
    .unwrap();
    assert_eq!(
        HomeRuntime::prepare(options, &fixture.0, fixture.0.as_os_str()).err(),
        Some(Error::Config)
    );
}

#[test]
fn candidate_requires_opt_in_explicit_inputs_and_valid_endpoint() {
    let fixture = Fixture::new();
    assert!(Options::parse(fixture.args()).is_ok());
    for index in [0, 1, 3, 5] {
        let mut args = fixture.args();
        args.remove(index);
        assert!(Options::parse(args).is_err());
    }
    for value in [
        "ws://gateway.test/connect",
        "wss://user:secret@gateway.test/connect",
        "wss://gateway.test/connect?q=secret",
    ] {
        let mut args = fixture.args();
        args[2] = value.into();
        assert_eq!(Options::parse(args).err(), Some(Error::Endpoint));
    }
    for flag in [
        "--url",
        "--config",
        "--log-file",
        "--unknown",
        "--experimental-home",
    ] {
        let mut args = fixture.args();
        args.extend([flag.into(), "secret-MUST-NOT-LEAK".into()]);
        assert_eq!(Options::parse(args).err(), Some(Error::Arguments));
    }
    let opts = Options::parse(fixture.args()).unwrap();
    assert_eq!(format!("{opts:?}"), "Options([redacted])");
    assert!(!fixture.0.join("state").exists());
}

#[test]
fn failed_preparation_never_creates_lock_and_redacts_private_values() {
    let fixture = Fixture::new();
    fixture.file("connector.token", "secret-MUST-NOT-LEAK", 0o600);
    assert_eq!(fixture.prepare().err(), Some(Error::Token));
    assert!(!fixture.0.join("state").exists());
    fixture.file(
        "connector.token",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        0o600,
    );
    fs::remove_file(fixture.0.join("home.toml")).unwrap();
    assert_eq!(fixture.prepare().err(), Some(Error::Config));
    assert!(!fixture.0.join("state").exists());
    fixture.file("home.toml", "private-invalid-config-MUST-NOT-LEAK", 0o600);
    let err = fixture.prepare().err().unwrap();
    assert_eq!(err, Error::Config);
    assert!(!format!("{err:?}: {err}").contains("MUST-NOT-LEAK"));
    assert!(!fixture.0.join("state").exists());
}

#[tokio::test]
async fn prepared_runtime_owns_lock_and_cancelled_start_never_executes_or_dials() {
    let fixture = Fixture::new();
    let first = fixture.prepare().unwrap();
    assert_eq!(
        fixture.prepare().err(),
        Some(Error::Connector(connector::Error::Lock(
            singleton::Error::AlreadyRunning
        )))
    );
    assert_eq!(format!("{first:?}"), "HomeRuntime([redacted])");
    let stop = CancellationToken::new();
    stop.cancel();
    assert_eq!(first.run(stop).await, Ok(()));
    drop(fixture.prepare().unwrap());
    assert!(fixture.0.join("state/home-connector.lock").is_file());
}

#[test]
fn path_resolution_handles_spaces_symlinks_and_explicit_overrides_without_shells() {
    let fixture = Fixture::new();
    let mut args = fixture.args();
    args.extend([OsString::from("--tmux"), fixture.0.join("tmux").into()]);
    drop(
        HomeRuntime::prepare(
            Options::parse(args.clone()).unwrap(),
            &fixture.0,
            OsStr::new(""),
        )
        .unwrap(),
    );
    args.pop();
    args.push("relative-tmux".into());
    assert_eq!(
        HomeRuntime::prepare(
            Options::parse(args).unwrap(),
            &fixture.0,
            fixture.0.as_os_str()
        )
        .err(),
        Some(Error::Tmux)
    );
    fs::rename(fixture.0.join("tmux"), fixture.0.join("actual binary")).unwrap();
    std::os::unix::fs::symlink(fixture.0.join("actual binary"), fixture.0.join("tmux")).unwrap();
    drop(fixture.prepare().unwrap());
    fs::set_permissions(
        fixture.0.join("actual binary"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert_eq!(fixture.prepare().err(), Some(Error::Tmux));
}

#[test]
fn uploads_require_an_explicit_safe_candidate_spool() {
    let fixture = Fixture::new();
    let mut args = fixture.args();
    args.extend(["--staging-root".into(), fixture.0.join("wrong-name").into()]);
    assert_eq!(
        HomeRuntime::prepare(
            Options::parse(args).unwrap(),
            &fixture.0,
            fixture.0.as_os_str()
        )
        .err(),
        Some(Error::Staging)
    );
    assert!(!fixture.0.join("state").exists());
    let mut args = fixture.args();
    let root = fixture.0.join("hmux/staged-files-v1");
    args.extend(["--staging-root".into(), root.clone().into()]);
    let runtime = HomeRuntime::prepare(
        Options::parse(args).unwrap(),
        &fixture.0,
        fixture.0.as_os_str(),
    )
    .unwrap();
    assert!(root.is_dir());
    drop(runtime);
}
