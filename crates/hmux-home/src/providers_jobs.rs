use super::*;
use hmux_core::PrivateDir;
use http::Uri;
use std::{ffi::OsStr, time::Duration};
use tokio::task::JoinHandle;

const SETUP_SCRIPT: &str = include_str!("provider_setup.sh");
const JOB_LIFETIME: Duration = Duration::from_secs(20 * 60);

pub(super) struct JobTask {
    cancel: CancellationToken,
    join: JoinHandle<()>,
}
fn session(p: Provider) -> String {
    format!("connect-{}", p.id)
}
fn pane(p: Provider) -> String {
    format!("={}:", session(p))
}
fn state_dir(env: &ProviderEnv) -> Result<PrivateDir, ProviderError> {
    PrivateDir::open_or_create_trusted(&env.home.join(".local/state/hmux-setup"))
        .map_err(|_| ProviderError::new("provider setup state directory is unsafe"))
}
fn state_name(p: Provider, suffix: &str) -> String {
    format!("connect-{}{}", p.id, suffix)
}
fn write_marker(
    env: &ProviderEnv,
    p: Provider,
    suffix: &str,
    bytes: &[u8],
) -> Result<(), ProviderError> {
    state_dir(env)?
        .write_atomic_private(OsStr::new(&state_name(p, suffix)), bytes)
        .map_err(|_| ProviderError::new("provider setup state write failed"))
}
fn read_marker(env: &ProviderEnv, p: Provider, suffix: &str) -> Option<Vec<u8>> {
    state_dir(env)
        .ok()?
        .read_private(OsStr::new(&state_name(p, suffix)), 4096)
        .ok()
}
fn remove_marker(env: &ProviderEnv, p: Provider, suffix: &str) {
    let path = env
        .home
        .join(".local/state/hmux-setup")
        .join(state_name(p, suffix));
    let _ = std::fs::remove_file(path);
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
fn setup_command(action: &str, p: Provider) -> String {
    ["bash", "-c", SETUP_SCRIPT, "hmux-setup", action, p.id]
        .iter()
        .map(|x| quote(x))
        .collect::<Vec<_>>()
        .join(" ")
}
async fn tmux(
    env: &ProviderEnv,
    runner: &CommandRunner,
    slots: &Arc<Semaphore>,
    args: &[&str],
    cancel: &CancellationToken,
) -> Result<Vec<u8>, ProviderError> {
    let mut full = vec!["-L", env.job_socket.as_str()];
    full.extend_from_slice(args);
    let spec = env.spec(&env.tmux_path, &full, 1 << 20);
    run_bounded(runner.clone(), slots.clone(), spec, cancel.clone())
        .await
        .map_err(|_| ProviderError::new("provider setup command failed"))
}
pub(super) async fn tmux_kill(
    env: &ProviderEnv,
    runner: &CommandRunner,
    slots: &Arc<Semaphore>,
    p: Provider,
) {
    let target = format!("={}", session(p));
    let _ = tmux(
        env,
        runner,
        slots,
        &["kill-session", "-t", &target],
        &CancellationToken::new(),
    )
    .await;
}
impl ProviderService {
    pub(super) async fn start_job(
        &self,
        p: Provider,
        action: &str,
        cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        if action != "connect" && action != "update" {
            return Err(ProviderError::new("unknown setup action"));
        }
        let _ = self.cancel_job(p, cancel).await;
        if p.id == "gemini" && action == "connect" {
            self.private_task(|env| {
                store::set_gemini_auth(&env.home, "oauth-personal", |current| {
                    current.is_empty() || current == "gemini-api-key"
                })
            })
            .await?;
        }
        let env = self.env.clone();
        let is_gemini_login = p.id == "gemini" && action == "connect";
        self.private_task(move |env| {
            if is_gemini_login {
                let baseline = store::oauth_fingerprint(&env.home).unwrap_or_else(|| "none".into());
                write_marker(&env, p, ".oauth-baseline", baseline.as_bytes())?;
            }
            write_marker(&env, p, "", b"")
        })
        .await?;
        let state = env
            .home
            .join(".local/state/hmux-setup")
            .join(state_name(p, ""));
        let script = setup_command(action, p);
        let home = env.home.to_string_lossy().to_string();
        let path = env.search_path().to_string_lossy().to_string();
        let state = state.to_string_lossy().to_string();
        let session = session(p);
        tmux(
            &env,
            &self.runner,
            &self.native_slots,
            &[
                "new-session",
                "-d",
                "-x",
                "400",
                "-y",
                "60",
                "-s",
                &session,
                "-c",
                &home,
                "-e",
                &format!("HOME={home}"),
                "-e",
                &format!("PATH={path}"),
                "-e",
                &format!("HMUX_JOB_STATE={state}"),
                &script,
            ],
            cancel,
        )
        .await?;
        let task_cancel = CancellationToken::new();
        let task_token = task_cancel.clone();
        let task_env = env.clone();
        let runner = self.runner.clone();
        let slots = self.native_slots.clone();
        let join = tokio::spawn(async move {
            tokio::select! { ()=tokio::time::sleep(JOB_LIFETIME)=>tmux_kill(&task_env,&runner,&slots,p).await, ()=task_token.cancelled()=>{} }
        });
        self.jobs.lock().await.insert(
            p.id,
            JobTask {
                cancel: task_cancel,
                join,
            },
        );
        Ok(())
    }
    pub(super) async fn cancel_job(
        &self,
        p: Provider,
        _cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        if let Some(task) = self.jobs.lock().await.remove(p.id) {
            task.cancel.cancel();
            let _ = task.join.await;
        }
        self.private_task(move |env| {
            remove_marker(&env, p, "");
            remove_marker(&env, p, ".oauth-baseline");
            remove_marker(&env, p, ".input-sent");
            Ok(())
        })
        .await?;
        tmux_kill(&self.env, &self.runner, &self.native_slots, p).await;
        Ok(())
    }
    pub(super) async fn job_input(
        &self,
        p: Provider,
        text: &str,
        cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        let text = text.trim();
        if text.is_empty() || text.len() > 2048 || !text.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err(ProviderError::new("코드 형식이 올바르지 않습니다"));
        }
        self.private_task(move |env| write_marker(&env, p, ".input-sent", b""))
            .await?;
        let target = pane(p);
        tmux(
            &self.env,
            &self.runner,
            &self.native_slots,
            &["send-keys", "-t", &target, "-l", text],
            cancel,
        )
        .await
        .map_err(|_| ProviderError::new("진행 중인 로그인이 없습니다"))?;
        tmux(
            &self.env,
            &self.runner,
            &self.native_slots,
            &["send-keys", "-t", &target, "Enter"],
            cancel,
        )
        .await
        .map(|_| ())
    }
    pub(super) async fn get_job(
        &self,
        p: Provider,
        cancel: &CancellationToken,
    ) -> Result<JobStatus, ProviderError> {
        let target = pane(p);
        let raw = match tmux(
            &self.env,
            &self.runner,
            &self.native_slots,
            &["capture-pane", "-p", "-J", "-S", "-300", "-t", &target],
            cancel,
        )
        .await
        {
            Ok(raw) => raw,
            Err(_) => return Ok(JobStatus::none()),
        };
        let (mut status, phase) = self
            .private_task(move |env| {
                let phase = read_marker(&env, p, "").unwrap_or_default();
                let phase = String::from_utf8_lossy(&phase).trim().to_owned();
                let mut status = parse_job(&phase, &String::from_utf8_lossy(&raw));
                if p.id == "gemini" && status.state == "login" {
                    let baseline = read_marker(&env, p, ".oauth-baseline")
                        .and_then(|raw| String::from_utf8(raw).ok());
                    if baseline.is_some()
                        && store::oauth_fingerprint(&env.home)
                            .is_some_and(|current| Some(current) != baseline)
                    {
                        status.state = "connected".into();
                    }
                }
                if read_marker(&env, p, ".input-sent").is_some() {
                    status.url.clear();
                    status.code.clear();
                    status.needs_input = false;
                    status.log.clear();
                }
                Ok((status, phase))
            })
            .await?;
        let terminal = matches!(status.state.as_str(), "connected" | "done" | "failed");
        if terminal {
            let _ = self.cancel_job(p, cancel).await;
            if p.id == "claude" && status.state == "done" && phase.ends_with(":login") {
                let version = self.status(p, cancel).await.version;
                let result = self
                    .private_task(move |env| {
                        store::mark_claude_ready(&env.home, "", Some(&version))
                    })
                    .await;
                if let Err(e) = result {
                    status.log.push(e.message);
                }
            }
        }
        Ok(status)
    }
}
fn parse_job(phase: &str, pane: &str) -> JobStatus {
    let mut s = JobStatus::none();
    s.state = if phase == "login" {
        "login"
    } else if phase.starts_with("done:0:") {
        "done"
    } else if phase.starts_with("done:") {
        "failed"
    } else {
        "installing"
    }
    .into();
    let mut lines = Vec::new();
    for raw in pane.lines() {
        let cleaned: String = raw
            .chars()
            .filter_map(|c| {
                if c == '\t' {
                    Some(' ')
                } else if c < ' ' || c == '\u{7f}' {
                    None
                } else {
                    Some(c)
                }
            })
            .collect();
        let line = cleaned.trim_end_matches(' ');
        if !line.trim().is_empty() {
            lines.push(line.to_owned());
        }
    }
    if s.state == "login" {
        for line in &lines {
            for candidate in line.split_whitespace() {
                if let Some(pos) = candidate.find("https://") {
                    let url = candidate[pos..].trim_end_matches(['"', '\'', '<', '>']);
                    if allowed_url(url) {
                        s.url = url.into();
                    }
                }
            }
            if let Some(code) = find_code(line) {
                s.code = code;
            }
        }
        s.needs_input = lines.last().is_some_and(|x| {
            let lower = x.to_ascii_lowercase();
            lower.contains("paste code here") || lower.contains("authorization code:")
        });
    }
    s.log = lines
        .into_iter()
        .rev()
        .take(12)
        .rev()
        .map(|line| {
            if line.len() > 300 {
                format!("{}…", line.chars().take(300).collect::<String>())
            } else {
                line
            }
        })
        .collect();
    s
}
fn allowed_url(raw: &str) -> bool {
    if raw.len() > 4096 {
        return false;
    }
    let Ok(uri) = raw.parse::<Uri>() else {
        return false;
    };
    let Some(authority) = uri.authority() else {
        return false;
    };
    uri.scheme_str() == Some("https")
        && !authority.as_str().contains('@')
        && matches!(
            Some(authority.host()),
            Some(
                "auth.openai.com"
                    | "claude.ai"
                    | "claude.com"
                    | "platform.claude.com"
                    | "console.anthropic.com"
                    | "accounts.google.com"
            )
        )
}
fn find_code(line: &str) -> Option<String> {
    for token in line.split(|c: char| !c.is_ascii_uppercase() && !c.is_ascii_digit() && c != '-') {
        let Some((a, b)) = token.split_once('-') else {
            continue;
        };
        if a.len() == 4
            && (4..=5).contains(&b.len())
            && a.bytes()
                .chain(b.bytes())
                .all(|x| x.is_ascii_uppercase() || x.is_ascii_digit())
        {
            return Some(token.into());
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parser_allowlists_and_redacts_control() {
        let s=parse_job("login","\u{1b}[31m https://evil.example/a https://auth.openai.com/c\nAB12-CD345\nauthorization code:");
        assert_eq!(s.url, "https://auth.openai.com/c");
        assert_eq!(s.code, "AB12-CD345");
        assert!(s.needs_input);
        assert!(s.log.iter().all(|x| !x.contains('\u{1b}')));
        assert!(!allowed_url("https://evil.example@auth.openai.com/login"));
    }
}
