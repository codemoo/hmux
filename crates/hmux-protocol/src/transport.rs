//! One I/O task per authenticated Home connection. Never one task per send.
//! Caller cancellation can discard queued work, but cannot interrupt a frame
//! once the writer starts it. This module does not authenticate an upgrade.

use crate::{protobuf, wire};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::hash_map::RandomState,
    future::Future,
    hash::BuildHasher,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, oneshot, Notify, OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
    time::{timeout_at, Instant, MissedTickBehavior},
};
use tokio_tungstenite::{
    tungstenite::{protocol::WebSocketConfig, Message},
    WebSocketStream,
};
use tokio_util::sync::CancellationToken;

pub const QUEUED_FRAMES: usize = 16;
pub const QUEUED_BYTES: usize = wire::MAX_MESSAGE;
pub const QUEUE_DEADLINE: Duration = Duration::from_secs(5);
pub const WRITE_DEADLINE: Duration = Duration::from_secs(5);
/// Leave time to join the I/O task inside the HTTP owner's five-second cleanup.
pub const CLOSE_DEADLINE: Duration = Duration::from_secs(2);
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
pub const HEARTBEAT_DEADLINE: Duration = Duration::from_secs(10);

struct Heartbeat {
    expected: AtomicU64,
    seen: AtomicU64,
    flush_pong: Notify,
}

// Preserve an in-progress frame across an acknowledged heartbeat deadline.
// Only connection teardown or an actual deadline may drop the write future.
async fn write_with_deadlines<F, E>(
    write: F,
    stopped: &CancellationToken,
    heartbeat: &Heartbeat,
    pending: &mut Option<(u64, Instant)>,
) -> Result<(), Error>
where
    F: Future<Output = Result<(), E>>,
{
    let write_deadline = Instant::now() + WRITE_DEADLINE;
    tokio::pin!(write);
    loop {
        if let Some((nonce, heartbeat_deadline)) = *pending {
            if heartbeat.seen.load(Ordering::Acquire) == nonce {
                heartbeat.expected.store(0, Ordering::Release);
                *pending = None;
                continue;
            }
            if Instant::now() >= heartbeat_deadline {
                return Err(Error::WriteTimeout);
            }
            tokio::select! {
                biased;
                _ = stopped.cancelled() => return Err(Error::Closed),
                _ = tokio::time::sleep_until(heartbeat_deadline) => continue,
                result = timeout_at(write_deadline, &mut write) => {
                    return match result {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(_)) => Err(Error::Transport),
                        Err(_) => Err(Error::WriteTimeout),
                    };
                }
            }
        } else {
            return tokio::select! {
                biased;
                _ = stopped.cancelled() => Err(Error::Closed),
                result = timeout_at(write_deadline, &mut write) => match result {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(_)) => Err(Error::Transport),
                    Err(_) => Err(Error::WriteTimeout),
                },
            };
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Busy,
    Size,
    Cancelled,
    QueueTimeout,
    WriteTimeout,
    Closed,
    Transport,
    Protocol,
}

/// Apply at socket creation, before polling the socket or splitting it.
pub fn socket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(wire::MAX_MESSAGE + 4096)
        .max_message_size(Some(wire::MAX_MESSAGE))
        .max_frame_size(Some(wire::MAX_MESSAGE))
}

#[derive(Clone)]
pub struct Sender {
    queue: mpsc::Sender<Job>,
    slots: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    stopped: CancellationToken,
    protocol: protobuf::Negotiated,
    byte_limit: usize,
}

/// Admission occurs before a producer encodes a frame. There are no suspended
/// admission waiters retaining unlimited payloads. Higher-level request/view
/// admission must also bound the original messages being encoded.
pub struct Reservation {
    queue: mpsc::Sender<Job>,
    slot: OwnedSemaphorePermit,
    bytes: OwnedSemaphorePermit,
    stopped: CancellationToken,
    protocol: protobuf::Negotiated,
    admitted: Instant,
}

struct Job {
    message: Message,
    _slot: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
    cancelled: CancellationToken,
    expires: Instant,
    reply: Option<oneshot::Sender<Result<(), Error>>>,
}

pub struct Receipt(oneshot::Receiver<Result<(), Error>>);
impl Receipt {
    /// Dropping this future/receipt cancels work that is still queued. A frame
    /// already being written completes within its independent transport budget.
    pub async fn wait(self) -> Result<(), Error> {
        self.0.await.unwrap_or(Err(Error::Closed))
    }
}

impl Sender {
    pub fn try_reserve(&self, max_encoded_bytes: usize) -> Result<Reservation, Error> {
        if self.stopped.is_cancelled() {
            return Err(Error::Closed);
        }
        if max_encoded_bytes == 0 || max_encoded_bytes > self.byte_limit {
            return Err(Error::Size);
        }
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let bytes = self
            .bytes
            .clone()
            .try_acquire_many_owned(max_encoded_bytes as u32)
            .map_err(|_| Error::Busy)?;
        Ok(Reservation {
            queue: self.queue.clone(),
            slot,
            bytes,
            stopped: self.stopped.clone(),
            protocol: self.protocol,
            admitted: Instant::now(),
        })
    }

    pub fn close(&self) {
        self.stopped.cancel();
    }

    pub fn is_closed(&self) -> bool {
        self.stopped.is_cancelled()
    }

    pub fn retained_bytes(&self) -> usize {
        self.byte_limit - self.bytes.available_permits()
    }

    pub fn retained_frames(&self) -> usize {
        QUEUED_FRAMES - self.slots.available_permits()
    }

    /// Best-effort bounded close frame, followed by unconditional local teardown.
    /// Close may interrupt an in-flight write because the connection is ending.
    /// Dropping this future also unconditionally stops the I/O task.
    pub async fn close_with(&self, code: u16, reason: &str) {
        let _close_on_drop = self.stopped.clone().drop_guard();
        use tokio_tungstenite::tungstenite::protocol::{frame::coding::CloseCode, CloseFrame};
        let code = CloseCode::from(code);
        if code.is_allowed() && reason.len() <= 123 {
            if let Ok(reservation) = self.try_reserve(reason.len() + 2) {
                let (reply, receive) = oneshot::channel();
                let result = reservation.enqueue(
                    Message::Close(Some(CloseFrame {
                        code,
                        reason: reason.to_owned().into(),
                    })),
                    reason.len() + 2,
                    CancellationToken::new(),
                    Some(reply),
                );
                if result.is_ok() {
                    let _ = tokio::time::timeout(CLOSE_DEADLINE, Receipt(receive).wait()).await;
                }
            }
        }
        self.close();
    }
}

impl Reservation {
    /// `raw` must come from the negotiated codec. Copy into an allocation owned
    /// by this job so a tiny Bytes slice cannot pin an unaccounted large buffer.
    /// The caller's temporary encoding allocation is outside this queue budget.
    pub fn submit(self, raw: &[u8], cancelled: CancellationToken) -> Result<Receipt, Error> {
        let (reply, receive) = oneshot::channel();
        self.submit_inner(raw, cancelled, Some(reply))?;
        Ok(Receipt(receive))
    }

    /// Bounded best-effort control delivery with no receiver or spawned waiter.
    /// Use for teardown controls that must outlive the request being dropped.
    /// Admission/queue/write limits are unchanged. Owners must handle admission
    /// failure (usually by closing the peer); completion is not acknowledged.
    /// Cancellation/expiry of an admitted detached job closes the connection,
    /// so remote cleanup cannot be silently skipped while the peer stays live.
    pub fn submit_detached(self, raw: &[u8], cancelled: CancellationToken) -> Result<(), Error> {
        self.submit_inner(raw, cancelled, None)
    }

    fn submit_inner(
        self,
        raw: &[u8],
        cancelled: CancellationToken,
        reply: Option<oneshot::Sender<Result<(), Error>>>,
    ) -> Result<(), Error> {
        let binary = self.protocol == protobuf::Negotiated::ProtobufV2;
        self.submit_data(raw, binary, cancelled, reply)
    }

    fn check(&self, size: usize, cancelled: &CancellationToken) -> Result<(), Error> {
        if self.stopped.is_cancelled() {
            return Err(Error::Closed);
        }
        if cancelled.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if self.admitted.elapsed() >= QUEUE_DEADLINE {
            return Err(Error::QueueTimeout);
        }
        if size == 0 || size > self.bytes.num_permits() {
            return Err(Error::Size);
        }
        Ok(())
    }

    fn submit_data(
        self,
        raw: &[u8],
        binary: bool,
        cancelled: CancellationToken,
        reply: Option<oneshot::Sender<Result<(), Error>>>,
    ) -> Result<(), Error> {
        self.check(raw.len(), &cancelled)?;
        let data = Bytes::copy_from_slice(raw);
        let message = if binary {
            Message::Binary(data)
        } else {
            Message::Text(data.try_into().map_err(|_| Error::Protocol)?)
        };
        self.enqueue(message, raw.len(), cancelled, reply)
    }

    fn enqueue(
        mut self,
        message: Message,
        size: usize,
        cancelled: CancellationToken,
        reply: Option<oneshot::Sender<Result<(), Error>>>,
    ) -> Result<(), Error> {
        self.check(size, &cancelled)?;
        // Keep the exact retained payload charge through the entire write.
        let unused = self.bytes.num_permits() - size;
        drop(self.bytes.split(unused));
        let job = Job {
            message,
            _slot: self.slot,
            _bytes: self.bytes,
            cancelled,
            expires: self.admitted + QUEUE_DEADLINE,
            reply,
        };
        self.queue.try_send(job).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => Error::Busy,
            mpsc::error::TrySendError::Closed(_) => Error::Closed,
        })?;
        Ok(())
    }
}

/// Payload-bearing types intentionally have no Debug implementation.
pub enum Incoming {
    Json(wire::Message),
    Protobuf(protobuf::types::Envelope),
}

// Only this small bounded handoff is retained outside the I/O task. The task
// owns both socket halves so an idle consumer can never prevent socket teardown.
pub struct Reader {
    frames: mpsc::Receiver<Message>,
    stopped: CancellationToken,
    protocol: protobuf::Negotiated,
    direction: protobuf::Direction,
}

pub struct Connection {
    pub sender: Sender,
    pub reader: Reader,
    pub task: JoinHandle<()>,
}

impl Connection {
    pub fn protocol(&self) -> protobuf::Negotiated {
        self.sender.protocol
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.stopped.cancel();
    }
}
impl Reader {
    async fn receive_frame(&mut self) -> Option<Message> {
        tokio::select! {
            biased;
            _ = self.stopped.cancelled() => None,
            message = self.frames.recv() => message,
        }
    }
    pub async fn receive(&mut self) -> Result<Incoming, Error> {
        let message = self.receive_frame().await;
        let decoded = match message {
            Some(Message::Text(raw)) if self.protocol == protobuf::Negotiated::JsonV1 => {
                wire::Message::decode(raw.as_bytes())
                    .map(Incoming::Json)
                    .map_err(|_| Error::Protocol)
            }
            Some(Message::Binary(raw)) => match self.protocol {
                protobuf::Negotiated::JsonV1 => wire::Message::decode(&raw)
                    .map(Incoming::Json)
                    .map_err(|_| Error::Protocol),
                protobuf::Negotiated::ProtobufV2 => protobuf::decode(raw, self.direction)
                    .map(Incoming::Protobuf)
                    .map_err(|_| Error::Protocol),
            },
            None => Err(Error::Closed),
            _ => Err(Error::Protocol),
        };
        if decoded.is_err() {
            self.stopped.cancel();
            self.frames.close();
            while self.frames.try_recv().is_ok() {}
        }
        decoded
    }
}

/// Browser frames use the same bounded I/O and heartbeat owner, with a smaller
/// socket/queue budget. This API never selects or decodes a Home wire protocol.
pub struct FrameConnection {
    pub sender: FrameSender,
    pub reader: FrameReader,
    pub task: JoinHandle<()>,
}
#[derive(Clone)]
pub struct FrameSender(Sender);
pub struct FrameReader(Reader);
impl FrameSender {
    pub async fn wait_closed(&self) {
        self.0.stopped.cancelled().await;
    }
    pub async fn send(&self, binary: bool, raw: &[u8]) -> Result<(), Error> {
        let reservation = self.0.try_reserve(raw.len())?;
        let (reply, receive) = oneshot::channel();
        reservation.submit_data(raw, binary, CancellationToken::new(), Some(reply))?;
        Receipt(receive).wait().await
    }
    pub fn close(&self) {
        self.0.close();
    }
    pub async fn close_with(&self, code: u16, reason: &str) {
        self.0.close_with(code, reason).await;
    }
}
impl FrameReader {
    pub async fn receive(&mut self) -> Result<Message, Error> {
        self.0.receive_frame().await.ok_or(Error::Closed)
    }
}
pub fn start_frames<S>(
    socket: WebSocketStream<S>,
    byte_limit: usize,
) -> Result<FrameConnection, Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let connection = start_with_limit(
        socket,
        protobuf::Negotiated::JsonV1,
        protobuf::Direction::ToGateway,
        byte_limit,
    )?;
    Ok(FrameConnection {
        sender: FrameSender(connection.sender),
        reader: FrameReader(connection.reader),
        task: connection.task,
    })
}

struct StopOnExit(CancellationToken);
impl Drop for StopOnExit {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Takes an already-authenticated socket configured with `socket_config()`.
/// The role owner must join the returned task at shutdown and handle account
/// revocation, generation changes and stateful frame authorization.
pub fn start<S>(
    socket: WebSocketStream<S>,
    protocol: protobuf::Negotiated,
    incoming: protobuf::Direction,
) -> Result<Connection, Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    start_with_limit(socket, protocol, incoming, QUEUED_BYTES)
}

fn start_with_limit<S>(
    socket: WebSocketStream<S>,
    protocol: protobuf::Negotiated,
    incoming: protobuf::Direction,
    byte_limit: usize,
) -> Result<Connection, Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    if byte_limit == 0 || byte_limit > QUEUED_BYTES {
        return Err(Error::Size);
    }
    let config = socket.get_config();
    if config.read_buffer_size > 4096
        || config.write_buffer_size != 0
        || config.max_write_buffer_size > byte_limit + 4096
        || config.max_message_size.is_none_or(|n| n > byte_limit)
        || config.max_frame_size.is_none_or(|n| n > byte_limit)
        || config.accept_unmasked_frames
    {
        return Err(Error::Protocol);
    }
    let (mut sink, mut stream) = socket.split();
    let stopped = CancellationToken::new();
    let (queue, mut jobs) = mpsc::channel::<Job>(QUEUED_FRAMES);
    let sender = Sender {
        queue,
        slots: Arc::new(Semaphore::new(QUEUED_FRAMES)),
        bytes: Arc::new(Semaphore::new(byte_limit)),
        stopped: stopped.clone(),
        protocol,
        byte_limit,
    };
    // At most one delivered frame and one frame being read/admitted, each
    // bounded by MAX_MESSAGE. The consumer owns any subsequently decoded frame.
    let (received, frames) = mpsc::channel(1);
    let reader = Reader {
        frames,
        stopped: stopped.clone(),
        protocol,
        direction: incoming,
    };
    // Construct outside the future so even abort-before-first-poll closes the
    // peer. Dropping an unpolled JoinHandle future must not leave a live reader.
    let stop = StopOnExit(stopped.clone());
    let heartbeat = Arc::new(Heartbeat {
        expected: AtomicU64::new(0),
        seen: AtomicU64::new(0),
        flush_pong: Notify::new(),
    });
    let task = tokio::spawn(async move {
        let _stop = stop;
        let read_heartbeat = heartbeat.clone();
        let write = async {
            let _writer_stop = StopOnExit(stopped.clone());
            let random = RandomState::new();
            let mut sequence = 0_u64;
            let mut pending: Option<(u64, Instant)> = None;
            let mut ticks =
                tokio::time::interval_at(Instant::now() + HEARTBEAT_INTERVAL, HEARTBEAT_INTERVAL);
            ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                if let Some((nonce, deadline)) = pending {
                    if heartbeat.seen.load(Ordering::Acquire) == nonce {
                        heartbeat.expected.store(0, Ordering::Release);
                        pending = None;
                    } else if Instant::now() >= deadline {
                        break;
                    }
                }
                let job = tokio::select! {
                    biased;
                    _ = stopped.cancelled() => break,
                    _ = async {
                        if let Some((_, deadline)) = pending {
                            tokio::time::sleep_until(deadline).await;
                        }
                    }, if pending.is_some() => continue,
                    tick = ticks.tick(), if pending.is_none() => {
                        sequence = sequence.wrapping_add(1);
                        let mut nonce = random.hash_one(sequence);
                        while nonce == 0 || heartbeat.seen.load(Ordering::Acquire) == nonce {
                            sequence = sequence.wrapping_add(1);
                            nonce = random.hash_one(sequence);
                        }
                        let deadline = tick + HEARTBEAT_DEADLINE;
                        if Instant::now() >= deadline {
                            break;
                        }
                        heartbeat.expected.store(nonce, Ordering::Release);
                        let sent = tokio::select! {
                            biased;
                            _ = stopped.cancelled() => break,
                            result = timeout_at(deadline, sink.send(Message::Ping(Bytes::copy_from_slice(&nonce.to_be_bytes())))) => result,
                        };
                        if !matches!(sent, Ok(Ok(()))) {
                            break;
                        }
                        pending = Some((nonce, deadline));
                        continue;
                    },
                    _ = heartbeat.flush_pong.notified() => {
                        if write_with_deadlines(sink.flush(), &stopped, &heartbeat, &mut pending)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    },
                    job = jobs.recv() => match job { Some(job) => job, None => break },
                };
                if job.cancelled.is_cancelled()
                    || job.reply.as_ref().is_some_and(|reply| reply.is_closed())
                {
                    if let Some(reply) = job.reply {
                        let _ = reply.send(Err(Error::Cancelled));
                    } else {
                        break;
                    }
                    continue;
                }
                if Instant::now() >= job.expires {
                    if let Some(reply) = job.reply {
                        let _ = reply.send(Err(Error::QueueTimeout));
                    } else {
                        break;
                    }
                    continue;
                }
                // Only connection teardown may interrupt an admitted write. The
                // request token and reply receiver no longer control this future.
                let closing = matches!(job.message, Message::Close(_));
                let result = write_with_deadlines(
                    sink.send(job.message),
                    &stopped,
                    &heartbeat,
                    &mut pending,
                )
                .await;
                let failed = result.is_err();
                if let Some(reply) = job.reply {
                    let _ = reply.send(result);
                }
                if failed || closing {
                    break;
                }
            }
            // Dropping this channel frees every queued frame and reservation and
            // signals all pending receipts. Never retry a possibly partial frame.
            drop(jobs);
            drop(sink);
        };
        let read = async {
            let _reader_stop = StopOnExit(stopped.clone());
            loop {
                let message = tokio::select! {
                    biased;
                    _ = stopped.cancelled() => break,
                    message = stream.next() => message,
                };
                let message = match message {
                    Some(Ok(Message::Ping(_))) => {
                        // Tungstenite queues the automatic Pong on read. The
                        // single writer flushes it even when no data is sent.
                        read_heartbeat.flush_pong.notify_one();
                        continue;
                    }
                    Some(Ok(Message::Pong(payload))) => {
                        if let Ok(raw) = <[u8; 8]>::try_from(payload.as_ref()) {
                            let nonce = u64::from_be_bytes(raw);
                            if nonce != 0
                                && read_heartbeat.expected.load(Ordering::Acquire) == nonce
                            {
                                read_heartbeat.seen.store(nonce, Ordering::Release);
                            }
                        }
                        continue;
                    }
                    Some(Ok(Message::Text(raw))) => Message::Text(raw),
                    Some(Ok(Message::Binary(raw))) => Message::Binary(raw),
                    _ => break,
                };
                let sent = tokio::select! {
                    biased;
                    _ = stopped.cancelled() => break,
                    sent = received.send(message) => sent,
                };
                if sent.is_err() {
                    break;
                }
            }
            drop(received);
            drop(stream);
        };
        tokio::join!(write, read);
    });
    Ok(Connection {
        sender,
        reader,
        task,
    })
}
