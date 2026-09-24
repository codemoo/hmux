//! One authenticated Home generation and bounded fan-out to HTTP owners.
//! Authorization, browser sockets, heartbeat and account revocation remain with
//! the gateway server. This owner never starts a Home replacement while active.
use crate::observation::{Event, Reporter, Span, Stage};
use bytes::Bytes;
use hmux_protocol::{
    actions::{self, ResponseContext},
    flow, legacy,
    protobuf::{self as pb, types as p, Direction, Negotiated},
    transport::{self, Incoming, Sender},
    wire,
};
use prost::Message as _;
use std::{
    collections::HashMap,
    fmt,
    sync::{
        atomic::{AtomicU16, AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

pub const MAX_PENDING: usize = 16;
pub const MAX_VIEWS: usize = wire::MAX_TERMINALS;
pub const MAX_UPLOADS: usize = 2;
pub const COMPLETION_EVENTS: usize = 64;
/// Across snapshots, queued/returned replies and output bytes, including slices
/// retained by callers after reconnect. Does not include parser/transient frames.
pub const RETAINED_PAYLOAD_BYTES: usize = 8 << 20;
// Data retains its 32-frame limit; reserve room for refresh/exit controls even
// when Home has sent its entire legal credit window. Match Go's 64 event slots.
const VIEW_FRAMES: usize = 2 * flow::FRAMES;
// Two in-flight progress events plus a reserved terminal result.
const UPLOAD_EVENTS: usize = 3;
const CATALOG_FRESH: Duration = Duration::from_secs(40);
const OUTPUT_CAP: &str = flow::CAPABILITY;
const UPLOAD_CAP: &str = "web-upload-v1";
const HOME_OFFLINE: u16 = wire::HOME_OFFLINE;
const OUTPUT_FULL: u16 = wire::OUTPUT_FULL;
const VIEW_EXITED: u16 = wire::VIEW_EXITED;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Generation(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Offline,
    Stale,
    Busy,
    Capacity,
    Unsupported,
    Invalid,
    Cancelled,
    Transport,
    OutputFull,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Offline => "Home is offline",
            Self::Stale => "Home connection changed",
            Self::Busy => "Home is busy",
            Self::Capacity => "Home lease limit reached",
            Self::Unsupported => "Home capability unavailable",
            Self::Invalid => "invalid Home message",
            Self::Cancelled => "Home request cancelled",
            Self::Transport => "Home transport failed",
            Self::OutputFull => "terminal output full",
        })
    }
}
impl std::error::Error for Error {}

#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}
struct Inner {
    reporter: Option<Reporter>,
    state: Mutex<State>,
    completions: mpsc::Sender<CompletionEvent>,
    payloads: Arc<PayloadBudget>,
}
#[derive(Default)]
struct PayloadBudget(AtomicUsize);
struct RetainedPayload {
    data: Vec<u8>,
    budget: Arc<PayloadBudget>,
    charged: usize,
}
impl RetainedPayload {
    fn charge(&mut self, capacity: usize) -> Result<(), Error> {
        let extra = capacity.saturating_sub(self.charged);
        self.budget
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(extra)
                    .filter(|next| *next <= RETAINED_PAYLOAD_BYTES)
            })
            .map_err(|_| Error::Capacity)?;
        self.charged += extra;
        Ok(())
    }
}
impl AsRef<[u8]> for RetainedPayload {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}
impl Drop for RetainedPayload {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.data));
        self.budget.0.fetch_sub(self.charged, Ordering::Relaxed);
    }
}
/// A typed reply keeps its allocation reservation until its last owner drops it,
/// including HTTP consumers that survive a Home reconnect.
#[derive(Clone)]
pub struct Reply(Arc<RetainedResponse>);
struct RetainedResponse {
    response: p::Response,
    budget: Arc<PayloadBudget>,
    charged: usize,
}
impl std::ops::Deref for Reply {
    type Target = p::Response;
    fn deref(&self) -> &Self::Target {
        &self.0.response
    }
}
impl Drop for RetainedResponse {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.response));
        self.budget.0.fetch_sub(self.charged, Ordering::Relaxed);
    }
}

struct State {
    next_generation: u64,
    next_id: u64,
    peer: Option<Peer>,
    pending: HashMap<String, Pending>,
    views: HashMap<String, ViewState>,
    uploads: HashMap<String, UploadState>,
    completion_admission: CompletionAdmission,
    catalog: Option<Bytes>,
    usage: [Option<Bytes>; 2],
    updated: Option<Instant>,
    output_cap: bool,
    upload_cap: bool,
}
#[derive(Clone)]
struct Peer {
    generation: Generation,
    sender: Sender,
    protocol: Negotiated,
}
struct Pending {
    generation: Generation,
    context: ResponseContext,
    reply: oneshot::Sender<Reply>,
}
struct ViewState {
    generation: Generation,
    events: mpsc::Sender<ViewEvent>,
    closed: Arc<AtomicU16>,
    queued_data: Arc<AtomicUsize>,
}
struct UploadState {
    generation: Generation,
    events: mpsc::Sender<UploadEvent>,
    terminal: bool,
}

/// The caller processes these on a separate bounded consumer. Notifications are
/// best effort: a full or absent consumer drops the event without blocking or
/// disconnecting the shared Home reader, matching Go's push queue policy.
pub struct CompletionEvent {
    pub id: String,
    pub session: p::Session,
    pub completed_at: String,
}

/// Admission precedes the lossy queue: stale/duplicate notifications cannot
/// evict a valid event. Fixed-size IDs avoid separate retained string buffers.
#[derive(Default)]
struct CompletionAdmission {
    seen: HashMap<[u8; 64], Instant>,
}
impl CompletionAdmission {
    fn enqueue(
        &mut self,
        event: p::Completion,
        sender: &mpsc::Sender<CompletionEvent>,
        wall: chrono::DateTime<chrono::Utc>,
        now: Instant,
    ) {
        if sender.is_closed() {
            return;
        }
        let Ok(key) = <[u8; 64]>::try_from(event.id.as_bytes()) else {
            return;
        };
        if !key.iter().all(u8::is_ascii_hexdigit) {
            return;
        }
        let Some(session) = event.session else {
            return;
        };
        if session.created_at < 1 || hmux_model::validate_session_id(&session.id).is_err() {
            return;
        }
        let Ok(completed) = chrono::DateTime::parse_from_rfc3339(&event.completed_at) else {
            return;
        };
        if completed.signed_duration_since(wall) > chrono::Duration::seconds(60)
            || wall.signed_duration_since(completed) > chrono::Duration::seconds(120)
        {
            return;
        }
        self.seen
            .retain(|_, at| now.duration_since(*at) <= Duration::from_secs(300));
        if self.seen.len() >= 4096 || self.seen.contains_key(&key) {
            return;
        }
        if sender
            .try_send(CompletionEvent {
                id: event.id,
                session,
                completed_at: event.completed_at,
            })
            .is_ok()
        {
            // A full/closed queue must not mark an unsent event as delivered.
            self.seen.insert(key, now);
        }
    }
}

pub struct Snapshot {
    pub connected: bool,
    pub online: bool,
    pub generation: Option<Generation>,
    pub catalog: Option<Bytes>,
    pub claude_usage: Option<Bytes>,
    pub codex_usage: Option<Bytes>,
    pub output_flow: bool,
    pub upload: bool,
}

/// Dropping closes this generation; `wait` joins its socket and hub reader.
pub struct HomeConnection {
    hub: Hub,
    generation: Generation,
    task: Option<JoinHandle<()>>,
}

/// A rejected transport remains owned until its HTTP handler joins cleanup.
pub struct AttachError {
    pub reason: Error,
    connection: transport::Connection,
}
impl fmt::Debug for AttachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachError")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}
impl AttachError {
    pub async fn close(self) {
        self.connection
            .sender
            .close_with(1008, "Home already connected")
            .await;
        drop(self.connection.reader);
        let _ = self.connection.task.await;
    }
}
impl HomeConnection {
    pub fn generation(&self) -> Generation {
        self.generation
    }
    pub async fn wait(mut self) {
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
impl Drop for HomeConnection {
    fn drop(&mut self) {
        self.hub.detach(self.generation);
    }
}

pub enum ViewEvent {
    Data(Bytes),
    Exit { error: String },
    RefreshResult { ok: bool },
}
pub struct ViewLease {
    hub: Hub,
    generation: Generation,
    id: String,
    events: mpsc::Receiver<ViewEvent>,
    closed: Arc<AtomicU16>,
    queued_data: Arc<AtomicUsize>,
    window: flow::OutputWindow,
    home_flow: bool,
    sent: bool,
    exited: bool,
}
impl ViewLease {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn generation(&self) -> Generation {
        self.generation
    }
    pub fn stalled(&self) -> bool {
        self.window.stalled(Instant::now().into_std())
    }
    pub fn output_flow(&self) -> bool {
        self.home_flow
    }
    pub async fn receive(&mut self) -> Result<ViewEvent, Error> {
        if self.exited {
            return Err(Error::Stale);
        }
        let event =
            self.events
                .recv()
                .await
                .ok_or_else(|| match self.closed.load(Ordering::Acquire) {
                    OUTPUT_FULL => Error::OutputFull,
                    HOME_OFFLINE => Error::Offline,
                    _ => Error::Stale,
                })?;
        if matches!(event, ViewEvent::Data(_)) {
            self.queued_data.fetch_sub(1, Ordering::AcqRel);
        }
        self.hub.peer(self.generation)?;
        if self.closed.load(Ordering::Acquire) == OUTPUT_FULL {
            return Err(Error::OutputFull);
        }
        if let ViewEvent::Data(data) = &event {
            if self.home_flow && !self.window.reserve(data.len(), Instant::now().into_std()) {
                self.hub
                    .remove_view(self.generation, &self.id, OUTPUT_FULL, true);
                return Err(Error::OutputFull);
            }
        }
        if matches!(event, ViewEvent::Exit { .. }) {
            self.exited = true;
        }
        Ok(event)
    }
    /// The exact oldest rendered frame size must be acknowledged in FIFO order.
    pub async fn acknowledge(&mut self, n: i64) -> Result<(), Error> {
        self.hub.ensure_view(self.generation, &self.id)?;
        if !self.home_flow {
            return Err(Error::Unsupported);
        }
        if !self.window.acknowledge(n) {
            self.hub
                .remove_view(self.generation, &self.id, OUTPUT_FULL, true);
            return Err(Error::Invalid);
        }
        self.send(p::envelope::Body::OutputAck(p::Ack {
            id: self.id.clone(),
            received: n,
        }))
        .await
    }
    pub async fn input(&self, data: Bytes) -> Result<(), Error> {
        self.send(p::envelope::Body::TerminalInput(p::Data {
            id: self.id.clone(),
            data,
        }))
        .await
    }
    pub async fn resize(&self, cols: u32, rows: u32) -> Result<(), Error> {
        self.send(p::envelope::Body::Resize(p::Resize {
            id: self.id.clone(),
            cols,
            rows,
        }))
        .await
    }
    pub async fn refresh(&self) -> Result<(), Error> {
        self.send(p::envelope::Body::Refresh(p::Reference {
            id: self.id.clone(),
        }))
        .await
    }
    async fn send(&self, body: p::envelope::Body) -> Result<(), Error> {
        self.hub.ensure_view(self.generation, &self.id)?;
        self.hub.send(self.generation, body).await
    }
}
impl Drop for ViewLease {
    fn drop(&mut self) {
        self.hub
            .remove_view(self.generation, &self.id, VIEW_EXITED, self.sent);
    }
}

pub enum UploadEvent {
    Ready,
    Ack(i64),
    Complete(Reply),
    Error(Reply),
}
pub struct UploadLease {
    hub: Hub,
    generation: Generation,
    id: String,
    events: mpsc::Receiver<UploadEvent>,
    sent: bool,
    terminal: bool,
}
impl UploadLease {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub async fn receive(&mut self) -> Result<UploadEvent, Error> {
        if self.terminal {
            return Err(Error::Stale);
        }
        self.hub.peer(self.generation)?;
        let event = self.events.recv().await.ok_or(Error::Stale)?;
        self.hub.peer(self.generation)?;
        if matches!(event, UploadEvent::Complete(_) | UploadEvent::Error(_)) {
            self.terminal = true;
        }
        Ok(event)
    }
    pub async fn data(&self, data: Bytes) -> Result<(), Error> {
        self.hub.ensure_upload(self.generation, &self.id)?;
        self.hub
            .send(
                self.generation,
                p::envelope::Body::UploadData(p::Data {
                    id: self.id.clone(),
                    data,
                }),
            )
            .await
    }
    pub async fn finish(&self) -> Result<(), Error> {
        self.hub.ensure_upload(self.generation, &self.id)?;
        self.hub
            .send(
                self.generation,
                p::envelope::Body::UploadFinish(p::Reference {
                    id: self.id.clone(),
                }),
            )
            .await
    }
}
impl Drop for UploadLease {
    fn drop(&mut self) {
        self.hub.remove_upload(self.generation, &self.id, self.sent);
    }
}

struct DetachOnExit {
    hub: Hub,
    generation: Generation,
}
impl Drop for DetachOnExit {
    fn drop(&mut self) {
        self.hub.detach(self.generation);
    }
}

struct PendingGuard {
    hub: Hub,
    generation: Generation,
    id: String,
    sent: bool,
    cancel_on_drop: bool,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.hub.remove_pending(self.generation, &self.id);
        if self.sent && self.cancel_on_drop {
            self.hub.cleanup(
                self.generation,
                p::envelope::Body::Cancel(p::Reference {
                    id: self.id.clone(),
                }),
            );
        }
    }
}

impl Hub {
    pub fn new() -> (Self, mpsc::Receiver<CompletionEvent>) {
        Self::with_reporter(None)
    }
    pub fn with_reporter(reporter: Option<Reporter>) -> (Self, mpsc::Receiver<CompletionEvent>) {
        let (completions, receiver) = mpsc::channel(COMPLETION_EVENTS);
        let state = State {
            next_generation: 0,
            next_id: 0,
            peer: None,
            pending: HashMap::new(),
            views: HashMap::new(),
            uploads: HashMap::new(),
            completion_admission: CompletionAdmission::default(),
            catalog: None,
            usage: [None, None],
            updated: None,
            output_cap: false,
            upload_cap: false,
        };
        (
            Self {
                inner: Arc::new(Inner {
                    reporter,
                    state: Mutex::new(state),
                    completions,
                    payloads: Arc::new(PayloadBudget::default()),
                }),
            },
            receiver,
        )
    }
    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
    pub fn retained_payload_bytes(&self) -> usize {
        self.inner.payloads.0.load(Ordering::Relaxed)
    }
    fn retain(&self, raw: Bytes) -> Result<Bytes, Error> {
        if raw.is_empty() {
            return Ok(Bytes::new());
        }
        let mut payload = RetainedPayload {
            data: Vec::new(),
            budget: self.inner.payloads.clone(),
            charged: 0,
        };
        payload.charge(raw.len())?;
        payload
            .data
            .try_reserve_exact(raw.len())
            .map_err(|_| Error::Capacity)?;
        payload.charge(payload.data.capacity())?;
        payload.data.extend_from_slice(&raw);
        // Decoded protobuf Bytes can share a larger source allocation. Detach
        // the field before retaining it; unknown wire fields remain rejected.
        // Accounting follows the owned allocation through clones and slices.
        Ok(Bytes::from_owner(payload))
    }
    fn retain_response(&self, response: p::Response) -> Result<Reply, Error> {
        let charged = actions::response_retained_bytes(&response)
            .checked_add(std::mem::size_of::<RetainedResponse>() + 2 * std::mem::size_of::<usize>())
            .ok_or(Error::Capacity)?;
        self.inner
            .payloads
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(charged)
                    .filter(|next| *next <= RETAINED_PAYLOAD_BYTES)
            })
            .map_err(|_| Error::Capacity)?;
        Ok(Reply(Arc::new(RetainedResponse {
            response,
            budget: self.inner.payloads.clone(),
            charged,
        })))
    }
    pub fn attach(&self, connection: transport::Connection) -> Result<HomeConnection, AttachError> {
        let protocol = connection.protocol();
        let generation = {
            let mut state = self.state();
            if state.peer.is_some() {
                return Err(AttachError {
                    reason: Error::Busy,
                    connection,
                });
            }
            let Some(next) = state.next_generation.checked_add(1) else {
                return Err(AttachError {
                    reason: Error::Capacity,
                    connection,
                });
            };
            state.next_generation = next;
            let generation = Generation(state.next_generation);
            state.peer = Some(Peer {
                generation,
                sender: connection.sender.clone(),
                protocol,
            });
            generation
        };
        let hub = self.clone();
        if let Some(report) = &self.inner.reporter {
            report(Event {
                stage: Stage::HomeConnected,
                operation: None,
                connection: generation.0,
                reason: None,
                duration_ms: 0,
                send_ms: 0,
            });
        }
        let task = tokio::spawn(async move {
            let _detach = DetachOnExit {
                hub: hub.clone(),
                generation,
            };
            let mut lifetime = Span::new(
                hub.inner.reporter.clone(),
                Stage::HomeDisconnected,
                None,
                generation.0,
            );
            let transport::Connection {
                sender,
                mut reader,
                task,
            } = connection;
            let reason = loop {
                let incoming = reader.receive().await;
                match incoming {
                    Ok(Incoming::Json(message)) if protocol == Negotiated::JsonV1 => {
                        // v1 response bodies have no operation discriminator. Only
                        // the live generation's pending request may select a decoder.
                        let context = if message.kind == "response" {
                            let state = hub.state();
                            match state.pending.get(&message.id) {
                                Some(pending) if pending.generation == generation => {
                                    Some(pending.context)
                                }
                                _ => continue, // Cancelled or unmatched late reply.
                            }
                        } else {
                            None
                        };
                        match legacy::from_json_with_context(message, Direction::ToGateway, context)
                        {
                            Ok(message) => {
                                if let Err(error) = hub.dispatch(generation, message) {
                                    break error;
                                }
                            }
                            Err(_) => break Error::Invalid,
                        }
                    }
                    Ok(Incoming::Protobuf(message)) if protocol == Negotiated::ProtobufV2 => {
                        if let Err(error) = hub.dispatch(generation, message) {
                            break error;
                        }
                    }
                    Err(error) => break map_transport(error),
                    _ => break Error::Invalid,
                }
            };
            sender.close();
            let _ = task.await;
            lifetime.finish::<()>(&Err(reason));
        });
        Ok(HomeConnection {
            hub: self.clone(),
            generation,
            task: Some(task),
        })
    }
    fn detach(&self, generation: Generation) {
        let mut state = self.state();
        if state
            .peer
            .as_ref()
            .is_none_or(|peer| peer.generation != generation)
        {
            return;
        }
        if let Some(peer) = state.peer.take() {
            peer.sender.close();
        }
        state.pending.clear();
        for view in state.views.values() {
            view.closed.store(HOME_OFFLINE, Ordering::Release);
        }
        state.views.clear();
        state.uploads.clear();
        state.catalog = None;
        state.usage = [None, None];
        state.updated = None;
        state.output_cap = false;
        state.upload_cap = false;
    }
    pub fn snapshot(&self) -> Snapshot {
        let state = self.state();
        Snapshot {
            connected: state.peer.is_some(),
            online: state.peer.is_some()
                && state.updated.is_some_and(|at| at.elapsed() < CATALOG_FRESH),
            generation: state.peer.as_ref().map(|peer| peer.generation),
            catalog: state.catalog.clone(),
            claude_usage: state.usage[0].clone(),
            codex_usage: state.usage[1].clone(),
            output_flow: state.output_cap,
            upload: state.upload_cap,
        }
    }
    fn peer(&self, generation: Generation) -> Result<Peer, Error> {
        let state = self.state();
        let peer = state.peer.as_ref().ok_or(Error::Offline)?;
        if peer.generation != generation {
            return Err(Error::Stale);
        }
        Ok(peer.clone())
    }
    fn next_id(state: &mut State, generation: Generation) -> Result<String, Error> {
        state.next_id = state.next_id.checked_add(1).ok_or(Error::Capacity)?;
        Ok(format!("{:016x}{:016x}", generation.0, state.next_id))
    }
    fn ensure_view(&self, generation: Generation, id: &str) -> Result<(), Error> {
        let state = self.state();
        if state
            .peer
            .as_ref()
            .is_none_or(|peer| peer.generation != generation)
        {
            return Err(Error::Stale);
        }
        if state
            .views
            .get(id)
            .is_none_or(|view| view.generation != generation)
        {
            return Err(Error::Stale);
        }
        Ok(())
    }
    fn ensure_upload(&self, generation: Generation, id: &str) -> Result<(), Error> {
        self.check_upload(generation, id, false)
    }
    fn check_upload(
        &self,
        generation: Generation,
        id: &str,
        allow_terminal: bool,
    ) -> Result<(), Error> {
        let state = self.state();
        if state
            .peer
            .as_ref()
            .is_none_or(|peer| peer.generation != generation)
        {
            return Err(Error::Stale);
        }
        if state.uploads.get(id).is_none_or(|upload| {
            upload.generation != generation || (!allow_terminal && upload.terminal)
        }) {
            return Err(Error::Stale);
        }
        Ok(())
    }
    fn remove_pending(&self, generation: Generation, id: &str) {
        let mut state = self.state();
        if state
            .pending
            .get(id)
            .is_some_and(|pending| pending.generation == generation)
        {
            state.pending.remove(id);
        }
    }
    fn remove_view(&self, generation: Generation, id: &str, code: u16, cleanup: bool) {
        let removed = {
            let mut state = self.state();
            if state
                .views
                .get(id)
                .is_some_and(|view| view.generation == generation)
            {
                let view = state.views.remove(id);
                if let Some(view) = &view {
                    view.closed.store(code, Ordering::Release);
                }
                view.is_some()
            } else {
                false
            }
        };
        if removed && cleanup {
            self.cleanup(
                generation,
                p::envelope::Body::Close(p::Reference { id: id.to_owned() }),
            );
        }
    }
    fn remove_upload(&self, generation: Generation, id: &str, cleanup: bool) {
        let should_cancel = {
            let mut state = self.state();
            if state
                .uploads
                .get(id)
                .is_some_and(|upload| upload.generation == generation)
            {
                state
                    .uploads
                    .remove(id)
                    .is_some_and(|upload| !upload.terminal)
            } else {
                false
            }
        };
        if should_cancel && cleanup {
            self.cleanup(
                generation,
                p::envelope::Body::UploadCancel(p::Reference { id: id.to_owned() }),
            );
        }
    }
    fn cleanup(&self, generation: Generation, body: p::envelope::Body) {
        let Ok(peer) = self.peer(generation) else {
            return;
        };
        let envelope = p::Envelope {
            version: pb::VERSION,
            body: Some(body),
        };
        let Ok(raw) = encode(peer.protocol, &envelope) else {
            peer.sender.close();
            return;
        };
        let result = peer
            .sender
            .try_reserve(raw.len())
            .and_then(|reservation| reservation.submit_detached(&raw, CancellationToken::new()));
        if result.is_err() {
            peer.sender.close();
        }
    }
    fn submit(
        &self,
        generation: Generation,
        body: p::envelope::Body,
    ) -> Result<transport::Receipt, Error> {
        let peer = self.peer(generation)?;
        let envelope = p::Envelope {
            version: pb::VERSION,
            body: Some(body),
        };
        // Conservative v1 upper bound permits admission before serialization.
        // Large valid frames can retry with their exact encoded length.
        let estimate = match peer.protocol {
            Negotiated::ProtobufV2 => envelope.encoded_len(),
            Negotiated::JsonV1 => envelope
                .encoded_len()
                .saturating_mul(6)
                .saturating_add(4096)
                .min(wire::MAX_MESSAGE),
        };
        let mut reservation = peer.sender.try_reserve(estimate).map_err(map_transport)?;
        let raw = encode(peer.protocol, &envelope)?;
        if raw.len() > estimate {
            drop(reservation);
            reservation = peer.sender.try_reserve(raw.len()).map_err(map_transport)?;
        }
        // A detach after peer selection closes this sender before it can reach a
        // new generation. Recheck to return Stale rather than claim success.
        self.peer(generation)?;
        reservation
            .submit(&raw, CancellationToken::new())
            .map_err(map_transport)
    }
    async fn send(&self, generation: Generation, body: p::envelope::Body) -> Result<(), Error> {
        let receipt = self.submit(generation, body)?;
        receipt.wait().await.map_err(map_transport)?;
        self.peer(generation)?;
        Ok(())
    }
    pub async fn request(
        &self,
        generation: Generation,
        request: p::Request,
    ) -> Result<Reply, Error> {
        let mut span = Span::new(
            self.inner.reporter.clone(),
            Stage::RequestComplete,
            p::Operation::try_from(request.operation).ok(),
            generation.0,
        );
        let result = self.request_observed(generation, request, &mut span).await;
        span.finish(&result);
        if result
            .as_ref()
            .is_ok_and(|response| !response.error.is_empty())
        {
            span.finish::<()>(&Err(Error::Invalid));
        }
        result
    }
    async fn request_observed(
        &self,
        generation: Generation,
        mut request: p::Request,
        span: &mut Span,
    ) -> Result<Reply, Error> {
        let (id, reply) = {
            let mut state = self.state();
            if state
                .peer
                .as_ref()
                .is_none_or(|peer| peer.generation != generation)
            {
                return Err(Error::Stale);
            }
            if state.pending.len() >= MAX_PENDING {
                return Err(Error::Busy);
            }
            let id = Self::next_id(&mut state, generation)?;
            let (send, reply) = oneshot::channel();
            state.pending.insert(
                id.clone(),
                Pending {
                    generation,
                    context: ResponseContext::Operation(
                        p::Operation::try_from(request.operation).map_err(|_| Error::Invalid)?,
                    ),
                    reply: send,
                },
            );
            (id, reply)
        };
        request.id = id.clone();
        let mut guard = PendingGuard {
            hub: self.clone(),
            generation,
            id,
            sent: false,
            cancel_on_drop: true,
        };
        let receipt = self.submit(generation, p::envelope::Body::Request(Box::new(request)))?;
        guard.sent = true;
        receipt.wait().await.map_err(map_transport)?;
        span.sent();
        let response = reply.await.map_err(|_| Error::Offline)?;
        self.peer(generation)?;
        guard.cancel_on_drop = false;
        Ok(response)
    }
    pub async fn open_view(
        &self,
        generation: Generation,
        open: p::TerminalOpen,
    ) -> Result<ViewLease, Error> {
        let mut span = Span::new(
            self.inner.reporter.clone(),
            Stage::TerminalOpenComplete,
            None,
            generation.0,
        );
        let result = self.open_view_observed(generation, open, &mut span).await;
        span.finish(&result);
        result
    }
    async fn open_view_observed(
        &self,
        generation: Generation,
        mut open: p::TerminalOpen,
        span: &mut Span,
    ) -> Result<ViewLease, Error> {
        let (id, events, closed, queued_data, reply, home_flow) = {
            let mut state = self.state();
            if state
                .peer
                .as_ref()
                .is_none_or(|peer| peer.generation != generation)
            {
                return Err(Error::Stale);
            }
            if state.views.len() >= MAX_VIEWS {
                return Err(Error::Capacity);
            }
            if state.pending.len() >= MAX_PENDING {
                return Err(Error::Busy);
            }
            let id = Self::next_id(&mut state, generation)?;
            let (send, events) = mpsc::channel(VIEW_FRAMES);
            let (reply_send, reply) = oneshot::channel();
            let closed = Arc::new(AtomicU16::new(0));
            let queued_data = Arc::new(AtomicUsize::new(0));
            state.views.insert(
                id.clone(),
                ViewState {
                    generation,
                    events: send,
                    closed: closed.clone(),
                    queued_data: queued_data.clone(),
                },
            );
            state.pending.insert(
                id.clone(),
                Pending {
                    generation,
                    context: ResponseContext::TerminalOpen,
                    reply: reply_send,
                },
            );
            (id, events, closed, queued_data, reply, state.output_cap)
        };
        let mut lease = ViewLease {
            hub: self.clone(),
            generation,
            id: id.clone(),
            events,
            closed,
            queued_data,
            window: flow::OutputWindow::default(),
            home_flow,
            sent: false,
            exited: false,
        };
        let mut guard = PendingGuard {
            hub: self.clone(),
            generation,
            id: id.clone(),
            sent: false,
            cancel_on_drop: false,
        };
        open.id = id;
        if home_flow {
            open.capabilities = vec![OUTPUT_CAP.to_owned()];
        } else {
            open.capabilities.clear();
        }
        let receipt = self.submit(generation, p::envelope::Body::TerminalOpen(open))?;
        lease.sent = true;
        guard.sent = true;
        receipt.wait().await.map_err(map_transport)?;
        span.sent();
        let response = reply.await.map_err(|_| Error::Offline)?;
        self.peer(generation)?;
        if !response.error.is_empty() {
            return Err(Error::Invalid);
        }
        Ok(lease)
    }
    pub async fn open_upload(
        &self,
        generation: Generation,
        mut start: p::UploadStart,
    ) -> Result<UploadLease, Error> {
        let (id, events) = {
            let mut state = self.state();
            if state
                .peer
                .as_ref()
                .is_none_or(|peer| peer.generation != generation)
            {
                return Err(Error::Stale);
            }
            if !state.upload_cap {
                return Err(Error::Unsupported);
            }
            if state.uploads.len() >= MAX_UPLOADS {
                return Err(Error::Capacity);
            }
            let id = Self::next_id(&mut state, generation)?;
            let (send, events) = mpsc::channel(UPLOAD_EVENTS);
            state.uploads.insert(
                id.clone(),
                UploadState {
                    generation,
                    events: send,
                    terminal: false,
                },
            );
            (id, events)
        };
        let mut lease = UploadLease {
            hub: self.clone(),
            generation,
            id: id.clone(),
            events,
            sent: false,
            terminal: false,
        };
        start.id = id.clone();
        if let Some(header) = &mut start.header {
            header.request_id = id;
        }
        let receipt = self.submit(generation, p::envelope::Body::UploadStart(start))?;
        lease.sent = true;
        receipt.wait().await.map_err(map_transport)?;
        self.check_upload(generation, &lease.id, true)?;
        Ok(lease)
    }
    fn cache_snapshot(
        &self,
        generation: Generation,
        provider: Option<usize>,
        raw: Bytes,
    ) -> Result<(), Error> {
        let mut state = self.state();
        if state
            .peer
            .as_ref()
            .is_none_or(|peer| peer.generation != generation)
        {
            return Err(Error::Stale);
        }
        // Drop our replaceable reference before charging the new allocation.
        // External readers still owning Bytes keep their budget reservation.
        let slot = match provider {
            Some(index) => &mut state.usage[index],
            None => &mut state.catalog,
        };
        drop(slot.take());
        *slot = Some(self.retain(raw)?);
        if provider.is_none() {
            state.updated = Some(Instant::now());
        }
        Ok(())
    }
    fn dispatch(&self, generation: Generation, envelope: p::Envelope) -> Result<(), Error> {
        use p::envelope::Body::*;
        let body = envelope.body.ok_or(Error::Invalid)?;
        // Snapshot trees are consumed outside the Hub lock. Retain only bounded
        // browser-facing JSON caches under the existing shared allocation budget.
        let body = match body {
            Catalog(catalog) => {
                return self.cache_snapshot(
                    generation,
                    None,
                    legacy::catalog_payload(*catalog).map_err(|_| Error::Invalid)?,
                )
            }
            Usage(usage) => {
                let index = match p::Provider::try_from(usage.provider) {
                    Ok(p::Provider::Claude) => 0,
                    Ok(p::Provider::Codex) => 1,
                    _ => return Err(Error::Invalid),
                };
                return self.cache_snapshot(
                    generation,
                    Some(index),
                    legacy::usage_payload(*usage).map_err(|_| Error::Invalid)?,
                );
            }
            body => body,
        };
        let mut cleanup = None;
        {
            let mut state = self.state();
            if state
                .peer
                .as_ref()
                .is_none_or(|peer| peer.generation != generation)
            {
                return Err(Error::Stale);
            }
            match body {
                Hello(hello) => {
                    state.output_cap = hello.capabilities.iter().any(|cap| cap == OUTPUT_CAP);
                    state.upload_cap = hello.capabilities.iter().any(|cap| cap == UPLOAD_CAP);
                }
                UsageUnavailable(_) => state.usage = [None, None],
                Response(response) => {
                    if let Some(pending) = state.pending.remove(&response.id) {
                        if pending.generation == generation {
                            if !actions::response_matches(&response, pending.context) {
                                return Err(Error::Invalid);
                            }
                            let response = self.retain_response(response)?;
                            let _ = pending.reply.send(response);
                        }
                    }
                }
                TerminalOutput(data) => {
                    if let Some(view) = state.views.get(&data.id) {
                        if view.generation == generation {
                            let admitted = !data.data.is_empty()
                                && data.data.len() <= flow::CHUNK
                                && view
                                    .queued_data
                                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                                        (n < flow::FRAMES).then_some(n + 1)
                                    })
                                    .is_ok();
                            let failed = !admitted
                                || self.retain(data.data).map_or(true, |data| {
                                    view.events.try_send(ViewEvent::Data(data)).is_err()
                                });
                            if admitted && failed {
                                view.queued_data.fetch_sub(1, Ordering::AcqRel);
                            }
                            if failed {
                                cleanup = Some(p::envelope::Body::Close(p::Reference {
                                    id: data.id.clone(),
                                }));
                                if let Some(view) = state.views.remove(&data.id) {
                                    view.closed.store(OUTPUT_FULL, Ordering::Release);
                                }
                            }
                        }
                    }
                }
                TerminalExit(response) => {
                    if let Some(view) = state.views.get(&response.id) {
                        let sent = view
                            .events
                            .try_send(ViewEvent::Exit {
                                error: response.error,
                            })
                            .is_ok();
                        if sent {
                            // Keep the lease bound until its browser drains queued
                            // data and the exit frame; final data may still need ACKs.
                            view.closed.store(VIEW_EXITED, Ordering::Release);
                        } else if let Some(view) = state.views.remove(&response.id) {
                            view.closed.store(OUTPUT_FULL, Ordering::Release);
                        }
                    }
                }
                RefreshResult(response) => {
                    if let Some(view) = state.views.get(&response.id) {
                        if view
                            .events
                            .try_send(ViewEvent::RefreshResult {
                                ok: response.error.is_empty(),
                            })
                            .is_err()
                        {
                            cleanup = Some(p::envelope::Body::Close(p::Reference {
                                id: response.id.clone(),
                            }));
                            if let Some(view) = state.views.remove(&response.id) {
                                view.closed.store(OUTPUT_FULL, Ordering::Release);
                            }
                        }
                    }
                }
                UploadReady(reference) => {
                    if let Some(upload) = state.uploads.get(&reference.id) {
                        if !upload.terminal
                            && (upload.events.capacity() <= 1
                                || upload.events.try_send(UploadEvent::Ready).is_err())
                        {
                            cleanup = Some(p::envelope::Body::UploadCancel(p::Reference {
                                id: reference.id.clone(),
                            }));
                            state.uploads.remove(&reference.id);
                        }
                    }
                }
                UploadAck(ack) => {
                    if let Some(upload) = state.uploads.get(&ack.id) {
                        if !upload.terminal
                            && (upload.events.capacity() <= 1
                                || upload
                                    .events
                                    .try_send(UploadEvent::Ack(ack.received))
                                    .is_err())
                        {
                            cleanup = Some(p::envelope::Body::UploadCancel(p::Reference {
                                id: ack.id.clone(),
                            }));
                            state.uploads.remove(&ack.id);
                        }
                    }
                }
                UploadComplete(response) => {
                    if let Some(upload) = state.uploads.get_mut(&response.id) {
                        if !upload.terminal {
                            let response = self.retain_response(response)?;
                            upload.terminal = true;
                            upload
                                .events
                                .try_send(UploadEvent::Complete(response))
                                .map_err(|_| Error::Capacity)?;
                        }
                    }
                }
                UploadError(response) => {
                    if let Some(upload) = state.uploads.get_mut(&response.id) {
                        if !upload.terminal {
                            let response = self.retain_response(response)?;
                            upload.terminal = true;
                            upload
                                .events
                                .try_send(UploadEvent::Error(response))
                                .map_err(|_| Error::Capacity)?;
                        }
                    }
                }
                TaskComplete(completion) => {
                    state.completion_admission.enqueue(
                        completion,
                        &self.inner.completions,
                        std::time::SystemTime::now().into(),
                        Instant::now(),
                    );
                }
                _ => return Err(Error::Invalid),
            }
        }
        if let Some(body) = cleanup {
            self.cleanup(generation, body);
        }
        Ok(())
    }
}

fn map_transport(error: transport::Error) -> Error {
    match error {
        transport::Error::Busy => Error::Busy,
        transport::Error::Cancelled => Error::Cancelled,
        transport::Error::Size | transport::Error::Protocol => Error::Invalid,
        transport::Error::Closed => Error::Offline,
        transport::Error::QueueTimeout
        | transport::Error::WriteTimeout
        | transport::Error::Transport => Error::Transport,
    }
}
fn encode(protocol: Negotiated, envelope: &p::Envelope) -> Result<Bytes, Error> {
    match protocol {
        Negotiated::JsonV1 => legacy::to_json(envelope.clone(), Direction::ToHome)
            .map_err(|_| Error::Invalid)?
            .encode()
            .map(Bytes::from)
            .map_err(|_| Error::Invalid),
        Negotiated::ProtobufV2 => {
            pb::encode(envelope, Direction::ToHome).map_err(|_| Error::Invalid)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn completion_admission_matches_go_freshness_dedup_and_capacity() {
        let wall = chrono::DateTime::<chrono::Utc>::from_timestamp(1_790_000_000, 0).unwrap();
        let now = Instant::now();
        let event = |index: usize, at: chrono::DateTime<chrono::Utc>| p::Completion {
            id: format!("{index:064x}"),
            session: Some(p::Session {
                id: "$1".into(),
                created_at: 42,
            }),
            completed_at: at.to_rfc3339(),
        };
        let mut admission = CompletionAdmission::default();
        let (sender, mut receiver) = mpsc::channel(4);
        for _ in 0..80 {
            admission.enqueue(event(0, wall), &sender, wall, now);
        }
        admission.enqueue(
            event(1, wall - chrono::Duration::seconds(120)),
            &sender,
            wall,
            now,
        );
        admission.enqueue(
            event(2, wall + chrono::Duration::seconds(60)),
            &sender,
            wall,
            now,
        );
        admission.enqueue(
            event(
                3,
                wall - chrono::Duration::seconds(120) - chrono::Duration::nanoseconds(1),
            ),
            &sender,
            wall,
            now,
        );
        admission.enqueue(
            event(
                4,
                wall + chrono::Duration::seconds(60) + chrono::Duration::nanoseconds(1),
            ),
            &sender,
            wall,
            now,
        );
        for id in ["a".repeat(63), "g".repeat(64), "é".repeat(32)] {
            let mut invalid = event(5, wall);
            invalid.id = id;
            admission.enqueue(invalid, &sender, wall, now);
        }
        let mut invalid = event(5, wall);
        invalid.session.as_mut().unwrap().created_at = 0;
        admission.enqueue(invalid, &sender, wall, now);
        let mut invalid = event(6, wall);
        invalid.completed_at = "not-a-timestamp".into();
        admission.enqueue(invalid, &sender, wall, now);
        assert_eq!(receiver.len(), 3);
        assert_eq!(admission.seen.len(), 3);

        let later = wall + chrono::Duration::seconds(300);
        admission.enqueue(
            event(0, later),
            &sender,
            later,
            now + Duration::from_secs(300),
        );
        assert_eq!(receiver.len(), 3); // Go retains at exactly five minutes.
        let tick = now + Duration::from_secs(300) + Duration::from_nanos(1);
        admission.enqueue(event(0, later), &sender, later, tick);
        assert_eq!(receiver.len(), 4);
        assert_eq!(admission.seen.len(), 1);
        admission.enqueue(event(99, later), &sender, later, tick);
        assert_eq!(admission.seen.len(), 1); // Full queue does not consume retry.
        while receiver.try_recv().is_ok() {}
        admission.enqueue(event(99, later), &sender, later, tick);
        assert_eq!(receiver.try_recv().unwrap().id, format!("{:064x}", 99));

        let mut admission = CompletionAdmission::default();
        for index in 0..4096 {
            admission.enqueue(event(index, wall), &sender, wall, now);
            assert_eq!(receiver.try_recv().unwrap().id, format!("{index:064x}"));
        }
        admission.enqueue(event(4096, wall), &sender, wall, now);
        assert!(receiver.try_recv().is_err());
        assert_eq!(admission.seen.len(), 4096);
        admission.enqueue(event(4096, later), &sender, later, tick);
        assert_eq!(receiver.try_recv().unwrap().id, format!("{:064x}", 4096));
        assert_eq!(admission.seen.len(), 1);
        // Go validates hex but deduplicates the original case-sensitive ID.
        for id in ["a".repeat(64), "A".repeat(64)] {
            let mut value = event(0, later);
            value.id = id.clone();
            admission.enqueue(value, &sender, later, tick);
            assert_eq!(receiver.try_recv().unwrap().id, id);
        }
    }

    struct Source {
        bytes: Vec<u8>,
        dropped: Arc<AtomicBool>,
    }
    impl AsRef<[u8]> for Source {
        fn as_ref(&self) -> &[u8] {
            &self.bytes
        }
    }
    impl Drop for Source {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn retained_slice_releases_source_and_keeps_its_own_budget_until_last_clone() {
        let (hub, _events) = Hub::new();
        let dropped = Arc::new(AtomicBool::new(false));
        let raw = Bytes::from_owner(Source {
            bytes: vec![b'x'; wire::MAX_MESSAGE],
            dropped: dropped.clone(),
        });
        let retained = hub.retain(raw.slice(..4096)).unwrap();
        drop(raw);
        assert!(dropped.load(Ordering::Relaxed));
        assert_eq!(hub.retained_payload_bytes(), 4096);
        let tiny = retained.slice(..1);
        drop(retained);
        assert_eq!(hub.retained_payload_bytes(), 4096);
        drop(tiny);
        assert_eq!(hub.retained_payload_bytes(), 0);
    }
}
