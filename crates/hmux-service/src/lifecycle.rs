//! One-shot launchd/systemd user service management. No resident supervisor.
use crate::{
    files,
    process::{self, Process},
    Action, Command, Spec, LABEL, UNIT,
};
use hmux_core::{
    command::{CommandRunner, CommandSpec},
    PrivateDir,
};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;
fn bad(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}
fn exited(error: &io::Error) -> bool {
    error
        .get_ref()
        .and_then(|e| e.downcast_ref::<hmux_core::command::RunError>())
        .is_some_and(|e| e.kind() == hmux_core::command::RunErrorKind::Exit)
}
fn active(stop: &CancellationToken) -> io::Result<()> {
    if stop.is_cancelled() {
        Err(bad("service operation cancelled"))
    } else {
        Ok(())
    }
}
async fn native<T: Send + 'static>(
    job: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|_| bad("service worker unavailable"))?
}
fn absolute(value: &str) -> io::Result<PathBuf> {
    let path = Path::new(value);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut clean = PathBuf::new();
    for item in path.components() {
        match item {
            std::path::Component::RootDir => clean.push("/"),
            std::path::Component::Normal(value) => clean.push(value),
            std::path::Component::ParentDir => {
                clean.pop();
            }
            std::path::Component::CurDir => (),
            _ => return Err(bad("invalid service path")),
        }
    }
    Ok(clean)
}
fn text(path: &Path) -> io::Result<String> {
    path.to_str()
        .filter(|s| crate::clean_value(s))
        .map(str::to_owned)
        .ok_or_else(|| bad("invalid service path"))
}
fn system_executable(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| {
        m.is_file() && m.uid() == 0 && m.mode() & 0o022 == 0 && m.permissions().mode() & 0o111 != 0
    })
}
type Scan = Arc<dyn Fn() -> io::Result<Vec<Process>> + Send + Sync>;
struct Manager {
    home: PathBuf,
    path: PathBuf,
    command: PathBuf,
    domain: String,
    target: String,
    mac: bool,
    runner: CommandRunner,
    scan: Scan,
    lock: Option<Arc<hmux_core::FileLock>>,
}
impl Manager {
    fn new() -> io::Result<Self> {
        let uid = rustix::process::getuid().as_raw();
        if uid == 0 {
            return Err(bad(
                "run Home service management as the tmux/provider user without sudo",
            ));
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| bad("HOME unavailable"))?;
        PrivateDir::open_existing_trusted(&home)?;
        let mac = cfg!(target_os = "macos");
        let command = if mac {
            PathBuf::from("/bin/launchctl")
        } else {
            ["/usr/bin/systemctl", "/bin/systemctl"]
                .into_iter()
                .map(PathBuf::from)
                .find(|p| system_executable(p))
                .ok_or_else(|| bad("systemd user services are required on Linux"))?
        };
        if !system_executable(&command) {
            return Err(bad("service manager is not a trusted system executable"));
        }
        let path = if mac {
            home.join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist"))
        } else {
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            if !config.is_absolute() {
                return Err(bad("XDG_CONFIG_HOME must be absolute"));
            }
            config.join("systemd/user").join(UNIT)
        };
        let domain = format!("gui/{uid}");
        let target = format!("{domain}/{LABEL}");
        Ok(Self {
            home,
            path,
            command,
            domain,
            target,
            mac,
            runner: CommandRunner::new(1).map_err(|e| bad(e.to_string()))?,
            scan: Arc::new(process::connectors),
            lock: None,
        })
    }
    async fn mutate<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> io::Result<T> + Send + 'static,
    ) -> io::Result<T> {
        let lease = self.lock.clone();
        native(move || {
            let _lease = lease;
            job()
        })
        .await
    }
    async fn execute(&self, args: &[&OsStr], stop: &CancellationToken) -> io::Result<Vec<u8>> {
        active(stop)?;
        let spec = CommandSpec::new(self.command.as_os_str(), 128 << 10, Duration::from_secs(30))
            .args(args.iter().copied());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let result = self.runner.run_cancelable(spec, rx);
        tokio::pin!(result);
        let result = tokio::select! {
            result = &mut result => result,
            () = stop.cancelled() => { let _ = tx.send(()); result.await },
        };
        result.map(|v| v.stdout).map_err(io::Error::other)
    }
    async fn run(&self, args: &[&str], stop: &CancellationToken) -> io::Result<Vec<u8>> {
        self.execute(&args.iter().map(OsStr::new).collect::<Vec<_>>(), stop)
            .await
    }
    async fn available(&self, stop: &CancellationToken) -> io::Result<()> {
        if self.mac {
            // Domain probing discards output: launchd can list a large GUI domain.
            // This fixed system command is the only uncollected manager operation.
            active(stop)?;
            let mut command = tokio::process::Command::new(&self.command);
            command
                .args(["print", &self.domain])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            let mut child = hmux_core::command::with_child_spawn(|| command.spawn())?;
            let status = tokio::select! {
                value = child.wait() => value?,
                () = stop.cancelled() => { let _=child.start_kill(); let _=child.wait().await; return Err(bad("service operation cancelled")); },
                () = tokio::time::sleep(Duration::from_secs(30)) => { let _=child.start_kill(); let _=child.wait().await; return Err(bad("service manager probe timed out")); },
            };
            if !status.success() {
                return Err(bad(
                    "macOS GUI service manager unavailable; run in the logged-in Home account",
                ));
            }
        } else {
            self.run(&["--user", "show", "--property=Version"], stop)
                .await?;
        }
        Ok(())
    }
    async fn loaded(&self, stop: &CancellationToken) -> io::Result<bool> {
        let result = if self.mac {
            self.run(&["print", &self.target], stop).await
        } else {
            self.run(&["--user", "is-active", "--quiet", UNIT], stop)
                .await
        };
        active(stop)?;
        match result {
            Ok(_) => Ok(true),
            Err(e) if exited(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }
    async fn pid(&self, stop: &CancellationToken) -> io::Result<i32> {
        let raw = if self.mac {
            self.run(&["print", &self.target], stop).await
        } else {
            self.run(
                &["--user", "show", "--property=MainPID", "--value", UNIT],
                stop,
            )
            .await
        };
        active(stop)?;
        let raw = match raw {
            Ok(raw) => raw,
            Err(e) if exited(&e) => return Ok(0),
            Err(e) => return Err(e),
        };
        let raw = std::str::from_utf8(&raw).map_err(|_| bad("invalid service manager response"))?;
        let pid = if self.mac {
            raw.lines()
                .find_map(|l| l.trim().strip_prefix("pid = "))
                .unwrap_or("0")
        } else {
            raw.trim()
        };
        Ok(pid.parse::<i32>().ok().filter(|v| *v > 1).unwrap_or(0))
    }
    async fn processes(&self) -> io::Result<Vec<Process>> {
        let scan = self.scan.clone();
        native(move || scan()).await
    }
    async fn wait_stopped(&self, stop: &CancellationToken) -> io::Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            active(stop)?;
            if self.processes().await.is_ok_and(|all| all.is_empty()) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(bad(
                    "a Home connector is still running; replacement service not started",
                ));
            }
            tokio::select! { () = stop.cancelled() => return Err(bad("service operation cancelled")), () = tokio::time::sleep(Duration::from_millis(100)) => () }
        }
    }
    async fn start(&self, stop: &CancellationToken) -> io::Result<()> {
        let pid = self.pid(stop).await?;
        if self.processes().await?.iter().any(|p| p.pid != pid) {
            return Err(bad(
                "another connector is running; stop it before starting the service",
            ));
        }
        if self.mac {
            self.run(&["enable", &self.target], stop).await?;
            if !self.loaded(stop).await? {
                self.execute(
                    &[
                        OsStr::new("bootstrap"),
                        OsStr::new(&self.domain),
                        self.path.as_os_str(),
                    ],
                    stop,
                )
                .await?;
            } else {
                self.run(&["kickstart", &self.target], stop).await?;
            }
        } else {
            self.run(&["--user", "daemon-reload"], stop).await?;
            self.run(&["--user", "enable", "--now", UNIT], stop).await?;
        }
        Ok(())
    }
    async fn stop(&self, disable: bool, stop: &CancellationToken) -> io::Result<()> {
        if self.mac {
            if disable {
                self.run(&["disable", &self.target], stop).await?;
            }
            if self.loaded(stop).await? {
                self.run(&["bootout", &self.target], stop).await?;
            }
        } else if disable {
            self.run(&["--user", "disable", "--now", UNIT], stop)
                .await?;
        } else {
            self.run(&["--user", "stop", UNIT], stop).await?;
        }
        Ok(())
    }
    async fn check_installed(&self) -> io::Result<()> {
        let path = self.path.clone();
        let mac = self.mac;
        native(move || {
            let bytes = files::read(&path, 64 << 10, true)?;
            let marker = if mac {
                format!("<string>{LABEL}</string>")
            } else {
                "Description=HMux Home connector".into()
            };
            if !bytes.windows(marker.len()).any(|s| s == marker.as_bytes()) {
                return Err(bad("refusing to manage an unrecognized service file"));
            }
            Ok(())
        })
        .await
    }
    async fn install(
        &self,
        mut cmd: Command,
        stop: &CancellationToken,
        source: Option<PathBuf>,
        bundle: Option<(PathBuf, PathBuf)>,
    ) -> io::Result<String> {
        let all = self.processes().await?;
        let managed = self.pid(stop).await?;
        let previous = if cmd.from_running {
            if all.len() != 1 {
                return Err(bad(
                    "--from-running requires exactly one Home connector owned by this user",
                ));
            }
            let previous = all.into_iter().next().unwrap();
            (cmd.endpoint, cmd.token_file, cmd.config_file) =
                process::connector_options(&previous).map_err(bad)?;
            Some(previous)
        } else {
            if !all.is_empty() && (all.len() != 1 || managed == 0 || all[0].pid != managed) {
                return Err(bad(
                    "a manual Home connector is running; use service install --from-running",
                ));
            }
            None
        };
        if self.path.try_exists()? {
            self.check_installed().await?;
        }
        let home = self.home.clone();
        let service_path = self.path.clone();
        let mac = self.mac;
        let inherited = previous.as_ref().map(|p| p.environment.clone());
        active(stop)?;
        let spec = self.mutate(move || prepare(&home, cmd, inherited)).await?;
        active(stop)?;
        let data = if mac {
            crate::launch_agent(&spec)
        } else {
            crate::systemd_unit(&spec)
        };
        let binary = PathBuf::from(&spec.binary);
        let source = if let Some((source_dir, _)) = &bundle {
            source_dir.join("hmux-web")
        } else {
            source.unwrap_or(std::env::current_exe()?)
        }
        .canonicalize()?;
        if let Some((_, target_dir)) = &bundle {
            if target_dir.join("hmux-web") != binary {
                return Err(bad(
                    "bundle binary destination differs from service definition",
                ));
            }
        }
        // A restart policy must not observe a candidate definition or binary
        // until the old managed owner has exited. Disable autostart as well:
        // failure between publication steps must stay stopped across reboot.
        let was_managed = managed != 0 || self.loaded(stop).await?;
        if was_managed || self.path.try_exists()? {
            self.stop(true, stop).await?;
        }
        if let Some(previous) = previous {
            if previous.pid != managed {
                stop_adopted(previous, stop).await?;
            }
        }
        self.wait_stopped(stop).await?;
        active(stop)?;
        if let Some((source_dir, target_dir)) = bundle {
            let recovery_dir = target_dir.clone();
            let result = self
                .mutate(move || hmux_install::install(&source_dir, &target_dir))
                .await;
            if let Err(error) = result {
                // A failed two-binary transaction can have reached either rename.
                // Recover its journal while the owner stays stopped.
                let recovery = self
                    .mutate(move || hmux_install::recover(&recovery_dir))
                    .await;
                // Recovery may complete a committed transaction. Do not start
                // until an operator can verify which executable is installed.
                return Err(match recovery {
                    Ok(()) => bad(format!("bundle publication failed: {error}; service remains stopped")),
                    Err(recovery) => bad(format!("bundle publication failed: {error}; recovery failed: {recovery}; service remains stopped")),
                });
            }
            // The installer already retained the old web binary in its own
            // backup. Skip the service pair's binary copy so its backup cannot
            // overwrite that known-good version with the new candidate.
            let publication = self
                .mutate(move || files::publish_pair(&service_path, &data, &binary, &binary))
                .await;
            publication.map_err(|e| bad(format!("binaries installed but service definition publication failed; service remains stopped: {e}")))?;
        } else {
            let publication = self
                .mutate(move || files::publish_pair(&service_path, &data, &source, &binary))
                .await;
            if let Err(error) = publication {
                return Err(bad(format!(
                    "service publication failed: {error}; service remains stopped"
                )));
            }
        }
        active(stop)?;
        self.start(stop).await.map_err(|e| bad(format!("service files installed but activation failed; run hmux-web service start after correcting the manager: {e}")))?;
        Ok(format!("Home service installed and start requested. Run hmux-web service status to check the process.\nBounded service log: {}\n{}\n", spec.log_file,
            if mac {"Starts at macOS login and restarts after process exit. The Mac must stay awake for remote access."} else {"Starts with the systemd user manager. For boot/logout persistence, an administrator can run: sudo loginctl enable-linger <home-user>"}))
    }
}
fn prepare(
    home: &Path,
    cmd: Command,
    source: Option<BTreeMap<String, String>>,
) -> io::Result<Spec> {
    hmux_home::dial::Endpoint::parse(&cmd.endpoint)
        .map_err(|_| bad("--url must be wss://host/connect"))?;
    if cmd.token_file.is_empty() {
        return Err(bad("--token-file is required"));
    }
    let token = absolute(&cmd.token_file)?;
    let raw = files::read(&token, 256, true)?;
    use base64::Engine;
    if !base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw.trim_ascii())
        .is_ok_and(|v| v.len() == 32)
    {
        return Err(bad("invalid connector token file"));
    }
    let config = if cmd.config_file.is_empty() {
        let primary = home.join(".config/hmux/home.toml");
        match std::fs::symlink_metadata(&primary) {
            Ok(_) => primary,
            Err(e) if e.kind() == io::ErrorKind::NotFound => home.join(".config/hmux/client.toml"),
            Err(e) => return Err(e),
        }
    } else {
        absolute(&cmd.config_file)?
    };
    files::read(&config, 1 << 20, false)?;
    let cfg = hmux_home::config::load_home_required(&config, home).map_err(bad)?;
    hmux_home::config::load_inventory(&cfg.inventory_path).map_err(bad)?;
    let account = rustix::process::getuid().as_raw().to_string();
    // Resolve the account from the OS, never adopt a process's USER/LOGNAME.
    let user = uzers::get_user_by_uid(rustix::process::getuid().as_raw())
        .and_then(|u| u.name().to_str().map(str::to_owned))
        .ok_or_else(|| bad(format!("account name unavailable for uid {account}")))?;
    let source = source.unwrap_or_else(|| {
        std::env::vars_os()
            .filter_map(|(k, v)| {
                let key = k.into_string().ok()?;
                if key != "HOME" && !crate::ENV_KEYS.contains(&key.as_str()) {
                    return None;
                }
                Some((key, v.into_string().ok()?))
            })
            .collect()
    });
    let environment = crate::service_environment_from(home, &user, &source).map_err(bad)?;
    if !crate::executable_in_path("tmux", &environment["PATH"]) {
        return Err(bad("tmux is missing from the service PATH"));
    }
    PrivateDir::open_or_create_trusted(&cfg.state_dir)?;
    let binary = if cmd.binary.is_empty() {
        crate::default_binary(home)
    } else {
        absolute(&cmd.binary)?
    };
    if binary.file_name() != Some(OsStr::new("hmux-web")) {
        return Err(bad("service binary destination must end in hmux-web"));
    }
    let log_file = cfg.state_dir.join("home-service.log");
    drop(crate::log::Log::open(&log_file)?);
    Ok(Spec {
        binary: text(&binary)?,
        home: text(home)?,
        endpoint: cmd.endpoint,
        token_file: text(&token)?,
        config_file: text(&config)?,
        log_file: text(&log_file)?,
        environment,
    })
}
async fn stop_adopted(previous: Process, stop: &CancellationToken) -> io::Result<()> {
    let expected = previous.clone();
    active(stop)?;
    native(move || {
        if expected.pid <= 1 || expected.pid == std::process::id() as i32 {
            return Err(bad("invalid connector PID"));
        }
        let pid = rustix::process::Pid::from_raw(expected.pid)
            .ok_or_else(|| bad("invalid connector PID"))?;
        // Pin the original Linux process before checking its identity. If it
        // exits, signaling this handle cannot target a recycled numeric PID.
        #[cfg(target_os = "linux")]
        let handle = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())
            .map_err(|_| bad("safe process adoption requires Linux pidfd support; stop the manual connector before installing"))?;
        let current = process::read_process(expected.pid)?;
        if current != expected {
            return Err(bad("connector identity changed; no process was signalled"));
        }
        #[cfg(target_os = "linux")]
        return rustix::process::pidfd_send_signal(handle, rustix::process::Signal::TERM)
            .map_err(Into::into);
        // macOS exposes no equivalent process handle here. Retain Go's immediate
        // full-identity recheck; it narrows but cannot atomically close PID reuse.
        #[cfg(not(target_os = "linux"))]
        return rustix::process::kill_process(pid, rustix::process::Signal::TERM)
            .map_err(Into::into);
    })
    .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        active(stop)?;
        let pid = previous.pid;
        match native(move || process::read_process(pid)).await {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Ok(current) if current != previous => return Ok(()),
            _ => (),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(bad(
                "connector did not stop within 20 seconds; service not started",
            ));
        }
        tokio::select! { ()=stop.cancelled()=>return Err(bad("service operation cancelled")), ()=tokio::time::sleep(Duration::from_millis(100))=>() }
    }
}
/// Manage the opt-in native user service. Arguments are parsed before any I/O.
pub async fn run(arguments: &[String], stop: CancellationToken) -> io::Result<String> {
    run_with_executable(arguments, stop, None).await
}

/// Use an explicit candidate executable for a direct service installation.
pub async fn run_with_executable(
    arguments: &[String],
    stop: CancellationToken,
    executable: Option<PathBuf>,
) -> io::Result<String> {
    let command = crate::parse_command(arguments).map_err(bad)?;
    let operation = stop.child_token();
    let _cancel = operation.clone().drop_guard();
    // The operation retains its lock through caller-future abort and joins all
    // blocking publication/manager work before cancellation ends the operation.
    tokio::spawn(run_owned(command, operation, executable, None))
        .await
        .map_err(|_| bad("service operation unavailable"))?
}
/// Install a candidate bundle under the same service-operation lock used for
/// owner shutdown and service activation.
pub async fn run_with_bundle(
    arguments: &[String],
    stop: CancellationToken,
    source_dir: PathBuf,
    target_dir: PathBuf,
) -> io::Result<String> {
    let command = crate::parse_command(arguments).map_err(bad)?;
    if command.action != Action::Install {
        return Err(bad("bundle requires service install"));
    }
    let operation = stop.child_token();
    let _cancel = operation.clone().drop_guard();
    tokio::spawn(run_owned(
        command,
        operation,
        None,
        Some((source_dir, target_dir)),
    ))
    .await
    .map_err(|_| bad("service operation unavailable"))?
}
async fn run_owned(
    command: Command,
    stop: CancellationToken,
    executable: Option<PathBuf>,
    bundle: Option<(PathBuf, PathBuf)>,
) -> io::Result<String> {
    let mut manager = native(Manager::new).await?;
    manager.available(&stop).await?;
    manager.lock = if command.action != Action::Status {
        let path = manager.path.clone();
        Some(Arc::new(
            native(move || {
                let dir = PrivateDir::open_or_create_trusted(
                    path.parent().ok_or_else(|| bad("invalid service path"))?,
                )?;
                let mut name = path.file_name().unwrap().to_os_string();
                name.push(".lock");
                dir.try_lock(&name)?
                    .ok_or_else(|| bad("another service change is in progress"))
            })
            .await?,
        ))
    } else {
        None
    };
    if command.action != Action::Status {
        let path = manager.path.clone();
        manager.mutate(move || files::recover_pair(&path)).await?;
    }
    if command.action == Action::Install {
        return manager.install(command, &stop, executable, bundle).await;
    }
    manager.check_installed().await?;
    match command.action {
        Action::Status => {
            let mut output = String::new();
            if manager.mac {
                match manager.run(&["print", &manager.target], &stop).await {
                    Ok(raw) => {
                        for line in String::from_utf8_lossy(&raw).lines().map(str::trim) {
                            if ["state = ", "pid = ", "last exit code = "]
                                .iter()
                                .any(|prefix| line.starts_with(prefix))
                            {
                                output.push_str(line);
                                output.push('\n');
                            }
                        }
                    }
                    Err(e) if exited(&e) => {
                        active(&stop)?;
                        output.push_str("Home service is installed but stopped.\n");
                    }
                    Err(e) => return Err(e),
                }
            } else {
                let raw = manager
                    .run(
                        &[
                            "--user",
                            "show",
                            UNIT,
                            "--property=ActiveState,SubState,MainPID,ExecMainStatus,Result",
                        ],
                        &stop,
                    )
                    .await?;
                output.push_str(&String::from_utf8_lossy(&raw));
            }
            output.push_str("Process state does not confirm gateway connectivity. Check the service log and web UI.\n");
            Ok(output)
        }
        Action::Start => {
            manager.start(&stop).await?;
            Ok(String::new())
        }
        Action::Stop => {
            manager.stop(true, &stop).await?;
            Ok(String::new())
        }
        Action::Restart => {
            manager.stop(false, &stop).await?;
            manager.wait_stopped(&stop).await?;
            manager.start(&stop).await?;
            Ok(String::new())
        }
        Action::Uninstall => {
            manager.stop(true, &stop).await?;
            let path = manager.path.clone();
            manager
                .mutate(move || files::remove_backed_up(&path))
                .await?;
            if !manager.mac {
                manager.run(&["--user", "daemon-reload"], &stop).await?;
            }
            Ok("Home service removed. Native binaries, credentials, workspaces and tmux sessions are retained.\n".into())
        }
        Action::Install => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            let root = PathBuf::from("/private/tmp").join(format!(
                "hmux-e2e-service-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let root = if cfg!(target_os = "macos") {
                root
            } else {
                std::env::temp_dir().join(root.file_name().unwrap())
            };
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn manager(&self, mac: bool, body: &str) -> Manager {
            let command = self.0.join("manager");
            std::fs::write(&command, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700)).unwrap();
            Manager {
                home: self.0.clone(),
                path: self.0.join("service file"),
                command,
                domain: "gui/123".into(),
                target: format!("gui/123/{LABEL}"),
                mac,
                runner: CommandRunner::new(1).unwrap(),
                scan: Arc::new(|| Ok(vec![])),
                lock: None,
            }
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn install_fixture(root: &Root) -> (Command, Process, PathBuf) {
        use base64::Engine;
        let config_dir = root.0.join(".config/hmux");
        std::fs::create_dir_all(&config_dir).unwrap();
        let config = config_dir.join("home.toml");
        let inventory = config_dir.join("inventory.toml");
        let state = root.0.join("state");
        std::fs::write(
            &config,
            format!(
                "schema_version = 1\nrole = \"home\"\ninventory_path = {:?}\nstate_dir = {:?}\n",
                inventory.display().to_string(),
                state.display().to_string()
            ),
        )
        .unwrap();
        std::fs::write(&inventory, "schema_version = 1\nrevision = \"test\"\n[[profiles]]\nid = \"shell\"\nlabel = \"Shell\"\ndefault_directory = \"~/.hmux\"\ncommand = [\"sh\"]\n").unwrap();
        let token = root.0.join("token");
        std::fs::write(
            &token,
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7; 32]),
        )
        .unwrap();
        for path in [&config, &inventory, &token] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let tools = root.0.join("tools");
        std::fs::create_dir(&tools).unwrap();
        let tmux = tools.join("tmux");
        std::fs::write(&tmux, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o700)).unwrap();
        let binary = root.0.join("hmux-web");
        std::fs::write(&binary, "old binary").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let candidate = root.0.join("candidate");
        std::fs::write(&candidate, "new binary").unwrap();
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint = "wss://example.test/connect".to_string();
        let cmd = Command {
            action: Action::Install,
            endpoint: endpoint.clone(),
            token_file: token.display().to_string(),
            config_file: config.display().to_string(),
            from_running: true,
            binary: binary.display().to_string(),
        };
        let process = Process {
            pid: 42,
            birth: "synthetic".into(),
            args: vec![
                binary.display().to_string(),
                "connect".into(),
                "--url".into(),
                endpoint,
                "--token-file".into(),
                token.display().to_string(),
                "--config".into(),
                config.display().to_string(),
            ],
            environment: BTreeMap::from([
                ("HOME".into(), root.0.display().to_string()),
                ("PATH".into(), tools.display().to_string()),
            ]),
        };
        (cmd, process, candidate)
    }
    #[tokio::test]
    async fn invalid_service_inputs_do_not_stop_or_publish() {
        let root = Root::new();
        let marker = root.0.join("stopped");
        let m = root.manager(true, &format!("if [ \"$1\" = bootout ]; then touch '{}'; fi\nif [ \"$1\" = print ]; then exit 3; fi", marker.display()));
        let cmd = Command {
            action: Action::Install,
            endpoint: "bad".into(),
            token_file: String::new(),
            config_file: String::new(),
            from_running: false,
            binary: String::new(),
        };
        assert!(m
            .install(cmd, &CancellationToken::new(), None, None)
            .await
            .is_err());
        assert!(!marker.exists());
        assert!(!m.path.exists());
    }
    #[tokio::test]
    async fn failed_manager_stop_keeps_old_service_and_binary() {
        let root = Root::new();
        let (cmd, previous, candidate) = install_fixture(&root);
        let stopped = root.0.join("stop-attempted");
        let mut m = root.manager(true, &format!(
            "if [ \"$1\" = print ]; then echo 'pid = 42'; exit 0; fi\nif [ \"$1\" = bootout ]; then touch '{}'; exit 7; fi",
            stopped.display()
        ));
        m.scan = Arc::new(move || Ok(vec![previous.clone()]));
        let original = format!("<string>{LABEL}</string>");
        std::fs::write(&m.path, &original).unwrap();
        std::fs::set_permissions(&m.path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(m
            .install(cmd, &CancellationToken::new(), Some(candidate), None)
            .await
            .is_err());
        assert!(stopped.exists());
        assert_eq!(std::fs::read_to_string(&m.path).unwrap(), original);
        assert_eq!(
            std::fs::read_to_string(root.0.join("hmux-web")).unwrap(),
            "old binary"
        );
    }
    #[tokio::test]
    async fn managed_owner_exits_before_service_pair_publication() {
        let root = Root::new();
        let (cmd, previous, candidate) = install_fixture(&root);
        let stopped = root.0.join("old-owner-exited");
        let binary = root.0.join("hmux-web");
        let mut m = root.manager(true, &format!(
            "if [ \"$1\" = print ]; then echo 'pid = 42'; exit 0; fi\nif [ \"$1\" = bootout ]; then [ \"$(cat '{}')\" = 'old binary' ] || exit 7; touch '{}'; fi",
            binary.display(), stopped.display()
        ));
        m.scan = Arc::new(move || {
            if stopped.exists() {
                Ok(vec![])
            } else {
                Ok(vec![previous.clone()])
            }
        });
        std::fs::write(&m.path, format!("<string>{LABEL}</string>")).unwrap();
        std::fs::set_permissions(&m.path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let output = m
            .install(cmd, &CancellationToken::new(), Some(candidate), None)
            .await
            .unwrap();
        assert!(output.contains("Home service installed"));
        assert_eq!(std::fs::read_to_string(binary).unwrap(), "new binary");
        assert!(m.path.exists());
    }
    #[tokio::test]
    async fn failed_publication_leaves_autostart_disabled_on_both_managers() {
        for mac in [false, true] {
            let root = Root::new();
            let (cmd, previous, candidate) = install_fixture(&root);
            let stopped = root.0.join("stopped");
            let calls = root.0.join("calls");
            // Force publication to fail after the old owner was stopped.
            std::fs::remove_file(&candidate).unwrap();
            std::fs::create_dir(&candidate).unwrap();
            let mut manager = root.manager(mac, &format!(
                "printf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = print ]; then echo 'pid = 42'; fi\nif [ \"$2\" = show ]; then echo 42; fi\nif [ \"$1\" = bootout ] || [ \"$2\" = disable ]; then touch '{}'; fi",
                calls.display(), stopped.display()
            ));
            manager.scan = Arc::new(move || {
                if stopped.exists() {
                    Ok(vec![])
                } else {
                    Ok(vec![previous.clone()])
                }
            });
            let old = if mac {
                format!("<string>{LABEL}</string>")
            } else {
                "Description=HMux Home connector".to_string()
            };
            std::fs::write(&manager.path, &old).unwrap();
            std::fs::set_permissions(&manager.path, std::fs::Permissions::from_mode(0o600))
                .unwrap();
            let error = manager
                .install(cmd, &CancellationToken::new(), Some(candidate), None)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("remains stopped"));
            let calls = std::fs::read_to_string(calls).unwrap();
            if mac {
                assert!(calls.contains("disable gui/123/"));
            } else {
                assert!(calls.contains("--user disable --now hmux-home.service"));
            }
            assert!(!calls
                .lines()
                .any(|line| line.starts_with("enable ") || line.starts_with("--user enable ")));
            assert_eq!(std::fs::read_to_string(&manager.path).unwrap(), old);
            assert_eq!(
                std::fs::read_to_string(root.0.join("hmux-web")).unwrap(),
                "old binary"
            );
        }
    }
    #[tokio::test]
    async fn adoption_signals_only_the_owned_child_with_matching_identity() {
        let mut command = tokio::process::Command::new("/bin/sleep");
        command.arg("30").kill_on_drop(true);
        let mut child = hmux_core::command::with_child_spawn(|| command.spawn()).unwrap();
        let pid = child.id().unwrap() as i32;
        let expected = process::read_process(pid).unwrap();
        let mut wrong = expected.clone();
        wrong.birth.push_str("-wrong");
        let stop = CancellationToken::new();
        assert!(stop_adopted(wrong, &stop).await.is_err());
        assert!(child.try_wait().unwrap().is_none());
        // Act as the original parent too: macOS cannot inspect procargs for an
        // unreaped zombie, so reap concurrently with the installer's exit check.
        let (stopped, status) = tokio::join!(
            stop_adopted(expected, &stop),
            tokio::time::timeout(Duration::from_secs(2), child.wait()),
        );
        stopped.unwrap();
        let status = status.unwrap().unwrap();
        assert!(!status.success());
    }
    #[tokio::test]
    async fn start_rejects_other_connector_before_enable() {
        let root = Root::new();
        let marker = root.0.join("enabled");
        let mut m = root.manager(
            false,
            &format!(
                "if [ \"$2\" = show ]; then echo 0; else touch '{}'; fi",
                marker.display()
            ),
        );
        m.scan = Arc::new(|| {
            Ok(vec![Process {
                pid: 42,
                birth: "test".into(),
                args: vec![],
                environment: BTreeMap::new(),
            }])
        });
        assert!(m
            .start(&CancellationToken::new())
            .await
            .unwrap_err()
            .to_string()
            .contains("another connector"));
        assert!(!marker.exists());
    }
    #[tokio::test]
    async fn linux_and_mac_commands_preserve_single_argv_path() {
        let root = Root::new();
        let log = root.0.join("calls");
        let body=format!("printf 'call\\n' >> '{log}'\nprintf '%s\\n' \"$@\" >> '{log}'\nif [ \"$1\" = print ]; then exit 3; fi\nif [ \"$2\" = show ]; then echo 0; fi",log=log.display());
        let stop = CancellationToken::new();
        let m = root.manager(false, &body);
        m.start(&stop).await.unwrap();
        m.stop(true, &stop).await.unwrap();
        let raw = std::fs::read_to_string(&log).unwrap();
        assert!(
            raw.contains("--user\ndaemon-reload\ncall\n--user\nenable\n--now\nhmux-home.service")
        );
        assert!(raw.contains("--user\ndisable\n--now\nhmux-home.service"));
        std::fs::write(&log, "").unwrap();
        let m = root.manager(true, &body);
        m.start(&stop).await.unwrap();
        let raw = std::fs::read_to_string(&log).unwrap();
        assert!(raw.contains(&format!("bootstrap\ngui/123\n{}\n", m.path.display())));
        assert!(!raw.contains("kickstart"));
    }
    #[tokio::test]
    async fn cancellation_joins_manager_and_never_continues_activation() {
        let root = Root::new();
        let marker = root.0.join("pid");
        let m = root.manager(
            false,
            &format!("echo $$ > '{}'\nexec /bin/sleep 30", marker.display()),
        );
        let stop = CancellationToken::new();
        let cancel = stop.clone();
        let path = marker.clone();
        let timer = tokio::spawn(async move {
            for _ in 0..100 {
                if path.exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            cancel.cancel();
        });
        assert!(m.start(&stop).await.is_err());
        timer.await.unwrap();
        assert_eq!(m.runner.available_slots(), 1);
        let pid = std::fs::read_to_string(marker)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        let pid = rustix::process::Pid::from_raw(pid).unwrap();
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
    }
    #[tokio::test]
    async fn domain_probe_discards_large_output_but_loaded_query_is_bounded() {
        let root = Root::new();
        let m = root.manager(true, "/usr/bin/head -c 200000 /dev/zero");
        m.available(&CancellationToken::new()).await.unwrap();
        assert!(m.loaded(&CancellationToken::new()).await.is_err());
    }
    #[tokio::test]
    async fn unreadable_processes_never_prove_shutdown() {
        let root = Root::new();
        let mut m = root.manager(false, "exit 0");
        m.scan = Arc::new(|| Err(io::Error::from(io::ErrorKind::PermissionDenied)));
        let stop = CancellationToken::new();
        let s = stop.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            s.cancel();
        });
        assert!(m.wait_stopped(&stop).await.is_err());
        timer.await.unwrap();
    }
    #[tokio::test]
    async fn aborted_publication_retains_lock_until_native_worker_exits() {
        let root = Root::new();
        let dir = PrivateDir::open_existing_trusted(&root.0).unwrap();
        let name = OsStr::new("service.lock");
        let mut manager = root.manager(false, "exit 0");
        manager.lock = Some(Arc::new(dir.try_lock(name).unwrap().unwrap()));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let task = tokio::spawn(async move {
            manager
                .mutate(move || {
                    let _ = started_tx.send(());
                    release_rx
                        .recv_timeout(Duration::from_secs(3))
                        .map_err(io::Error::other)?;
                    Ok(())
                })
                .await
        });
        started_rx.await.unwrap();
        task.abort();
        let _ = task.await;
        assert!(dir.try_lock(name).unwrap().is_none());
        release_tx.send(()).unwrap();
        let released = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if dir.try_lock(name).unwrap().is_some() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(released.is_ok());
    }
    #[test]
    fn removal_retains_one_timestamped_backup_and_rejects_symlinks() {
        let root = Root::new();
        let file = root.0.join("service");
        for content in [b"old", b"new"] {
            std::fs::write(&file, content).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
            files::remove_backed_up(&file).unwrap();
            assert!(!file.exists());
        }
        let backups = std::fs::read_dir(&root.0)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains("hmux-pair-backup-")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(backups[0].path()).unwrap(), b"new");
        let other = root.0.join("untouched");
        std::fs::write(&other, b"preserved").unwrap();
        std::os::unix::fs::symlink(&other, &file).unwrap();
        assert!(files::remove_backed_up(&file).is_err());
        assert_eq!(std::fs::read(other).unwrap(), b"preserved");
    }
}
