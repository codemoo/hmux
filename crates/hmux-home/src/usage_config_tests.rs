use super::*;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

fn captured(home: &Path, values: &[(&str, &str)]) -> Options {
    let values: BTreeMap<&str, OsString> = values
        .iter()
        .map(|(name, value)| (*name, OsString::from(value)))
        .collect();
    Options::from_env(home, OsStr::new("/usr/bin:/bin"), |name| {
        values.get(name).cloned()
    })
    .unwrap()
}

static NEXT: AtomicU64 = AtomicU64::new(0);
// load_lb deliberately shares one process-wide admission slot. Independent
// fixtures must not make these success/validation tests race for that slot.
static KEY_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct SyntheticHome(PathBuf);
impl SyntheticHome {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-usage-config-test-{}-{epoch}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(path.join(".codex")).unwrap();
        Self(path)
    }
    fn key(&self) -> PathBuf {
        self.0.join(".codex/lb-api-key")
    }
    fn write_key(&self, value: &str, mode: u32) {
        fs::write(self.key(), value).unwrap();
        fs::set_permissions(self.key(), fs::Permissions::from_mode(mode)).unwrap();
    }
}
impl Drop for SyntheticHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn capture_is_pure_and_defaults_match_go_roots() {
    let home = Path::new("/private/tmp/hmux-config-no-files");
    let opts = captured(home, &[]);
    assert_eq!(opts.home, home);
    assert_eq!(opts.path, OsStr::new("/usr/bin:/bin"));
    assert!(opts.cswap);
    assert_eq!(
        opts.accounts,
        Some(home.join(".config/token-usage/codex-lb-accounts.json"))
    );
    let activity = opts.activity.as_ref().unwrap();
    assert_eq!(activity.claude_projects_root, home.join(".claude/projects"));
    assert_eq!(activity.codex_sessions_root, home.join(".codex/sessions"));
    assert_eq!(
        activity.claude_swap_sessions_root,
        Some(home.join(".claude-swap-backup/sessions"))
    );
    assert!(opts.lb.is_some());
    assert_eq!(format!("{opts:?}"), "UsageOptions([redacted])");
}

#[test]
fn disabled_sources_and_invalid_optional_paths_do_not_fail_startup() {
    let home = Path::new("/private/tmp/hmux-config-no-files");
    let opts = captured(
        home,
        &[
            ("TOKEN_USAGE_DISABLE_JSONL", "1"),
            ("TOKEN_USAGE_DISABLE_CLAUDE_SWAP", "1"),
            ("TOKEN_USAGE_DISABLE_CODEX_ACCOUNTS", "1"),
            ("TOKEN_USAGE_DISABLE_CODEX_LB", "1"),
        ],
    );
    assert!(opts.activity.is_none());
    assert!(!opts.cswap);
    assert!(opts.accounts.is_none());
    assert!(opts.lb.is_none());

    let opts = captured(
        home,
        &[
            ("TOKEN_USAGE_CLAUDE_PROJECTS", "../unsafe"),
            ("TOKEN_USAGE_CODEX_ACCOUNTS", "../unsafe"),
            ("TOKEN_USAGE_CODEX_LB_URL", "http://example.test:2455"),
        ],
    );
    assert!(opts.activity.is_none());
    assert!(opts.accounts.is_none());
    assert!(opts.lb.is_none());

    let opts = captured(
        home,
        &[
            ("TOKEN_USAGE_DISABLE_CLAUDE_SWAP_SESSIONS", "1"),
            (
                "TOKEN_USAGE_CLAUDE_PROJECTS",
                "/private/tmp/synthetic-claude",
            ),
            ("TOKEN_USAGE_CODEX_SESSIONS", "/private/tmp/synthetic-codex"),
        ],
    );
    let activity = opts.activity.as_ref().unwrap();
    assert_eq!(
        activity.claude_projects_root,
        Path::new("/private/tmp/synthetic-claude")
    );
    assert_eq!(
        activity.codex_sessions_root,
        Path::new("/private/tmp/synthetic-codex")
    );
    assert!(activity.claude_swap_sessions_root.is_none());
    assert!(opts.cswap);
}

#[test]
fn bad_required_home_or_path_is_redacted_error() {
    assert_eq!(
        Options::from_env(Path::new("relative"), OsStr::new("/bin"), |_| None).err(),
        Some(Error::InvalidHome)
    );
    assert_eq!(
        Options::from_env(Path::new("/private/tmp/home"), OsStr::new(""), |_| None).err(),
        Some(Error::InvalidPath)
    );
    assert_eq!(
        format!("{:?}", Error::InvalidHome),
        "UsageConfigError::InvalidHome"
    );
}

#[tokio::test]
async fn env_key_precedence_and_deferred_private_file() {
    let _serial = KEY_TESTS.lock().await;
    let home = SyntheticHome::new();
    let cancel = CancellationToken::new();
    let opts = captured(
        &home.0,
        &[
            ("TOKEN_USAGE_CODEX_LB_API_KEY", " bad key "),
            ("CODEX_LB_API_KEY", "  secondary-key  "),
        ],
    );
    assert_eq!(opts.load_lb(&cancel).await.unwrap().1, "secondary-key");
    assert!(!format!("{opts:?}").contains("secondary-key"));

    let opts = captured(&home.0, &[]);
    assert!(opts.load_lb(&cancel).await.is_none());
    home.write_key("  file-key\n", 0o600);
    assert_eq!(opts.load_lb(&cancel).await.unwrap().1, "file-key");
    assert!(!format!("{opts:?}").contains("file-key"));

    cancel.cancel();
    assert!(opts.load_lb(&cancel).await.is_none());
}

#[tokio::test]
async fn private_key_file_rejects_permissions_links_and_oversize() {
    let _serial = KEY_TESTS.lock().await;
    let home = SyntheticHome::new();
    let opts = captured(&home.0, &[]);
    let cancel = CancellationToken::new();
    home.write_key("private-key", 0o644);
    assert!(opts.load_lb(&cancel).await.is_none());
    fs::set_permissions(home.key(), fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        read_private_key(&home.key(), &cancel),
        Some("private-key".into())
    );
    assert_eq!(opts.load_lb(&cancel).await.unwrap().1, "private-key");

    fs::remove_file(home.key()).unwrap();
    let target = home.0.join("target");
    fs::write(&target, "linked-key").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&target, home.key()).unwrap();
    assert!(opts.load_lb(&cancel).await.is_none());
    fs::remove_file(home.key()).unwrap();

    home.write_key(&"x".repeat(MAX_KEY_FILE_BYTES as usize + 1), 0o600);
    assert!(opts.load_lb(&cancel).await.is_none());
}

#[test]
fn secret_validation_is_bounded_printable_ascii() {
    assert_eq!(normalize_key("  key  "), Some("key"));
    for invalid in [
        "",
        "bad key",
        "bad\nkey",
        "é",
        &"x".repeat(MAX_KEY_BYTES + 1),
    ] {
        assert!(normalize_key(invalid).is_none());
    }
}
