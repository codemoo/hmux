use super::*;
use std::{
    os::unix::fs::symlink,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    thread,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-create-plan-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn executable(&self, name: &str) -> PathBuf {
        let bin = self.0.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let path = bin.join(name);
        fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    fn inventory(&self, name: &str, command: Vec<String>, base: PathBuf) -> Inventory {
        Inventory {
            schema_version: 1,
            revision: "synthetic".into(),
            profiles: Some(vec![Profile {
                id: name.into(),
                label: "Synthetic".into(),
                default_directory: base.to_string_lossy().into_owned(),
                command: Some(command),
                tags: None,
            }]),
        }
    }
    fn prepare(&self, inventory: &Inventory, profile: &str, name: &str) -> Result<Plan, Error> {
        Plan::prepare(
            inventory,
            profile,
            name,
            &self.0,
            self.0.join("bin").as_os_str(),
            OsStr::new("invalid-relative-shell"),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn slug_matches_go_letter_number_and_space_categories() {
    for (input, expected) in [
        ("한글 세션-01", "한글-세션-01"),
        ("../My Project/a:1", "My-Project-a-1"),
        ("../../", "session"),
        ("🚀", "session"),
        ("  --hello__  ", "hello"),
        ("hello;$(touch nope)", "hello-touch-nope"),
        ("a\u{0301}b", "a-b"),
        ("\u{0301}", "session"),
        ("Ⅻ①", "Ⅻ①"),
        ("A\u{00a0}B", "A-B"),
    ] {
        assert_eq!(workspace_slug(input, 36), expected, "{input:?}");
    }
    assert!(in_ranges('①', go_unicode::LETTER_OR_NUMBER));
    assert!(!in_ranges('\u{0301}', go_unicode::LETTER_OR_NUMBER));
    assert!(in_ranges('\u{0085}', go_unicode::SPACE));
    assert!(in_ranges('\u{0000}', go_unicode::CONTROL));
}

#[test]
fn prepare_validates_without_creating_base_or_running_command() {
    let fixture = Fixture::new();
    fixture.executable("synthetic-tool");
    let base = fixture.0.join("missing/work");
    let inventory = fixture.inventory("shell", vec!["synthetic-tool".into()], base.clone());
    for name in ["", "한글 project/one", &"가".repeat(80)] {
        assert!(fixture.prepare(&inventory, "shell", name).is_ok());
    }
    assert!(!base.exists());
    for name in ["bad\nname", &"a".repeat(81)] {
        assert!(matches!(
            fixture.prepare(&inventory, "shell", name),
            Err(Error::Name)
        ));
    }
    assert!(matches!(
        fixture.prepare(&inventory, "unknown", "okay"),
        Err(Error::UnknownProfile)
    ));
    assert!(!base.exists());

    let mut invalid = inventory.clone();
    invalid.profiles.as_mut().unwrap()[0].default_directory = "relative/path".into();
    assert!(matches!(
        fixture.prepare(&invalid, "shell", "okay"),
        Err(Error::Directory)
    ));
    invalid.profiles.as_mut().unwrap()[0].default_directory = "/".into();
    assert!(matches!(
        fixture.prepare(&invalid, "shell", "okay"),
        Err(Error::Directory)
    ));
    invalid.profiles.as_mut().unwrap()[0].default_directory = base.to_string_lossy().into_owned();
    invalid.profiles.as_mut().unwrap()[0].command = Some(vec!["/bin/sh".into()]);
    assert!(matches!(
        fixture.prepare(&invalid, "shell", "okay"),
        Err(Error::Command)
    ));
    invalid.profiles.as_mut().unwrap()[0].command =
        Some(vec!["synthetic-tool".into(), "x".repeat(4097)]);
    assert!(matches!(
        fixture.prepare(&invalid, "shell", "okay"),
        Err(Error::Command)
    ));
    invalid.profiles.as_mut().unwrap()[0].command = Some(vec!["synthetic-tool".into(); 65]);
    assert!(matches!(
        fixture.prepare(&invalid, "shell", "okay"),
        Err(Error::Command)
    ));
    assert!(!base.exists());
}

#[test]
fn command_argv_is_literal_and_provider_wrapper_matches_go() {
    let fixture = Fixture::new();
    let tool = fixture.executable("synthetic-tool");
    for provider in ["codex", "claude"] {
        fixture.executable(provider);
    }
    let base = fixture.0.join("work");
    let single = fixture.inventory("shell", vec!["synthetic-tool".into()], base.clone());
    let allocated = fixture
        .prepare(&single, "shell", "one")
        .unwrap()
        .allocate()
        .unwrap();
    assert_eq!(
        allocated.command,
        [
            OsString::from("/bin/sh"),
            OsString::from("-c"),
            OsString::from("exec \"$1\""),
            OsString::from("hmux-launch"),
            tool.as_os_str().to_owned(),
        ]
    );
    let tmux = allocated.tmux_args();
    assert_eq!(
        &tmux[..9],
        &[
            OsString::from("new-session"),
            OsString::from("-d"),
            OsString::from("-P"),
            OsString::from("-F"),
            OsString::from("#{session_id} #{session_created}"),
            OsString::from("-s"),
            OsString::from(&allocated.name),
            OsString::from("-c"),
            allocated.directory.as_os_str().to_owned(),
        ]
    );
    assert_eq!(&tmux[9..], allocated.command.as_slice());

    let literal = "has space; $(false) 'quoted'";
    let multiple = fixture.inventory(
        "shell",
        vec!["synthetic-tool".into(), literal.into(), String::new()],
        base.clone(),
    );
    let multi = fixture.prepare(&multiple, "shell", "two").unwrap();
    assert_eq!(
        multi.command,
        [
            tool.as_os_str().to_owned(),
            OsString::from(literal),
            OsString::new()
        ]
    );

    for provider in ["codex", "claude"] {
        let inventory = fixture.inventory(
            provider,
            vec![provider.into(), literal.into(), String::new()],
            base.clone(),
        );
        let plan = fixture.prepare(&inventory, provider, "provider").unwrap();
        assert_eq!(plan.command[0], "/bin/sh");
        assert_eq!(plan.command[1], "-c");
        assert_eq!(plan.command[2], PROVIDER_SCRIPT);
        assert_eq!(plan.command[3], "hmux-provider");
        assert_ne!(plan.command[4], "invalid-relative-shell");
        assert!(Path::new(&plan.command[4]).is_absolute());
        assert_eq!(plan.command[5], fixture.0.join("bin").join(provider));
        assert_eq!(plan.command[6], literal);
        assert!(plan.command[7].is_empty());
        assert_eq!(format!("{plan:?}"), "Plan([redacted])");
    }
}

#[test]
fn relative_path_search_is_rejected_and_home_fallback_is_absolute() {
    let fixture = Fixture::new();
    fixture.executable("synthetic-tool");
    let inventory = fixture.inventory(
        "shell",
        vec!["synthetic-tool".into()],
        fixture.0.join("work"),
    );
    assert!(matches!(
        Plan::prepare(
            &inventory,
            "shell",
            "x",
            &fixture.0,
            OsStr::new("bin:."),
            OsStr::new("/no/shell")
        ),
        Err(Error::Executable)
    ));
    let mut home_inventory = inventory.clone();
    home_inventory.profiles.as_mut().unwrap()[0].default_directory = "~/projects/../work".into();
    let plan = fixture.prepare(&home_inventory, "shell", "x").unwrap();
    assert_eq!(plan.base, fixture.0.join("work"));
}

#[test]
fn concurrent_allocation_never_reuses_an_occupied_child_or_follows_its_symlink() {
    let fixture = Arc::new(Fixture::new());
    fixture.executable("synthetic-tool");
    let base = fixture.0.join("work");
    fs::create_dir(&base).unwrap();
    let outside = fixture.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"unchanged").unwrap();
    symlink(&outside, base.join("same")).unwrap();
    let selected = fixture.0.join("configured-link");
    symlink(&base, &selected).unwrap();
    let inventory = Arc::new(fixture.inventory("shell", vec!["synthetic-tool".into()], selected));
    let start = Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let fixture = fixture.clone();
            let inventory = inventory.clone();
            let start = start.clone();
            thread::spawn(move || {
                let plan = fixture.prepare(&inventory, "shell", "same").unwrap();
                start.wait();
                plan.allocate().unwrap()
            })
        })
        .collect();
    let allocations: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    let names: std::collections::HashSet<_> = allocations
        .iter()
        .map(|allocation| &allocation.name)
        .collect();
    let folders: std::collections::HashSet<_> = allocations
        .iter()
        .map(|allocation| &allocation.directory)
        .collect();
    assert_eq!(names.len(), 8);
    assert_eq!(folders.len(), 8);
    for allocated in &allocations {
        assert!(allocated
            .directory
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("same-"));
        assert_eq!(
            allocated.directory.canonicalize().unwrap().parent(),
            Some(base.as_path())
        );
        assert_eq!(
            fs::metadata(&allocated.directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(allocated
            .name
            .starts_with(allocated.directory.file_name().unwrap().to_str().unwrap()));
        assert_eq!(fs::read_dir(&allocated.directory).unwrap().count(), 0);
    }
    drop(allocations);
    assert_eq!(fs::read_dir(base).unwrap().count(), 9);
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"unchanged");
}
