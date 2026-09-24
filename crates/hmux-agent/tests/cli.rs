use std::{
    env, fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
};
fn root() -> PathBuf {
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).unwrap();
    let path = env::temp_dir().join(format!("hmux-agent-cli-{:016x}", u64::from_le_bytes(nonce)));
    fs::create_dir(&path).unwrap();
    fs::canonicalize(path).unwrap()
}
fn cli(root: &PathBuf, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hmux-agent"));
    c.args(args).env("HOME", root).env("PATH", root.join("bin"));
    c
}
#[test]
fn setup_preserves_existing_inventory_and_backs_up_explicit_change() {
    let root = root();
    let dir = root.join("cfg");
    let first = cli(
        &root,
        &["setup-home", "--config-dir", dir.to_str().unwrap()],
    )
    .output()
    .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let inv = dir.join("inventory.toml");
    let initial = fs::read_to_string(&inv).unwrap();
    assert!(initial.contains("~/.hmux"));
    assert!(fs::read_to_string(dir.join("home.toml"))
        .unwrap()
        .contains("role = \"home\""));
    let legacy =
        "\n[[clients]]\nid = \"old-home\"\nrole = \"home\"\nhostnames = [\"fixture.invalid\"]\n";
    let mut f = fs::OpenOptions::new().append(true).open(&inv).unwrap();
    f.write_all(legacy.as_bytes()).unwrap();
    drop(f);
    let workspace = root.join("custom work");
    let changed = cli(
        &root,
        &[
            "setup-home",
            "--config-dir",
            dir.to_str().unwrap(),
            "--workspace-dir",
            workspace.to_str().unwrap(),
        ],
    )
    .output()
    .unwrap();
    assert!(
        changed.status.success(),
        "{}",
        String::from_utf8_lossy(&changed.stderr)
    );
    let updated = fs::read_to_string(&inv).unwrap();
    assert!(updated.contains("fixture.invalid"));
    assert!(updated.contains(workspace.to_str().unwrap()));
    let backups: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("hmux-backup"))
        .collect();
    assert_eq!(backups.len(), 1);
    let again = cli(
        &root,
        &["setup-home", "--config-dir", dir.to_str().unwrap()],
    )
    .output()
    .unwrap();
    assert!(again.status.success());
    assert_eq!(fs::read_to_string(&inv).unwrap(), updated);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn terminate_keeps_exact_identity_in_one_tmux_command() {
    let root = root();
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let tmux = bin.join("tmux");
    fs::write(
        &tmux,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HMUX_TEST_LOG\"\n",
    )
    .unwrap();
    fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
    let log = root.join("args");
    let out = cli(
        &root,
        &[
            "terminate",
            "--confirmed",
            "--created-at",
            "1700000000",
            "$12",
        ],
    )
    .env("HMUX_TEST_LOG", &log)
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let args = fs::read_to_string(&log).unwrap();
    assert_eq!(args,"if-shell\n-F\n-t\n$12\n#{==:#{session_created},1700000000}\nkill-session -t $12\ndisplay-message -p hmux-session-changed\n");
    let invalid = cli(
        &root,
        &["terminate", "--confirmed", "--created-at", "0", "$12"],
    )
    .output()
    .unwrap();
    assert!(!invalid.status.success());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn hook_always_prints_empty_json_even_when_input_invalid() {
    let root = root();
    let mut child = cli(&root, &["workflow-hook"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"invalid").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, b"{}\n");
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn hook_records_sanitized_state_with_explicit_identity_without_tmux() {
    let root = root();
    let mut child = cli(&root, &["workflow-hook"])
        .env("HMUX_TMUX_SESSION_ID", "$4")
        .env("HMUX_TMUX_SESSION_CREATED_AT", "1700000000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(br#"{"session_id":"raw-session","turn_id":"raw-turn","hook_event_name":"UserPromptSubmit","prompt":"private prompt"}"#).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, b"{}\n");
    let raw = fs::read_to_string(root.join(".local/state/hmux/workflows/state.json")).unwrap();
    for secret in ["raw-session", "raw-turn", "private prompt"] {
        assert!(!raw.contains(secret));
    }
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn pending_stdin_is_cancelled_and_joined_on_sigterm() {
    let root = root();
    let child = cli(&root, &["alias-set", "--created-at", "1700000000", "$12"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(150));
    let started = std::time::Instant::now();
    let status = Command::new("/bin/kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success());
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn slow_tmux_child_is_cancelled_before_helper_exits() {
    let root = root();
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let tmux = bin.join("tmux");
    fs::write(&tmux, "#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
    fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
    let child = cli(
        &root,
        &[
            "terminate",
            "--confirmed",
            "--created-at",
            "1700000000",
            "$12",
        ],
    )
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(150));
    let started = std::time::Instant::now();
    let status = Command::new("/bin/kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success());
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn create_uses_explicit_inventory_for_actual_launch() {
    let root = root();
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    for command in ["one", "two"] {
        let path = bin.join(command);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let tmux = bin.join("tmux");
    fs::write(
        &tmux,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HMUX_TEST_LOG\"\nprintf '$21 1700000000\\n'\n",
    )
    .unwrap();
    fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
    let setup = cli(&root, &["setup-home"]).output().unwrap();
    assert!(setup.status.success());
    let config_dir = root.join(".config/hmux");
    let original = config_dir.join("inventory.toml");
    let custom = root.join("custom.toml");
    let fixture = |folder: &str, command: &str| {
        format!("schema_version = 1\nrevision = \"fixture\"\n[[profiles]]\nid = \"test\"\nlabel = \"Test\"\ndefault_directory = \"{}\"\ncommand = [\"{}\"]\n",root.join(folder).display(),command)
    };
    fs::write(&original, fixture("original-root", "one")).unwrap();
    fs::write(&custom, fixture("custom-root", "two")).unwrap();
    let log = root.join("create-args");
    let out = cli(
        &root,
        &[
            "create",
            "test",
            "--inventory",
            custom.to_str().unwrap(),
            "--json",
        ],
    )
    .env("HMUX_TEST_LOG", &log)
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["id"],
        "$21"
    );
    let args = fs::read_to_string(log).unwrap();
    assert!(args.contains(root.join("custom-root").to_str().unwrap()));
    assert!(args.contains(bin.join("two").to_str().unwrap()));
    assert!(!args.contains(bin.join("one").to_str().unwrap()));
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn workspace_initializes_absent_state_directory() {
    let root = root();
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let tmux = bin.join("tmux");
    fs::write(
        &tmux,
        "#!/bin/sh\necho 'no server running on /tmp/tmux-fixture' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(cli(&root, &["setup-home"])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!root.join(".local/state/hmux").exists());
    let mut child = cli(&root, &["workspace"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"null\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(root.join(".local/state/hmux/shared-workspace").is_dir());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn setup_rejects_malformed_existing_inventory_without_workspace_change() {
    let root = root();
    assert!(cli(&root, &["setup-home"])
        .output()
        .unwrap()
        .status
        .success());
    let inventory = root.join(".config/hmux/inventory.toml");
    fs::write(&inventory, "not valid toml").unwrap();
    let out = cli(&root, &["setup-home"]).output().unwrap();
    assert!(!out.status.success());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn name_stdin_accepts_dev_null_as_empty_name() {
    let root = root();
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(cli(&root, &["setup-home"])
        .output()
        .unwrap()
        .status
        .success());
    let out = cli(&root, &["create", "codex", "--name-stdin", "--dry-run"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"codex\n");
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn terminal_stdin_reads_valid_hook_and_restores_fd_flags() {
    let root = root();
    let (mut pty, pts) = pty_process::blocking::open().unwrap();
    let mut child = pty_process::blocking::Command::new(env!("CARGO_BIN_EXE_hmux-agent"))
        .arg("workflow-hook")
        .env("HOME", &root)
        .env("HMUX_TMUX_SESSION_ID", "$4")
        .env("HMUX_TMUX_SESSION_CREATED_AT", "1700000000")
        .spawn(pts)
        .unwrap();
    pty.write_all(b"{\"session_id\":\"raw-session\",\"turn_id\":\"raw-turn\",\"hook_event_name\":\"UserPromptSubmit\"}\n\x04").unwrap();
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if started.elapsed() > std::time::Duration::from_secs(3) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("TTY hook stalled");
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(root.join(".local/state/hmux/workflows/state.json").exists());
    fs::remove_dir_all(root).unwrap();
}
