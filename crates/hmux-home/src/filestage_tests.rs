use super::*;
use hmux_protocol::protobuf::types::{FileHeader, Session};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const REQUEST_ID: &str = "00112233445566778899aabbccddeeff";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().canonicalize().unwrap();
        let path = base.join(format!(
            "hmux-e2e-filestage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn root(&self) -> PathBuf {
        self.0.join("hmux/staged-files-v1")
    }
    fn store(&self) -> Store {
        Store::open(self.root()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn header(sizes: &[(i64, &str)]) -> UploadHeader {
    UploadHeader {
        protocol_version: 1,
        request_id: REQUEST_ID.into(),
        session: Some(Session {
            id: "$7".into(),
            created_at: 1_700_000_000,
        }),
        file_count: sizes.len() as u32,
        total_bytes: sizes.iter().map(|(size, _)| size).sum(),
        files: sizes
            .iter()
            .enumerate()
            .map(|(index, (size, extension))| FileHeader {
                index: index as u32,
                size: *size,
                extension: (*extension).into(),
            })
            .collect(),
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}
fn names(root: &Path) -> Vec<String> {
    let mut names = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn streaming_boundaries_hashes_manifest_and_delivery_accept() {
    let f = Fixture::new();
    let store = f.store();
    let mut stage = store
        .begin(
            header(&[(5, "png"), (3, "")]),
            1_700_000_000,
            CancellationToken::new(),
            deadline(),
        )
        .unwrap();
    assert_eq!(stage.write(b"abcd"), Ok(4));
    assert_eq!(stage.write(b"efgh"), Ok(8));
    let completion = stage.commit(1_700_000_600).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&completion).unwrap();
    assert_eq!(value["expires_at_unix"], 1_700_011_400);
    assert_eq!(value["request_id"], REQUEST_ID);
    assert_eq!(value["session"]["id"], "$7");
    assert_eq!(
        value["files"][0]["sha256"],
        format!("{:x}", Sha256::digest(b"abcde"))
    );
    assert_eq!(
        value["files"][1]["sha256"],
        format!("{:x}", Sha256::digest(b"fgh"))
    );
    let first = PathBuf::from(value["files"][0]["path"].as_str().unwrap());
    let second = PathBuf::from(value["files"][1]["path"].as_str().unwrap());
    assert_eq!(first.file_name().unwrap(), "file-0001.png");
    assert_eq!(second.file_name().unwrap(), "file-0002");
    assert_eq!(fs::read(&first).unwrap(), b"abcde");
    assert_eq!(fs::read(&second).unwrap(), b"fgh");
    assert_eq!(
        fs::metadata(&first).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(first.parent().unwrap().join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest, value);
    assert_eq!(completion.last(), Some(&b'\n'));
    stage.accept();
    assert!(first.exists());
    store
        .sweep(1_700_011_399, CancellationToken::new(), deadline())
        .unwrap();
    assert!(first.exists());
    store
        .sweep(1_700_011_400, CancellationToken::new(), deadline())
        .unwrap();
    assert!(!first.exists());
}

#[test]
fn incomplete_oversized_cancelled_and_undelivered_stages_roll_back() {
    let f = Fixture::new();
    let store = f.store();
    {
        let mut stage = store
            .begin(
                header(&[(2, "")]),
                1_700_000_000,
                CancellationToken::new(),
                deadline(),
            )
            .unwrap();
        stage.write(b"x").unwrap();
        assert_eq!(stage.commit(1_700_000_100), Err(Error::Size));
    }
    assert_eq!(names(&f.root()), [".lock"]);
    {
        let mut stage = store
            .begin(
                header(&[(1, "")]),
                1_700_000_000,
                CancellationToken::new(),
                deadline(),
            )
            .unwrap();
        assert_eq!(stage.write(b"xy"), Err(Error::Size));
    }
    assert_eq!(names(&f.root()), [".lock"]);
    {
        let cancel = CancellationToken::new();
        let mut stage = store
            .begin(
                header(&[(1, "")]),
                1_700_000_000,
                cancel.clone(),
                deadline(),
            )
            .unwrap();
        cancel.cancel();
        assert_eq!(stage.write(b"x"), Err(Error::Cancelled));
    }
    assert_eq!(names(&f.root()), [".lock"]);
    {
        let mut stage = store
            .begin(
                header(&[(1, "")]),
                1_700_000_000,
                CancellationToken::new(),
                deadline(),
            )
            .unwrap();
        stage.write(b"x").unwrap();
        stage.commit(1_700_000_100).unwrap();
    }
    assert_eq!(names(&f.root()), [".lock"]);
}

#[test]
fn independent_header_validation_and_chunk_limit() {
    let f = Fixture::new();
    let store = f.store();
    for invalid in [
        {
            let mut h = header(&[(1, "")]);
            h.protocol_version = 2;
            h
        },
        {
            let mut h = header(&[(1, "")]);
            h.request_id = "wrong".into();
            h
        },
        {
            let mut h = header(&[(1, "")]);
            h.files[0].extension = "../x".into();
            h
        },
        {
            let mut h = header(&[(1, "")]);
            h.total_bytes = 2;
            h
        },
        {
            let mut h = header(&[(1, "")]);
            h.files[0].size = FILE_MAX + 1;
            h.total_bytes = FILE_MAX + 1;
            h
        },
        {
            let mut h = header(&[(1, "")]);
            h.session.as_mut().unwrap().created_at = 0;
            h
        },
    ] {
        assert_eq!(
            store
                .begin(invalid, 1_700_000_000, CancellationToken::new(), deadline())
                .err(),
            Some(Error::Header)
        );
    }
    let mut stage = store
        .begin(
            header(&[((CHUNK_MAX + 1) as i64, "")]),
            1_700_000_000,
            CancellationToken::new(),
            deadline(),
        )
        .unwrap();
    assert_eq!(stage.write(&vec![0; CHUNK_MAX + 1]), Err(Error::Size));
}

#[test]
fn lock_wait_cancellation_and_stage_count_quota() {
    let f = Fixture::new();
    let store = f.store();
    let first = store
        .begin(
            header(&[(1, "")]),
            1_700_000_000,
            CancellationToken::new(),
            deadline(),
        )
        .unwrap();
    let stop = CancellationToken::new();
    let worker = {
        let store = store.clone();
        let stop = stop.clone();
        thread::spawn(move || {
            store
                .begin(
                    header(&[(1, "")]),
                    1_700_000_000,
                    stop,
                    Instant::now() + Duration::from_secs(2),
                )
                .err()
        })
    };
    thread::sleep(Duration::from_millis(60));
    stop.cancel();
    assert_eq!(worker.join().unwrap(), Some(Error::Cancelled));
    drop(first);
    for index in 0..MAX_STAGES {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(f.root().join(format!("1700001000-{index:032x}")))
            .unwrap();
    }
    assert_eq!(
        store
            .begin(
                header(&[(1, "")]),
                1_700_000_000,
                CancellationToken::new(),
                deadline()
            )
            .err(),
        Some(Error::Quota)
    );
}

#[test]
fn spool_byte_quota_reserves_manifest_space() {
    let f = Fixture::new();
    let store = f.store();
    let existing = f.root().join("1700001000-11112222333344445555666677778888");
    fs::DirBuilder::new().mode(0o700).create(&existing).unwrap();
    let file = existing.join("file-0001");
    let handle = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)
        .unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    handle
        .set_len((SPOOL_MAX - MANIFEST_RESERVATION - 1) as u64)
        .unwrap();
    let stage = store
        .begin(
            header(&[(1, "")]),
            1_700_000_000,
            CancellationToken::new(),
            deadline(),
        )
        .unwrap();
    drop(stage);
    handle
        .set_len((SPOOL_MAX - MANIFEST_RESERVATION) as u64)
        .unwrap();
    assert_eq!(
        store
            .begin(
                header(&[(1, "")]),
                1_700_000_000,
                CancellationToken::new(),
                deadline()
            )
            .err(),
        Some(Error::Quota)
    );
}

#[test]
fn sweep_only_recognized_expired_stages_and_preserves_unsafe_entries() {
    let f = Fixture::new();
    let store = f.store();
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let expired = format!("{:010}-{}", now - 1, "00112233445566778899aabbccddeeff");
    let fresh = format!("{:010}-{}", now + 1000, "11112222333344445555666677778888");
    let incoming = ".incoming-99990000111122223333444455556666";
    for name in [&expired, &fresh, incoming] {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(f.root().join(name))
            .unwrap();
    }
    store
        .sweep(now + INCOMING_TTL + 1, CancellationToken::new(), deadline())
        .unwrap();
    assert!(!f.root().join(&expired).exists());
    assert!(!f.root().join(incoming).exists());
    assert!(f.root().join(&fresh).exists());
    fs::create_dir(f.root().join("unknown")).unwrap();
    assert_eq!(
        store.sweep(now + INCOMING_TTL + 2, CancellationToken::new(), deadline()),
        Err(Error::Unsafe)
    );
    assert!(f.root().join("unknown").exists());
}

#[test]
fn unsafe_modes_symlinks_and_hardlinks_are_preserved() {
    let f = Fixture::new();
    let root = f.root();
    let store = f.store();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(Store::open(root.clone()).err(), Some(Error::Root));
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let name = "1700000000-00112233445566778899aabbccddeeff";
    let stage = root.join(name);
    fs::DirBuilder::new().mode(0o700).create(&stage).unwrap();
    let victim = f.0.join("victim");
    fs::write(&victim, b"keep").unwrap();
    std::os::unix::fs::symlink(&victim, stage.join("file-0001")).unwrap();
    assert_eq!(
        store.sweep(1_700_000_001, CancellationToken::new(), deadline()),
        Err(Error::Unsafe)
    );
    assert_eq!(fs::read(&victim).unwrap(), b"keep");
    fs::remove_file(stage.join("file-0001")).unwrap();
    fs::hard_link(&victim, stage.join("file-0001")).unwrap();
    assert_eq!(
        store.sweep(1_700_000_001, CancellationToken::new(), deadline()),
        Err(Error::Unsafe)
    );
    assert_eq!(fs::read(&victim).unwrap(), b"keep");
}

#[test]
fn root_symlink_is_rejected_without_touching_target() {
    let f = Fixture::new();
    let parent = f.0.join("hmux");
    let target = f.0.join("target");
    fs::DirBuilder::new().mode(0o700).create(&parent).unwrap();
    fs::DirBuilder::new().mode(0o700).create(&target).unwrap();
    std::os::unix::fs::symlink(&target, f.root()).unwrap();
    assert_eq!(Store::open(f.root()).err(), Some(Error::Root));
    assert!(target.exists());
}

#[test]
fn fresh_cache_ancestors_are_created_private() {
    let f = Fixture::new();
    let root = f.0.join("missing/cache/hmux/staged-files-v1");
    Store::open(root.clone()).unwrap();
    for path in [f.0.join("missing"), f.0.join("missing/cache"), root] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
