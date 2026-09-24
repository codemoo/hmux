//! Typed catalog and usage conversion. Move owned fields; bound trees before conversion.
use crate::protobuf::types as p;
use hmux_model as m;
use hmux_usage as u;
use prost::Message;
use std::collections::BTreeMap;
#[path = "snapshot_json.rs"]
mod snapshot_json;
pub use snapshot_json::catalog_from_json;

pub const MAX_CATALOG_SESSIONS: usize = 10_000;
pub const MAX_CATALOG_WINDOWS: usize = 100_000;
pub const MAX_CATALOG_TAGS: usize = 64;
pub const MAX_WORKFLOWS: usize = 1024;
pub const MAX_WORKFLOW_NODES: usize = 128;
// A decoded tree can retain Vec capacity as well as each element; charge twice
// the element size plus owned UTF-8 bytes, within the web frame allocation budget.
pub const MAX_DECODED_SNAPSHOT_BYTES: usize = 16 << 20;
pub const MAX_CATALOG_TEXT: usize = 4096;
pub const MAX_CATALOG_BYTES: usize = 4 << 20;
pub const MAX_USAGE_BYTES: usize = u::model::MAX_SNAPSHOT_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Limit,
    Invalid,
}
fn text(s: &str, max: usize) -> Result<(), Error> {
    if s.len() > max {
        Err(Error::Limit)
    } else {
        Ok(())
    }
}
fn strings(xs: &[String], max: usize) -> Result<(), Error> {
    if xs.len() > max {
        return Err(Error::Limit);
    }
    for x in xs {
        text(x, MAX_CATALOG_TEXT)?;
    }
    Ok(())
}
fn opt_strings(xs: &Option<Vec<String>>, max: usize) -> Result<(), Error> {
    if let Some(xs) = xs {
        strings(xs, max)?;
    }
    Ok(())
}
fn identity(id: &str, created_at: i64) -> Result<(), Error> {
    // serde accepts a null session or empty identity as the Go zero value.
    if id.is_empty() && created_at == 0 {
        return Ok(());
    }
    m::validate_session_id(id).map_err(|_| Error::Invalid)?;
    if created_at <= 0 {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn check_node(n: &m::WorkflowNode) -> Result<(), Error> {
    for s in [&n.id, &n.parent_id, &n.node_type, &n.provider, &n.status] {
        text(s, MAX_CATALOG_TEXT)?;
    }
    Ok(())
}
fn check_workflow(w: &m::Workflow) -> Result<(), Error> {
    for s in [
        &w.id,
        &w.source,
        &w.session_id,
        &w.turn_id,
        &w.status,
        &w.model,
    ] {
        text(s, MAX_CATALOG_TEXT)?;
    }
    if let Some(ns) = &w.nodes {
        if ns.len() > MAX_WORKFLOW_NODES {
            return Err(Error::Limit);
        }
        for n in ns {
            check_node(n)?;
        }
    }
    Ok(())
}
fn check_session(s: &m::Session) -> Result<(), Error> {
    identity(&s.id, s.created_at)?;
    if let Some(r) = &s.restored_from {
        identity(&r.id, r.created_at)?;
    }
    for x in [
        &s.name,
        &s.alias,
        &s.active_window,
        &s.current_path,
        &s.current_command,
        &s.profile,
        &s.label,
        &s.kind,
        &s.runtime,
        &s.model,
        &s.state,
        &s.process,
    ] {
        text(x, MAX_CATALOG_TEXT)?;
    }
    opt_strings(&s.window_names, MAX_CATALOG_WINDOWS)?;
    opt_strings(&s.tags, MAX_CATALOG_TAGS)?;
    if let Some(w) = &s.workflows {
        if w.len() > MAX_WORKFLOWS {
            return Err(Error::Limit);
        }
        for w in w {
            check_workflow(w)?;
        }
    }
    Ok(())
}
fn check_catalog(c: &m::Catalog) -> Result<(), Error> {
    text(&c.generated_at, 128)?;
    if !c.generated_at.is_empty() && u::model::parse_time(&c.generated_at).is_none() {
        return Err(Error::Invalid);
    }
    if let Some(ss) = &c.sessions {
        if ss.len() > MAX_CATALOG_SESSIONS {
            return Err(Error::Limit);
        }
        for s in ss {
            check_session(s)?;
        }
    }
    Ok(())
}
fn check_proto_catalog(c: &p::CatalogSnapshot) -> Result<(), Error> {
    text(&c.generated_at, 128)?;
    if !c.generated_at.is_empty() && u::model::parse_time(&c.generated_at).is_none() {
        return Err(Error::Invalid);
    }
    let Some(sessions) = &c.sessions else {
        return Ok(());
    };
    if sessions.items.len() > MAX_CATALOG_SESSIONS {
        return Err(Error::Limit);
    }
    for s in &sessions.items {
        let identity = s.identity.as_ref().ok_or(Error::Invalid)?;
        identity_check_proto(identity)?;
        if let Some(restored) = &s.restored_from {
            identity_check_proto(restored)?;
        }
        for x in [
            &s.name,
            &s.alias,
            &s.active_window,
            &s.current_path,
            &s.current_command,
            &s.profile,
            &s.label,
            &s.kind,
            &s.runtime,
            &s.model,
            &s.state,
            &s.process,
        ] {
            text(x, MAX_CATALOG_TEXT)?;
        }
        if let Some(xs) = &s.window_names {
            strings(&xs.items, MAX_CATALOG_WINDOWS)?;
        }
        if let Some(xs) = &s.tags {
            strings(&xs.items, MAX_CATALOG_TAGS)?;
        }
        if let Some(ws) = &s.workflows {
            if ws.items.len() > MAX_WORKFLOWS {
                return Err(Error::Limit);
            }
            for w in &ws.items {
                for x in [
                    &w.id,
                    &w.source,
                    &w.session_id,
                    &w.turn_id,
                    &w.status,
                    &w.model,
                ] {
                    text(x, MAX_CATALOG_TEXT)?;
                }
                if let Some(ns) = &w.nodes {
                    if ns.items.len() > MAX_WORKFLOW_NODES {
                        return Err(Error::Limit);
                    }
                    for n in &ns.items {
                        for x in [&n.id, &n.parent_id, &n.node_type, &n.provider, &n.status] {
                            text(x, MAX_CATALOG_TEXT)?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
fn identity_check_proto(v: &p::Session) -> Result<(), Error> {
    identity(&v.id, v.created_at)
}
fn to_identity(id: m::SessionIdentity) -> p::Session {
    p::Session {
        id: id.id,
        created_at: id.created_at,
    }
}
fn from_identity(id: p::Session) -> m::SessionIdentity {
    m::SessionIdentity {
        id: id.id,
        created_at: id.created_at,
    }
}
fn to_strings(xs: Option<Vec<String>>) -> Option<p::CatalogStrings> {
    xs.map(|items| p::CatalogStrings { items })
}
fn from_strings(xs: Option<p::CatalogStrings>) -> Option<Vec<String>> {
    xs.map(|v| v.items)
}
fn to_summary(w: m::WorkflowSummary) -> p::CatalogWorkflowSummary {
    p::CatalogWorkflowSummary {
        running: w.running,
        waiting_approval: w.waiting_approval,
        waiting_input: w.waiting_input,
        completed: w.completed,
        failed: w.failed,
        interrupted: w.interrupted,
        stale: w.stale,
        updated_at: w.updated_at,
    }
}
fn from_summary(w: p::CatalogWorkflowSummary) -> m::WorkflowSummary {
    m::WorkflowSummary {
        running: w.running,
        waiting_approval: w.waiting_approval,
        waiting_input: w.waiting_input,
        completed: w.completed,
        failed: w.failed,
        interrupted: w.interrupted,
        stale: w.stale,
        updated_at: w.updated_at,
    }
}
fn to_node(n: m::WorkflowNode) -> p::CatalogWorkflowNode {
    p::CatalogWorkflowNode {
        id: n.id,
        parent_id: n.parent_id,
        node_type: n.node_type,
        provider: n.provider,
        status: n.status,
        started_at: n.started_at,
        updated_at: n.updated_at,
        ended_at: n.ended_at,
    }
}
fn from_node(n: p::CatalogWorkflowNode) -> m::WorkflowNode {
    m::WorkflowNode {
        id: n.id,
        parent_id: n.parent_id,
        node_type: n.node_type,
        provider: n.provider,
        status: n.status,
        started_at: n.started_at,
        updated_at: n.updated_at,
        ended_at: n.ended_at,
    }
}
fn to_workflow(w: m::Workflow) -> p::CatalogWorkflow {
    p::CatalogWorkflow {
        id: w.id,
        source: w.source,
        session_id: w.session_id,
        turn_id: w.turn_id,
        status: w.status,
        model: w.model,
        started_at: w.started_at,
        updated_at: w.updated_at,
        ended_at: w.ended_at,
        nodes: w.nodes.map(|items| p::CatalogWorkflowNodes {
            items: items.into_iter().map(to_node).collect(),
        }),
    }
}
fn from_workflow(w: p::CatalogWorkflow) -> m::Workflow {
    m::Workflow {
        id: w.id,
        source: w.source,
        session_id: w.session_id,
        turn_id: w.turn_id,
        status: w.status,
        model: w.model,
        started_at: w.started_at,
        updated_at: w.updated_at,
        ended_at: w.ended_at,
        nodes: w
            .nodes
            .map(|xs| xs.items.into_iter().map(from_node).collect()),
    }
}
fn to_session(s: m::Session) -> p::CatalogSession {
    p::CatalogSession {
        identity: Some(p::Session {
            id: s.id,
            created_at: s.created_at,
        }),
        name: s.name,
        alias: s.alias,
        hidden: s.hidden,
        restored_from: s.restored_from.map(to_identity),
        activity_at: s.activity_at,
        attached_clients: s.attached,
        window_count: s.window_count,
        window_names: to_strings(s.window_names),
        active_window: s.active_window,
        current_path: s.current_path,
        current_command: s.current_command,
        profile: s.profile,
        label: s.label,
        tags: to_strings(s.tags),
        kind: s.kind,
        runtime: s.runtime,
        model: s.model,
        state: s.state,
        process: s.process,
        working_since: s.working_since,
        width: s.width,
        height: s.height,
        workflow: s.workflow.map(to_summary),
        workflows: s.workflows.map(|items| p::CatalogWorkflows {
            items: items.into_iter().map(to_workflow).collect(),
        }),
    }
}
fn from_session(s: p::CatalogSession) -> Result<m::Session, Error> {
    let id = s.identity.ok_or(Error::Invalid)?;
    Ok(m::Session {
        id: id.id,
        created_at: id.created_at,
        name: s.name,
        alias: s.alias,
        hidden: s.hidden,
        restored_from: s.restored_from.map(from_identity),
        activity_at: s.activity_at,
        attached: s.attached_clients,
        window_count: s.window_count,
        window_names: from_strings(s.window_names),
        active_window: s.active_window,
        current_path: s.current_path,
        current_command: s.current_command,
        profile: s.profile,
        label: s.label,
        tags: from_strings(s.tags),
        kind: s.kind,
        runtime: s.runtime,
        model: s.model,
        state: s.state,
        process: s.process,
        working_since: s.working_since,
        width: s.width,
        height: s.height,
        workflow: s.workflow.map(from_summary),
        workflows: s
            .workflows
            .map(|xs| xs.items.into_iter().map(from_workflow).collect()),
        pane_pid: 0,
    })
}
fn to_metrics(x: m::HostMetrics) -> p::CatalogHostMetrics {
    p::CatalogHostMetrics {
        observed_at: x.observed_at,
        cpu_percent: x.cpu_percent,
        gpu_percent: x.gpu_percent,
        memory_used_bytes: x.memory_used_bytes,
        memory_total_bytes: x.memory_total_bytes,
        disk_used_bytes: x.disk_used_bytes,
        disk_total_bytes: x.disk_total_bytes,
    }
}
fn from_metrics(x: p::CatalogHostMetrics) -> m::HostMetrics {
    m::HostMetrics {
        observed_at: x.observed_at,
        cpu_percent: x.cpu_percent,
        gpu_percent: x.gpu_percent,
        memory_used_bytes: x.memory_used_bytes,
        memory_total_bytes: x.memory_total_bytes,
        disk_used_bytes: x.disk_used_bytes,
        disk_total_bytes: x.disk_total_bytes,
    }
}
fn normalized_metrics(x: Option<m::HostMetrics>) -> Option<m::HostMetrics> {
    x.map(|v| {
        if v.validate().is_ok() {
            v
        } else {
            m::HostMetrics::default()
        }
    })
}
pub fn catalog_to_proto(mut c: m::Catalog) -> Result<p::CatalogSnapshot, Error> {
    c.host_metrics = normalized_metrics(c.host_metrics);
    check_catalog(&c)?;
    budget_model_catalog(&c)?;
    let out = p::CatalogSnapshot {
        protocol_version: c.protocol_version,
        generated_at: c.generated_at,
        sessions: c.sessions.map(|items| p::CatalogSessions {
            items: items.into_iter().map(to_session).collect(),
        }),
        host_metrics: c.host_metrics.map(to_metrics),
    };
    validate_catalog(&out)?;
    Ok(out)
}
pub fn catalog_from_proto(c: p::CatalogSnapshot) -> Result<m::Catalog, Error> {
    validate_catalog(&c)?;
    let out = m::Catalog {
        protocol_version: c.protocol_version,
        generated_at: if c.generated_at.is_empty() {
            m::Catalog::default().generated_at
        } else {
            c.generated_at
        },
        sessions: c
            .sessions
            .map(|xs| {
                xs.items
                    .into_iter()
                    .map(from_session)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?,
        host_metrics: normalized_metrics(c.host_metrics.map(from_metrics)),
    };
    check_catalog(&out)?;
    budget_model_catalog(&out)?;
    Ok(out)
}

fn usage_provider_to(pv: u::Provider) -> i32 {
    match pv {
        u::Provider::Claude => p::Provider::Claude as i32,
        u::Provider::Codex => p::Provider::Codex as i32,
    }
}
fn usage_provider_from(pv: i32) -> Result<u::Provider, Error> {
    match p::Provider::try_from(pv).map_err(|_| Error::Invalid)? {
        p::Provider::Claude => Ok(u::Provider::Claude),
        p::Provider::Codex => Ok(u::Provider::Codex),
        _ => Err(Error::Invalid),
    }
}
fn to_window(w: u::Window) -> p::UsageWindow {
    p::UsageWindow {
        used_pct: w.used_pct,
        remaining_seconds: w.remaining_seconds,
        resets_at: w.resets_at,
    }
}
fn from_window(w: p::UsageWindow) -> u::Window {
    u::Window {
        used_pct: w.used_pct,
        remaining_seconds: w.remaining_seconds,
        resets_at: w.resets_at,
    }
}
fn to_account_window(w: u::AccountWindow) -> p::UsageAccountWindow {
    p::UsageAccountWindow {
        used_pct: w.used_pct,
        resets_at: w.resets_at,
    }
}
fn from_account_window(w: p::UsageAccountWindow) -> u::AccountWindow {
    u::AccountWindow {
        used_pct: w.used_pct,
        resets_at: w.resets_at,
    }
}
fn to_status(s: u::Status) -> p::UsageStatus {
    p::UsageStatus {
        state: s.state,
        data_source: s.data_source,
        quota_source: s.quota_source,
        stale: s.stale,
        quota_observed_at: s.quota_observed_at,
        retry_at: s.retry_at,
    }
}
fn from_status(s: p::UsageStatus) -> u::Status {
    u::Status {
        state: s.state,
        data_source: s.data_source,
        quota_source: s.quota_source,
        stale: s.stale,
        quota_observed_at: s.quota_observed_at,
        retry_at: s.retry_at,
    }
}
fn to_account(a: u::Account) -> p::UsageAccount {
    p::UsageAccount {
        number: a.number,
        email: a.email,
        display_name: a.display_name,
        active: a.active,
        status: a.status,
        five_hour: a.five_hour.map(to_account_window),
        seven_day: a.seven_day.map(to_account_window),
        tokens_per_hour: a.tokens_per_hour,
        total_tokens: a.total_tokens,
        last_refresh_at: a.last_refresh_at,
        plan_type: a.plan_type,
    }
}
fn from_account(a: p::UsageAccount) -> u::Account {
    u::Account {
        number: a.number,
        email: a.email,
        display_name: a.display_name,
        active: a.active,
        status: a.status,
        five_hour: a.five_hour.map(from_account_window),
        seven_day: a.seven_day.map(from_account_window),
        tokens_per_hour: a.tokens_per_hour,
        total_tokens: a.total_tokens,
        last_refresh_at: a.last_refresh_at,
        plan_type: a.plan_type,
    }
}
fn to_usage(s: u::Snapshot) -> p::UsageSnapshot {
    p::UsageSnapshot {
        schema: s.schema,
        seq: s.seq,
        generated_at_utc: s.generated_at_utc,
        provider: usage_provider_to(s.provider),
        plan_type: s.plan_type,
        burn_rate_per_min: s.burn_rate_per_min,
        burn_state: s.burn_state,
        today_total_tokens: s.today_total_tokens,
        today_sessions: s.today_sessions,
        rolling_5h: Some(to_window(s.rolling_5h)),
        weekly: Some(to_window(s.weekly)),
        rolling_5h_observed: s.rolling_5h_observed,
        weekly_observed: s.weekly_observed,
        status: Some(to_status(s.status)),
        accounts: s.accounts.into_iter().map(to_account).collect(),
        accounts_updated_at: s.accounts_updated_at,
        sources: s
            .sources
            .into_iter()
            .map(|(name, snapshot)| p::UsageSource {
                name,
                snapshot: Some(to_usage(snapshot)),
            })
            .collect(),
    }
}
fn from_usage(s: p::UsageSnapshot, child: bool) -> Result<u::Snapshot, Error> {
    if child && !s.sources.is_empty() {
        return Err(Error::Invalid);
    }
    if s.sources.len() > 2 || s.accounts.len() > u::model::MAX_ACCOUNTS {
        return Err(Error::Limit);
    }
    let mut sources = BTreeMap::new();
    for src in s.sources {
        text(&src.name, 16)?;
        let snapshot = from_usage(src.snapshot.ok_or(Error::Invalid)?, true)?;
        if sources.insert(src.name, snapshot).is_some() {
            return Err(Error::Invalid);
        }
    }
    Ok(u::Snapshot {
        schema: s.schema,
        seq: s.seq,
        generated_at_utc: s.generated_at_utc,
        provider: usage_provider_from(s.provider)?,
        plan_type: s.plan_type,
        burn_rate_per_min: s.burn_rate_per_min,
        burn_state: s.burn_state,
        today_total_tokens: s.today_total_tokens,
        today_sessions: s.today_sessions,
        rolling_5h: from_window(s.rolling_5h.ok_or(Error::Invalid)?),
        weekly: from_window(s.weekly.ok_or(Error::Invalid)?),
        rolling_5h_observed: s.rolling_5h_observed,
        weekly_observed: s.weekly_observed,
        status: from_status(s.status.ok_or(Error::Invalid)?),
        accounts: s.accounts.into_iter().map(from_account).collect(),
        accounts_updated_at: s.accounts_updated_at,
        sources,
    })
}
pub fn usage_to_proto(s: u::Snapshot) -> Result<p::UsageSnapshot, Error> {
    u::transport::validate(&s).map_err(|_| Error::Invalid)?;
    let out = to_usage(s);
    validate_usage(&out)?;
    Ok(out)
}
pub fn usage_from_proto(s: p::UsageSnapshot) -> Result<u::Snapshot, Error> {
    validate_usage(&s)?;
    let out = from_usage(s, false)?;
    u::transport::validate(&out).map_err(|_| Error::Invalid)?;
    Ok(out)
}

struct TreeBudget(usize);
impl TreeBudget {
    fn new<T>() -> Self {
        Self(std::mem::size_of::<T>())
    }
    fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        self.0 = self.0.checked_add(bytes).ok_or(Error::Limit)?;
        if self.0 > MAX_DECODED_SNAPSHOT_BYTES {
            Err(Error::Limit)
        } else {
            Ok(())
        }
    }
    fn list<T>(&mut self, count: usize) -> Result<(), Error> {
        self.charge(
            count
                .checked_mul(2 * std::mem::size_of::<T>())
                .ok_or(Error::Limit)?,
        )
    }
    fn text(&mut self, s: &str) -> Result<(), Error> {
        self.charge(s.len())
    }
    fn strings(&mut self, items: &[String]) -> Result<(), Error> {
        self.list::<String>(items.len())?;
        for s in items {
            self.text(s)?;
        }
        Ok(())
    }
}
fn budget_model_catalog(c: &m::Catalog) -> Result<(), Error> {
    let mut b = TreeBudget::new::<m::Catalog>();
    b.text(&c.generated_at)?;
    if let Some(metrics) = &c.host_metrics {
        b.charge(2 * std::mem::size_of::<m::HostMetrics>())?;
        b.text(&metrics.observed_at)?;
    }
    if let Some(sessions) = &c.sessions {
        b.list::<m::Session>(sessions.len())?;
        for s in sessions {
            for x in [
                &s.id,
                &s.name,
                &s.alias,
                &s.active_window,
                &s.current_path,
                &s.current_command,
                &s.profile,
                &s.label,
                &s.kind,
                &s.runtime,
                &s.model,
                &s.state,
                &s.process,
            ] {
                b.text(x)?;
            }
            if let Some(x) = &s.restored_from {
                b.charge(2 * std::mem::size_of::<m::SessionIdentity>())?;
                b.text(&x.id)?;
            }
            if let Some(xs) = &s.window_names {
                b.strings(xs)?;
            }
            if let Some(xs) = &s.tags {
                b.strings(xs)?;
            }
            if s.workflow.is_some() {
                b.charge(2 * std::mem::size_of::<m::WorkflowSummary>())?;
            }
            if let Some(ws) = &s.workflows {
                b.list::<m::Workflow>(ws.len())?;
                for w in ws {
                    for x in [
                        &w.id,
                        &w.source,
                        &w.session_id,
                        &w.turn_id,
                        &w.status,
                        &w.model,
                    ] {
                        b.text(x)?;
                    }
                    if let Some(ns) = &w.nodes {
                        b.list::<m::WorkflowNode>(ns.len())?;
                        for n in ns {
                            for x in [&n.id, &n.parent_id, &n.node_type, &n.provider, &n.status] {
                                b.text(x)?;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
fn budget_proto_catalog(c: &p::CatalogSnapshot) -> Result<(), Error> {
    let mut b = TreeBudget::new::<p::CatalogSnapshot>();
    b.text(&c.generated_at)?;
    if let Some(metrics) = &c.host_metrics {
        b.charge(2 * std::mem::size_of::<p::CatalogHostMetrics>())?;
        b.text(&metrics.observed_at)?;
    }
    if let Some(sessions) = &c.sessions {
        b.list::<p::CatalogSession>(sessions.items.len())?;
        for s in &sessions.items {
            let id = s.identity.as_ref().ok_or(Error::Invalid)?;
            b.text(&id.id)?;
            for x in [
                &s.name,
                &s.alias,
                &s.active_window,
                &s.current_path,
                &s.current_command,
                &s.profile,
                &s.label,
                &s.kind,
                &s.runtime,
                &s.model,
                &s.state,
                &s.process,
            ] {
                b.text(x)?;
            }
            if let Some(x) = &s.restored_from {
                b.charge(2 * std::mem::size_of::<p::Session>())?;
                b.text(&x.id)?;
            }
            if let Some(xs) = &s.window_names {
                b.strings(&xs.items)?;
            }
            if let Some(xs) = &s.tags {
                b.strings(&xs.items)?;
            }
            if s.workflow.is_some() {
                b.charge(2 * std::mem::size_of::<p::CatalogWorkflowSummary>())?;
            }
            if let Some(ws) = &s.workflows {
                b.list::<p::CatalogWorkflow>(ws.items.len())?;
                for w in &ws.items {
                    for x in [
                        &w.id,
                        &w.source,
                        &w.session_id,
                        &w.turn_id,
                        &w.status,
                        &w.model,
                    ] {
                        b.text(x)?;
                    }
                    if let Some(ns) = &w.nodes {
                        b.list::<p::CatalogWorkflowNode>(ns.items.len())?;
                        for n in &ns.items {
                            for x in [&n.id, &n.parent_id, &n.node_type, &n.provider, &n.status] {
                                b.text(x)?;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Validate a decoded typed catalog in place before dispatch or encoding.
pub fn validate_catalog(c: &p::CatalogSnapshot) -> Result<(), Error> {
    check_proto_catalog(c)?;
    budget_proto_catalog(c)?;
    if c.encoded_len() > MAX_CATALOG_BYTES {
        return Err(Error::Limit);
    }
    Ok(())
}

fn usage_ratio(x: f64) -> bool {
    x.is_finite() && (0.0..=1.0).contains(&x)
}
fn usage_timestamp(x: &Option<String>) -> bool {
    x.as_ref().is_none_or(|s| s.len() <= 128)
}
fn usage_window(w: &p::UsageWindow) -> bool {
    usage_ratio(w.used_pct) && w.remaining_seconds >= 0 && usage_timestamp(&w.resets_at)
}
fn usage_account_window(w: &Option<p::UsageAccountWindow>) -> bool {
    w.as_ref()
        .is_none_or(|x| usage_ratio(x.used_pct) && usage_timestamp(&x.resets_at))
}
fn usage_retry(s: &p::UsageSnapshot, status: &p::UsageStatus) -> bool {
    let Some(raw) = &status.retry_at else {
        return true;
    };
    if raw.len() > 128 || !(status.state == "rateLimited" || (status.state == "ok" && status.stale))
    {
        return false;
    }
    let (Some(reset), Some(generated)) = (
        u::model::parse_time(raw),
        u::model::parse_time(&s.generated_at_utc),
    ) else {
        return false;
    };
    let delta = reset.signed_duration_since(generated);
    delta
        .num_nanoseconds()
        .is_some_and(|ns| ns > 0 && ns <= 24 * 60 * 60 * 1_000_000_000)
}
fn validate_usage_inner(s: &p::UsageSnapshot, child: bool) -> Result<(), Error> {
    if child && !s.sources.is_empty() {
        return Err(Error::Invalid);
    }
    if s.sources.len() > 2 || s.accounts.len() > u::model::MAX_ACCOUNTS {
        return Err(Error::Limit);
    }
    let provider = usage_provider_from(s.provider)?;
    let (Some(rolling), Some(weekly), Some(status)) = (&s.rolling_5h, &s.weekly, &s.status) else {
        return Err(Error::Invalid);
    };
    if s.schema != 1
        || s.seq < 0
        || s.generated_at_utc.is_empty()
        || s.generated_at_utc.len() > 128
        || s.burn_state.len() > 32
        || !s.burn_rate_per_min.is_finite()
        || s.burn_rate_per_min < 0.0
        || s.today_total_tokens < 0
        || s.today_sessions < 0
        || s.plan_type != u::model::normalize_plan(&s.plan_type)
        || !usage_window(rolling)
        || !usage_window(weekly)
        || status.state.is_empty()
        || status.state.len() > 64
        || status.data_source.len() > 128
        || status.quota_source.len() > 128
        || !usage_timestamp(&status.quota_observed_at)
        || !usage_retry(s, status)
        || !usage_timestamp(&s.accounts_updated_at)
    {
        return Err(Error::Invalid);
    }
    for (i, a) in s.accounts.iter().enumerate() {
        if a.number <= 0
            || s.accounts[..i].iter().any(|prev| prev.number == a.number)
            || (provider == u::Provider::Codex && !a.email.is_empty())
            || a.email != u::model::safe_label(&a.email)
            || a.display_name != u::model::safe_label(&a.display_name)
            || a.status.len() > 64
            || !usage_account_window(&a.five_hour)
            || !usage_account_window(&a.seven_day)
            || a.tokens_per_hour.is_some_and(|n| !n.is_finite() || n < 0.0)
            || a.total_tokens.is_some_and(|n| n < 0)
            || !usage_timestamp(&a.last_refresh_at)
            || a.plan_type != u::model::normalize_plan(&a.plan_type)
        {
            return Err(Error::Invalid);
        }
    }
    for (i, source) in s.sources.iter().enumerate() {
        text(&source.name, 16)?;
        if s.sources[..i].iter().any(|prior| prior.name == source.name) {
            return Err(Error::Invalid);
        }
        let nested = source.snapshot.as_ref().ok_or(Error::Invalid)?;
        let quota = nested
            .status
            .as_ref()
            .ok_or(Error::Invalid)?
            .quota_source
            .as_str();
        let allowed = matches!(
            (provider, source.name.as_str(), quota),
            (_, "cli", "oauth_api" | "none")
                | (u::Provider::Claude, "cswap", "claude_swap")
                | (u::Provider::Codex, "codex-lb", "codex_lb")
        );
        if !allowed || nested.provider != s.provider {
            return Err(Error::Invalid);
        }
        validate_usage_inner(nested, true)?;
    }
    Ok(())
}
/// Validate a typed usage snapshot by borrowing its tree. Source depth is one.
pub fn validate_usage(s: &p::UsageSnapshot) -> Result<(), Error> {
    validate_usage_inner(s, false)?;
    if s.encoded_len() > MAX_USAGE_BYTES {
        return Err(Error::Limit);
    }
    Ok(())
}
