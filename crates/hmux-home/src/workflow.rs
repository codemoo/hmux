//! Sanitized workflow hooks and Go-compatible private lifecycle storage. No
//! prompt, transcript, tool arguments or output is retained. Callers own binding.
use chrono::{DateTime, SecondsFormat, Utc};
use hmux_core::PrivateDir;
use hmux_model::{
    safe_text, validate_session_id, Catalog, SessionIdentity, Workflow, WorkflowNode,
    WorkflowSummary,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

#[path = "workflow_commands.rs"]
mod commands;
pub use commands::{resolve_binding, views, write_views, BindingEnvironment, SessionView};

const MAX_HOOK: usize = 256 << 10;
const MAX_BYTES: usize = 16 << 20;
const MAX_WORKFLOWS: usize = 1024;
const MAX_NODES: usize = 128;
const STALE: i64 = 2 * 3600;
const RETAIN: i64 = 7 * 86400;
static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Unavailable,
    Busy,
    Cancelled,
}
pub type Binding = SessionIdentity;
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct HookEvent {
    pub session_id: String,
    pub turn_id: String,
    pub hook_event_name: String,
    pub model: String,
    pub agent_id: String,
    pub agent_type: String,
    pub tool_name: String,
}
impl HookEvent {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.is_empty() || raw.len() > MAX_HOOK {
            return Err(Error::Invalid);
        }
        let value: Self = serde_json::from_slice(raw).map_err(|_| Error::Invalid)?;
        value.validate()?;
        Ok(value)
    }
    fn validate(&self) -> Result<(), Error> {
        if !opaque(&self.session_id, 512)
            || (self.hook_event_name != "SessionEnd" && !opaque(&self.turn_id, 512))
            || [
                self.model.len(),
                self.agent_type.len(),
                self.tool_name.len(),
            ]
            .iter()
            .any(|&n| n > 256)
            || !matches!(
                self.hook_event_name.as_str(),
                "UserPromptSubmit"
                    | "SubagentStart"
                    | "SubagentStop"
                    | "PermissionRequest"
                    | "PreToolUse"
                    | "PostToolUse"
                    | "Stop"
                    | "SessionEnd"
            )
            || (matches!(
                self.hook_event_name.as_str(),
                "SubagentStart" | "SubagentStop"
            ) && !opaque(&self.agent_id, 512))
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct Report {
    pub task_id: String,
    pub status: String,
}
#[derive(Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Node {
    id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    parent_id: String,
    #[serde(rename = "type")]
    node_type: String,
    provider: String,
    status: String,
    started_at: i64,
    updated_at: i64,
    #[serde(skip_serializing_if = "zero")]
    ended_at: i64,
}
fn zero(v: &i64) -> bool {
    *v == 0
}
#[derive(Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Item {
    id: String,
    tmux_session_id: String,
    tmux_created_at: i64,
    source: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    session_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    turn_id: String,
    status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    model: String,
    started_at: i64,
    updated_at: i64,
    #[serde(skip_serializing_if = "zero")]
    ended_at: i64,
    nodes: BTreeMap<String, Node>,
}
#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct State {
    version: i32,
    workflows: BTreeMap<String, Item>,
    updated_at: String,
}
fn active(s: &str) -> bool {
    matches!(s, "running" | "waiting_approval" | "waiting_input")
}
fn terminal(s: &str) -> bool {
    matches!(s, "completed" | "failed" | "interrupted" | "stale")
}
fn opaque(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.trim() == s && safe_text(s, max) == s
}
fn hashed(s: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| {
        s.strip_prefix(p).is_some_and(|v| {
            v.len() == 32
                && v.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    })
}
fn hash(prefix: &str, value: &str) -> String {
    let digest = Sha256::digest(format!("{prefix}\0{value}").as_bytes());
    let mut result = prefix.to_owned();
    for b in &digest[..16] {
        use std::fmt::Write;
        write!(result, "{b:02x}").expect("String write");
    }
    result
}
fn binding_valid(b: &Binding) -> bool {
    validate_session_id(&b.id).is_ok() && b.created_at > 0
}
fn status<'a>(nodes: impl Iterator<Item = &'a str>) -> String {
    let values: Vec<_> = nodes.collect();
    for state in [
        "waiting_approval",
        "waiting_input",
        "running",
        "failed",
        "stale",
        "interrupted",
    ] {
        if values.contains(&state) {
            return state.into();
        }
    }
    "completed".into()
}
impl State {
    fn valid(&self) -> bool {
        self.version == 1
            && self.workflows.len() <= MAX_WORKFLOWS
            && self.workflows.iter().all(|(id, item)| {
                id == &item.id
                    && hashed(id, &["wf-", "orch-"])
                    && binding_valid(&Binding {
                        id: item.tmux_session_id.clone(),
                        created_at: item.tmux_created_at,
                    })
                    && match item.source.as_str() {
                        "codex-hook" => {
                            hashed(&item.session_id, &["cx-"]) && hashed(&item.turn_id, &["turn-"])
                        }
                        "codex-orchestra" => item.session_id.is_empty() && item.turn_id.is_empty(),
                        _ => false,
                    }
                    && safe_text(&item.model, 128) == item.model
                    && (active(&item.status) || terminal(&item.status))
                    && item.started_at > 0
                    && item.updated_at >= item.started_at
                    && item.nodes.len() <= MAX_NODES
                    && item.nodes.iter().all(|(id, node)| {
                        id == &node.id
                            && hashed(id, &["root-", "agent-", "task-"])
                            && (node.parent_id.is_empty()
                                || (hashed(&node.parent_id, &["root-", "agent-"])
                                    && item.nodes.contains_key(&node.parent_id)))
                            && (active(&node.status) || terminal(&node.status))
                            && node.started_at > 0
                            && node.updated_at >= node.started_at
                            && safe_text(&node.node_type, 128) == node.node_type
                            && safe_text(&node.provider, 64) == node.provider
                    })
            })
    }
    fn prune(&mut self, now: i64) -> bool {
        let mut changed = false;
        for item in self.workflows.values_mut() {
            if active(&item.status) && item.updated_at < now - STALE {
                for node in item.nodes.values_mut() {
                    if active(&node.status) {
                        node.status = "stale".into();
                        node.ended_at = node.updated_at;
                    }
                }
                item.status = "stale".into();
                item.ended_at = item.updated_at;
                changed = true;
            }
        }
        let len = self.workflows.len();
        self.workflows
            .retain(|_, v| !terminal(&v.status) || v.updated_at >= now - RETAIN);
        changed |= len != self.workflows.len();
        while self.workflows.len() > MAX_WORKFLOWS {
            let id = self
                .workflows
                .values()
                .min_by_key(|v| (v.updated_at, &v.id))
                .expect("nonempty")
                .id
                .clone();
            self.workflows.remove(&id);
            changed = true;
        }
        changed
    }
    fn bytes(&mut self) -> Result<Vec<u8>, Error> {
        loop {
            let mut bytes = serde_json::to_vec(self).map_err(|_| Error::Invalid)?;
            bytes.push(b'\n');
            if bytes.len() <= MAX_BYTES {
                return Ok(bytes);
            }
            let id = self
                .workflows
                .values()
                .min_by_key(|v| (!terminal(&v.status), v.updated_at, &v.id))
                .ok_or(Error::Invalid)?
                .id
                .clone();
            self.workflows.remove(&id);
        }
    }
}
impl Item {
    fn new(id: String, binding: &Binding, source: &str, now: i64) -> Self {
        Self {
            id,
            tmux_session_id: binding.id.clone(),
            tmux_created_at: binding.created_at,
            source: source.into(),
            status: "running".into(),
            started_at: now,
            updated_at: now,
            ..Default::default()
        }
    }
    fn matches(&self, binding: &Binding) -> bool {
        self.tmux_session_id == binding.id && self.tmux_created_at == binding.created_at
    }
    // The lifecycle transition keeps each persisted node field explicit.
    #[allow(clippy::too_many_arguments)]
    fn node(
        &mut self,
        id: String,
        parent: &str,
        kind: &str,
        provider: &str,
        state: &str,
        now: i64,
        force: bool,
    ) -> Result<(), Error> {
        if !self.nodes.contains_key(&id) && self.nodes.len() >= MAX_NODES {
            let evict = self
                .nodes
                .values()
                .filter(|v| {
                    terminal(&v.status)
                        && v.node_type != "root"
                        && !self.nodes.values().any(|n| n.parent_id == v.id)
                })
                .min_by_key(|v| (v.updated_at, &v.id))
                .ok_or(Error::Invalid)?
                .id
                .clone();
            self.nodes.remove(&evict);
        }
        let node = self.nodes.entry(id.clone()).or_insert_with(|| Node {
            id,
            parent_id: parent.into(),
            node_type: kind.into(),
            provider: provider.into(),
            started_at: now,
            ..Default::default()
        });
        if !force && terminal(&node.status) && node.status != "stale" {
            return Ok(());
        }
        node.status = state.into();
        node.updated_at = now;
        node.ended_at = if terminal(state) { now } else { 0 };
        self.updated_at = now;
        Ok(())
    }
    fn refresh(&mut self, now: i64) {
        self.status = status(self.nodes.values().map(|n| n.status.as_str()));
        self.updated_at = now;
        self.ended_at = if terminal(&self.status) { now } else { 0 };
    }
    fn interrupt(&mut self, now: i64) {
        for n in self.nodes.values_mut() {
            if active(&n.status) {
                n.status = "interrupted".into();
                n.updated_at = now;
                n.ended_at = now;
            }
        }
        self.refresh(now);
    }
    fn visible(&self, now: i64) -> Workflow {
        let mut nodes: Vec<_> = self
            .nodes
            .values()
            .map(|n| {
                let stale = active(&n.status) && now - n.updated_at > STALE;
                WorkflowNode {
                    id: n.id.clone(),
                    parent_id: n.parent_id.clone(),
                    node_type: n.node_type.clone(),
                    provider: n.provider.clone(),
                    status: if stale {
                        "stale".into()
                    } else {
                        n.status.clone()
                    },
                    started_at: n.started_at,
                    updated_at: n.updated_at,
                    ended_at: if stale { 0 } else { n.ended_at },
                }
            })
            .collect();
        nodes.sort_by(|a, b| (a.started_at, &a.id).cmp(&(b.started_at, &b.id)));
        let status = if active(&self.status) && now - self.updated_at > STALE {
            "stale".into()
        } else {
            status(nodes.iter().map(|n| n.status.as_str()))
        };
        Workflow {
            id: self.id.clone(),
            source: self.source.clone(),
            session_id: self.session_id.clone(),
            turn_id: self.turn_id.clone(),
            status,
            model: self.model.clone(),
            started_at: self.started_at,
            updated_at: self.updated_at,
            ended_at: self.ended_at,
            nodes: Some(nodes),
        }
    }
}
fn record_hook(
    state: &mut State,
    binding: &Binding,
    event: &HookEvent,
    now: i64,
) -> Result<(), Error> {
    let session_id = hash("cx-", &event.session_id);
    if event.hook_event_name == "SessionEnd" {
        for item in state.workflows.values_mut() {
            if item.matches(binding) && item.session_id == session_id {
                item.interrupt(now);
            }
        }
        return Ok(());
    }
    let id = hash("wf-", &format!("{}\0{}", event.session_id, event.turn_id));
    if event.hook_event_name == "UserPromptSubmit" {
        for item in state.workflows.values_mut() {
            if item.id != id
                && item.source == "codex-hook"
                && item.session_id == session_id
                && item.matches(binding)
                && active(&item.status)
            {
                item.interrupt(now);
            }
        }
    }
    let turn_id = hash("turn-", &event.turn_id);
    let item = state.workflows.entry(id.clone()).or_insert_with(|| Item {
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        ..Item::new(id, binding, "codex-hook", now)
    });
    if !item.matches(binding)
        || item.source != "codex-hook"
        || item.session_id != session_id
        || item.turn_id != turn_id
    {
        return Err(Error::Invalid);
    }
    let model = safe_text(&event.model, 128);
    if !model.is_empty() {
        item.model = model;
    }
    let root = hash("root-", &event.session_id);
    match event.hook_event_name.as_str() {
        "UserPromptSubmit" => item.node(root, "", "root", "codex", "running", now, true)?,
        "SubagentStart" | "SubagentStop" => {
            item.node(root.clone(), "", "root", "codex", "running", now, false)?;
            let kind = safe_text(&event.agent_type, 128);
            item.node(
                hash("agent-", &event.agent_id),
                &root,
                if kind.is_empty() { "subagent" } else { &kind },
                "native",
                if event.hook_event_name == "SubagentStart" {
                    "running"
                } else {
                    "completed"
                },
                now,
                true,
            )?;
        }
        "PermissionRequest" => {
            item.node(root, "", "root", "codex", "waiting_approval", now, false)?
        }
        "PreToolUse" => item.node(
            root,
            "",
            "root",
            "codex",
            if matches!(
                event.tool_name.trim().to_ascii_lowercase().as_str(),
                "request_user_input" | "askuserquestion" | "ask_user_question"
            ) {
                "waiting_input"
            } else {
                "running"
            },
            now,
            false,
        )?,
        "PostToolUse" => item.node(root, "", "root", "codex", "running", now, false)?,
        "Stop" => {
            item.node(root.clone(), "", "root", "codex", "completed", now, true)?;
            for node in item.nodes.values_mut() {
                if node.id != root && active(&node.status) {
                    node.status = "interrupted".into();
                    node.updated_at = now;
                    node.ended_at = now;
                }
            }
        }
        _ => return Err(Error::Invalid),
    }
    item.refresh(now);
    Ok(())
}
fn record_report(
    state: &mut State,
    binding: &Binding,
    report: &Report,
    now: i64,
) -> Result<(), Error> {
    let id = hash("orch-", &format!("{}\0{}", binding.id, binding.created_at));
    let item = state
        .workflows
        .entry(id.clone())
        .or_insert_with(|| Item::new(id, binding, "codex-orchestra", now));
    if !item.matches(binding) || item.source != "codex-orchestra" {
        return Err(Error::Invalid);
    }
    item.node(
        hash("task-", &report.task_id),
        "",
        "task",
        "detached-codex",
        &report.status,
        now,
        true,
    )?;
    item.refresh(now);
    Ok(())
}
#[derive(Clone)]
pub struct Store {
    state_dir: PathBuf,
}
impl Store {
    pub fn new(state_dir: PathBuf) -> Self {
        Self { state_dir }
    }
    fn directory(&self, create: bool) -> Result<Option<PrivateDir>, Error> {
        let root = if create {
            PrivateDir::open_or_create_trusted(&self.state_dir)
        } else {
            PrivateDir::open(&self.state_dir)
        };
        let root = match root {
            Ok(root) => root,
            Err(e) if !create && e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Error::Unavailable),
        };
        if create {
            return root
                .create_private_child(OsStr::new("workflows"))
                .map(Some)
                .map_err(|_| Error::Unavailable);
        }
        match PrivateDir::open(&self.state_dir.join("workflows")) {
            Ok(dir) => Ok(Some(dir)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(Error::Unavailable),
        }
    }
    fn read(dir: &PrivateDir) -> Result<Option<State>, Error> {
        let raw = match dir.read_private(OsStr::new("state.json"), MAX_BYTES) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Error::Unavailable),
        };
        let state: State = serde_json::from_slice(&raw).map_err(|_| Error::Invalid)?;
        if !state.valid() {
            return Err(Error::Invalid);
        }
        Ok(Some(state))
    }
    fn lock(dir: &PrivateDir, cancel: &CancellationToken) -> Result<hmux_core::FileLock, Error> {
        let deadline = Instant::now() + Duration::from_millis(750);
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if let Some(lock) = dir
                .try_lock(OsStr::new("state.lock"))
                .map_err(|_| Error::Unavailable)?
            {
                return Ok(lock);
            }
            if Instant::now() >= deadline {
                return Err(Error::Busy);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    async fn update(
        &self,
        now: DateTime<Utc>,
        cancel: &CancellationToken,
        action: impl FnOnce(&mut State, i64) -> Result<(), Error> + Send + 'static,
    ) -> Result<(), Error> {
        if now.timestamp() < 1 {
            return Err(Error::Invalid);
        }
        let permit = WORKERS
            .get_or_init(|| Arc::new(Semaphore::new(2)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let store = self.clone();
        let stop = cancel.child_token();
        let _guard = stop.clone().drop_guard();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if stop.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let dir = store.directory(true)?.ok_or(Error::Unavailable)?;
            let _lock = Self::lock(&dir, &stop)?;
            let mut state = Self::read(&dir)?.unwrap_or(State {
                version: 1,
                ..Default::default()
            });
            action(&mut state, now.timestamp())?;
            state.prune(now.timestamp());
            state.updated_at = now.to_rfc3339_opts(SecondsFormat::AutoSi, true);
            let bytes = state.bytes()?;
            if !state.valid() {
                return Err(Error::Invalid);
            }
            if stop.is_cancelled() {
                return Err(Error::Cancelled);
            }
            dir.write_atomic_private(OsStr::new("state.json"), &bytes)
                .map_err(|_| Error::Unavailable)
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    pub async fn record_hook(
        &self,
        binding: Binding,
        event: HookEvent,
        now: DateTime<Utc>,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        if !binding_valid(&binding) {
            return Err(Error::Invalid);
        }
        event.validate()?;
        self.update(now, cancel, move |s, t| record_hook(s, &binding, &event, t))
            .await
    }
    pub async fn record_report(
        &self,
        binding: Binding,
        report: Report,
        now: DateTime<Utc>,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        if !binding_valid(&binding)
            || !opaque(&report.task_id, 256)
            || !matches!(
                report.status.as_str(),
                "running" | "completed" | "failed" | "interrupted"
            )
        {
            return Err(Error::Invalid);
        }
        self.update(now, cancel, move |s, t| {
            record_report(s, &binding, &report, t)
        })
        .await
    }
    /// Synchronous catalog enrichment; invoke under the existing catalog I/O slot.
    pub fn apply(
        &self,
        catalog: &mut Catalog,
        now: DateTime<Utc>,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        let Some(dir) = self.directory(false)? else {
            return Ok(());
        };
        let Some(mut state) = Self::read(&dir)? else {
            return Ok(());
        };
        if state.prune(now.timestamp()) {
            let _lock = Self::lock(&dir, cancel)?;
            state = Self::read(&dir)?.ok_or(Error::Unavailable)?;
            if state.prune(now.timestamp()) {
                state.updated_at = now.to_rfc3339_opts(SecondsFormat::AutoSi, true);
                let bytes = state.bytes()?;
                if !state.valid() {
                    return Err(Error::Invalid);
                }
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                dir.write_atomic_private(OsStr::new("state.json"), &bytes)
                    .map_err(|_| Error::Unavailable)?;
            }
        }
        for session in catalog.sessions.iter_mut().flatten() {
            let mut items: Vec<_> = state
                .workflows
                .values()
                .filter(|v| {
                    v.tmux_session_id == session.id && v.tmux_created_at == session.created_at
                })
                .map(|v| v.visible(now.timestamp()))
                .collect();
            items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
            items.truncate(32);
            if items.is_empty() {
                continue;
            }
            let has_active = items.iter().any(|w| active(&w.status));
            let mut summary = WorkflowSummary::default();
            for item in items
                .iter()
                .filter(|w| !has_active || active(&w.status))
                .take(if has_active { 32 } else { 1 })
            {
                summary.updated_at = summary.updated_at.max(item.updated_at);
                for n in item.nodes.iter().flatten() {
                    match n.status.as_str() {
                        "running" => summary.running += 1,
                        "waiting_approval" => summary.waiting_approval += 1,
                        "waiting_input" => summary.waiting_input += 1,
                        "completed" => summary.completed += 1,
                        "failed" => summary.failed += 1,
                        "interrupted" => summary.interrupted += 1,
                        "stale" => summary.stale += 1,
                        _ => {}
                    }
                }
            }
            session.workflow = Some(summary);
            session.workflows = Some(items);
        }
        Ok(())
    }
}
#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;
