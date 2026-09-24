//! Typed transport diagnostics: no endpoint, session/account ID, payload or raw
//! remote error can enter this channel. A sink must enqueue without blocking.
use crate::hub::Error;
use hmux_protocol::protobuf::types::Operation;
use std::{fmt, sync::Arc, time::Instant};

pub type Reporter = Arc<dyn Fn(Event) + Send + Sync>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    HomeConnected,
    HomeDisconnected,
    RequestComplete,
    TerminalOpenComplete,
    ActionFailed,
}
/// Fixed local categories; never copy a Home error string into diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionFailure {
    WorkspaceUnavailable,
    HomeOffline,
    HomeRequest,
    HomeBusy,
    HomeOperation,
    ResponseInvalid,
    Deadline,
}
impl ActionFailure {
    fn label(self) -> &'static str {
        match self {
            Self::WorkspaceUnavailable => "workspace-unavailable",
            Self::HomeOffline => "home-offline",
            Self::HomeRequest => "home-request",
            Self::HomeBusy => "home-busy",
            Self::HomeOperation => "home-operation",
            Self::ResponseInvalid => "response-invalid",
            Self::Deadline => "deadline",
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub stage: Stage,
    pub operation: Option<Operation>,
    pub connection: u64,
    pub reason: Option<Error>,
    pub duration_ms: u64,
    pub send_ms: u64,
    pub action_failure: Option<ActionFailure>,
    pub http_status: Option<u16>,
}
impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = match self.stage {
            Stage::HomeConnected => "home-connected",
            Stage::HomeDisconnected => "home-disconnected",
            Stage::RequestComplete => "request-complete",
            Stage::TerminalOpenComplete => "terminal-open-complete",
            Stage::ActionFailed => "action-failed",
        };
        let reason = self.action_failure.map_or_else(
            || match self.reason {
                None => "ok",
                Some(Error::Offline) => "home-offline",
                Some(Error::Stale) => "home-changed",
                Some(Error::Busy) => "busy",
                Some(Error::Capacity) => "capacity",
                Some(Error::Unsupported) => "unsupported",
                Some(Error::Invalid) => "invalid",
                Some(Error::Cancelled) => "cancelled",
                Some(Error::Transport) => "transport",
                Some(Error::OutputFull) => "output-full",
                Some(Error::RemoteOperation) => "home-operation",
            },
            ActionFailure::label,
        );
        write!(
            f,
            "connection={} stage={stage} operation={} reason={reason} duration_ms={} send_ms={}",
            self.connection,
            self.operation.map_or("none", |v| v.as_str_name()),
            self.duration_ms,
            self.send_ms
        )?;
        if let Some(status) = self.http_status {
            write!(f, " http_status={status}")?;
        }
        Ok(())
    }
}
pub(crate) struct Span {
    reporter: Option<Reporter>,
    event: Event,
    started: Instant,
}
impl Span {
    pub(crate) fn new(
        reporter: Option<Reporter>,
        stage: Stage,
        operation: Option<Operation>,
        connection: u64,
    ) -> Self {
        Self {
            reporter,
            event: Event {
                stage,
                operation,
                connection,
                reason: Some(Error::Cancelled),
                duration_ms: 0,
                send_ms: 0,
                action_failure: None,
                http_status: None,
            },
            started: Instant::now(),
        }
    }
    pub(crate) fn sent(&mut self) {
        self.event.send_ms = millis(self.started);
    }
    pub(crate) fn finish<T>(&mut self, result: &Result<T, Error>) {
        self.event.reason = result.as_ref().err().copied();
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some(reporter) = &self.reporter {
            self.event.duration_ms = millis(self.started);
            reporter(self.event);
        }
    }
}
fn millis(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u64::MAX as u128) as u64
}
