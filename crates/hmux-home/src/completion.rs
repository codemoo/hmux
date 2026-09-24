//! One best-effort completion observer per connected Home. A single replaceable
//! pending snapshot contains only identities/PIDs, never catalog/transcript text.
use crate::{
    completion_tracker::{Observation, Tracker},
    inspection::{self, Inspector, ScanPurpose},
    peer,
};
use hmux_model::{Catalog, SessionIdentity};
use hmux_protocol::{
    protobuf::{types as p, Negotiated},
    transport::Sender,
};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const MAX_TARGETS: usize = 4096;
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(3);
const COOLDOWN: Duration = Duration::from_secs(5);

struct Target {
    identity: SessionIdentity,
    pane: Option<i32>,
}
#[derive(Default)]
pub(crate) struct Inbox {
    pending: Mutex<Option<Vec<Target>>>,
    changed: Notify,
}
impl Inbox {
    /// Called before digest suppression. Invalid/oversized input resets baselines
    /// instead of retaining potentially stale state or allocating an unbounded list.
    pub fn enqueue(&self, catalog: &Catalog) {
        let sessions = catalog.sessions.as_deref().unwrap_or_default();
        let targets = if sessions.len() > MAX_TARGETS
            || sessions
                .iter()
                .any(|s| hmux_model::validate_session_id(&s.id).is_err() || s.created_at < 1)
        {
            Vec::new()
        } else {
            sessions
                .iter()
                .map(|s| Target {
                    identity: SessionIdentity {
                        id: s.id.clone(),
                        created_at: s.created_at,
                    },
                    pane: i32::try_from(s.pane_pid).ok().filter(|&p| p > 0),
                })
                .collect()
        };
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(targets);
        self.changed.notify_one();
    }
    fn take(&self) -> Option<Vec<Target>> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

pub(crate) async fn run(
    inbox: Arc<Inbox>,
    inspector: Arc<Inspector>,
    sender: Sender,
    protocol: Negotiated,
    stop: CancellationToken,
) {
    let mut tracker = Tracker::default();
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => return,
            _ = inbox.changed.notified() => {},
        }
        let Some(targets) = inbox.take() else {
            continue;
        };
        let Some(permit) = inspection::wait_background(&stop).await else {
            // A missing scan must not leave an old armed baseline around.
            tracker.clear();
            continue;
        };
        let inspector = inspector.clone();
        let child = stop.child_token();
        let _cancel = child.clone().drop_guard();
        let runtime = tokio::runtime::Handle::current();
        let deadline = Instant::now() + OBSERVE_TIMEOUT;
        // Always join the blocking worker. It owns its permit through command
        // cancellation/reaping and bounded file I/O, including peer shutdown.
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let panes: Vec<_> = targets
                .iter()
                .filter_map(|t| t.pane)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let scan = inspector.scan(&panes, ScanPurpose::Completion, &child, deadline, &runtime);
            let events = match scan {
                Ok(scan) => {
                    let observations: Vec<_> = targets
                        .into_iter()
                        .map(|t| Observation {
                            identity: t.identity,
                            binding: t.pane.and_then(|p| scan.bindings.get(&p).cloned()),
                        })
                        .collect();
                    tracker.observe(&observations, &child, deadline).ok()
                }
                Err(_) => {
                    tracker.clear();
                    None
                }
            };
            let over_budget = Instant::now() >= deadline;
            if over_budget || child.is_cancelled() {
                tracker.clear();
            }
            (tracker, events, over_budget)
        })
        .await;
        let Ok((next, events, over_budget)) = result else {
            // Recover a fresh baseline on worker failure; keep terminals alive.
            tracker = Tracker::default();
            continue;
        };
        tracker = next;
        if stop.is_cancelled() {
            return;
        }
        if over_budget {
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = tokio::time::sleep(COOLDOWN) => {},
            }
            continue;
        }
        for event in events.into_iter().flatten() {
            if stop.is_cancelled() {
                return;
            }
            let body = p::envelope::Body::TaskComplete(p::Completion {
                id: event.id,
                session: Some(p::Session {
                    id: event.identity.id,
                    created_at: event.identity.created_at,
                }),
                completed_at: event.completed_at,
            });
            // The transport has finite queue/write deadlines. A failed optional
            // notification does not independently tear down catalog/terminal work.
            if peer::send(&sender, protocol, body, stop.clone())
                .await
                .is_err()
            {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmux_model::Session;

    #[test]
    fn pending_catalog_is_replaced_and_retains_only_identity_and_pid() {
        let inbox = Inbox::default();
        for n in 1..100 {
            inbox.enqueue(&Catalog {
                sessions: Some(vec![Session {
                    id: format!("${n}"),
                    created_at: n,
                    pane_pid: 42,
                    ..Session::default()
                }]),
                ..Catalog::default()
            });
        }
        let values = inbox.take().unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].identity.id, "$99");
        assert_eq!(values[0].pane, Some(42));
        assert!(inbox.take().is_none());
    }

    #[test]
    fn oversize_or_invalid_catalog_requests_a_safe_reset() {
        let inbox = Inbox::default();
        inbox.enqueue(&Catalog {
            sessions: Some(vec![
                Session {
                    id: "$1".into(),
                    created_at: 1,
                    ..Session::default()
                };
                MAX_TARGETS + 1
            ]),
            ..Catalog::default()
        });
        assert!(inbox.take().unwrap().is_empty());
        inbox.enqueue(&Catalog {
            sessions: Some(vec![Session {
                id: "bad".into(),
                created_at: 1,
                ..Session::default()
            }]),
            ..Catalog::default()
        });
        assert!(inbox.take().unwrap().is_empty());
    }
}
