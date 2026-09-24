//! Streaming browser uploads. Only admitted transfers get a data-sized socket;
//! the Gateway hashes chunks and never creates files or retains complete bodies.
use crate::{
    auth_store::{AuthStore, SessionAccess},
    hub::{Hub, UploadEvent, UploadLease},
    upload_contract::{self as contract, UploadHashes},
};
use futures_util::{SinkExt, StreamExt};
use hmux_protocol::{
    protobuf::types as p,
    transport::{self, FrameConnection, FrameReader, FrameSender},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{timeout, timeout_at, Instant, MissedTickBehavior},
};
use tokio_tungstenite::{
    tungstenite::{protocol::WebSocketConfig, Message},
    WebSocketStream,
};
use tokio_util::sync::CancellationToken;

pub const HEADER_BYTES: usize = 16 << 10;
pub const CHUNK_BYTES: usize = 256 << 10;
const FIRST: Duration = Duration::from_secs(10);
const IDLE: Duration = Duration::from_secs(30);
const OVERALL: Duration = Duration::from_secs(300);
const AUTH_TICK: Duration = Duration::from_secs(5);
const ERROR_WRITE: Duration = Duration::from_secs(2);

pub fn socket_config() -> WebSocketConfig {
    config(HEADER_BYTES)
}
fn config(limit: usize) -> WebSocketConfig {
    transport::socket_config()
        .max_message_size(Some(limit))
        .max_frame_size(Some(limit))
        .max_write_buffer_size(limit + 4096)
}

/// Exactly two fixed slots; no per-login map or resident upload worker.
#[derive(Clone, Default)]
pub struct Limiter(Arc<Mutex<[Option<[u8; 32]>; 2]>>);
struct Permit {
    owner: Limiter,
    index: usize,
    key: [u8; 32],
}
impl Limiter {
    fn acquire(&self, login: &str) -> Option<Permit> {
        let key = Sha256::digest(login.as_bytes()).into();
        let mut slots = self.0.lock().ok()?;
        if slots.contains(&Some(key)) {
            return None;
        }
        let index = slots.iter().position(Option::is_none)?;
        slots[index] = Some(key);
        Some(Permit {
            owner: self.clone(),
            index,
            key,
        })
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut slots) = self.owner.0.lock() {
            if slots[self.index] == Some(self.key) {
                slots[self.index] = None;
            }
        }
    }
}
#[derive(Clone, Copy)]
struct End(u16, Option<&'static str>);
const INVALID: End = End(1008, Some("Invalid upload request"));
const DATA: End = End(1008, Some("Upload data rejected"));
const HOME: End = End(1013, Some("Home upload unavailable"));
const ENDED: End = End(1008, None);
const CLOSED: End = End(1000, None);
const STOPPING: End = End(1001, None);

pub async fn serve<S>(
    socket: S,
    hub: Hub,
    auth: Arc<AuthStore>,
    token: String,
    mut access: SessionAccess,
    limiter: Limiter,
    shutdown: CancellationToken,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Reserve before constructing a data-sized WebSocket. The first frame has
    // a FIRST deadline; rejected sockets keep only header capacity.
    let permit = limiter.acquire(&access.id);
    let mut socket = WebSocketStream::from_raw_socket(
        socket,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        Some(if permit.is_some() {
            config(CHUNK_BYTES)
        } else {
            socket_config()
        }),
    )
    .await;
    let deadline = Instant::now() + OVERALL;
    let expiry = Instant::now()
        + access
            .expires_at
            .signed_duration_since(chrono::DateTime::<chrono::Utc>::from(SystemTime::now()))
            .to_std()
            .unwrap_or_default();
    let csrf = access.csrf.clone();
    let prepared = tokio::select! {
        biased;
        _ = shutdown.cancelled() => Err(STOPPING),
        _ = access.wait_cancelled() => Err(ENDED),
        _ = tokio::time::sleep_until(expiry) => Err(ENDED),
        result = timeout_at(deadline, prepare(&mut socket, &csrf, permit.is_some(), &auth, &token)) => result.unwrap_or(Err(DATA)),
    };
    let header = match prepared {
        Ok(prepared) => prepared,
        Err(end) => {
            if let Some(error) = end.1 {
                let raw = json!({"type":"error","error":error}).to_string();
                let _ = timeout(ERROR_WRITE, socket.send(Message::Text(raw.into()))).await;
            }
            // No transport task exists before admission. Dropping closes the
            // socket even if its peer stops reading this best-effort error.
            return;
        }
    };
    let Ok(FrameConnection {
        sender,
        mut reader,
        task,
    }) = transport::start_frames(socket, CHUNK_BYTES)
    else {
        return;
    };
    let end = tokio::select! {
        biased;
        _ = shutdown.cancelled() => STOPPING,
        _ = access.wait_cancelled() => ENDED,
        _ = tokio::time::sleep_until(expiry) => ENDED,
        _ = tokio::time::sleep_until(deadline) => DATA,
        _ = sender.wait_closed() => CLOSED,
        end = monitor_auth(&auth, &token) => end,
        result = relay(header, &mut reader, &sender, &hub, &auth, &token) => result.err().unwrap_or(CLOSED),
    };
    if let Some(error) = end.1 {
        let raw = json!({"type":"error","error":error}).to_string();
        let _ = timeout(ERROR_WRITE, sender.send(false, raw.as_bytes())).await;
    }
    sender.close_with(end.0, "Upload ended").await;
    drop(reader);
    let _ = task.await;
    // Keep both admission limits held until the actual browser IO owner exits.
    drop(permit);
}
async fn check_auth(auth: &AuthStore, token: &str, touch: bool) -> Result<(), End> {
    match auth.access(token, touch, SystemTime::now().into()).await {
        Ok(Some(_)) => Ok(()),
        _ => Err(ENDED),
    }
}
async fn monitor_auth(auth: &AuthStore, token: &str) -> End {
    let mut ticks = tokio::time::interval_at(Instant::now() + AUTH_TICK, AUTH_TICK);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        if let Err(end) = check_auth(auth, token, false).await {
            return end;
        }
    }
}
async fn prepare<S>(
    socket: &mut WebSocketStream<S>,
    csrf: &str,
    admitted: bool,
    auth: &AuthStore,
    token: &str,
) -> Result<p::UploadHeader, End>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let raw = timeout(FIRST, async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(raw))) => return Ok(raw),
                Some(Ok(Message::Ping(_))) => {
                    socket.flush().await.map_err(|_| CLOSED)?;
                }
                Some(Ok(Message::Pong(_))) => {}
                _ => return Err(INVALID),
            }
        }
    })
    .await
    .map_err(|_| INVALID)??;
    let header = contract::decode_start(raw.as_bytes(), csrf).map_err(|_| INVALID)?;
    check_auth(auth, token, true).await?;
    if !admitted {
        return Err(End(1013, Some("Upload limit reached")));
    }
    Ok(header)
}
async fn home_event(upload: &mut UploadLease) -> Result<UploadEvent, End> {
    timeout(IDLE, upload.receive())
        .await
        .map_err(|_| HOME)?
        .map_err(|_| HOME)
}
async fn browser_frame(reader: &mut FrameReader, upload: &mut UploadLease) -> Result<Message, End> {
    tokio::select! {
        biased;
        // There is no valid unsolicited Home event while waiting for browser
        // bytes. This also detects connector death without waiting for idle.
        _ = upload.receive() => Err(HOME),
        message = timeout(IDLE, reader.receive()) => message.map_err(|_| DATA)?.map_err(|_| CLOSED),
    }
}
async fn relay(
    mut header: p::UploadHeader,
    reader: &mut FrameReader,
    sender: &FrameSender,
    hub: &Hub,
    auth: &AuthStore,
    token: &str,
) -> Result<(), End> {
    let generation = hub.snapshot().generation.ok_or(HOME)?;
    let mut hashes = UploadHashes::new(&header).map_err(|_| INVALID)?;
    let mut upload = timeout(
        IDLE,
        hub.open_upload(
            generation,
            p::UploadStart {
                id: String::new(),
                header: Some(header.clone()),
            },
        ),
    )
    .await
    .map_err(|_| HOME)?
    .map_err(|_| HOME)?;
    header.request_id = upload.id().into();
    if !matches!(home_event(&mut upload).await?, UploadEvent::Ready) {
        return Err(HOME);
    }
    sender
        .send(false, br#"{"type":"ready"}"#)
        .await
        .map_err(|_| CLOSED)?;
    while !hashes.is_complete() {
        let Message::Binary(raw) = browser_frame(reader, &mut upload).await? else {
            return Err(DATA);
        };
        check_auth(auth, token, true).await?;
        let received = hashes.consume(&raw).map_err(|_| DATA)?;
        upload.data(raw).await.map_err(|_| HOME)?;
        if !matches!(home_event(&mut upload).await?, UploadEvent::Ack(n) if n == received) {
            return Err(HOME);
        }
        let reply = json!({"type":"ack","received":received}).to_string();
        sender
            .send(false, reply.as_bytes())
            .await
            .map_err(|_| CLOSED)?;
    }
    let Message::Text(raw) = browser_frame(reader, &mut upload).await? else {
        return Err(DATA);
    };
    contract::decode_finish(raw.as_bytes()).map_err(|_| DATA)?;
    check_auth(auth, token, true).await?;
    upload.finish().await.map_err(|_| HOME)?;
    let UploadEvent::Complete(response) = home_event(&mut upload).await? else {
        return Err(HOME);
    };
    if !response.error.is_empty() {
        return Err(HOME);
    }
    check_auth(auth, token, true).await?;
    let hashes = hashes.finish().map_err(|_| DATA)?;
    let Some(p::response::Result::Staged(value)) = response.result.as_ref() else {
        return Err(HOME);
    };
    let stage = contract::validate_completion(value, &header, &hashes).map_err(|_| HOME)?;
    #[derive(serde::Serialize)]
    struct Complete<'a> {
        #[serde(rename = "type")]
        kind: &'static str,
        stage: &'a contract::StageResponse,
    }
    let reply = serde_json::to_vec(&Complete {
        kind: "complete",
        stage: &stage,
    })
    .map_err(|_| HOME)?;
    sender.send(false, &reply).await.map_err(|_| CLOSED)?;
    Ok(())
}
