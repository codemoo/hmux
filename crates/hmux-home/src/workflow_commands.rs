//! Read-only workflow helper contracts. CLI argument parsing belongs to hmux-agent.
use super::{binding_valid, Binding, Error};
use crate::catalog::TmuxCatalogReader;
use chrono::{Local, TimeZone};
use hmux_core::command::CommandRunner;
use hmux_model::{safe_text, Session, Workflow, WorkflowSummary};
use serde::Serialize;
use std::io::{self, Write};
use tokio_util::sync::CancellationToken;

/// Capture these three environment values once at helper startup. An invalid
/// explicit identity never falls back to a different pane.
#[derive(Default)]
pub struct BindingEnvironment {
    pub session_id: String,
    pub created_at: String,
    pub pane: String,
}
pub async fn resolve_binding(
    env: &BindingEnvironment,
    reader: &TmuxCatalogReader,
    runner: &CommandRunner,
    cancel: &CancellationToken,
) -> Result<Binding, Error> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    if !env.session_id.is_empty() {
        let binding = Binding {
            id: env.session_id.clone(),
            created_at: env.created_at.parse().map_err(|_| Error::Invalid)?,
        };
        return if binding_valid(&binding) {
            Ok(binding)
        } else {
            Err(Error::Invalid)
        };
    }
    let pane_digits = env.pane.strip_prefix('%').ok_or(Error::Invalid)?;
    if !(1..=12).contains(&pane_digits.len()) || !pane_digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Invalid);
    }
    let spec = reader.command(
        4096,
        [
            "display-message",
            "-p",
            "-t",
            &env.pane,
            "#{session_id}\t#{session_created}",
        ],
    );
    let (signal, cancelled) = tokio::sync::oneshot::channel();
    let task = runner.run_cancelable(spec, cancelled);
    tokio::pin!(task);
    let output = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            let _ = signal.send(());
            let _ = task.await;
            return Err(Error::Cancelled);
        },
        result = &mut task => result.map_err(|_| Error::Unavailable)?,
    };
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| Error::Invalid)?
        .trim();
    let (id, created_at) = text.split_once('\t').ok_or(Error::Invalid)?;
    let binding = Binding {
        id: id.into(),
        created_at: created_at.parse().map_err(|_| Error::Invalid)?,
    };
    if binding_valid(&binding) {
        Ok(binding)
    } else {
        Err(Error::Invalid)
    }
}

#[derive(Debug, Serialize)]
pub struct SessionView {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub alias: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<WorkflowSummary>,
    pub workflows: Option<Vec<Workflow>>,
}
pub fn views(sessions: &[Session], filter: &str) -> Result<Vec<SessionView>, String> {
    let clean = safe_text(filter, 512);
    let filter = clean.trim();
    let mut matched: Vec<_> = sessions
        .iter()
        .filter(|s| {
            if filter.is_empty() {
                s.workflows.as_ref().is_some_and(|w| !w.is_empty())
            } else {
                filter == s.id || filter == s.name || filter == s.alias
            }
        })
        .collect();
    if !filter.is_empty() {
        if matched.is_empty() {
            return Err(format!("session {filter:?} does not exist"));
        }
        if matched.len() > 1 {
            return Err(format!(
                "session {filter:?} is ambiguous; use its stable ID"
            ));
        }
    }
    let updated = |s: &Session| {
        s.activity_at
            .max(s.workflow.as_ref().map_or(0, |w| w.updated_at))
    };
    matched.sort_by(|a, b| updated(b).cmp(&updated(a)).then(a.id.cmp(&b.id)));
    Ok(matched
        .into_iter()
        .map(|s| SessionView {
            id: s.id.clone(),
            name: safe_text(&s.name, 512),
            alias: safe_text(&s.alias, crate::sessionstate::MAX_ALIAS_BYTES),
            summary: s.workflow.clone(),
            workflows: s.workflows.clone().filter(|w| !w.is_empty()),
        })
        .collect())
}
/// Stream human-readable output instead of retaining a second rendered copy.
pub fn write_views(views: &[SessionView], mut out: impl Write) -> io::Result<()> {
    if views.is_empty() {
        return writeln!(
            out,
            "No workflow state is available for live tmux sessions."
        );
    }
    for (session_index, s) in views.iter().enumerate() {
        if session_index > 0 {
            writeln!(out)?;
        }
        let name = if s.alias.is_empty() {
            &s.name
        } else {
            &s.alias
        };
        write!(out, "{name} ({})", s.id)?;
        if let Some(summary) = &s.summary {
            let attention = summary
                .waiting_approval
                .saturating_add(summary.waiting_input)
                .saturating_add(summary.failed)
                .saturating_add(summary.interrupted)
                .saturating_add(summary.stale);
            let mut badges = Vec::with_capacity(3);
            if summary.running > 0 {
                badges.push(format!("{}▶", summary.running));
            }
            if summary.completed > 0 {
                badges.push(format!("{}✓", summary.completed));
            }
            if attention > 0 {
                badges.push(format!("{attention}!"));
            }
            if !badges.is_empty() {
                write!(out, "  {}", badges.join(" "))?;
            }
        }
        writeln!(out)?;
        let workflows = s.workflows.as_deref().unwrap_or_default();
        if workflows.is_empty() {
            writeln!(out, "└─ no recorded workflow")?;
        }
        for (index, w) in workflows.iter().enumerate() {
            let (branch, prefix) = if index + 1 == workflows.len() {
                ("└─", "   ")
            } else {
                ("├─", "│  ")
            };
            write!(
                out,
                "{branch} {}  {}  {}",
                short_id(&w.id),
                w.source,
                w.status
            )?;
            if !w.model.is_empty() {
                write!(out, "  {}", w.model)?;
            }
            let time = if w.updated_at < 1 {
                None
            } else {
                Local.timestamp_opt(w.updated_at, 0).single()
            };
            writeln!(
                out,
                "  updated {}",
                time.map_or_else(
                    || "unknown".into(),
                    |v| v.format("%Y-%m-%d %H:%M:%S").to_string()
                )
            )?;
            let nodes = w.nodes.as_deref().unwrap_or_default();
            for (index, n) in nodes.iter().enumerate() {
                let branch = if index + 1 == nodes.len() {
                    "└─"
                } else {
                    "├─"
                };
                writeln!(
                    out,
                    "{prefix}{branch} {}  {}  {}  {}",
                    short_id(&n.id),
                    n.node_type,
                    n.provider,
                    n.status
                )?;
            }
        }
    }
    Ok(())
}
fn short_id(id: &str) -> &str {
    let end = id
        .char_indices()
        .map(|(i, _)| i)
        .nth(15)
        .unwrap_or(id.len());
    &id[..end]
}
