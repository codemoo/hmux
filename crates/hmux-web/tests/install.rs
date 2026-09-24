//! Synthetic native-install command checks; no service manager or real HOME.
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-e2e-native-install-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("source")).unwrap();
        for name in ["hmux-web", "hmux-agent"] {
            let path = root.join("source").join(name);
            fs::write(
                &path,
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HMUX_TEST_ARGS\"\n",
            )
            .unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(root)
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hmux-web"));
        cmd.arg("install-home")
            .arg("--source-dir")
            .arg(self.0.join("source"))
            .arg("--bin-dir")
            .arg(self.0.join("installed"))
            .arg("--config-dir")
            .arg(self.0.join("config"))
            .env("HOME", &self.0)
            .env("HMUX_TEST_ARGS", self.0.join("args"));
        cmd
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn native_install_preserves_argv_and_existing_configuration() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("config")).unwrap();
    fs::write(
        fixture.0.join("config/home.toml"),
        b"existing private config",
    )
    .unwrap();
    let out = fixture
        .command()
        .args(["--workspace-dir", "~/work with spaces;literal"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let args = fs::read_to_string(fixture.0.join("args")).unwrap();
    assert_eq!(
        args,
        format!(
            "setup-home\n--config-dir\n{}\n--workspace-dir\n~/work with spaces;literal\n",
            fixture.0.join("config").display()
        )
    );
    assert_eq!(
        fs::read(fixture.0.join("config/home.toml")).unwrap(),
        b"existing private config"
    );
    for name in ["hmux-web", "hmux-agent"] {
        assert_eq!(
            fs::read(fixture.0.join("source").join(name)).unwrap(),
            fs::read(fixture.0.join("installed").join(name)).unwrap()
        );
    }
}
#[test]
fn options_and_second_binary_fail_before_partial_install() {
    let fixture = Fixture::new();
    let out = fixture
        .command()
        .args(["--binaries-only", "--enable-service"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!fixture.0.join("installed").exists());
    assert!(!fixture.0.join("config").exists());
    fs::set_permissions(
        fixture.0.join("source/hmux-agent"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let out = fixture.command().arg("--binaries-only").output().unwrap();
    assert!(!out.status.success());
    assert!(!fixture.0.join("installed/hmux-web").exists());
    assert!(!fixture.0.join("config").exists());
}
