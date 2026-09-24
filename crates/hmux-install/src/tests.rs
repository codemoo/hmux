use super::*;
use std::os::unix::fs::symlink;

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    bin: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = (0..1000)
            .map(|n| {
                base.join(format!(
                    "hmux-install-test-{}-{}-{n}",
                    std::process::id(),
                    id().unwrap()
                ))
            })
            .find(|candidate| match fs::create_dir(candidate) {
                Ok(()) => true,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
                Err(e) => panic!("{e}"),
            })
            .unwrap();
        let source = root.join("source");
        let bin = root.join("bin");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&bin).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self { root, source, bin }
    }
    fn put(&self, dir: &Path, name: &str, bytes: &[u8], mode: u32) {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    fn sources(&self, web: &[u8], agent: &[u8]) {
        self.put(&self.source, "hmux-web", web, 0o755);
        self.put(&self.source, "hmux-agent", agent, 0o755);
    }
    fn content(&self, dir: &Path, name: &str) -> Vec<u8> {
        fs::read(dir.join(name)).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn first_install_and_upgrade_retain_old_pair() {
    let f = Fixture::new();
    f.sources(b"web-one", b"agent-one");
    install(&f.source, &f.bin).unwrap();
    assert_eq!(f.content(&f.bin, "hmux-web"), b"web-one");
    assert_eq!(f.content(&f.bin, "hmux-agent"), b"agent-one");
    f.sources(b"web-two", b"agent-two");
    install(&f.source, &f.bin).unwrap();
    assert_eq!(f.content(&f.bin, "hmux-web"), b"web-two");
    assert_eq!(f.content(&f.bin, "hmux-agent"), b"agent-two");
    let backups: Vec<_> = fs::read_dir(&f.bin)
        .unwrap()
        .map(|x| x.unwrap().file_name())
        .filter(|x| x.to_string_lossy().contains(".backup-"))
        .collect();
    assert_eq!(backups.len(), 2);
    assert!(absent(&f.bin.join(JOURNAL)).unwrap());
}

#[test]
fn invalid_second_source_leaves_pair_intact() {
    let f = Fixture::new();
    f.sources(b"web-old", b"agent-old");
    install(&f.source, &f.bin).unwrap();
    f.sources(b"web-new", b"agent-new");
    fs::set_permissions(
        f.source.join("hmux-agent"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(install(&f.source, &f.bin).is_err());
    assert_eq!(f.content(&f.bin, "hmux-web"), b"web-old");
    assert_eq!(f.content(&f.bin, "hmux-agent"), b"agent-old");
}

#[test]
fn interruption_after_first_replace_recovers_old_pair_and_mode() {
    let f = Fixture::new();
    f.sources(b"web-old", b"agent-old");
    install(&f.source, &f.bin).unwrap();
    fs::set_permissions(f.bin.join("hmux-web"), fs::Permissions::from_mode(0o700)).unwrap();
    f.sources(b"web-new", b"agent-new");
    assert_eq!(
        install_inner(&f.source, &f.bin, Some(Fault::Replace(0)))
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(f.content(&f.bin, "hmux-web"), b"web-new");
    assert_eq!(f.content(&f.bin, "hmux-agent"), b"agent-old");
    recover(&f.bin).unwrap();
    assert_eq!(f.content(&f.bin, "hmux-web"), b"web-old");
    assert_eq!(f.content(&f.bin, "hmux-agent"), b"agent-old");
    assert_eq!(
        fs::metadata(f.bin.join("hmux-web")).unwrap().mode() & 0o777,
        0o700
    );
    recover(&f.bin).unwrap();
}

#[test]
fn altered_installed_target_refuses_rollback_without_touching_other_target() {
    let f = Fixture::new();
    f.sources(b"web-old", b"agent-old");
    install(&f.source, &f.bin).unwrap();
    f.sources(b"web-new", b"agent-new");
    install_inner(&f.source, &f.bin, Some(Fault::Replace(0))).unwrap_err();
    fs::write(f.bin.join("hmux-web"), b"external-change").unwrap();
    assert!(recover(&f.bin).is_err());
    assert_eq!(f.content(&f.bin, "hmux-web"), b"external-change");
    assert_eq!(f.content(&f.bin, "hmux-agent"), b"agent-old");
    assert!(!absent(&f.bin.join(JOURNAL)).unwrap());
}

#[test]
fn symlink_sources_and_targets_are_rejected() {
    let f = Fixture::new();
    f.sources(b"web", b"agent");
    let real = f.source.join("hmux-web");
    fs::rename(&real, f.source.join("real-web")).unwrap();
    symlink("real-web", &real).unwrap();
    assert!(install(&f.source, &f.bin).is_err());
    fs::remove_file(&real).unwrap();
    fs::rename(f.source.join("real-web"), &real).unwrap();
    symlink(&real, f.bin.join("hmux-web")).unwrap();
    assert!(install(&f.source, &f.bin).is_err());
    assert!(absent(&f.bin.join("hmux-agent")).unwrap());
}

#[test]
fn next_install_recovers_pending_pair_before_retry() {
    let f = Fixture::new();
    f.sources(b"web-old", b"agent-old");
    install(&f.source, &f.bin).unwrap();
    f.sources(b"web-new", b"agent-new");
    install_inner(&f.source, &f.bin, Some(Fault::Replace(0))).unwrap_err();
    install(&f.source, &f.bin).unwrap();
    assert_eq!(f.content(&f.bin, "hmux-web"), b"web-new");
    assert_eq!(f.content(&f.bin, "hmux-agent"), b"agent-new");
    assert!(absent(&f.bin.join(JOURNAL)).unwrap());
}

#[test]
fn interrupted_preparation_cleans_only_scratch_files() {
    for fault in [Fault::Backup, Fault::Stage] {
        let f = Fixture::new();
        f.sources(b"web-old", b"agent-old");
        install(&f.source, &f.bin).unwrap();
        f.sources(b"web-new", b"agent-new");
        assert_eq!(
            install_inner(&f.source, &f.bin, Some(fault))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        let journal = read_journal(&f.bin).unwrap().unwrap();
        assert_eq!(journal.phase, Phase::Preparing);
        if fault == Fault::Backup {
            fs::write(
                f.bin.join(journal.entries[0].backup.as_ref().unwrap()),
                b"partial",
            )
            .unwrap();
        } else {
            fs::write(f.bin.join(&journal.entries[0].stage), b"partial").unwrap();
        }
        assert_eq!(f.content(&f.bin, "hmux-web"), b"web-old");
        assert_eq!(f.content(&f.bin, "hmux-agent"), b"agent-old");
        recover(&f.bin).unwrap();
        assert!(absent(&f.bin.join(JOURNAL)).unwrap());
        assert_eq!(f.content(&f.bin, "hmux-web"), b"web-old");
        assert_eq!(f.content(&f.bin, "hmux-agent"), b"agent-old");
        assert!(fs::read_dir(&f.bin).unwrap().all(|x| {
            let name = x.unwrap().file_name();
            let name = name.to_string_lossy();
            !name.contains(".stage-") && !name.contains(".restore-") && !name.contains(".backup-")
        }));
    }
}

#[test]
fn partial_restore_is_retried_from_valid_backup() {
    let f = Fixture::new();
    f.sources(b"web-old", b"agent-old");
    install(&f.source, &f.bin).unwrap();
    f.sources(b"web-new", b"agent-new");
    install_inner(&f.source, &f.bin, Some(Fault::Replace(0))).unwrap_err();
    let journal = read_journal(&f.bin).unwrap().unwrap();
    let restore = f.bin.join(&journal.entries[0].restore);
    fs::write(&restore, b"partial").unwrap();
    recover(&f.bin).unwrap();
    assert_eq!(f.content(&f.bin, "hmux-web"), b"web-old");
    assert!(absent(&restore).unwrap());
}

#[test]
fn retained_backup_set_stays_bounded() {
    let f = Fixture::new();
    for version in 0..5 {
        let web = format!("web-{version}");
        let agent = format!("agent-{version}");
        f.sources(web.as_bytes(), agent.as_bytes());
        install(&f.source, &f.bin).unwrap();
        let count = fs::read_dir(&f.bin)
            .unwrap()
            .filter(|x| {
                x.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".backup-")
            })
            .count();
        assert_eq!(count, if version == 0 { 0 } else { 2 });
    }
}

#[test]
fn lock_contention_times_out() {
    let f = Fixture::new();
    let _held = lock(&f.bin).unwrap();
    let start = Instant::now();
    let error = recover(&f.bin).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(start.elapsed() >= LOCK_WAIT);
    assert!(start.elapsed() < LOCK_WAIT + Duration::from_secs(1));
}

#[test]
fn oversized_journal_and_binary_growth_are_rejected() {
    let f = Fixture::new();
    fs::write(f.bin.join(JOURNAL), vec![b'x'; JOURNAL_LIMIT as usize + 1]).unwrap();
    fs::set_permissions(f.bin.join(JOURNAL), fs::Permissions::from_mode(0o600)).unwrap();
    assert!(read_journal(&f.bin).is_err());
    fs::remove_file(f.bin.join(JOURNAL)).unwrap();
    f.sources(b"web", b"agent");
    let path = f.source.join("hmux-web");
    let (mut file, meta) = open_binary(&path, true).unwrap();
    let mut append = OpenOptions::new().append(true).open(&path).unwrap();
    append.write_all(b"more").unwrap();
    assert!(hash(&mut file, meta.len()).is_err());
}

#[test]
fn hardlinked_binary_is_rejected() {
    let f = Fixture::new();
    f.sources(b"web", b"agent");
    fs::hard_link(f.source.join("hmux-web"), f.source.join("web-link")).unwrap();
    assert!(install(&f.source, &f.bin).is_err());
}

#[test]
fn orphaned_prejournal_temp_is_removed_without_touching_targets() {
    let f = Fixture::new();
    f.sources(b"web-old", b"agent-old");
    install(&f.source, &f.bin).unwrap();
    let orphan = f.bin.join(format!("{JOURNAL_TEMP}{}", id().unwrap()));
    fs::write(&orphan, b"partial journal").unwrap();
    fs::set_permissions(&orphan, fs::Permissions::from_mode(0o600)).unwrap();
    recover(&f.bin).unwrap();
    assert!(absent(&orphan).unwrap());
    assert_eq!(f.content(&f.bin, "hmux-web"), b"web-old");
    assert_eq!(f.content(&f.bin, "hmux-agent"), b"agent-old");
}

#[test]
fn failed_upgrade_keeps_previous_known_good_backup_pair() {
    let f = Fixture::new();
    f.sources(b"web-zero", b"agent-zero");
    install(&f.source, &f.bin).unwrap();
    f.sources(b"web-one", b"agent-one");
    install(&f.source, &f.bin).unwrap();
    f.sources(b"web-two", b"agent-two");
    install_inner(&f.source, &f.bin, Some(Fault::Replace(0))).unwrap_err();
    recover(&f.bin).unwrap();
    assert_eq!(f.content(&f.bin, "hmux-web"), b"web-one");
    assert_eq!(f.content(&f.bin, "hmux-agent"), b"agent-one");
    let mut contents: Vec<Vec<u8>> = fs::read_dir(&f.bin)
        .unwrap()
        .filter_map(|item| {
            let path = item.unwrap().path();
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".backup-")
                .then(|| fs::read(path).unwrap())
        })
        .collect();
    contents.sort();
    assert_eq!(contents, [b"agent-zero".to_vec(), b"web-zero".to_vec()]);
}
