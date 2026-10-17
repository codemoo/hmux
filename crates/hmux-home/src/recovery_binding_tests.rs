use super::*;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    inspector: Inspector,
    runtime: tokio::runtime::Runtime,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-e2e-recovery-binding-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let fixture = Self {
            inspector: Inspector::new(root.clone(), root.join("ps"), Some(root.join("lsof")))
                .unwrap(),
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap(),
            root,
        };
        fixture.tool(
            "ps",
            "#!/bin/sh\nroot=${0%/*}\n/bin/cat \"$root/processes\"\n",
        );
        fixture.tool(
            "lsof",
            "#!/bin/sh\nroot=${0%/*}\n/bin/cat \"$root/files-$4\"\n",
        );
        fixture.put(
            "processes",
            "101 1 S 0 zsh\n201 101 S+ 0 codex\n102 1 S 0 zsh\n202 102 S+ 0 codex\n",
        );
        fixture
    }
    fn put(&self, name: &str, body: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        path
    }
    fn tool(&self, name: &str, body: &str) {
        let path = self.put(name, body);
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn thread(&self, id: &str) -> PathBuf {
        self.put(
            &format!(".codex/sessions/2026/10/17/rollout-fixture-{id}.jsonl"),
            &format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"source\":\"cli\",\"cwd\":\"/synthetic/shared-project\"}}}}\n"),
        )
    }
    fn scan(&self, purpose: ScanPurpose) -> Result<inspection::Scan, inspection::Error> {
        self.inspector.scan(
            &[101, 102],
            purpose,
            &CancellationToken::new(),
            Instant::now() + Duration::from_secs(10),
            self.runtime.handle(),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn recovery_keeps_same_directory_threads_distinct_and_rejects_failed_observations() {
    let f = Fixture::new();
    let a = f.thread("thread-a");
    let b = f.thread("thread-b");
    // This newer, unowned thread shares the cwd and must never be selected.
    f.thread("thread-newest");
    f.put(
        "files-201,202",
        &format!("p201\nn{}\np202\nn{}\n", a.display(), b.display()),
    );
    let first = f.scan(ScanPurpose::Recovery).unwrap();
    let second = f.scan(ScanPurpose::Recovery).unwrap();
    let refs = stable(first.bindings, &second.bindings);
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[&101].session_id, "thread-a");
    assert_eq!(refs[&102].session_id, "thread-b");
    assert_eq!(
        refs[&101].config_dir,
        f.root.join(".codex").to_str().unwrap()
    );

    // An omitted process is unknown, even when lsof reports partial success.
    f.put("files-201,202", &format!("p201\nn{}\n", a.display()));
    f.tool(
        "lsof",
        "#!/bin/sh\nroot=${0%/*}\n/bin/cat \"$root/files-$4\"\nexit 1\n",
    );
    assert!(matches!(
        f.scan(ScanPurpose::Recovery),
        Err(inspection::Error::Unavailable)
    ));
    let soft = f.scan(ScanPurpose::Conversation).unwrap();
    assert_eq!(soft.bindings[&101].status, Status::Ready);
    assert_eq!(soft.bindings[&102].status, Status::Unavailable);

    f.tool("lsof", "#!/bin/sh\nexit 2\n");
    assert!(matches!(
        f.scan(ScanPurpose::Recovery),
        Err(inspection::Error::Unavailable)
    ));
    assert!(f.scan(ScanPurpose::Conversation).is_ok());
    assert!(f.scan(ScanPurpose::Completion).is_ok());

    // A live provider without an exact transcript must not clear old references.
    f.tool("lsof", "#!/bin/sh\nprintf 'p201\\np202\\n'\n");
    let first = f.scan(ScanPurpose::Recovery).unwrap();
    let second = f.scan(ScanPurpose::Recovery).unwrap();
    assert!(stable_checked(first, second).is_err());
}

#[test]
fn recovery_requires_complete_wrapper_observations() {
    let f = Fixture::new();
    f.put(
        "processes",
        "101 1 S 0 zsh\n201 101 S+ 0 codex\n301 201 S 0 node\n102 1 S 0 zsh\n",
    );
    let a = f.thread("wrapped-thread");
    f.put("files-201", "p201\n");
    f.put("files-301", &format!("p301\nn{}\n", a.display()));
    let scan = f.scan(ScanPurpose::Recovery).unwrap();
    assert_eq!(scan.bindings[&101].record_id, "wrapped-thread");
    assert_eq!(scan.bindings[&101].file_pid, 301);

    f.put("files-301", "");
    assert!(matches!(
        f.scan(ScanPurpose::Recovery),
        Err(inspection::Error::Unavailable)
    ));
    assert!(f.scan(ScanPurpose::Conversation).is_ok());

    f.tool(
        "lsof",
        "#!/bin/sh\ncase \"$4\" in 201) printf 'p201\\n';; *) exit 2;; esac\n",
    );
    assert!(matches!(
        f.scan(ScanPurpose::Recovery),
        Err(inspection::Error::Unavailable)
    ));

    // A fully observed empty wrapper is a verified unavailable binding.
    f.tool("lsof", "#!/bin/sh\nprintf 'p%s\\n' \"$4\"\n");
    let scan = f.scan(ScanPurpose::Recovery).unwrap();
    assert_eq!(scan.bindings[&101].status, Status::Unavailable);
}

#[test]
fn recovery_without_codex_does_not_require_lsof_and_still_binds_claude() {
    let f = Fixture::new();
    f.tool("lsof", "#!/bin/sh\nexit 2\n");
    f.put("processes", "101 1 S 0 zsh\n102 1 S 0 zsh\n");
    assert!(f.scan(ScanPurpose::Recovery).unwrap().bindings.is_empty());

    f.put(
        "processes",
        "101 1 S 0 zsh\n201 101 S+ 0 claude\n102 1 S 0 zsh\n",
    );
    f.put(
        ".claude/sessions/201.json",
        "{\"pid\":201,\"sessionId\":\"claude-thread\"}",
    );
    f.put(
        ".claude/projects/shared-project/claude-thread.jsonl",
        "{}\n",
    );
    let first = f.scan(ScanPurpose::Recovery).unwrap();
    let second = f.scan(ScanPurpose::Recovery).unwrap();
    let refs = stable(first.bindings, &second.bindings);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[&101].provider, "claude");
    assert_eq!(refs[&101].session_id, "claude-thread");
}
