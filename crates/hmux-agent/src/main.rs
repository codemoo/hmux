//! Native administrative CLI. All tmux and provider access is delegated to
//! bounded Home owners; arguments are parsed before opening private state.
mod setup;
use bytes::Bytes;
use hmux_home::{agent_support::AgentSupport, config, create_plan::Plan, workflow};
use hmux_model::{SessionIdentity, PROTOCOL_VERSION};
use hmux_protocol::protobuf::types as p;
use std::{
    env,
    io::{self, Write},
    path::PathBuf,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

type Result<T> = std::result::Result<T, String>;
const USAGE: &str = "usage: hmux-agent <catalog|recovery|conversation|workspace|workflow|workflow-hook|workflow-report|setup-home|create|alias-set|hidden-set|terminate|metadata-migrate|doctor|version>";
fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or("HOME is unavailable".into())
}
fn backend() -> Result<AgentSupport> {
    let home = home()?;
    let cfg = config::load_home(None, &home)?;
    AgentSupport::new(
        cfg,
        &home,
        &env::var_os("PATH").unwrap_or_default(),
        env::var_os("SHELL").unwrap_or_default(),
    )
}
async fn line(limit: usize, stop: &CancellationToken) -> Result<String> {
    let mut raw = read_bytes(limit + 1, stop).await?;
    if raw.len() > limit + 1 {
        return Err(format!("input exceeds {limit} bytes"));
    }
    if raw.ends_with(b"\n") {
        raw.pop();
    }
    if raw.ends_with(b"\r") {
        raw.pop();
    }
    if raw.len() > limit {
        return Err(format!("input exceeds {limit} bytes"));
    }
    if raw.contains(&b'\r') || raw.contains(&b'\n') {
        return Err("input must be a single line".into());
    }
    String::from_utf8(raw).map_err(|_| "input is not UTF-8".into())
}
fn identity(args: &[String]) -> Result<SessionIdentity> {
    if args.len() != 3 || args[0] != "--created-at" {
        return Err("invalid expected identity arguments".into());
    }
    let created_at = args[1]
        .parse::<i64>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or("invalid session creation time")?;
    hmux_model::validate_session_id(&args[2]).map_err(|e| e.to_string())?;
    Ok(SessionIdentity {
        id: args[2].clone(),
        created_at,
    })
}
struct RestoringStdin {
    input: io::Stdin,
    original: rustix::fs::OFlags,
}
impl std::os::fd::AsRawFd for RestoringStdin {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        std::os::fd::AsRawFd::as_raw_fd(&self.input)
    }
}
impl Drop for RestoringStdin {
    fn drop(&mut self) {
        let _ = rustix::fs::fcntl_setfl(&self.input, self.original);
    }
}
async fn read_terminal(max: usize, stop: &CancellationToken) -> Result<Vec<u8>> {
    use tokio::io::{unix::AsyncFd, Interest};
    let input = io::stdin();
    let original = rustix::fs::fcntl_getfl(&input).map_err(|e| e.to_string())?;
    rustix::fs::fcntl_setfl(&input, original | rustix::fs::OFlags::NONBLOCK)
        .map_err(|e| e.to_string())?;
    let descriptor = RestoringStdin { input, original };
    let ready =
        AsyncFd::with_interest(descriptor, Interest::READABLE).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let mut guard = tokio::select! {
            biased;
            _ = stop.cancelled() => return Err("input cancelled".into()),
            ready = ready.readable() => ready.map_err(|e| e.to_string())?,
        };
        let count = match guard
            .try_io(|fd| rustix::io::read(&fd.get_ref().input, &mut buffer).map_err(Into::into))
        {
            Ok(Ok(count)) => count,
            Ok(Err(error)) => return Err(error.to_string()),
            Err(_) => continue,
        };
        if count == 0 {
            break;
        }
        raw.extend_from_slice(&buffer[..count]);
        if raw.len() > max {
            return Err("request exceeds byte limit".into());
        }
    }
    Ok(raw)
}
async fn read_bytes(max: usize, stop: &CancellationToken) -> Result<Vec<u8>> {
    use std::io::IsTerminal;
    let input = io::stdin();
    if input.is_terminal() {
        return read_terminal(max, stop).await;
    }
    let stat = rustix::fs::fstat(&input).map_err(|e| e.to_string())?;
    let kind = rustix::fs::FileType::from_raw_mode(stat.st_mode);
    // macOS poll rejects /dev/null. This known, immediate-EOF device is safe
    // to read directly; any other non-TTY device retains cancellable polling.
    let direct = kind == rustix::fs::FileType::CharacterDevice
        && rustix::fs::stat("/dev/null").is_ok_and(|null| null.st_rdev == stat.st_rdev);
    let stop = stop.clone();
    tokio::task::spawn_blocking(move || {
        use rustix::event::{poll, PollFd, PollFlags, Timespec};
        let stdin = io::stdin();
        let input = stdin.lock();
        let mut raw = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            if stop.is_cancelled() {
                return Err("input cancelled".into());
            }
            if !direct {
                let mut fds = [PollFd::new(&input, PollFlags::IN)];
                let ready = poll(
                    &mut fds,
                    Some(&Timespec {
                        tv_sec: 0,
                        tv_nsec: 100_000_000,
                    }),
                )
                .map_err(|e| e.to_string())?;
                if ready == 0 {
                    continue;
                }
            }
            let count = rustix::io::read(&input, &mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            raw.extend_from_slice(&buffer[..count]);
            if raw.len() > max {
                return Err("request exceeds byte limit".into());
            }
        }
        Ok(raw)
    })
    .await
    .map_err(|_| "input worker unavailable".to_string())?
}
fn json_line(raw: &[u8]) -> Result<()> {
    io::stdout().write_all(raw).map_err(|e| e.to_string())?;
    println!();
    Ok(())
}
fn pretty<T: serde::Serialize>(value: &T) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| e.to_string())?
    );
    Ok(())
}
fn workflow_args(args: &[String]) -> Result<(String, bool)> {
    let mut filter = String::new();
    let mut json = false;
    for arg in args {
        if arg == "--json" && !json {
            json = true;
        } else if arg.starts_with('-') || !filter.is_empty() {
            return Err("usage: hmux-agent workflow [session] [--json]".into());
        } else {
            filter.clone_from(arg);
        }
    }
    Ok((filter, json))
}
fn workflow_env() -> workflow::BindingEnvironment {
    workflow::BindingEnvironment {
        session_id: env::var("HMUX_TMUX_SESSION_ID").unwrap_or_default(),
        created_at: env::var("HMUX_TMUX_SESSION_CREATED_AT").unwrap_or_default(),
        pane: env::var("TMUX_PANE").unwrap_or_default(),
    }
}
async fn workflow_binding(
    backend: Option<&AgentSupport>,
    stop: &CancellationToken,
) -> Result<workflow::Binding> {
    let env = workflow_env();
    if !env.session_id.is_empty() {
        hmux_model::validate_session_id(&env.session_id).map_err(|_| "invalid workflow binding")?;
        let created_at = env
            .created_at
            .parse::<i64>()
            .ok()
            .filter(|v| *v > 0)
            .ok_or("invalid workflow binding")?;
        return Ok(SessionIdentity {
            id: env.session_id,
            created_at,
        });
    }
    let backend = backend.ok_or("workflow binding unavailable")?;
    let binding_stop = stop.child_token();
    let timer_stop = binding_stop.clone();
    let timer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        timer_stop.cancel();
    });
    let result =
        workflow::resolve_binding(&env, backend.reader(), backend.runner(), &binding_stop).await;
    timer.abort();
    let _ = timer.await;
    result.map_err(|e| format!("workflow binding: {e:?}"))
}
async fn hook(stop: &CancellationToken) {
    let _ = async {
        let raw = read_bytes(256 << 10, stop).await?;
        let event = workflow::HookEvent::parse(&raw).map_err(|e| format!("hook: {e:?}"))?;
        let cfg = config::load_home(None, &home()?)?;
        let b = if env::var_os("HMUX_TMUX_SESSION_ID").is_some_and(|v| !v.is_empty()) {
            None
        } else {
            Some(backend()?)
        };
        let binding = workflow_binding(b.as_ref(), stop).await?;
        let hook_stop = stop.child_token();
        let timer_stop = hook_stop.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            timer_stop.cancel();
        });
        let result = workflow::Store::new(cfg.state_dir)
            .record_hook(binding, event, chrono::Utc::now(), &hook_stop)
            .await;
        timer.abort();
        let _ = timer.await;
        result.map_err(|e| format!("hook: {e:?}"))
    }
    .await;
    println!("{{}}");
}
async fn parse_create(
    args: &[String],
    stop: &CancellationToken,
) -> Result<(String, String, PathBuf, bool, bool)> {
    let mut profile = String::new();
    let mut name = String::new();
    let mut name_set = false;
    let mut name_stdin = false;
    let mut dry = false;
    let mut json = false;
    let mut inventory = home()?.join(".config/hmux/inventory.toml");
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                if name_set || name_stdin {
                    return Err("--name and --name-stdin are mutually exclusive".into());
                }
                i += 1;
                name = args.get(i).ok_or("--name requires a value")?.clone();
                name_set = true;
            }
            "--name-stdin" => {
                if name_set || name_stdin {
                    return Err("--name and --name-stdin are mutually exclusive".into());
                }
                name_stdin = true;
            }
            "--inventory" => {
                i += 1;
                inventory = PathBuf::from(args.get(i).ok_or("--inventory requires a value")?);
            }
            "--dry-run" => dry = true,
            "--json" => json = true,
            value => {
                if !profile.is_empty() {
                    return Err("too many create arguments".into());
                }
                profile = value.into();
            }
        }
        i += 1;
    }
    if dry && json {
        return Err("--json cannot be combined with --dry-run".into());
    }
    if name_stdin {
        name = line(512, stop).await?;
    }
    if profile.is_empty() {
        return Err(
            "usage: hmux-agent create <profile-id> [--name name|--name-stdin] [--json]".into(),
        );
    }
    Ok((profile, name, inventory, dry, json))
}
async fn run(args: &[String], stop: &CancellationToken) -> Result<()> {
    let Some((command, rest)) = args.split_first() else {
        return Err(USAGE.into());
    };
    match command.as_str() {
        "version" => {
            println!(
                "hmux-agent {} protocol={PROTOCOL_VERSION}",
                option_env!("HMUX_VERSION").unwrap_or("dev")
            );
            Ok(())
        }
        "setup-home" => {
            let rest = rest.to_vec();
            let stop = stop.clone();
            tokio::task::spawn_blocking(move || {
                if stop.is_cancelled() {
                    return Err("setup cancelled".into());
                }
                setup::run(&rest)
            })
            .await
            .map_err(|_| "setup worker unavailable")?
        }
        "workflow-hook" => {
            hook(stop).await;
            Ok(())
        }
        "catalog" => {
            if !rest.is_empty() {
                return Err("usage: hmux-agent catalog".into());
            }
            pretty(&backend()?.catalog(stop).await?)
        }
        "recovery" => {
            if rest.len() != 1 || !["save", "restore", "sync"].contains(&rest[0].as_str()) {
                return Err("usage: hmux-agent recovery save|restore|sync".into());
            }
            backend()?.recovery(&rest[0], stop).await?;
            println!("Home recovery checkpoint is up to date.");
            Ok(())
        }
        "workspace" => {
            if !rest.is_empty() {
                return Err("usage: hmux-agent workspace (JSON on stdin)".into());
            }
            let raw = read_bytes(32 << 10, stop).await?;
            let change: Option<hmux_model::workspace::Change> =
                serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
            let wrapped = serde_json::to_vec(&serde_json::json!({"change":change}))
                .map_err(|e| e.to_string())?;
            let result = backend()?.workspace(&wrapped, stop).await?;
            json_line(&result)
        }
        "conversation" => {
            if rest.len() != 4 || rest[0] != "--session" || rest[2] != "--created-at" {
                return Err(
                    "usage: hmux-agent conversation --session id --created-at timestamp".into(),
                );
            }
            let created_at = rest[3]
                .parse::<i64>()
                .ok()
                .filter(|v| *v > 0)
                .ok_or("invalid session identity")?;
            hmux_model::validate_session_id(&rest[1]).map_err(|_| "invalid session identity")?;
            let raw = backend()?
                .conversation(
                    SessionIdentity {
                        id: rest[1].clone(),
                        created_at,
                    },
                    stop,
                )
                .await?;
            json_line(&raw)
        }
        "workflow-report" => {
            let mut task = String::new();
            let mut status = String::new();
            let mut i = 0;
            while i < rest.len() {
                let (flag, value, advance) = if let Some((flag, value)) = rest[i].split_once('=') {
                    (flag, value.to_owned(), 1)
                } else {
                    (rest[i].as_str(),rest.get(i+1).ok_or("usage: hmux-agent workflow-report --task-id id --status running|completed|failed|interrupted")?.clone(),2)
                };
                match flag{"--task-id"=>task=value,"--status"=>status=value,_=>return Err("usage: hmux-agent workflow-report --task-id id --status running|completed|failed|interrupted".into())}
                i += advance;
            }
            if task.is_empty() || status.is_empty() {
                return Err("usage: hmux-agent workflow-report --task-id id --status running|completed|failed|interrupted".into());
            }
            let cfg = config::load_home(None, &home()?)?;
            let b = if env::var_os("HMUX_TMUX_SESSION_ID").is_some_and(|v| !v.is_empty()) {
                None
            } else {
                Some(backend()?)
            };
            let binding = workflow_binding(b.as_ref(), stop).await?;
            workflow::Store::new(cfg.state_dir)
                .record_report(
                    binding,
                    workflow::Report {
                        task_id: task,
                        status,
                    },
                    chrono::Utc::now(),
                    stop,
                )
                .await
                .map_err(|e| format!("workflow report: {e:?}"))
        }
        "workflow" => {
            let (filter, json) = workflow_args(rest)?;
            let catalog = backend()?.catalog(stop).await?;
            let views = workflow::views(catalog.sessions.as_deref().unwrap_or_default(), &filter)?;
            if json {
                pretty(
                    &serde_json::json!({"protocol_version":catalog.protocol_version,"generated_at":catalog.generated_at,"sessions":views}),
                )
            } else {
                workflow::write_views(&views, io::stdout()).map_err(|e| e.to_string())
            }
        }
        "create" => {
            let (profile, name, inventory_path, dry, json) = parse_create(rest, stop).await?;
            let inventory = config::load_inventory(&inventory_path)?;
            if dry {
                let h = home()?;
                let path = env::var_os("PATH").unwrap_or_default();
                let shell = env::var_os("SHELL").unwrap_or_default();
                let plan = Plan::prepare(&inventory, &profile, &name, &h, &path, &shell)
                    .map_err(|e| format!("create: {e:?}"))?;
                println!("{}", plan.folder());
                Ok(())
            } else {
                let raw = serde_json::to_vec(&serde_json::json!({"profile":profile,"name":name}))
                    .map_err(|e| e.to_string())?;
                let result = backend()?
                    .with_inventory_path(inventory_path)
                    .action(p::Operation::Create, None, Bytes::from(raw), stop)
                    .await?;
                if json {
                    json_line(&result)
                } else {
                    let v: serde_json::Value =
                        serde_json::from_slice(&result).map_err(|e| e.to_string())?;
                    println!("{}", v["id"].as_str().ok_or("missing created session ID")?);
                    Ok(())
                }
            }
        }
        "alias-set" | "hidden-set" => {
            let id = identity(rest).map_err(|_| {
                format!("usage: hmux-agent {command} --created-at unix-seconds <stable-session-id>")
            })?;
            let value = line(if command == "alias-set" { 512 } else { 16 }, stop).await?;
            let (op, payload) = if command == "alias-set" {
                (p::Operation::Alias, serde_json::json!({"alias":value}))
            } else {
                let hidden = match value.trim() {
                    "true" | "TRUE" | "True" | "1" | "t" | "T" => true,
                    "false" | "FALSE" | "False" | "0" | "f" | "F" => false,
                    _ => return Err("hidden state must be true or false".into()),
                };
                (p::Operation::Hidden, serde_json::json!({"hidden":hidden}))
            };
            backend()?
                .action(
                    op,
                    Some(id),
                    Bytes::from(serde_json::to_vec(&payload).map_err(|e| e.to_string())?),
                    stop,
                )
                .await?;
            Ok(())
        }
        "terminate" => {
            if rest.len() != 4 || rest[0] != "--confirmed" {
                return Err("usage: hmux-agent terminate --confirmed --created-at unix-seconds <session-id>".into());
            }
            let id = identity(&rest[1..])?;
            backend()?.terminate(id, stop).await
        }
        "metadata-migrate" => {
            if rest.len() > 1 || rest.first().is_some_and(|s| s != "--clear") {
                return Err("usage: hmux-agent metadata-migrate [--clear]".into());
            }
            let clear = !rest.is_empty();
            let count = backend()?.migrate_legacy(clear, stop).await?;
            println!("migrated metadata for {count} sessions; cleared={clear}");
            Ok(())
        }
        "doctor" => doctor(stop).await,
        _ => Err(USAGE.into()),
    }
}
async fn doctor(stop: &CancellationToken) -> Result<()> {
    let mut result = serde_json::json!({"protocol_version":PROTOCOL_VERSION,"version":option_env!("HMUX_VERSION").unwrap_or("dev"),"tmux_env":env::var_os("TMUX").is_some_and(|v|!v.is_empty())});
    match backend() {
        Ok(b) => {
            result["tmux_path"] = serde_json::json!(b.tmux_path());
            result["tmux_version"] = serde_json::json!(b.tmux_version(stop).await);
            match b.catalog(stop).await {
                Ok(c) => {
                    result["catalog_ok"] = serde_json::json!(true);
                    result["session_count"] =
                        serde_json::json!(c.sessions.as_ref().map_or(0, Vec::len));
                }
                Err(e) => {
                    result["catalog_ok"] = serde_json::json!(false);
                    result["catalog_error"] = serde_json::json!(e);
                }
            }
        }
        Err(e) => {
            if e.contains("tmux executable unavailable") {
                result["tmux_error"] = serde_json::json!(e);
            }
            result["catalog_ok"] = serde_json::json!(false);
            result["catalog_error"] = serde_json::json!(e);
        }
    }
    pretty(&result)?;
    if result["catalog_ok"] != true {
        return Err("catalog check failed".into());
    }
    Ok(())
}
fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = hmux_core::runtime::run_process(async move {
        // Register before setup, stdin or native owners can begin. Failure to
        // register is a startup error, never silently ignored.
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|e| format!("signal registration failed: {e}"))?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .map_err(|e| format!("signal registration failed: {e}"))?;
        let stop = CancellationToken::new();
        let notify = stop.clone();
        let monitor = tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(30)) => {},
                _ = interrupt.recv() => {},
                _ = term.recv() => {},
            }
            notify.cancel();
        });
        let result = run(&args, &stop).await;
        stop.cancel();
        monitor.abort();
        let _ = monitor.await;
        result
    })
    .unwrap_or_else(|e| Err(format!("runtime unavailable: {e}")));
    if let Err(e) = result {
        eprintln!("hmux-agent: {e}");
        std::process::exit(1)
    }
}
