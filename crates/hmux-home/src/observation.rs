//! Fixed, privacy-safe Home diagnostics. Callers enqueue without blocking.
use crate::catalog::CatalogError;
use hmux_core::command::RunErrorKind;
use hmux_protocol::protobuf::types::Operation;
use std::{fmt, sync::Arc, time::Instant};

pub type Reporter = Arc<dyn Fn(Event) + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Catalog,
    Action,
    ViewCleanup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Busy,
    Cancelled,
    QueryTimeout,
    QueryFailure,
    Parse,
    Metadata,
    Encoding,
    Transport,
    Worker,
    Unavailable,
    Slow,
    Recovered,
    Published,
    Quarantined,
}

impl Reason {
    pub fn catalog(error: &CatalogError) -> Self {
        match error {
            CatalogError::Command(error) => match error.kind() {
                RunErrorKind::Busy => Self::Busy,
                RunErrorKind::Cancelled => Self::Cancelled,
                RunErrorKind::TimedOut => Self::QueryTimeout,
                _ => Self::QueryFailure,
            },
            CatalogError::Parse(_) => Self::Parse,
            CatalogError::InvalidOption => Self::Cancelled,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Event {
    pub stage: Stage,
    pub operation: Option<Operation>,
    pub reason: Reason,
    pub duration_ms: u64,
}

impl Event {
    pub fn new(stage: Stage, operation: Option<Operation>, reason: Reason, start: Instant) -> Self {
        Self {
            stage,
            operation,
            reason,
            duration_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
        }
    }
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = match self.stage {
            Stage::Catalog => "catalog",
            Stage::Action => "action",
            Stage::ViewCleanup => "view-cleanup",
        };
        let reason = match self.reason {
            Reason::Busy => "busy",
            Reason::Cancelled => "cancelled",
            Reason::QueryTimeout => "query-timeout",
            Reason::QueryFailure => "query-failure",
            Reason::Parse => "parse",
            Reason::Metadata => "metadata",
            Reason::Encoding => "encoding",
            Reason::Transport => "transport",
            Reason::Worker => "worker",
            Reason::Unavailable => "unavailable",
            Reason::Slow => "slow",
            Reason::Recovered => "recovered",
            Reason::Published => "published",
            Reason::Quarantined => "quarantined",
        };
        write!(
            f,
            "home stage={stage} operation={} reason={reason} duration_ms={}",
            self.operation.map_or("none", |v| v.as_str_name()),
            self.duration_ms
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_contains_only_fixed_categories_and_duration() {
        let event = Event::new(
            Stage::Action,
            Some(Operation::Conversation),
            Reason::QueryTimeout,
            Instant::now(),
        );
        let text = event.to_string();
        assert!(text.contains("stage=action"));
        assert!(text.contains("operation=CONVERSATION"));
        assert!(text.contains("reason=query-timeout"));
        assert!(text.contains("duration_ms="));
    }
}
