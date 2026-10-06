//! Shared per-tmux-lifetime input coordination; only weak registry entries persist.
use hmux_model::SessionIdentity;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
};
use tokio::sync::Mutex as AsyncMutex;
static STATES: OnceLock<Mutex<HashMap<SessionIdentity, Weak<State>>>> = OnceLock::new();
#[derive(Default)]
pub(crate) struct State {
    pub gate: AsyncMutex<()>,
    pub epoch: AtomicU64,
    pending: AtomicUsize,
}
pub(crate) fn state(identity: &SessionIdentity) -> Arc<State> {
    let mut states = STATES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    states.retain(|_, v| v.strong_count() > 0);
    if let Some(v) = states.get(identity).and_then(Weak::upgrade) {
        return v;
    }
    let value = Arc::new(State::default());
    // Terminal admission already caps live states; one status probe may add one.
    if states.len() < 128 {
        states.insert(identity.clone(), Arc::downgrade(&value));
    }
    value
}
pub(crate) struct Ticket {
    pub state: Arc<State>,
}
impl Ticket {
    pub fn new(state: Arc<State>) -> Self {
        state.epoch.fetch_add(1, Ordering::SeqCst);
        state.pending.fetch_add(1, Ordering::SeqCst);
        Self { state }
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        self.state.pending.fetch_sub(1, Ordering::SeqCst);
    }
}
impl State {
    pub fn quiet(&self) -> bool {
        self.pending.load(Ordering::SeqCst) == 0
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_input_is_seen_across_views_and_drop_releases_pending() {
        let id = SessionIdentity {
            id: "$1".into(),
            created_at: 42,
        };
        let a = state(&id);
        let b = state(&id);
        assert!(Arc::ptr_eq(&a, &b));
        let ticket = Ticket::new(a.clone());
        assert!(!b.quiet());
        assert_eq!(b.epoch.load(Ordering::SeqCst), 1);
        drop(ticket);
        assert!(b.quiet());
        assert!(!Arc::ptr_eq(
            &a,
            &state(&SessionIdentity {
                id: "$1".into(),
                created_at: 43
            })
        ));
    }
}
