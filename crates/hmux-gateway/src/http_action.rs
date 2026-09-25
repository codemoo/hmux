//! Browser action authorization and forwarding. Home owns operation validation
//! and exact session binding; account workspace state stays at this gateway.
use super::*;
use crate::hub::Error as HomeError;
use crate::observation::ActionFailure;
use hmux_core::workspace::Error as WorkspaceError;
use hmux_model::workspace::Change;
use hmux_protocol::{actions, legacy, protobuf::types as p, wire};
use serde_json::value::RawValue;
use std::time::{Duration, Instant};

const ACTION_TIMEOUT: Duration = Duration::from_secs(20);
fn home_error_status(error: HomeError) -> StatusCode {
    match error {
        HomeError::Offline
        | HomeError::Stale
        | HomeError::Busy
        | HomeError::Capacity
        | HomeError::Cancelled
        | HomeError::Transport
        | HomeError::OutputFull => StatusCode::SERVICE_UNAVAILABLE,
        HomeError::Unsupported | HomeError::Invalid | HomeError::RemoteOperation => {
            StatusCode::BAD_GATEWAY
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceRequest {
    #[serde(default)]
    change: Option<Change>,
}
fn workspace_request(raw: Option<&RawValue>) -> Result<WorkspaceRequest, ()> {
    let raw = raw.ok_or(())?.get().trim();
    if raw == "null" {
        return Ok(WorkspaceRequest::default());
    }
    if !raw.starts_with('{') {
        return Err(());
    }
    serde_json::from_str(raw).map_err(|_| ())
}
impl Gateway {
    pub(super) async fn action(&self, request: Request<Incoming>, token: &str) -> Reply {
        if request.method() != Method::POST {
            return boundary::error(StatusCode::METHOD_NOT_ALLOWED);
        }
        let Ok(message) = boundary::decode_json::<_, wire::Message>(request).await else {
            return boundary::error(StatusCode::BAD_REQUEST);
        };
        let Ok(operation) = legacy::operation(&message.operation) else {
            return boundary::error(StatusCode::BAD_REQUEST);
        };
        let workspace = if operation == p::Operation::Workspace {
            match workspace_request(message.payload.as_deref()) {
                Ok(q) => Some(q),
                Err(_) => return boundary::error(StatusCode::BAD_REQUEST),
            }
        } else {
            None
        };
        let touch = workspace.as_ref().is_none_or(|q| q.change.is_some());
        // Slow bodies must reauthorize before dispatch, and reads must not keep
        // an idle browser's login alive just by polling its workspace.
        let access = match self.auth.access(token, touch, now()).await {
            Ok(Some(access)) => access,
            Ok(None) => return boundary::error(StatusCode::UNAUTHORIZED),
            Err(_) => return boundary::error(StatusCode::SERVICE_UNAVAILABLE),
        };
        let Some(home) = self.home.as_ref() else {
            return boundary::retryable_error(StatusCode::SERVICE_UNAVAILABLE);
        };
        let started = Instant::now();
        let initial_generation = home.snapshot().generation;
        let failed = |failure: ActionFailure, status: StatusCode| {
            home.report_action_failure(
                operation,
                initial_generation,
                failure,
                status.as_u16(),
                started.elapsed(),
            );
            if matches!(
                status,
                StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT
            ) {
                boundary::retryable_error(status)
            } else {
                boundary::error(status)
            }
        };
        let deadline = started + ACTION_TIMEOUT;
        let mut cancellation = access.clone();
        let expiry = access
            .expires_at
            .signed_duration_since(now())
            .to_std()
            .unwrap_or_default();
        let work = async {
            if !access.profile.is_empty() {
                if let Some(query) = workspace {
                    let Some(store) = &self.workspaces else {
                        return boundary::error(StatusCode::SERVICE_UNAVAILABLE);
                    };
                    let home = home.clone();
                    let guard = access.clone();
                    let result = store
                        .sync(
                            Some(access.profile.clone()),
                            query.change,
                            move || {
                                let snapshot = home.snapshot();
                                if !snapshot.online {
                                    return Err(WorkspaceError::Unavailable);
                                }
                                let raw = snapshot.catalog.ok_or(WorkspaceError::Unavailable)?;
                                hmux_model::workspace::decode_catalog(&raw)
                                    .map_err(|_| WorkspaceError::Unavailable)
                            },
                            move || {
                                Instant::now() < deadline
                                    && now() < guard.expires_at
                                    && !*guard.cancelled.borrow()
                                    && guard.cancelled.has_changed().is_ok()
                            },
                        )
                        .await;
                    if let Err(status) = self.revalidate(token).await {
                        return boundary::error(status);
                    }
                    return match result {
                        Ok(reply) => boundary::json(&reply),
                        Err(WorkspaceError::Busy) => {
                            boundary::retryable_error(StatusCode::SERVICE_UNAVAILABLE)
                        }
                        Err(_) => failed(
                            ActionFailure::WorkspaceUnavailable,
                            StatusCode::SERVICE_UNAVAILABLE,
                        ),
                    };
                }
            }
            let Some(generation) = home.snapshot().generation else {
                return failed(ActionFailure::HomeOffline, StatusCode::SERVICE_UNAVAILABLE);
            };
            // Construct only the allowlisted request fields. Browser-supplied
            // type/ID, data, geometry, errors and transport controls cannot escape.
            let payload = match actions::request_from_json(
                operation,
                message
                    .payload
                    .as_ref()
                    .map_or(&[][..], |v| v.get().as_bytes()),
            ) {
                Ok(payload) => payload,
                Err(_) => return boundary::error(StatusCode::BAD_REQUEST),
            };
            let request = p::Request {
                id: String::new(),
                operation: operation as i32,
                session: (message.session != wire::SessionIdentity::default()).then_some(
                    p::Session {
                        id: message.session.id,
                        created_at: message.session.created_at,
                    },
                ),
                payload: Some(payload),
            };
            let result = home.request(generation, request).await;
            if let Err(status) = self.revalidate(token).await {
                return boundary::error(status);
            }
            let reply = match result {
                Ok(reply) => reply,
                Err(error) => return failed(ActionFailure::HomeRequest, home_error_status(error)),
            };
            if !reply.error.is_empty() {
                return if reply.error == "Home is busy" {
                    failed(ActionFailure::HomeBusy, StatusCode::SERVICE_UNAVAILABLE)
                } else if reply.error == "Home operation timed out" {
                    failed(ActionFailure::Deadline, StatusCode::GATEWAY_TIMEOUT)
                } else {
                    failed(ActionFailure::HomeOperation, StatusCode::BAD_GATEWAY)
                };
            }
            let raw = match actions::response_payload(&reply) {
                Ok(raw) => raw,
                Err(_) => return failed(ActionFailure::ResponseInvalid, StatusCode::BAD_GATEWAY),
            };
            match serde_json::from_slice::<&RawValue>(&raw) {
                Ok(raw) => boundary::json(&raw),
                Err(_) => failed(ActionFailure::ResponseInvalid, StatusCode::BAD_GATEWAY),
            }
        };
        tokio::select! {
            biased;
            _=cancellation.wait_cancelled()=>boundary::error(StatusCode::UNAUTHORIZED),
            _=tokio::time::sleep(expiry)=>boundary::error(StatusCode::UNAUTHORIZED),
            _=tokio::time::sleep(ACTION_TIMEOUT)=>failed(ActionFailure::Deadline, StatusCode::GATEWAY_TIMEOUT),
            reply=work=>reply,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_home_failures_keep_protocol_failures_distinct() {
        for error in [
            HomeError::Offline,
            HomeError::Stale,
            HomeError::Busy,
            HomeError::Capacity,
            HomeError::Cancelled,
            HomeError::Transport,
            HomeError::OutputFull,
        ] {
            assert_eq!(home_error_status(error), StatusCode::SERVICE_UNAVAILABLE);
        }
        for error in [
            HomeError::Unsupported,
            HomeError::Invalid,
            HomeError::RemoteOperation,
        ] {
            assert_eq!(home_error_status(error), StatusCode::BAD_GATEWAY);
        }
    }
}
