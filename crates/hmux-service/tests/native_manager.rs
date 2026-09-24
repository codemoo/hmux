//! Opt-in native manager acceptance test. Run only with --ignored and
//! HMUX_RUN_NATIVE_MANAGER_TEST=1 in a disposable user service session.
#![forbid(unsafe_code)]

use hmux_service::{launch_agent, systemd_unit, Spec, LABEL};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const OPT_IN: &str = "HMUX_RUN_NATIVE_MANAGER_TEST";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const SETTLE_TIMEOUT: Duration = Duration::from_secs(20);

fn fixture(args: Vec<String>) -> io::Result<()> {
    // The rendered service starts this executable via /usr/bin/env -i. It
    // writes exactly what the manager launched, then stays alive until stopped.
    let mut log = None;
    let mut tail = args.iter().skip(1);
    while let Some(arg) = tail.next() {
        if arg == "--log-file" {
            log = tail.next().cloned();
        }
    }
    let path = PathBuf::from(log.ok_or_else(|| io::Error::other("missing fixture log path"))?);
    let observation = serde_json::json!({
        "pid": std::process::id(),
        "argv": args,
        "environment": std::env::vars().collect::<BTreeMap<_, _>>(),
        "cwd": std::env::current_dir()?.to_string_lossy(),
    });
    let pending = path.with_extension("pending");
    fs::write(&pending, serde_json::to_vec(&observation)?)?;
    fs::rename(pending, path)?;
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

#[derive(Clone, Copy)]
enum Manager {
    Systemd,
    Launchd,
}

struct OwnedService {
    manager: Manager,
    root: PathBuf,
    name: String,
    unit_path: PathBuf,
    domain: String,
    cleaned: bool,
}

impl OwnedService {
    fn command(&self, args: &[&str]) -> io::Result<String> {
        let program = match self.manager {
            Manager::Systemd => "/usr/bin/systemctl",
            Manager::Launchd => "/bin/launchctl",
        };
        let capture = self.root.join("manager-output");
        let output = File::create(&capture)?;
        let mut child = Command::new(program);
        child
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdout(Stdio::from(output.try_clone()?))
            .stderr(Stdio::from(output));
        // Preserve only session coordinates needed by the native user manager.
        for key in [
            "HOME",
            "USER",
            "LOGNAME",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
        ] {
            if let Some(value) = std::env::var_os(key) {
                child.env(key, value);
            }
        }
        let mut child = child.spawn()?;
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native manager command timed out",
                ));
            }
            thread::sleep(Duration::from_millis(50));
        };
        let mut text = String::new();
        File::open(capture)?.take(8193).read_to_string(&mut text)?;
        if text.len() > 8192 {
            return Err(io::Error::other("native manager output exceeds 8192 bytes"));
        }
        if !status.success() {
            return Err(io::Error::other(format!(
                "{program} {args:?}: {status}: {text}"
            )));
        }
        Ok(text)
    }

    fn systemctl(&self, args: &[&str]) -> io::Result<String> {
        let mut all = vec!["--user"];
        all.extend_from_slice(args);
        self.command(&all)
    }

    fn target(&self) -> String {
        format!("{}/{}", self.domain, self.name)
    }

    fn start(&self) -> io::Result<()> {
        match self.manager {
            Manager::Systemd => {
                self.systemctl(&["link", "--runtime", self.unit_path.to_str().unwrap()])?;
                self.systemctl(&["start", &self.name])?;
            }
            Manager::Launchd => {
                self.command(&["bootstrap", &self.domain, self.unit_path.to_str().unwrap()])?;
            }
        }
        Ok(())
    }

    fn restart(&self) -> io::Result<()> {
        match self.manager {
            Manager::Systemd => self.systemctl(&["restart", &self.name]).map(|_| ()),
            Manager::Launchd => self
                .command(&["kickstart", "-k", &self.target()])
                .map(|_| ()),
        }
    }

    fn pid(&self) -> io::Result<u32> {
        match self.manager {
            Manager::Systemd => {
                let raw = self.systemctl(&["show", "--property=MainPID", "--value", &self.name])?;
                raw.trim()
                    .parse()
                    .map_err(|_| io::Error::other(format!("invalid MainPID: {raw:?}")))
            }
            Manager::Launchd => {
                let raw = self.command(&["print", &self.target()])?;
                raw.lines()
                    .find_map(|line| line.trim().strip_prefix("pid = "))
                    .ok_or_else(|| io::Error::other("launchd reported no PID"))?
                    .parse()
                    .map_err(|_| io::Error::other("invalid launchd PID"))
            }
        }
    }

    fn active(&self) -> bool {
        match self.manager {
            Manager::Systemd => self
                .systemctl(&["is-active", "--quiet", &self.name])
                .is_ok(),
            Manager::Launchd => self.command(&["print", &self.target()]).is_ok(),
        }
    }

    fn stop_and_unlink(&mut self) -> io::Result<()> {
        let result = match self.manager {
            Manager::Systemd => {
                let stop = self.systemctl(&["stop", &self.name]);
                let unlink = self.systemctl(&["disable", "--runtime", &self.name]);
                let reload = self.systemctl(&["daemon-reload"]);
                stop.and(unlink).and(reload).map(|_| ())
            }
            Manager::Launchd => self.command(&["bootout", &self.target()]).map(|_| ()),
        };
        if result.is_ok() {
            self.cleaned = true;
        }
        result
    }
}

impl Drop for OwnedService {
    fn drop(&mut self) {
        if !self.cleaned {
            if let Err(error) = self.stop_and_unlink() {
                eprintln!(
                    "owned service cleanup failed: {error}; retained {}",
                    self.root.display()
                );
                return;
            }
        }
        // This path was created by this test and has a unique hmux-e2e name.
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn eventually<T>(mut check: impl FnMut() -> io::Result<Option<T>>) -> io::Result<T> {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        if let Some(value) = check()? {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "service state did not settle",
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn observation(path: &Path) -> io::Result<Option<serde_json::Value>> {
    match fs::read(path) {
        Ok(raw) => Ok(Some(serde_json::from_slice(&raw)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn alive(pid: u32) -> bool {
    rustix::process::Pid::from_raw(pid as i32)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

fn check_environment(value: &serde_json::Value, expected: &serde_json::Value) {
    let mut observed = value.clone();
    // macOS can add this native text-encoding coordinate after env -i; it is
    // not a connector setting. Every explicitly rendered value remains exact.
    if cfg!(target_os = "macos") {
        observed
            .as_object_mut()
            .unwrap()
            .remove("__CF_USER_TEXT_ENCODING");
    }
    assert_eq!(&observed, expected);
}

fn run_test() -> io::Result<()> {
    let manager = if cfg!(target_os = "macos") {
        Manager::Launchd
    } else if cfg!(target_os = "linux") {
        Manager::Systemd
    } else {
        return Err(io::Error::other(
            "native manager test supports Linux and macOS",
        ));
    };
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("hmux-e2e-{}-{nonce}", std::process::id());
    let root = std::env::temp_dir().join(&name);
    fs::create_dir(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let root = fs::canonicalize(root)?;
    let work = root.join("work space%&");
    fs::create_dir(&work)?;
    let record = root.join("record & < > \" ' $ %.json");
    let exe = std::env::current_exe()?;
    let spec = Spec {
        binary: exe.to_string_lossy().into_owned(),
        home: work.to_string_lossy().into_owned(),
        endpoint: String::new(), // An actual empty argv element.
        token_file: root
            .join("token space&<>'\"$%\\.txt")
            .to_string_lossy()
            .into_owned(),
        config_file: root
            .join("config space&<>'\"$%\\.toml")
            .to_string_lossy()
            .into_owned(),
        log_file: record.to_string_lossy().into_owned(),
        environment: BTreeMap::from([
            ("HOME".into(), work.to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HMUX_E2E_EMPTY".into(), String::new()),
            ("HMUX_E2E_SPECIAL".into(), "space & < > ' \" $ % \\".into()),
        ]),
    };
    let unit_path = root.join(match manager {
        Manager::Systemd => format!("{name}.service"),
        Manager::Launchd => format!("{name}.plist"),
    });
    let bytes = match manager {
        Manager::Systemd => systemd_unit(&spec),
        Manager::Launchd => {
            let rendered = String::from_utf8(launch_agent(&spec)).unwrap();
            let original = format!("<key>Label</key><string>{LABEL}</string>");
            if rendered.matches(&original).count() != 1 {
                return Err(io::Error::other(
                    "expected exactly one rendered launchd label",
                ));
            }
            rendered
                .replacen(
                    &original,
                    &format!("<key>Label</key><string>{name}</string>"),
                    1,
                )
                .into_bytes()
        }
    };
    fs::write(&unit_path, bytes)?;
    let domain = format!("gui/{}", rustix::process::getuid().as_raw());
    let mut owned = OwnedService {
        manager,
        root,
        name,
        unit_path,
        domain,
        cleaned: false,
    };
    owned.start()?;
    let expected_argv: Vec<_> = spec
        .arguments()
        .into_iter()
        .skip(2 + spec.environment.len())
        .collect();
    let expected_env = serde_json::to_value(&spec.environment)?;
    let first = eventually(|| {
        let Some(value) = observation(&record)? else {
            return Ok(None);
        };
        let pid = value["pid"]
            .as_u64()
            .ok_or_else(|| io::Error::other("fixture omitted PID"))? as u32;
        if owned.pid().ok() != Some(pid) || !owned.active() {
            return Ok(None);
        }
        Ok(Some(value))
    })?;
    assert_eq!(first["argv"], serde_json::to_value(&expected_argv)?);
    check_environment(&first["environment"], &expected_env);
    assert_eq!(first["cwd"], spec.home);
    let first_pid = first["pid"].as_u64().unwrap() as u32;
    owned.restart()?;
    let second = eventually(|| {
        let Some(value) = observation(&record)? else {
            return Ok(None);
        };
        let pid = value["pid"].as_u64().unwrap_or(0) as u32;
        if pid == first_pid || owned.pid().ok() != Some(pid) || !owned.active() {
            return Ok(None);
        }
        Ok(Some(value))
    })?;
    assert_eq!(second["argv"], serde_json::to_value(&expected_argv)?);
    check_environment(&second["environment"], &expected_env);
    assert_eq!(second["cwd"], spec.home);
    let second_pid = second["pid"].as_u64().unwrap() as u32;
    assert_ne!(first_pid, second_pid);
    owned.stop_and_unlink()?;
    eventually(|| Ok((!owned.active() && !alive(second_pid)).then_some(())))?;
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "connect") {
        fixture(args).unwrap();
        return;
    }
    if !args.iter().any(|arg| arg == "--ignored") || std::env::var(OPT_IN).as_deref() != Ok("1") {
        println!("native_manager: ignored (requires --ignored and {OPT_IN}=1)");
        return;
    }
    run_test().unwrap();
    println!("native_manager: passed");
}
