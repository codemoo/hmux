use super::*;
use hmux_core::command::with_child_spawn;
use std::process::Stdio;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::oneshot,
};

impl ProviderService {
    pub(super) async fn statuses(&self, cancel: &CancellationToken) -> Vec<ProviderStatus> {
        let (a, b, c) = tokio::join!(
            self.status(PROVIDERS[0], cancel),
            self.status(PROVIDERS[1], cancel),
            self.status(PROVIDERS[2], cancel)
        );
        let mut values = vec![a, b, c];
        let registered = self
            .private_task(|env| {
                Ok(crate::config::load_inventory(&env.inventory_path)
                    .ok()
                    .and_then(|i| i.profiles)
                    .unwrap_or_default())
            })
            .await
            .unwrap_or_default();
        for value in &mut values {
            if let Some(profile) = registered.iter().find(|x| {
                x.command
                    .as_deref()
                    .and_then(|c| c.first())
                    .is_some_and(|s| Path::new(s).file_name() == Some(OsStr::new(value.id)))
            }) {
                value.profile = true;
                value.profile_id = profile.id.clone();
            }
        }
        values
    }
    pub(super) async fn status(&self, p: Provider, cancel: &CancellationToken) -> ProviderStatus {
        let path = self
            .private_task(move |env| Ok(env.executable(p.command)))
            .await
            .unwrap_or(None);
        let mut s = ProviderStatus {
            id: p.id,
            label: p.label,
            installed: path.is_some(),
            version: String::new(),
            auth: "none",
            key_hint: String::new(),
            profile: false,
            profile_id: String::new(),
        };
        if let Some(path) = &path {
            if let Ok(raw) = self.command(path, &["--version"], 64 * 1024, cancel).await {
                s.version = clean_version(&raw);
            }
        }
        match p.id {
            "codex" => {
                if let Some(path) = &path {
                    if let Ok(out) = self
                        .native(
                            path.clone(),
                            vec!["login".into(), "status".into()],
                            None,
                            cancel.clone(),
                        )
                        .await
                    {
                        if out.success {
                            let text = format!(
                                "{}{}",
                                String::from_utf8_lossy(&out.stdout),
                                String::from_utf8_lossy(&out.stderr)
                            );
                            if text.contains("API key") {
                                s.auth = "api-key";
                                s.key_hint = self
                                    .private_task(|env| Ok(store::codex_hint(&env.home)))
                                    .await
                                    .unwrap_or_default();
                            } else if text.contains("Logged in") {
                                s.auth = "account";
                            }
                        }
                    }
                }
            }
            "claude" => {
                let key = self
                    .private_task(|env| Ok(store::claude_key(&env.home)))
                    .await
                    .unwrap_or_default();
                if !key.is_empty() {
                    s.auth = "api-key";
                    s.key_hint = store::hint(&key);
                } else if let Some(path) = &path {
                    if let Ok(raw) = self
                        .command(path, &["auth", "status", "--json"], 64 * 1024, cancel)
                        .await
                    {
                        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) {
                            if v.get("loggedIn").and_then(|x| x.as_bool()) == Some(true) {
                                s.auth = if v.get("authMethod").and_then(|x| x.as_str())
                                    == Some("api_key")
                                {
                                    "api-key"
                                } else {
                                    "account"
                                };
                            }
                        }
                    }
                }
            }
            "gemini" => {
                let key = self
                    .private_task(move |env| Ok(store::dotenv_value(&env.home, p.key_name)))
                    .await
                    .unwrap_or_default();
                if !key.is_empty() {
                    s.auth = "api-key";
                    s.key_hint = store::hint(&key);
                } else if self
                    .private_task(|env| Ok(store::oauth_fingerprint(&env.home).is_some()))
                    .await
                    .unwrap_or(false)
                {
                    s.auth = "account";
                }
            }
            _ => {}
        }
        s
    }
    pub(super) async fn codex_login(
        &self,
        path: &Path,
        key: &str,
        cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        let output = self
            .native(
                path.to_path_buf(),
                vec!["login".into(), "--with-api-key".into()],
                Some(format!("{key}\n").into_bytes()),
                cancel.clone(),
            )
            .await?;
        if output.success {
            Ok(())
        } else {
            Err(ProviderError::new("codex login failed"))
        }
    }
    async fn native(
        &self,
        program: PathBuf,
        args: Vec<OsString>,
        input: Option<Vec<u8>>,
        cancel: CancellationToken,
    ) -> Result<NativeOutput, ProviderError> {
        let permit = self
            .native_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ProviderError::new("provider command busy"))?;
        let env = self.env.clone();
        let (mut tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let _permit = permit;
            let result = run_native(env, program, args, input, cancel, &mut tx).await;
            let _ = tx.send(result);
        });
        rx.await
            .unwrap_or_else(|_| Err(ProviderError::new("provider command failed")))
    }
}
struct NativeOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
async fn run_native(
    env: ProviderEnv,
    program: PathBuf,
    args: Vec<OsString>,
    input: Option<Vec<u8>>,
    cancel: CancellationToken,
    sender: &mut oneshot::Sender<Result<NativeOutput, ProviderError>>,
) -> Result<NativeOutput, ProviderError> {
    if cancel.is_cancelled() || sender.is_closed() {
        return Err(ProviderError::new("provider request cancelled"));
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(&env.home)
        .env("HOME", &env.home)
        .env("PATH", env.search_path())
        .env("NO_COLOR", "1")
        .env("NO_BROWSER", "true")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = with_child_spawn(|| command.spawn())
        .map_err(|_| ProviderError::new("provider command failed"))?;
    let result = run_child(&mut child, input, env.timeout, cancel, sender).await;
    if result.is_err() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    result
}
async fn run_child(
    child: &mut Child,
    input: Option<Vec<u8>>,
    timeout: Duration,
    cancel: CancellationToken,
    sender: &mut oneshot::Sender<Result<NativeOutput, ProviderError>>,
) -> Result<NativeOutput, ProviderError> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProviderError::new("provider command failed"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ProviderError::new("provider command failed"))?;
    let stdin = child.stdin.take();
    let work = async {
        let write = async {
            if let (Some(mut stream), Some(bytes)) = (stdin, input) {
                stream
                    .write_all(&bytes)
                    .await
                    .map_err(|_| ProviderError::new("provider command failed"))?;
            }
            Ok(())
        };
        let ((), out, err, status) = tokio::try_join!(
            write,
            read_limit(stdout, 64 * 1024),
            read_limit(stderr, 64 * 1024),
            async {
                child
                    .wait()
                    .await
                    .map_err(|_| ProviderError::new("provider command failed"))
            }
        )?;
        Ok(NativeOutput {
            success: status.success(),
            stdout: out,
            stderr: err,
        })
    };
    tokio::select! {
        r=work=>r,
        ()=tokio::time::sleep(timeout)=>Err(ProviderError::new("provider command timed out")),
        ()=cancel.cancelled()=>Err(ProviderError::new("provider request cancelled")),
        ()=sender.closed()=>Err(ProviderError::new("provider request cancelled")),
    }
}
async fn read_limit(
    mut pipe: impl AsyncRead + Unpin,
    limit: usize,
) -> Result<Vec<u8>, ProviderError> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = pipe
            .read(&mut buf)
            .await
            .map_err(|_| ProviderError::new("provider command failed"))?;
        if n == 0 {
            return Ok(out);
        }
        if n > limit - out.len() {
            return Err(ProviderError::new("provider output too large"));
        }
        out.extend_from_slice(&buf[..n]);
    }
}
pub(super) fn clean_version(raw: &[u8]) -> String {
    let line = String::from_utf8_lossy(raw);
    let line = line.lines().next().unwrap_or("");
    for (start, _) in line.char_indices().filter(|(_, c)| c.is_ascii_digit()) {
        let tail = &line[start..];
        let token: String = tail
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
            .collect();
        let mut parts = token.split('.');
        let first = parts.next().unwrap_or("");
        let second = parts.next().unwrap_or("");
        if !first.is_empty() && second.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return token.trim_end_matches(['.', '-', '+']).into();
        }
    }
    String::new()
}
