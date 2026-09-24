//! One gateway-owned push consumer and bounded browser presence. The Hub has
//! already admitted, deduplicated and queued completion events.
use crate::{
    auth_store::{AuthStore, SessionAccess, StoreError},
    hub::{CompletionEvent, Generation, Hub},
    push_state::{self, Subscription},
    push_transport,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use hmux_core::workspace;
use hmux_model::{self, SessionIdentity};
use hmux_protocol::protobuf::types as p;
use http::StatusCode;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::mpsc, time::Instant};
use tokio_util::sync::CancellationToken;

const PRESENCE_TTL: Duration = Duration::from_secs(45);
const TEST_INTERVAL: Duration = Duration::from_secs(30);
const EVENT_LIFETIME: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct Presence {
    session: SessionIdentity,
    until: Instant,
}
#[derive(Default)]
struct Transient {
    presence: HashMap<String, HashMap<String, Presence>>,
    last_test: HashMap<String, Instant>,
}
impl Transient {
    fn expire(&mut self, now: Instant) {
        self.presence.retain(|_, clients| {
            clients.retain(|_, value| value.until > now);
            !clients.is_empty()
        });
    }
    fn retain_subscribers(&mut self, current: &HashSet<String>) {
        self.presence.retain(|id, _| current.contains(id));
        self.last_test.retain(|id, _| current.contains(id));
    }
    fn update_presence(
        &mut self,
        login: &str,
        client: &str,
        session: Option<SessionIdentity>,
        now: Instant,
    ) {
        self.expire(now);
        if let Some(session) = session {
            if !self.presence.contains_key(login) && self.presence.len() >= 256 {
                return;
            }
            let clients = self.presence.entry(login.to_owned()).or_default();
            if !clients.contains_key(client) && clients.len() >= 16 {
                return;
            }
            clients.insert(
                client.to_owned(),
                Presence {
                    session,
                    until: now + PRESENCE_TTL,
                },
            );
        } else if let Some(clients) = self.presence.get_mut(login) {
            clients.remove(client);
            if clients.is_empty() {
                self.presence.remove(login);
            }
        }
    }
    fn watching(&mut self, login: &str, session: &SessionIdentity, now: Instant) -> bool {
        self.expire(now);
        self.presence
            .get(login)
            .is_some_and(|clients| clients.values().any(|value| &value.session == session))
    }
    fn test_admit(&mut self, login: &str, now: Instant) -> bool {
        self.last_test
            .retain(|_, at| now.duration_since(*at) < TEST_INTERVAL);
        if self.last_test.contains_key(login) || self.last_test.len() >= 256 {
            return false;
        }
        self.last_test.insert(login.to_owned(), now);
        true
    }
}

struct Inner {
    store: push_state::Store,
    client: push_transport::Client,
    receiver: Mutex<Option<mpsc::Receiver<CompletionEvent>>>,
    transient: Mutex<Transient>,
    stop: CancellationToken,
}
pub(crate) struct DeliveryContext {
    pub auth: Arc<AuthStore>,
    pub home: Hub,
    pub workspaces: Option<workspace::Store>,
    pub origin: String,
}

/// Install only after private state and native TLS initialization both succeed.
/// The concrete transport remains responsible for public-address pinning and TLS.
#[derive(Clone)]
pub struct Push(Arc<Inner>);
impl Push {
    pub fn new(
        store: push_state::Store,
        client: push_transport::Client,
        receiver: mpsc::Receiver<CompletionEvent>,
    ) -> Self {
        Self(Arc::new(Inner {
            store,
            client,
            receiver: Mutex::new(Some(receiver)),
            transient: Mutex::new(Transient::default()),
            stop: CancellationToken::new(),
        }))
    }
    pub(crate) fn store(&self) -> &push_state::Store {
        &self.0.store
    }
    pub(crate) fn start(
        &self,
        auth: Arc<AuthStore>,
        home: Hub,
        workspaces: Option<workspace::Store>,
        origin: String,
        shutdown: CancellationToken,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let mut receiver = self.0.receiver.lock().unwrap().take()?;
        let owner = self.clone();
        Some(tokio::spawn(async move {
            loop {
                let event = tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break,
                    _ = owner.0.stop.cancelled() => break,
                    event = receiver.recv() => match event { Some(event) => event, None => break },
                };
                let deadline = Instant::now() + EVENT_LIFETIME;
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break,
                    _ = owner.0.stop.cancelled() => break,
                    _ = tokio::time::sleep_until(deadline) => {},
                    _ = owner.deliver(event, &auth, &home, workspaces.as_ref(), &origin, deadline) => {},
                }
            }
            owner.0.stop.cancel();
            owner.0.client.shutdown();
        }))
    }
    pub(crate) async fn shutdown(&self, worker: Option<tokio::task::JoinHandle<()>>) {
        self.0.stop.cancel();
        self.0.client.shutdown();
        if let Some(worker) = worker {
            let _ = worker.await;
        }
        self.0.store.shutdown().await;
    }
    pub(crate) fn presence(&self, login: &str, client: &str, session: Option<SessionIdentity>) {
        self.0
            .transient
            .lock()
            .unwrap()
            .update_presence(login, client, session, Instant::now());
    }
    fn watching(&self, login: &str, session: &SessionIdentity) -> bool {
        self.0
            .transient
            .lock()
            .unwrap()
            .watching(login, session, Instant::now())
    }
    pub(crate) fn test_admit(&self, login: &str) -> bool {
        self.0
            .transient
            .lock()
            .unwrap()
            .test_admit(login, Instant::now())
    }
    pub(crate) fn forget(&self, login: &str) {
        let mut state = self.0.transient.lock().unwrap();
        state.presence.remove(login);
        state.last_test.remove(login);
    }
    pub(crate) async fn prune_transient(&self) {
        if let Ok(subs) = self.0.store.snapshot().await {
            let ids = subs.into_iter().map(|(id, _)| id).collect();
            self.0.transient.lock().unwrap().retain_subscribers(&ids);
        }
    }
    pub(crate) async fn subscription(
        &self,
        id: &str,
    ) -> Result<Option<Subscription>, push_state::Error> {
        self.0.store.subscription(id).await
    }
    async fn deliver(
        &self,
        event: CompletionEvent,
        auth: &Arc<AuthStore>,
        home: &Hub,
        workspaces: Option<&workspace::Store>,
        origin: &str,
        deadline: Instant,
    ) {
        let session = SessionIdentity {
            id: event.session.id,
            created_at: event.session.created_at,
        };
        let Some((generation, name)) = current_tab(home, &session) else {
            return;
        };
        let Ok(subs) = self.0.store.snapshot().await else {
            return;
        };
        let context = DeliveryContext {
            auth: auth.clone(),
            home: home.clone(),
            workspaces: workspaces.cloned(),
            origin: origin.to_owned(),
        };
        let mut loaded = HashMap::<String, Option<Vec<SessionIdentity>>>::new();
        for (id, sub) in subs {
            if self.0.stop.is_cancelled() || Instant::now() >= deadline {
                return;
            }
            let access = match auth.push_login_by_id(&id, now()) {
                Ok(Some(access)) => access,
                Ok(None) => {
                    let _ = self.0.store.remove(&id, Some(&sub.endpoint)).await;
                    self.forget(&id);
                    continue;
                }
                Err(_) => continue, // admission/storage uncertainty never prunes
            };
            if !loaded.contains_key(&access.profile) {
                let tabs = self
                    .workspace_tabs(home, workspaces, &access, generation, deadline)
                    .await;
                loaded.insert(access.profile.clone(), tabs);
            }
            if !loaded
                .get(&access.profile)
                .is_some_and(|tabs| tabs.as_ref().is_some_and(|tabs| tabs.contains(&session)))
                || self.watching(&id, &session)
            {
                continue;
            }
            let payload = json!({"type":"codex-complete", "tab_name":name,
                "session":session, "login_id":id, "event_id":event.id});
            let _ = self
                .send(
                    &context,
                    access,
                    sub,
                    payload,
                    Some((session.clone(), generation)),
                    Some(deadline),
                )
                .await;
        }
    }
    async fn workspace_tabs(
        &self,
        home: &Hub,
        workspaces: Option<&workspace::Store>,
        access: &SessionAccess,
        generation: Generation,
        deadline: Instant,
    ) -> Option<Vec<SessionIdentity>> {
        if access.profile.is_empty() {
            let request = p::Request {
                id: String::new(),
                operation: p::Operation::Workspace as i32,
                session: None,
                payload: Some(p::request::Payload::Workspace(p::WorkspaceRequest {
                    change: None,
                })),
            };
            let reply = tokio::time::timeout_at(deadline, home.request(generation, request))
                .await
                .ok()?
                .ok()?;
            if !reply.error.is_empty() || home.snapshot().generation != Some(generation) {
                return None;
            }
            let p::response::Result::Workspace(value) = reply.result.as_ref()? else {
                return None;
            };
            return Some(
                value
                    .tabs
                    .iter()
                    .map(|tab| SessionIdentity {
                        id: tab.id.clone(),
                        created_at: tab.created_at,
                    })
                    .collect(),
            );
        }
        let store = workspaces?;
        let hub = home.clone();
        let guard = access.clone();
        let stop = self.0.stop.clone();
        let result = tokio::time::timeout_at(
            deadline,
            store.sync(
                Some(access.profile.clone()),
                None,
                move || {
                    let snap = hub.snapshot();
                    if !snap.online || snap.generation != Some(generation) {
                        return Err(workspace::Error::Unavailable);
                    }
                    let raw = snap.catalog.ok_or(workspace::Error::Unavailable)?;
                    hmux_model::workspace::decode_catalog(&raw)
                        .map_err(|_| workspace::Error::Unavailable)
                },
                move || {
                    !stop.is_cancelled()
                        && Instant::now() < deadline
                        && now() < guard.expires_at
                        && !*guard.cancelled.borrow()
                        && guard.cancelled.has_changed().is_ok()
                },
            ),
        )
        .await
        .ok()?
        .ok()?;
        Some(result.tabs)
    }
    pub(crate) async fn send(
        &self,
        context: &DeliveryContext,
        access: SessionAccess,
        sub: Subscription,
        payload: Value,
        completion: Option<(SessionIdentity, Generation)>,
        deadline: Option<Instant>,
    ) -> bool {
        if self.0.stop.is_cancelled() {
            return false;
        }
        let raw = match serde_json::to_vec(&payload) {
            Ok(raw) => raw,
            Err(_) => return false,
        };
        let prepared = match self
            .0
            .store
            .prepare_for_delivery(&access, &sub, &context.origin, &raw)
            .await
        {
            Ok(prepared) => prepared,
            Err(_) => return false,
        };
        let owner = self.clone();
        let auth = context.auth.clone();
        let home = context.home.clone();
        let workspaces = context.workspaces.clone();
        let id = access.id.clone();
        let check_id = id.clone();
        let profile = access.profile.clone();
        let workspace_access = access.clone();
        let endpoint = sub.endpoint.clone();
        let expected = sub.clone();
        let status = self
            .0
            .client
            .send(prepared, access, deadline, move || async move {
                if owner.0.stop.is_cancelled() {
                    return Err(push_transport::Error::Cancelled);
                }
                // Workspace is the final async read. The exact subscription,
                // authoritative login, catalog and presence checks below never
                // await, so no store queue wait can stale that authorization.
                let membership = if let Some((ref session, generation)) = completion {
                    owner
                        .workspace_tabs(
                            &home,
                            workspaces.as_ref(),
                            &workspace_access,
                            generation,
                            deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(10)),
                        )
                        .await
                        .is_some_and(|tabs| tabs.contains(session))
                } else {
                    true
                };
                if !membership {
                    return Err(push_transport::Error::Unauthorized);
                }
                if !owner
                    .0
                    .store
                    .current_subscription(&check_id, &expected)
                    .map_err(map_store)?
                {
                    return Err(push_transport::Error::Unauthorized);
                }
                let current = auth
                    .push_login_by_id(&check_id, now())
                    .map_err(map_auth)?
                    .ok_or(push_transport::Error::Unauthorized)?;
                if current.profile != profile {
                    return Err(push_transport::Error::Unauthorized);
                }
                if let Some((session, generation)) = completion {
                    if current_tab(&home, &session)
                        .is_none_or(|(current_generation, _)| current_generation != generation)
                    {
                        return Err(push_transport::Error::Unauthorized);
                    }
                    if owner.watching(&check_id, &session) {
                        return Err(push_transport::Error::Unauthorized);
                    }
                }
                Ok(())
            })
            .await;
        match status {
            Ok(StatusCode::NOT_FOUND | StatusCode::GONE) => {
                let _ = self.0.store.remove(&id, Some(&endpoint)).await;
                self.prune_transient().await;
                false
            }
            Ok(status) => status.is_success(),
            Err(_) => false,
        }
    }
}

fn map_auth(error: StoreError) -> push_transport::Error {
    match error {
        StoreError::Busy => push_transport::Error::Busy,
        _ => push_transport::Error::Unavailable,
    }
}
fn map_store(error: push_state::Error) -> push_transport::Error {
    match error {
        push_state::Error::Busy => push_transport::Error::Busy,
        push_state::Error::Unauthorized => push_transport::Error::Unauthorized,
        push_state::Error::Invalid => push_transport::Error::Invalid,
        push_state::Error::Unavailable => push_transport::Error::Unavailable,
    }
}
fn current_tab(home: &Hub, session: &SessionIdentity) -> Option<(Generation, String)> {
    let snap = home.snapshot();
    if !snap.online {
        return None;
    }
    let generation = snap.generation?;
    let catalog: hmux_model::Catalog = serde_json::from_slice(&snap.catalog?).ok()?;
    let tab = catalog
        .sessions?
        .into_iter()
        .find(|tab| tab.id == session.id && tab.created_at == session.created_at)?;
    let name = if tab.alias.is_empty() {
        tab.name
    } else {
        tab.alias
    };
    if name.is_empty() {
        return None;
    }
    Some((generation, hmux_model::safe_text(&name, 120)))
}
pub(crate) fn test_event_id() -> Option<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(URL_SAFE_NO_PAD.encode(bytes))
}
fn now() -> DateTime<Utc> {
    std::time::SystemTime::now().into()
}

#[cfg(test)]
#[path = "push_tests.rs"]
mod tests;
