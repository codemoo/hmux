use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::sync::atomic::{AtomicU64, Ordering};
static TEST_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
async fn test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-e2e-usage-source-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn script(&self, relative: &str, body: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    fn account_path(&self) -> PathBuf {
        self.0.join("derived.json")
    }
    fn account(&self, stamp: &str, alias: &str) {
        std::fs::write(self.account_path(), format!(r#"{{"schemaVersion":1,"accountsUpdatedAt":"{stamp}","accounts":[{{"number":1,"alias":"{alias}","status":"active"}}]}}"#)).unwrap();
        std::fs::set_permissions(self.account_path(), std::fs::Permissions::from_mode(0o600))
            .unwrap();
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn now() -> DateTime<Utc> {
    "2026-09-24T05:00:00Z".parse().unwrap()
}
fn output(alias: &str) -> String {
    format!(
        r#"{{"schemaVersion":1,"activeAccountNumber":1,"accounts":[{{"number":1,"alias":"{alias}","active":true,"usageStatus":"ok","usage":{{"fiveHour":{{"pct":30}}}},"usageFetchedAt":"2026-09-24T05:00:00Z"}}]}}"#
    )
}

#[tokio::test]
async fn cswap_literal_argv_path_then_home_fallback_and_cadence() {
    let _serial = test_lock().await;
    let temp = Temp::new();
    let bin = temp.0.join("bin");
    let script = temp.script("bin/cswap", &format!("[ \"$#\" = 2 ] && [ \"$1\" = list ] && [ \"$2\" = --json ] || exit 4\nprintf '%s' '{}'", output("path")));
    let mut source = Cswap::new(temp.0.clone(), bin.into_os_string()).unwrap();
    source
        .refresh(now(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        source.current(now()).unwrap().accounts[0].display_name,
        "path"
    );
    std::fs::remove_file(script).unwrap();
    source
        .refresh(
            now() + chrono::Duration::seconds(30),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        source.current(now()).unwrap().accounts[0].display_name,
        "path"
    );
    temp.script(
        ".local/bin/cswap",
        &format!("printf '%s' '{}'", output("fallback")),
    );
    source
        .refresh(
            now() + chrono::Duration::seconds(61),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        source.current(now()).unwrap().accounts[0].display_name,
        "fallback"
    );
}

#[tokio::test]
async fn cswap_failure_keeps_last_good_until_original_age_expires() {
    let _serial = test_lock().await;
    let temp = Temp::new();
    let bin = temp.0.join("bin");
    let path = temp.script("bin/cswap", &format!("printf '%s' '{}'", output("first")));
    let mut source = Cswap::new(temp.0.clone(), bin.into_os_string()).unwrap();
    source
        .refresh(now(), &CancellationToken::new())
        .await
        .unwrap();
    std::fs::write(&path, "#!/bin/sh\nexit 2\n").unwrap();
    assert_eq!(
        source
            .refresh(
                now() + chrono::Duration::seconds(61),
                &CancellationToken::new()
            )
            .await,
        Err(Error::Command)
    );
    assert!(source
        .current(now() + chrono::Duration::minutes(29))
        .is_some());
    assert!(source
        .current(now() + chrono::Duration::minutes(31))
        .is_none());
}

#[tokio::test]
async fn cswap_output_limit_and_cancel_reap_direct_child() {
    let _serial = test_lock().await;
    let temp = Temp::new();
    let bin = temp.0.join("bin");
    let path = temp.script("bin/cswap", "exec /usr/bin/yes x");
    let mut source = Cswap::new(temp.0.clone(), bin.clone().into_os_string()).unwrap();
    assert_eq!(
        source.refresh(now(), &CancellationToken::new()).await,
        Err(Error::Command)
    );
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\necho $$ > '{}'\nexec /bin/sleep 30\n",
            temp.0.join("pid").display()
        ),
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let stopper = cancel.clone();
    let pid_file = temp.0.join("pid");
    let waiter = tokio::spawn(async move {
        for _ in 0..100 {
            if pid_file.exists() {
                stopper.cancel();
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    assert_eq!(
        source
            .refresh(now() + chrono::Duration::seconds(61), &cancel)
            .await,
        Err(Error::Cancelled)
    );
    waiter.await.unwrap();
    let pid: i32 = std::fs::read_to_string(temp.0.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_err()
    );
}

#[tokio::test]
async fn account_rotation_invalid_replacement_and_source_age() {
    let _serial = test_lock().await;
    let temp = Temp::new();
    temp.account("2026-09-24T05:00:00Z", "first");
    let accounts = Accounts::new(temp.account_path()).unwrap();
    let cancel = CancellationToken::new();
    assert_eq!(
        accounts.read(now(), &cancel).await.unwrap().accounts[0].display_name,
        "first"
    );
    std::fs::rename(temp.account_path(), temp.0.join("old")).unwrap();
    temp.account("2026-09-24T05:00:00Z", "second");
    assert_eq!(
        accounts.read(now(), &cancel).await.unwrap().accounts[0].display_name,
        "first"
    );
    tokio::time::sleep(CHECK_EVERY).await;
    assert_eq!(
        accounts.read(now(), &cancel).await.unwrap().accounts[0].display_name,
        "second"
    );
    std::fs::write(temp.account_path(), "bad").unwrap();
    tokio::time::sleep(CHECK_EVERY).await;
    assert_eq!(
        accounts
            .read(now() + chrono::Duration::minutes(29), &cancel)
            .await
            .unwrap()
            .accounts[0]
            .display_name,
        "second"
    );
    assert_eq!(
        accounts
            .read(now() + chrono::Duration::minutes(31), &cancel)
            .await,
        Err(Error::Stale)
    );
}

#[tokio::test]
async fn account_unsafe_file_and_symlink_parent_rejected() {
    let _serial = test_lock().await;
    let temp = Temp::new();
    temp.account("2026-09-24T05:00:00Z", "safe");
    let path = temp.account_path();
    let accounts = Accounts::new(path.clone()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert_eq!(
        accounts.read(now(), &CancellationToken::new()).await,
        Err(Error::UnsafeFile)
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(&path, temp.0.join("hard")).unwrap();
    tokio::time::sleep(CHECK_EVERY).await;
    assert_eq!(
        accounts.read(now(), &CancellationToken::new()).await,
        Err(Error::UnsafeFile)
    );
    let parent = temp.0.join("linked");
    symlink(&temp.0, &parent).unwrap();
    let linked = Accounts::new(parent.join("derived.json")).unwrap();
    assert_eq!(
        linked.read(now(), &CancellationToken::new()).await,
        Err(Error::UnsafeFile)
    );
}

#[tokio::test]
async fn cswap_timeout_and_dropped_caller_reap_direct_child() {
    let _serial = test_lock().await;
    let temp = Temp::new();
    let bin = temp.0.join("bin");
    temp.script(
        "bin/cswap",
        &format!(
            "echo $$ > '{}'\nexec /bin/sleep 30",
            temp.0.join("pid").display()
        ),
    );
    let mut source = Cswap::new(temp.0.clone(), bin.clone().into_os_string()).unwrap();
    assert_eq!(
        source.refresh(now(), &CancellationToken::new()).await,
        Err(Error::Command)
    );
    let pid: i32 = std::fs::read_to_string(temp.0.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_err()
    );

    std::fs::remove_file(temp.0.join("pid")).unwrap();
    let mut abandoned = Cswap::new(temp.0.clone(), bin.into_os_string()).unwrap();
    let handle =
        tokio::spawn(async move { abandoned.refresh(now(), &CancellationToken::new()).await });
    for _ in 0..100 {
        if temp.0.join("pid").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(temp.0.join("pid").exists());
    let pid: i32 = std::fs::read_to_string(temp.0.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    handle.abort();
    let _ = handle.await;
    for _ in 0..100 {
        if rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_err()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("owned direct child remained alive after caller drop");
}

#[tokio::test]
async fn account_old_source_and_final_symlink_are_rejected() {
    let _serial = test_lock().await;
    let temp = Temp::new();
    temp.account("2026-09-24T04:29:59Z", "old");
    let path = temp.account_path();
    let accounts = Accounts::new(path.clone()).unwrap();
    assert_eq!(
        accounts.read(now(), &CancellationToken::new()).await,
        Err(Error::Stale)
    );
    std::fs::rename(&path, temp.0.join("real")).unwrap();
    symlink(temp.0.join("real"), &path).unwrap();
    tokio::time::sleep(CHECK_EVERY).await;
    assert_eq!(
        accounts.read(now(), &CancellationToken::new()).await,
        Err(Error::Stale)
    );
    let fresh = Accounts::new(path).unwrap();
    assert_eq!(
        fresh.read(now(), &CancellationToken::new()).await,
        Err(Error::UnsafeFile)
    );
}

#[tokio::test]
async fn explicit_auth_change_drops_cswap_cadence_and_previous_account() {
    let _serial = test_lock().await;
    let temp = Temp::new();
    let script = temp.script("cswap", &format!("printf '%s' '{}'", output("before")));
    let mut owner = Cswap::new(temp.0.clone(), temp.0.clone().into_os_string()).unwrap();
    let stop = CancellationToken::new();
    owner.refresh(now(), &stop).await.unwrap();
    temp.script("cswap", &format!("printf '%s' '{}'", output("after")));
    owner.invalidate();
    assert!(owner.current(now()).is_none());
    owner.refresh(now(), &stop).await.unwrap();
    assert_eq!(
        owner.current(now()).unwrap().accounts[0].display_name,
        "after"
    );
    std::fs::remove_file(script).unwrap();
    owner.invalidate();
    assert!(owner.refresh(now(), &stop).await.is_err());
    assert!(owner.current(now()).is_none());
}
