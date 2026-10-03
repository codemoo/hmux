use super::*;
use crate::{
    binding::{Binding, Provider},
    completion_tracker::Tracker,
    conversation_link::{save, Link},
};
use hmux_model::SessionIdentity;
use std::{
    fs,
    io::Write,
    os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
const START: &str = "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n";
const DONE: &str = "{\"type\":\"event_msg\",\"timestamp\":\"2026-10-03T01:00:00Z\",\"payload\":{\"type\":\"task_complete\"}}\n";
struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    inspector: Inspector,
    reader: TmuxCatalogReader,
    targets: Vec<Target>,
    runtime: tokio::runtime::Runtime,
    stop: CancellationToken,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-pinned-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let write = |name: &str, body: &str, mode| {
            let p = dir.join(name);
            fs::write(&p, body).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
        };
        write("ps", "#!/bin/sh\nroot=${0%/*}\nif [ \"$1\" = -p ]; then /bin/cat \"$root/stamp\"; else /bin/cat \"$root/processes\"; fi\n", 0o700);
        write(
            "lsof",
            "#!/bin/sh\nroot=${0%/*}\n/bin/cat \"$root/files\"\n",
            0o700,
        );
        write("tmux", "#!/bin/sh\nroot=${0%/*}\ncase \"$1\" in\nlist-sessions) /bin/cat \"$root/sessions\" ;;\nlist-windows) /bin/cat \"$root/windows\" ;;\n*) exit 99 ;;\nesac\n",0o700);
        write("stamp", "start-one\n", 0o600);
        write("processes", "80 1 S 0.0 zsh\n90 80 S+ 0.1 codex\n", 0o600);
        write("files", "", 0o600);
        write("sessions","$7|:hmux-sep-v1:|example|:hmux-sep-v1:|1700000000|:hmux-sep-v1:|1700000200|:hmux-sep-v1:|0|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|\n",0o600);
        write("windows","$7|:hmux-sep-v1:|main|:hmux-sep-v1:|1|:hmux-sep-v1:|/synthetic|:hmux-sep-v1:|zsh|:hmux-sep-v1:|80|:hmux-sep-v1:|24|:hmux-sep-v1:|80\n",0o600);
        let path = dir.join(".codex/sessions/2026/10/03/rollout-selected.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"selected\",\"source\":\"cli\"}}}}\n{START}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            inspector: Inspector::new(dir.clone(), dir.join("ps"), Some(dir.join("lsof"))).unwrap(),
            reader: TmuxCatalogReader::new(dir.join("tmux"), None, Duration::from_secs(3)).unwrap(),
            path,
            targets: vec![Target {
                identity: SessionIdentity {
                    id: "$7".into(),
                    created_at: 1700000000,
                },
                pane: Some(80),
            }],
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
            stop: CancellationToken::new(),
            dir,
        }
    }
    fn sources(&self) -> Sources<'_> {
        Sources {
            state: &self.dir,
            reader: &self.reader,
            inspector: &self.inspector,
            stop: &self.stop,
            deadline: Instant::now() + Duration::from_secs(10),
            runtime: self.runtime.handle(),
        }
    }
    fn link(&self, notify: bool) {
        let source = self.sources();
        let mut link = Link::new(
            self.targets[0].identity.clone(),
            80,
            &Binding::unavailable(Provider::Codex, 90),
            "start-one".into(),
            ("selected".into(), self.path.clone()),
            &self.stop,
            source.deadline,
        )
        .unwrap();
        if notify {
            link.enable_notifications().unwrap()
        }
        save(&self.dir, &self.targets[0].identity, Some(&link)).unwrap();
    }
    fn observations(&self) -> Vec<Observation> {
        let source = self.sources();
        let scan = self
            .inspector
            .scan(
                &[80],
                ScanPurpose::Completion,
                &self.stop,
                source.deadline,
                source.runtime,
            )
            .unwrap();
        source.observations(&self.targets, &scan).unwrap()
    }
    fn append(&self, s: &str) {
        fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .unwrap()
            .write_all(s.as_bytes())
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.dir).unwrap()
    }
}
#[test]
fn pinned_completion_requires_opt_in_and_never_replays_history() {
    let f = Fixture::new();
    f.link(false);
    let mut tracker = Tracker::default();
    let before = f.observations();
    assert!(!before[0].ownership.starts_with("link:"));
    assert!(tracker
        .observe(&before, &f.stop, f.sources().deadline)
        .unwrap()
        .is_empty());
    f.append(DONE);
    f.link(true);
    let before = f.observations();
    assert!(before[0].ownership.starts_with("link:"));
    assert!(tracker
        .observe(&before, &f.stop, f.sources().deadline)
        .unwrap()
        .is_empty());
    f.append(START);
    let before = f.observations();
    assert!(tracker
        .observe(&before, &f.stop, f.sources().deadline)
        .unwrap()
        .is_empty());
    f.append(DONE);
    let before = f.observations();
    assert_eq!(
        tracker
            .observe(&before, &f.stop, f.sources().deadline)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.sources().revalidate(&f.targets, &before).unwrap().len(),
        1
    );
    assert!(tracker
        .observe(&f.observations(), &f.stop, f.sources().deadline)
        .unwrap()
        .is_empty());
}
#[test]
fn source_changes_between_scan_and_send_reject_pinned_events() {
    for change in [
        "unlink",
        "disable",
        "relink",
        "stamp",
        "pid",
        "pane",
        "lifetime",
        "inode",
        "symlink",
        "exact",
        "ambiguous",
    ] {
        let f = Fixture::new();
        f.link(true);
        let before = f.observations();
        assert!(before[0].ownership.starts_with("link:"), "{change}");
        let mut tracker = Tracker::default();
        assert!(tracker
            .observe(&before, &f.stop, f.sources().deadline)
            .unwrap()
            .is_empty());
        f.append(DONE);
        assert_eq!(
            tracker
                .observe(&before, &f.stop, f.sources().deadline)
                .unwrap()
                .len(),
            1
        );
        match change {
   "unlink"=> save(&f.dir,&f.targets[0].identity,None).unwrap(),
   "disable"=>f.link(false), "relink"=>f.link(true),
   "stamp"=>fs::write(f.dir.join("stamp"),"start-two\n").unwrap(),
   "pid"=>fs::write(f.dir.join("processes"),"80 1 S 0.0 zsh\n91 80 S+ 0.1 codex\n").unwrap(),
   "pane"=>fs::write(f.dir.join("windows"),"$7|:hmux-sep-v1:|main|:hmux-sep-v1:|1|:hmux-sep-v1:|/synthetic|:hmux-sep-v1:|zsh|:hmux-sep-v1:|81|:hmux-sep-v1:|24|:hmux-sep-v1:|81\n").unwrap(),
   "lifetime"=>{let p=f.dir.join("sessions");let s=fs::read_to_string(&p).unwrap();fs::write(p,s.replace("1700000000","1700000001")).unwrap()},
   "inode"|"symlink"=>{let old=f.path.with_extension("old");fs::rename(&f.path,&old).unwrap();if change=="inode" {fs::copy(old,&f.path).unwrap();}else{symlink(old,&f.path).unwrap();}},
   "exact"=>fs::write(f.dir.join("files"),format!("p90\nn{}\n",f.path.display())).unwrap(),
   "ambiguous"=>{let second=f.path.with_file_name("rollout-other.jsonl");fs::write(&second,"{\"type\":\"session_meta\",\"payload\":{\"id\":\"other\",\"source\":\"cli\"}}\n").unwrap();fs::write(f.dir.join("files"),format!("p90\nn{}\nn{}\n",f.path.display(),second.display())).unwrap()},
   _=>unreachable!()
  }
        assert!(
            f.sources()
                .revalidate(&f.targets, &before)
                .unwrap()
                .is_empty(),
            "{change}"
        );
    }
}
