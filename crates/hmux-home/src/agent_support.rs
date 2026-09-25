//! Native administrative facade. The CLI uses the same bounded Home owners as
//! the connector, with one captured tmux executable and exact session lifetimes.
use crate::{
    catalog::TmuxCatalogReader,
    config::HomeConfig,
    conversation,
    inspection::{self, Inspector},
    recovery, recovery_binding, sessions, view, workspace,
};
use bytes::Bytes;
use hmux_core::command::{CommandRunner, CommandSpec, RunOutput};
use hmux_model::{safe_text, Catalog, Session, SessionIdentity};
use hmux_protocol::{actions, protobuf::types as p};
use std::{
    ffi::{OsStr, OsString},
    fs,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct AgentSupport {
    pub config: HomeConfig,
    reader: TmuxCatalogReader,
    runner: CommandRunner,
    target: view::Target,
    context: Arc<sessions::Context>,
    inspector: Option<Arc<Inspector>>,
    recovery: recovery::Store,
}

impl AgentSupport {
    /// Use the administrator-selected, previously validated CLI inventory.
    pub fn with_inventory_path(mut self, path: PathBuf) -> Self {
        self.config.inventory_path = path;
        self
    }
    pub fn new(
        config: HomeConfig,
        home: &Path,
        path: &OsStr,
        shell: OsString,
    ) -> Result<Self, String> {
        let tmux = find_tool(
            path,
            "tmux",
            &[
                "/opt/homebrew/bin/tmux",
                "/usr/local/bin/tmux",
                "/usr/bin/tmux",
            ],
        )
        .ok_or("tmux executable unavailable")?;
        let reader = TmuxCatalogReader::new(tmux.clone(), None, Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        let runner = CommandRunner::new(2).map_err(|e| e.to_string())?;
        let target =
            view::Target::new(tmux.clone(), None).map_err(|e| format!("tmux target: {e:?}"))?;
        let context = Arc::new(
            sessions::Context::new(home.to_path_buf(), path.to_os_string(), shell)
                .map_err(|e| format!("Home context: {e:?}"))?,
        );
        let inspector = find_tool(path, "ps", &["/bin/ps", "/usr/bin/ps"])
            .and_then(|ps| {
                Inspector::new(
                    home.to_path_buf(),
                    ps,
                    find_tool(path, "lsof", &["/usr/sbin/lsof", "/usr/bin/lsof"]),
                )
                .ok()
            })
            .map(Arc::new);
        let resolver = if let Some(ref inspector) = inspector {
            recovery_binding::resolver(inspector.clone())
        } else {
            Arc::new(
                |panes: Vec<i32>| -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<_, recovery::Error>> + Send>,
                > {
                    Box::pin(async move {
                        if panes.is_empty() {
                            Ok(Default::default())
                        } else {
                            Err(recovery::Error::Unavailable)
                        }
                    })
                },
            )
        };
        let recovery = recovery::Store::new(
            config.state_dir.clone(),
            tmux,
            None,
            runner.clone(),
            resolver,
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            config,
            reader,
            runner,
            target,
            context,
            inspector,
            recovery,
        })
    }

    pub async fn catalog(&self, stop: &CancellationToken) -> Result<Catalog, String> {
        let mut catalog = self
            .reader
            .read_basic_cancelable(
                &self.runner,
                stop,
                tokio::time::Instant::now() + Duration::from_secs(30),
            )
            .await
            .map_err(|e| e.to_string())?;
        if let Some(inspector) = &self.inspector {
            catalog = inspector
                .clone()
                .annotate(catalog, stop)
                .await
                .map_err(|e| format!("catalog inspection: {e:?}"))?;
        }
        sessions::overlay(
            catalog,
            self.config.state_dir.clone(),
            Some(self.recovery.clone()),
            stop,
        )
        .await
        .map_err(|e| format!("catalog overlay: {e:?}"))
    }

    pub async fn recovery(&self, mode: &str, stop: &CancellationToken) -> Result<(), String> {
        match mode {
            "save" => self.recovery.save_cancelable(stop.clone()).await,
            "restore" => self.recovery.restore_cancelable(stop.clone()).await,
            "sync" => self.recovery.sync_cancelable(stop.clone()).await,
            _ => return Err("invalid recovery operation".into()),
        }
        .map_err(|e| e.to_string())
    }

    pub async fn workspace(&self, raw: &[u8], stop: &CancellationToken) -> Result<Bytes, String> {
        let workspace = workspace::Workspace::open(&self.config.state_dir)
            .map_err(|e| format!("workspace: {e:?}"))?
            .with_recovery(self.recovery.clone());
        let value = workspace
            .request(raw, self.reader.clone(), self.runner.clone(), stop)
            .await
            .map_err(|e| format!("workspace: {e:?}"));
        workspace.shutdown().await;
        value
    }

    pub async fn conversation(
        &self,
        identity: SessionIdentity,
        stop: &CancellationToken,
    ) -> Result<Bytes, String> {
        let inspector = self
            .inspector
            .clone()
            .ok_or("conversation unavailable for this session")?;
        let permit = inspection::admit().ok_or("conversation unavailable for this session")?;
        conversation::Job {
            inspector,
            reader: self.reader.clone(),
            identity,
            stop: stop.clone(),
        }
        .run(permit)
        .await
        .and_then(|value| conversation::json(&value))
        .map_err(|_| "conversation unavailable for this session".into())
    }

    pub async fn action(
        &self,
        operation: p::Operation,
        identity: Option<SessionIdentity>,
        payload: Bytes,
        stop: &CancellationToken,
    ) -> Result<Bytes, String> {
        let permit = sessions::admit().ok_or("Home is busy")?;
        let body = actions::request_from_json(operation, &payload)
            .map_err(|e| format!("Home action: {e:?}"))?;
        let request = p::Request {
            id: "agent-cli".into(),
            operation: operation as i32,
            session: identity.map(|v| p::Session {
                id: v.id,
                created_at: v.created_at,
            }),
            payload: Some(body),
        };
        sessions::Job {
            context: self.context.clone(),
            config: self.config.clone(),
            target: self.target.clone(),
            catalog: self.reader.clone(),
            request,
            stop: stop.clone(),
        }
        .run(permit)
        .await
        .map_err(|e| format!("Home action: {e:?}"))
        .and_then(|result| {
            actions::response_payload(&p::Response {
                id: "agent-cli".into(),
                error: String::new(),
                result: Some(result),
            })
            .map_err(|e| format!("Home action: {e:?}"))
        })
    }

    pub async fn terminate(
        &self,
        identity: SessionIdentity,
        stop: &CancellationToken,
    ) -> Result<(), String> {
        hmux_model::validate_session_id(&identity.id).map_err(|e| e.to_string())?;
        if identity.created_at < 1 {
            return Err("invalid session creation time".into());
        }
        let condition = format!("#{{==:#{{session_created}},{}}}", identity.created_at);
        let output = self
            .target
            .command(
                &self.runner,
                vec![
                    "if-shell".into(),
                    "-F".into(),
                    "-t".into(),
                    identity.id.clone(),
                    condition,
                    format!("kill-session -t {}", identity.id),
                    "display-message -p hmux-session-changed".into(),
                ],
                stop,
                tokio::time::Instant::now() + Duration::from_secs(15),
            )
            .await
            .map_err(|e| format!("tmux terminate expected session: {e:?}"))?;
        if !String::from_utf8_lossy(&output).trim().is_empty() {
            return Err("session identity changed".into());
        }
        Ok(())
    }

    pub fn reader(&self) -> &TmuxCatalogReader {
        &self.reader
    }
    pub fn runner(&self) -> &CommandRunner {
        &self.runner
    }

    pub async fn tmux_version(&self, stop: &CancellationToken) -> String {
        let spec =
            hmux_core::command::CommandSpec::new(self.tmux_path(), 4096, Duration::from_secs(5))
                .arg("-V");
        self.run_command(spec, stop)
            .await
            .ok()
            .map(|v| String::from_utf8_lossy(&v.stdout).trim().to_owned())
            .unwrap_or_default()
    }
    pub fn tmux_path(&self) -> PathBuf {
        self.target.executable_path().to_path_buf()
    }

    /// Import Go's seven-field legacy session options before optionally clearing
    /// them. Every clear uses a fixed option name and validated stable ID.
    pub async fn migrate_legacy(
        &self,
        clear: bool,
        stop: &CancellationToken,
    ) -> Result<usize, String> {
        const SEP: &str = "|:hmux-sep-v1:|";
        let format = [
            "#{session_id}",
            "#{session_name}",
            "#{session_created}",
            "#{@hmux_profile}",
            "#{@hmux_tags}",
            "#{@hmux_label}",
            "#{@hmux_alias}",
        ]
        .join(SEP);
        let raw = self
            .run_command(
                self.reader
                    .command(16 << 20, ["list-sessions", "-F", &format]),
                stop,
            )
            .await
            .map_err(|e| format!("tmux legacy metadata: {e}"))?
            .stdout;
        let text = std::str::from_utf8(&raw).map_err(|_| "malformed legacy tmux metadata")?;
        let mut sessions = Vec::new();
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            let fields: Vec<_> = line.split(SEP).collect();
            if fields.len() != 7 {
                return Err("malformed legacy tmux metadata row".into());
            }
            hmux_model::validate_session_id(fields[0]).map_err(|e| e.to_string())?;
            let created_at = fields[2]
                .parse::<i64>()
                .ok()
                .filter(|v| *v > 0)
                .ok_or("invalid legacy session creation time")?;
            sessions.push(Session {
                id: fields[0].into(),
                name: safe_text(fields[1], 512),
                created_at,
                profile: safe_text(fields[3], 128),
                tags: Some(
                    fields[4]
                        .split(',')
                        .map(|s| safe_text(s, 64))
                        .filter(|s| !s.is_empty())
                        .collect(),
                ),
                label: safe_text(fields[5], 256),
                alias: safe_text(fields[6], crate::sessionstate::MAX_ALIAS_BYTES),
                ..Session::default()
            });
        }
        let state_dir = self.config.state_dir.clone();
        let imported = sessions.clone();
        let cancel = stop.clone();
        tokio::task::spawn_blocking(move || {
            crate::sessionstate::Store::new(state_dir).import_legacy(
                &imported,
                cancel,
                std::time::Instant::now() + Duration::from_secs(30),
            )
        })
        .await
        .map_err(|_| "metadata import worker unavailable")?
        .map_err(|e| e.to_string())?;
        if clear {
            for session in &sessions {
                for option in ["@hmux_profile", "@hmux_tags", "@hmux_label", "@hmux_alias"] {
                    self.run_command(
                        self.reader
                            .command(4096, ["set-option", "-u", "-t", &session.id, option]),
                        stop,
                    )
                    .await
                    .map_err(|e| format!("clear legacy tmux metadata: {e}"))?;
                }
            }
        }
        Ok(sessions.len())
    }
    async fn run_command(
        &self,
        spec: CommandSpec,
        stop: &CancellationToken,
    ) -> Result<RunOutput, String> {
        let (cancel, receiver) = tokio::sync::oneshot::channel();
        let work = self.runner.run_cancelable(spec, receiver);
        tokio::pin!(work);
        tokio::select! {
            _=stop.cancelled()=>{drop(cancel);let _=work.await;Err("command cancelled".into())},
            result=&mut work=>result.map_err(|e|e.to_string()),
        }
    }
}

fn find_tool(path: &OsStr, name: &str, fallbacks: &[&str]) -> Option<PathBuf> {
    if path.as_bytes().len() > 64 << 10 || path.as_bytes().contains(&0) {
        return None;
    }
    std::env::split_paths(path)
        .take(256)
        .filter(|p| p.is_absolute())
        .map(|p| p.join(name))
        .chain(fallbacks.iter().map(PathBuf::from))
        .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}
