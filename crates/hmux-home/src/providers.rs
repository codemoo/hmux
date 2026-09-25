//! Home-owned provider setup. All paths and executables are injected at construction.
//! The service lives for the connector lifetime; call `shutdown` before it exits.
use hmux_core::command::{CommandRunner, CommandSpec, RunErrorKind};
use hmux_protocol::protobuf::types as p;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fmt, fs,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{oneshot, Mutex, Semaphore};
use tokio_util::sync::CancellationToken;

#[path = "providers_jobs.rs"]
mod jobs;
#[path = "providers_status.rs"]
mod status;
#[path = "providers_store.rs"]
mod store;

const PROVIDERS: [Provider; 3] = [
    Provider {
        id: "codex",
        label: "Codex",
        command: "codex",
        key_name: "OPENAI_API_KEY",
    },
    Provider {
        id: "claude",
        label: "Claude Code",
        command: "claude",
        key_name: "ANTHROPIC_API_KEY",
    },
    Provider {
        id: "gemini",
        label: "Gemini",
        command: "gemini",
        key_name: "GEMINI_API_KEY",
    },
];
#[derive(Clone, Copy)]
struct Provider {
    id: &'static str,
    label: &'static str,
    command: &'static str,
    key_name: &'static str,
}
fn lookup(id: &str) -> Option<Provider> {
    PROVIDERS.iter().copied().find(|p| p.id == id)
}

#[derive(Clone)]
pub struct ProviderEnv {
    pub home: PathBuf,
    pub path: OsString,
    pub inventory_path: PathBuf,
    pub tmux_path: PathBuf,
    pub job_socket: String,
    pub timeout: Duration,
    pub system_dirs: Option<Vec<PathBuf>>,
}
impl ProviderEnv {
    pub fn for_home(
        home: PathBuf,
        path: OsString,
        inventory_path: PathBuf,
        tmux_path: PathBuf,
    ) -> Self {
        Self {
            home,
            path,
            inventory_path,
            tmux_path,
            job_socket: "hmux-setup".into(),
            timeout: Duration::from_secs(5),
            system_dirs: None,
        }
    }
    fn validate(&self) -> Result<(), ProviderError> {
        let path_entries: Vec<_> = std::env::split_paths(&self.path).collect();
        let system_count = self.system_dirs.as_ref().map_or(4, Vec::len);
        if !safe_absolute(&self.home)
            || !safe_absolute(&self.inventory_path)
            || !safe_absolute(&self.tmux_path)
            || self.path.as_bytes().len() > 8192
            || path_entries.len() > 128
            || system_count > 32
            || path_entries.len() + system_count + 1 > 128
            || self
                .system_dirs
                .as_ref()
                .is_some_and(|dirs| dirs.iter().any(|dir| !safe_absolute(dir)))
            || self.timeout.is_zero()
            || self.timeout > Duration::from_secs(30)
            || self.job_socket.is_empty()
            || self.job_socket.len() > 64
            || !self
                .job_socket
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(ProviderError::new("invalid provider environment"));
        }
        Ok(())
    }
    fn search_path(&self) -> OsString {
        let mut dirs = vec![self.home.join(".local/bin")];
        dirs.extend(std::env::split_paths(&self.path).filter(|entry| safe_absolute(entry)));
        dirs.extend(self.system_dirs.clone().unwrap_or_else(|| {
            ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]
                .into_iter()
                .map(PathBuf::from)
                .collect()
        }));
        std::env::join_paths(dirs).unwrap_or_else(|_| OsString::from("/usr/bin:/bin"))
    }
    fn executable(&self, name: &str) -> Option<PathBuf> {
        for dir in std::env::split_paths(&self.search_path()) {
            let path = dir.join(name);
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                return Some(path);
            }
        }
        None
    }
    fn spec(&self, program: &Path, args: &[&str], max: usize) -> CommandSpec {
        CommandSpec::new(program.as_os_str(), max, self.timeout)
            .args(args.iter().copied())
            .current_dir(&self.home)
            .env("HOME", self.home.as_os_str())
            .env("PATH", self.search_path())
            .env("NO_COLOR", "1")
            .env("NO_BROWSER", "true")
    }
}

fn safe_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().as_bytes().len() <= 4096
        && !path.as_os_str().as_bytes().contains(&b':')
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

pub struct ProviderService {
    env: ProviderEnv,
    runner: CommandRunner,
    native_slots: Arc<Semaphore>,
    jobs: Mutex<HashMap<&'static str, jobs::JobTask>>,
    mutation: Mutex<()>,
    active: Arc<Semaphore>,
    closed: AtomicBool,
}
impl ProviderService {
    pub fn new(env: ProviderEnv, runner: CommandRunner) -> Result<Self, ProviderError> {
        env.validate()?;
        Ok(Self {
            env,
            runner,
            native_slots: Arc::new(Semaphore::new(3)),
            jobs: Mutex::new(HashMap::new()),
            mutation: Mutex::new(()),
            active: Arc::new(Semaphore::new(16)),
            closed: AtomicBool::new(false),
        })
    }
    pub async fn action(
        &self,
        operation: &str,
        payload: &[u8],
        cancel: CancellationToken,
    ) -> Result<ProviderActionResult, ProviderError> {
        if payload.len() > 16 * 1024 || operation.len() > 64 {
            return Err(ProviderError::new("provider request too large"));
        }
        let (key, job) = match operation {
            "provider-key" => (Some(decode(payload, "invalid provider key request")?), None),
            "provider-job-start"
            | "provider-job"
            | "provider-job-input"
            | "provider-job-cancel" => {
                (None, Some(decode(payload, "invalid provider job request")?))
            }
            "providers" => {
                if !payload.is_empty() && payload != b"null" {
                    decode::<Empty>(payload, "invalid providers request")?;
                }
                (None, None)
            }
            _ => return Err(ProviderError::new("unknown provider operation")),
        };
        let result = self.execute(operation, key, job, cancel).await?;
        let json = serde_json::to_vec(&result.result)
            .map_err(|_| ProviderError::new("encode provider result"))?;
        Ok(ProviderActionResult {
            json,
            refresh_auth: result.refresh_auth,
        })
    }
    pub async fn action_typed(
        &self,
        operation: p::Operation,
        payload: p::request::Payload,
        cancel: CancellationToken,
    ) -> Result<TypedProviderActionResult, ProviderError> {
        let (name, key, job) = match (operation, payload) {
            (p::Operation::Providers, p::request::Payload::Empty(_)) => ("providers", None, None),
            (p::Operation::ProviderKey, p::request::Payload::ProviderKey(q)) => (
                "provider-key",
                Some(KeyRequest {
                    provider: q.provider,
                    key: q.key,
                }),
                None,
            ),
            (p::Operation::ProviderJobStart, p::request::Payload::ProviderJob(q)) => (
                "provider-job-start",
                None,
                Some(JobRequest {
                    provider: q.provider,
                    action: q.action,
                    text: q.text,
                }),
            ),
            (p::Operation::ProviderJob, p::request::Payload::ProviderJob(q)) => (
                "provider-job",
                None,
                Some(JobRequest {
                    provider: q.provider,
                    action: q.action,
                    text: q.text,
                }),
            ),
            (p::Operation::ProviderJobInput, p::request::Payload::ProviderJob(q)) => (
                "provider-job-input",
                None,
                Some(JobRequest {
                    provider: q.provider,
                    action: q.action,
                    text: q.text,
                }),
            ),
            (p::Operation::ProviderJobCancel, p::request::Payload::ProviderJob(q)) => (
                "provider-job-cancel",
                None,
                Some(JobRequest {
                    provider: q.provider,
                    action: q.action,
                    text: q.text,
                }),
            ),
            _ => return Err(ProviderError::new("invalid provider request")),
        };
        let result = self.execute(name, key, job, cancel).await?;
        Ok(TypedProviderActionResult {
            result: result.result.into_proto(),
            refresh_auth: result.refresh_auth,
        })
    }
    async fn execute(
        &self,
        operation: &str,
        key: Option<KeyRequest>,
        job: Option<JobRequest>,
        cancel: CancellationToken,
    ) -> Result<DomainProviderActionResult, ProviderError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ProviderError::new("provider service stopped"));
        }
        let _active = self
            .active
            .clone()
            .try_acquire_owned()
            .map_err(|_| ProviderError::new("provider request busy"))?;
        if self.closed.load(Ordering::Acquire) {
            return Err(ProviderError::new("provider service stopped"));
        }
        if operation == "providers" {
            return self.result(Some(self.statuses(&cancel).await), None, None, false);
        }
        let _guard = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ProviderError::new("provider request cancelled")),
            result = tokio::time::timeout(Duration::from_secs(2), self.mutation.lock()) =>
                result.map_err(|_| ProviderError::new("provider setup busy"))?,
        };
        if self.closed.load(Ordering::Acquire) {
            return Err(ProviderError::new("provider service stopped"));
        }
        match operation {
            "provider-key" => {
                let q = key.ok_or_else(|| ProviderError::new("invalid provider key request"))?;
                let Some(p) = lookup(&q.provider) else {
                    return self.result(None, None, Some("unknown provider".into()), false);
                };
                if let Err(error) = self.set_key(p, &q.key, &cancel).await {
                    return self.result(None, None, Some(error.message), false);
                }
                let mut error = None;
                if !q.key.is_empty() {
                    if let Err(e) = self.ensure_profile(p).await {
                        error = Some(e.message);
                    }
                }
                let refresh = error.is_none();
                self.result(Some(self.statuses(&cancel).await), None, error, refresh)
            }
            "provider-job-start"
            | "provider-job"
            | "provider-job-input"
            | "provider-job-cancel" => {
                let q = job.ok_or_else(|| ProviderError::new("invalid provider job request"))?;
                let p =
                    lookup(&q.provider).ok_or_else(|| ProviderError::new("unknown provider"))?;
                if operation == "provider-job-start" && q.action == "use-existing" {
                    if self.jobs.lock().await.contains_key(p.id) {
                        return self.result(
                            None,
                            None,
                            Some("provider setup already in progress".into()),
                            false,
                        );
                    }
                    let status = self.status(p, &cancel).await;
                    if !status.installed || status.auth == "none" {
                        return self.result(
                            Some(self.statuses(&cancel).await),
                            None,
                            Some("provider is not installed and authenticated on Home".into()),
                            false,
                        );
                    }
                    if let Err(e) = self.ensure_profile(p).await {
                        return self.result(None, None, Some(e.message), false);
                    }
                    return self.result(
                        Some(self.statuses(&cancel).await),
                        Some(JobStatus::done()),
                        None,
                        false,
                    );
                }
                if operation == "provider-job-cancel" {
                    let _ = self.cancel_job(p, &cancel).await;
                    return self.result(None, Some(JobStatus::none()), None, false);
                }
                let outcome = match operation {
                    "provider-job-start" => self.start_job(p, &q.action, &cancel).await,
                    "provider-job-input" => self.job_input(p, &q.text, &cancel).await,
                    _ => Ok(()),
                };
                if let Err(e) = outcome {
                    return self.result(None, None, Some(e.message), false);
                }
                let job = self.get_job(p, &cancel).await?;
                let terminal = matches!(job.state.as_str(), "connected" | "done" | "failed");
                let mut error = None;
                if terminal && job.state != "failed" {
                    if let Err(e) = self.ensure_profile(p).await {
                        error = Some(e.message);
                    }
                }
                let refresh = error.is_none() && matches!(job.state.as_str(), "connected" | "done");
                let statuses = if terminal {
                    Some(self.statuses(&cancel).await)
                } else {
                    None
                };
                self.result(statuses, Some(job), error, refresh)
            }
            _ => Err(ProviderError::new("unknown provider operation")),
        }
    }
    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        let _active = self
            .active
            .clone()
            .acquire_many_owned(16)
            .await
            .expect("provider admission stays open");
        let _guard = self.mutation.lock().await;
        let ids: Vec<_> = self.jobs.lock().await.keys().copied().collect();
        let cancel = CancellationToken::new();
        for id in &ids {
            if let Some(p) = lookup(id) {
                let _ = self.cancel_job(p, &cancel).await;
            }
        }
        let drained = self
            .native_slots
            .clone()
            .acquire_many_owned(3)
            .await
            .expect("provider native admission stays open");
        drop(drained);
        // A kill attempted while all native permits were occupied is retried after drain.
        for id in ids {
            if let Some(p) = lookup(id) {
                jobs::tmux_kill(&self.env, &self.runner, &self.native_slots, p).await;
            }
        }
    }
    fn result(
        &self,
        providers: Option<Vec<ProviderStatus>>,
        job: Option<JobStatus>,
        error: Option<String>,
        refresh_auth: bool,
    ) -> Result<DomainProviderActionResult, ProviderError> {
        let result = ProviderResult {
            providers,
            job,
            error,
        };
        Ok(DomainProviderActionResult {
            result,
            refresh_auth,
        })
    }
    async fn command(
        &self,
        program: &Path,
        args: &[&str],
        max: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, ProviderError> {
        let spec = self.env.spec(program, args, max);
        run_bounded(
            self.runner.clone(),
            self.native_slots.clone(),
            spec,
            cancel.clone(),
        )
        .await
    }
}

async fn run_bounded(
    runner: CommandRunner,
    slots: Arc<Semaphore>,
    spec: CommandSpec,
    cancel: CancellationToken,
) -> Result<Vec<u8>, ProviderError> {
    let permit = slots
        .try_acquire_owned()
        .map_err(|_| ProviderError::new("provider command busy"))?;
    let (mut sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let _permit = permit;
        let (cancel_sender, cancel_receiver) = oneshot::channel();
        let mut cancel_sender = Some(cancel_sender);
        let run = runner.run_cancelable(spec, cancel_receiver);
        tokio::pin!(run);
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                if let Some(sender) = cancel_sender.take() { let _ = sender.send(()); }
                let _ = run.await;
                Err(ProviderError::new("provider request cancelled"))
            },
            () = sender.closed() => {
                if let Some(sender) = cancel_sender.take() { let _ = sender.send(()); }
                let _ = run.await;
                Err(ProviderError::new("provider request cancelled"))
            },
            result = &mut run => result.map(|o| o.stdout).map_err(|e| ProviderError::new(match e.kind() {
                RunErrorKind::Busy => "provider command busy",
                RunErrorKind::TimedOut => "provider command timed out",
                RunErrorKind::Cancelled => "provider request cancelled",
                _ => "provider command failed",
            })),
        };
        let _ = sender.send(result);
    });
    receiver
        .await
        .unwrap_or_else(|_| Err(ProviderError::new("provider command failed")))
}

#[derive(Serialize)]
struct ProviderResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    providers: Option<Vec<ProviderStatus>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    job: Option<JobStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
impl ProviderResult {
    fn into_proto(self) -> p::ProviderResult {
        p::ProviderResult {
            providers: self.providers.map(|items| p::ProviderStatuses {
                items: items
                    .into_iter()
                    .map(|v| p::ProviderStatus {
                        id: v.id.into(),
                        label: v.label.into(),
                        installed: v.installed,
                        version: v.version,
                        auth: v.auth.into(),
                        key_hint: v.key_hint,
                        profile: v.profile,
                        profile_id: v.profile_id,
                    })
                    .collect(),
            }),
            job: self.job.map(|v| p::ProviderJobStatus {
                state: v.state,
                url: v.url,
                code: v.code,
                needs_input: v.needs_input,
                log: v.log,
            }),
            error: self.error,
        }
    }
}
#[derive(Serialize)]
struct ProviderStatus {
    id: &'static str,
    label: &'static str,
    installed: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    version: String,
    auth: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    key_hint: String,
    profile: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    profile_id: String,
}
#[derive(Serialize, Clone, Debug)]
struct JobStatus {
    state: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    code: String,
    #[serde(skip_serializing_if = "is_false")]
    needs_input: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    log: Vec<String>,
}
fn is_false(value: &bool) -> bool {
    !*value
}
impl JobStatus {
    fn done() -> Self {
        Self {
            state: "done".into(),
            ..Self::none()
        }
    }
    fn none() -> Self {
        Self {
            state: "none".into(),
            url: String::new(),
            code: String::new(),
            needs_input: false,
            log: vec![],
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Empty {}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct KeyRequest {
    provider: String,
    key: String,
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct JobRequest {
    provider: String,
    #[serde(default)]
    action: String,
    #[serde(default)]
    text: String,
}
fn decode<T: Default + for<'de> Deserialize<'de>>(
    raw: &[u8],
    message: &'static str,
) -> Result<T, ProviderError> {
    if raw == b"null" {
        return Ok(T::default());
    }
    serde_json::from_slice(raw).map_err(|_| ProviderError::new(message))
}
pub struct ProviderActionResult {
    pub json: Vec<u8>,
    pub refresh_auth: bool,
}
struct DomainProviderActionResult {
    result: ProviderResult,
    refresh_auth: bool,
}
pub struct TypedProviderActionResult {
    pub result: p::ProviderResult,
    pub refresh_auth: bool,
}
pub struct ProviderError {
    message: String,
}
impl ProviderError {
    pub(crate) fn is_busy(&self) -> bool {
        matches!(
            self.message.as_str(),
            "provider request busy" | "provider setup busy" | "provider command busy"
        )
    }

    fn new(value: impl Into<String>) -> Self {
        Self {
            message: value.into(),
        }
    }
}
impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl fmt::Debug for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ProviderError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
        service: ProviderService,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "hmux-provider-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            fs::create_dir(root.join("bin")).unwrap();
            let fake="#!/bin/sh\ncase \"$1 $2 $3\" in\n  '--version  ') echo \"$0 1.2.3\";;\n  'auth status --json') echo '{\"loggedIn\":false}' ;;\n  'login status ') echo 'Not logged in' >&2; exit 1;;\n  *) exit 0;;\nesac\n";
            for name in ["codex", "claude", "gemini"] {
                executable(&root.join("bin").join(name), fake);
            }
            let tmux="#!/bin/sh\n[ \"$1\" = '-L' ] || exit 2\nshift 2\nprintf '%s\\n' \"$@\" >> \"$HOME/tmux-argv\"\ncase \"$1\" in\n  capture-pane) /bin/cat \"$HOME/pane.txt\";;\n  *) exit 0;;\nesac\n";
            executable(&root.join("bin/tmux"), tmux);
            fs::write(root.join("pane.txt"), "").unwrap();
            fs::create_dir(root.join("config")).unwrap();
            let inventory = root.join("config/inventory.toml");
            fs::write(&inventory,"schema_version=1\nrevision='synthetic'\n[[profiles]]\nid='shell'\nlabel='Shell'\ndefault_directory='~/work'\ncommand=['sh']\n").unwrap();
            fs::set_permissions(&inventory, fs::Permissions::from_mode(0o600)).unwrap();
            let mut env = ProviderEnv::for_home(
                root.clone(),
                root.join("bin").into_os_string(),
                inventory,
                root.join("bin/tmux"),
            );
            env.system_dirs = Some(Vec::new());
            env.job_socket = "hmux-provider-test".into();
            let service = ProviderService::new(env, CommandRunner::new(8).unwrap()).unwrap();
            Self { root, service }
        }
        async fn act(&self, operation: &str, value: serde_json::Value) -> ProviderActionResult {
            self.service
                .action(
                    operation,
                    &serde_json::to_vec(&value).unwrap(),
                    CancellationToken::new(),
                )
                .await
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn executable(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    #[tokio::test]
    async fn rejects_large_requests_and_filters_relative_path() {
        let f = Fixture::new();
        let too_large = vec![b'x'; 16 * 1024 + 1];
        assert!(f
            .service
            .action("providers", &too_large, CancellationToken::new())
            .await
            .is_err());
        let mut env = f.service.env.clone();
        env.path = OsString::from("relative:/nonexistent");
        env.system_dirs = Some(vec![]);
        assert!(env.validate().is_ok());
        assert!(env.executable("codex").is_none());
        assert!(!env.search_path().to_string_lossy().contains("relative"));
        env.path = OsString::from("a".repeat(8193));
        assert!(env.validate().is_err());
        env.path = OsString::new();
        env.job_socket = "x".repeat(65);
        assert!(env.validate().is_err());
    }

    #[tokio::test]
    async fn key_write_preserves_settings_and_registers_profile() {
        let f = Fixture::new();
        fs::create_dir(f.root.join(".claude")).unwrap();
        let settings = f.root.join(".claude/settings.json");
        fs::write(&settings, "{\"other\":7,\"env\":{\"STAY\":\"yes\"}}\n").unwrap();
        fs::set_permissions(&settings, fs::Permissions::from_mode(0o644)).unwrap();
        let result = f
            .act(
                "provider-key",
                serde_json::json!({"provider":"claude","key":"abcdefghijklmno1"}),
            )
            .await;
        assert!(result.refresh_auth);
        let json: serde_json::Value = serde_json::from_slice(&result.json).unwrap();
        assert_eq!(json["providers"][1]["auth"], "api-key");
        assert_eq!(json["providers"][1]["key_hint"], "…mno1");
        assert_eq!(json["providers"][1]["profile_id"], "claude");
        assert_eq!(
            fs::metadata(&settings).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        assert_eq!(saved["env"]["STAY"], "yes");
        assert_eq!(saved["env"]["ANTHROPIC_API_KEY"], "abcdefghijklmno1");
        assert_eq!(saved["other"], 7);
        let inventory = crate::config::load_inventory(&f.service.env.inventory_path).unwrap();
        assert_eq!(
            inventory
                .profiles
                .unwrap()
                .last()
                .unwrap()
                .default_directory,
            "~/work"
        );
        let backups = fs::read_dir(f.root.join("config"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|x| x.file_name().to_string_lossy().contains("hmux-backup"))
            .count();
        assert_eq!(backups, 1);
    }
    #[tokio::test]
    async fn valid_read_only_inventory_is_upgraded_privately() {
        let f = Fixture::new();
        fs::set_permissions(
            &f.service.env.inventory_path,
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let result = f
            .act(
                "provider-key",
                serde_json::json!({"provider":"gemini","key":"abcdefghijklmno1"}),
            )
            .await;
        assert!(result.refresh_auth);
        assert_eq!(
            fs::metadata(&f.service.env.inventory_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            crate::config::load_inventory(&f.service.env.inventory_path)
                .unwrap()
                .profiles
                .unwrap()
                .last()
                .unwrap()
                .id,
            "gemini"
        );
    }

    #[tokio::test]
    async fn exhausted_provider_admission_is_retryable() {
        let f = Fixture::new();
        let held = f
            .service
            .active
            .clone()
            .try_acquire_many_owned(f.service.active.available_permits() as u32)
            .unwrap();
        let result = f
            .service
            .action_typed(
                p::Operation::Providers,
                p::request::Payload::Empty(p::Empty {}),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(result, Err(error) if error.is_busy()));
        drop(held);
    }

    #[tokio::test]
    async fn typed_use_existing_reuses_account_login_without_running_auth_commands() {
        let f = Fixture::new();
        executable(
            &f.root.join("bin/codex"),
            "#!/bin/sh\ncase \"$1 $2\" in\n  '--version ') echo 'codex 1.2.3';;\n  'login status') echo 'Logged in using ChatGPT';;\n  *) echo unexpected > \"$HOME/unexpected-auth-command\"; exit 42;;\nesac\n",
        );
        fs::create_dir(f.root.join(".codex")).unwrap();
        let auth = f.root.join(".codex/auth.json");
        let original = b"{\"tokens\":{\"access_token\":\"synthetic-token\"}}\n";
        fs::write(&auth, original).unwrap();
        let result = f
            .service
            .action_typed(
                p::Operation::ProviderJobStart,
                p::request::Payload::ProviderJob(p::ProviderJobRequest {
                    provider: "codex".into(),
                    action: "use-existing".into(),
                    text: String::new(),
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.refresh_auth);
        assert!(result.result.error.is_none());
        assert_eq!(result.result.job.unwrap().state, "done");
        assert_eq!(fs::read(auth).unwrap(), original);
        assert!(!f.root.join("unexpected-auth-command").exists());
        assert!(!f.root.join("tmux-argv").exists());
        let inventory = crate::config::load_inventory(&f.service.env.inventory_path).unwrap();
        assert!(inventory
            .profiles
            .unwrap()
            .iter()
            .any(|profile| profile.id == "codex"));
    }

    #[tokio::test]
    async fn use_existing_registers_authenticated_cli_without_changing_credentials() {
        let f = Fixture::new();
        let settings = f.root.join(".claude/settings.json");
        fs::create_dir(f.root.join(".claude")).unwrap();
        let credential = b"{\"env\":{\"ANTHROPIC_API_KEY\":\"synthetic-key-123456\"}}\n";
        fs::write(&settings, credential).unwrap();
        let result = f
            .act(
                "provider-job-start",
                serde_json::json!({"provider":"claude","action":"use-existing"}),
            )
            .await;
        let body: serde_json::Value = serde_json::from_slice(&result.json).unwrap();
        assert_eq!(body["job"]["state"], "done");
        assert_eq!(body["providers"][1]["auth"], "api-key");
        assert_eq!(body["providers"][1]["profile_id"], "claude");
        assert_eq!(fs::read(&settings).unwrap(), credential);
        assert!(!f.root.join("tmux-argv").exists());
    }

    #[tokio::test]
    async fn use_existing_preserves_custom_profile_and_rejects_missing_setup() {
        let f = Fixture::new();
        fs::create_dir(f.root.join(".claude")).unwrap();
        fs::write(
            f.root.join(".claude/settings.json"),
            "{\"env\":{\"ANTHROPIC_API_KEY\":\"synthetic-key-123456\"}}\n",
        )
        .unwrap();
        let inventory = &f.service.env.inventory_path;
        let custom = "schema_version=1\nrevision='synthetic'\n[[profiles]]\nid='shell'\nlabel='Shell'\ndefault_directory='~/work'\ncommand=['sh']\n[[profiles]]\nid='my-claude'\nlabel='Custom Claude'\ndefault_directory='~/custom'\ncommand=['/opt/example/claude','--model','example']\n";
        fs::write(inventory, custom).unwrap();
        let result = f
            .act(
                "provider-job-start",
                serde_json::json!({"provider":"claude","action":"use-existing"}),
            )
            .await;
        let body: serde_json::Value = serde_json::from_slice(&result.json).unwrap();
        assert_eq!(body["providers"][1]["profile_id"], "my-claude");
        assert_eq!(fs::read_to_string(inventory).unwrap(), custom);

        let unauthenticated = f
            .act(
                "provider-job-start",
                serde_json::json!({"provider":"codex","action":"use-existing"}),
            )
            .await;
        let body: serde_json::Value = serde_json::from_slice(&unauthenticated.json).unwrap();
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("not installed and authenticated"));
        fs::remove_file(f.root.join("bin/claude")).unwrap();
        let missing = f
            .act(
                "provider-job-start",
                serde_json::json!({"provider":"claude","action":"use-existing"}),
            )
            .await;
        let body: serde_json::Value = serde_json::from_slice(&missing.json).unwrap();
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("not installed and authenticated"));
        assert_eq!(fs::read_to_string(inventory).unwrap(), custom);
    }

    #[tokio::test]
    async fn use_existing_does_not_interrupt_active_setup() {
        let f = Fixture::new();
        fs::create_dir(f.root.join(".claude")).unwrap();
        fs::write(
            f.root.join(".claude/settings.json"),
            "{\"env\":{\"ANTHROPIC_API_KEY\":\"synthetic-key-123456\"}}\n",
        )
        .unwrap();
        let started = f
            .act(
                "provider-job-start",
                serde_json::json!({"provider":"claude","action":"update"}),
            )
            .await;
        let started: serde_json::Value = serde_json::from_slice(&started.json).unwrap();
        assert_eq!(started["job"]["state"], "installing");
        let refused = f
            .act(
                "provider-job-start",
                serde_json::json!({"provider":"claude","action":"use-existing"}),
            )
            .await;
        let refused: serde_json::Value = serde_json::from_slice(&refused.json).unwrap();
        assert_eq!(refused["error"], "provider setup already in progress");
        assert!(f.service.jobs.lock().await.contains_key("claude"));
        f.service
            .cancel_job(PROVIDERS[1], &CancellationToken::new())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn removing_gemini_key_restores_only_usable_selected_account() {
        let f = Fixture::new();
        let gemini = f.root.join(".gemini");
        fs::create_dir(&gemini).unwrap();
        let settings = gemini.join("settings.json");
        let dotenv = gemini.join(".env");
        fs::write(
            &settings,
            "{\"other\":7,\"security\":{\"auth\":{\"selectedType\":\"gemini-api-key\",\"other\":true}}}\n",
        )
        .unwrap();
        fs::write(&dotenv, "GEMINI_API_KEY=synthetic-key-123456\n").unwrap();
        fs::write(
            gemini.join("oauth_creds.json"),
            "{\"refresh_token\":\"synthetic-refresh-123\"}\n",
        )
        .unwrap();
        let result = f
            .act(
                "provider-key",
                serde_json::json!({"provider":"gemini","key":""}),
            )
            .await;
        let body: serde_json::Value = serde_json::from_slice(&result.json).unwrap();
        assert_eq!(body["providers"][2]["auth"], "account");
        let after: serde_json::Value =
            serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        assert_eq!(after["security"]["auth"]["selectedType"], "oauth-personal");
        assert_eq!(after["security"]["auth"]["other"], true);
        assert_eq!(after["other"], 7);
        assert!(!fs::read_to_string(&dotenv)
            .unwrap()
            .contains("GEMINI_API_KEY"));

        fs::write(
            &settings,
            "{\"security\":{\"auth\":{\"selectedType\":\"custom\"}}}\n",
        )
        .unwrap();
        fs::write(&dotenv, "GEMINI_API_KEY=synthetic-key-123456\n").unwrap();
        let result = f
            .act(
                "provider-key",
                serde_json::json!({"provider":"gemini","key":""}),
            )
            .await;
        let body: serde_json::Value = serde_json::from_slice(&result.json).unwrap();
        assert_eq!(body["providers"][2]["auth"], "none");
        assert!(fs::read_to_string(&settings).unwrap().contains("custom"));

        fs::remove_file(gemini.join("oauth_creds.json")).unwrap();
        fs::write(
            &settings,
            "{\"security\":{\"auth\":{\"selectedType\":\"gemini-api-key\"}}}\n",
        )
        .unwrap();
        fs::write(&dotenv, "GEMINI_API_KEY=synthetic-key-123456\n").unwrap();
        let result = f
            .act(
                "provider-key",
                serde_json::json!({"provider":"gemini","key":""}),
            )
            .await;
        let body: serde_json::Value = serde_json::from_slice(&result.json).unwrap();
        assert_eq!(body["providers"][2]["auth"], "none");
        assert_eq!(store::gemini_selected_auth(&f.root).unwrap(), "");
    }

    #[tokio::test]
    async fn unicode_saved_key_has_safe_hint_and_invalid_settings_are_preserved() {
        let f = Fixture::new();
        fs::create_dir(f.root.join(".claude")).unwrap();
        let settings = f.root.join(".claude/settings.json");
        fs::write(
            &settings,
            "{\"env\":{\"ANTHROPIC_API_KEY\":\"abcdefgh🙂\"}}\n",
        )
        .unwrap();
        fs::set_permissions(&settings, fs::Permissions::from_mode(0o600)).unwrap();
        let result = f.act("providers", serde_json::json!({})).await;
        let v: serde_json::Value = serde_json::from_slice(&result.json).unwrap();
        assert_eq!(v["providers"][1]["key_hint"], "…fgh🙂");
        let gemini = f.root.join(".gemini");
        fs::create_dir(&gemini).unwrap();
        let gemini_settings = gemini.join("settings.json");
        let original = b"{\"security\":\"preserve\"}\n";
        fs::write(&gemini_settings, original).unwrap();
        fs::set_permissions(&gemini_settings, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(store::set_gemini_auth(&f.root, "gemini-api-key", |_| true).is_err());
        assert_eq!(fs::read(&gemini_settings).unwrap(), original);
    }

    #[tokio::test]
    async fn dropped_command_is_reaped_before_shutdown_returns() {
        let f = Fixture::new();
        let slow = f.root.join("bin/slow");
        executable(&slow, "#!/bin/sh\nexec /bin/sleep 10\n");
        let spec = f.service.env.spec(&slow, &[], 1024);
        let command = run_bounded(
            f.service.runner.clone(),
            f.service.native_slots.clone(),
            spec,
            CancellationToken::new(),
        );
        let mut command = Box::pin(command);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut command)
                .await
                .is_err()
        );
        drop(command);
        tokio::time::timeout(Duration::from_secs(3), f.service.shutdown())
            .await
            .unwrap();
        assert_eq!(f.service.native_slots.available_permits(), 3);
        assert!(f
            .service
            .action("providers", b"{}", CancellationToken::new())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn job_input_redacts_pane_and_terminal_job_is_evicted() {
        let f = Fixture::new();
        fs::write(
            f.root.join("pane.txt"),
            "https://auth.openai.com/device\nAB12-CD345\nauthorization code:\n",
        )
        .unwrap();
        let started = f
            .act(
                "provider-job-start",
                serde_json::json!({"provider":"codex","action":"connect"}),
            )
            .await;
        let s: serde_json::Value = serde_json::from_slice(&started.json).unwrap();
        assert_eq!(s["job"]["state"], "installing");
        let marker = f.root.join(".local/state/hmux-setup/connect-codex");
        fs::write(&marker, "login").unwrap();
        let ready = f
            .act("provider-job", serde_json::json!({"provider":"codex"}))
            .await;
        let r: serde_json::Value = serde_json::from_slice(&ready.json).unwrap();
        assert_eq!(r["job"]["url"], "https://auth.openai.com/device");
        assert_eq!(r["job"]["code"], "AB12-CD345");
        let sent = f
            .act(
                "provider-job-input",
                serde_json::json!({"provider":"codex","text":"secret-123"}),
            )
            .await;
        let text = String::from_utf8(sent.json).unwrap();
        assert!(!text.contains("secret-123"));
        assert!(!text.contains("auth.openai.com"));
        fs::write(&marker, "done:0:login").unwrap();
        let done = f
            .act("provider-job", serde_json::json!({"provider":"codex"}))
            .await;
        let d: serde_json::Value = serde_json::from_slice(&done.json).unwrap();
        assert_eq!(d["job"]["state"], "done");
        assert!(done.refresh_auth);
        assert!(f.service.jobs.lock().await.is_empty());
        let commands = fs::read_to_string(f.root.join("tmux-argv")).unwrap();
        assert!(commands.contains("kill-session"));
        assert!(commands.contains("=connect-codex"));
        f.service.shutdown().await;
    }
}
