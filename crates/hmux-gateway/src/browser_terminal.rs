//! Browser wire remains binary terminal bytes plus small JSON controls.
//! Closing this owner drops only its disposable Home view, never the session.

use crate::{
    auth_store::{AuthStore, SessionAccess},
    browser_control::Control,
    hub::{self, Hub, ViewEvent},
};
use hmux_protocol::{
    protobuf::types as p,
    transport::{self, FrameConnection, FrameReader, FrameSender},
    wire,
};
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::time::{interval_at, timeout, Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::{protocol::WebSocketConfig, Message};
use tokio_util::sync::CancellationToken;

pub const FRAME_BYTES: usize = 64 << 10;
const FIRST_FRAME: Duration = Duration::from_secs(10);
const OPEN_TIMEOUT: Duration = Duration::from_secs(20);
const TICK: Duration = Duration::from_secs(5);
const READY: &[u8] = br#"{"type":"ready","heartbeat":true}"#;
const FLOW_READY: &[u8] = br#"{"type":"ready","heartbeat":true,"output_flow":true}"#;

pub fn socket_config() -> WebSocketConfig {
    transport::socket_config()
        .max_message_size(Some(FRAME_BYTES))
        .max_frame_size(Some(FRAME_BYTES))
        .max_write_buffer_size(FRAME_BYTES + 4096)
}

struct End(u16, &'static str);
fn hub_error(error: hub::Error) -> End {
    match error {
        hub::Error::OutputFull => End(wire::OUTPUT_FULL, "Terminal rendering stalled"),
        hub::Error::Capacity | hub::Error::Busy => End(1013, "Terminal limit reached"),
        hub::Error::Invalid | hub::Error::Unsupported => End(1008, "Terminal unavailable"),
        _ => End(wire::HOME_OFFLINE, "Home unavailable"),
    }
}
fn geometry(message: &Control) -> bool {
    (2..=500).contains(&message.cols) && (2..=250).contains(&message.rows)
}

/// The HTTP upgrade owner must await this function through socket cleanup.
pub async fn serve(
    connection: FrameConnection,
    hub: Hub,
    auth: Arc<AuthStore>,
    token: String,
    mut access: SessionAccess,
    shutdown: CancellationToken,
) {
    let FrameConnection {
        sender,
        mut reader,
        task,
    } = connection;
    let remaining = access
        .expires_at
        .signed_duration_since(chrono::DateTime::<chrono::Utc>::from(SystemTime::now()))
        .to_std()
        .unwrap_or_default();
    let end = tokio::select! {
        biased;
        _ = shutdown.cancelled() => End(1001, "Gateway stopping"),
        _ = access.wait_cancelled() => End(1008, "Session ended"),
        _ = tokio::time::sleep(remaining) => End(1008, "Session ended"),
        _ = sender.wait_closed() => End(1000, "Terminal transport ended"),
        end = relay(&mut reader, &sender, &hub, &auth, &token) => end,
    };
    sender.close_with(end.0, end.1).await;
    drop(reader);
    let _ = task.await;
}

async fn relay(
    reader: &mut FrameReader,
    sender: &FrameSender,
    hub: &Hub,
    auth: &AuthStore,
    token: &str,
) -> End {
    let first = match timeout(FIRST_FRAME, reader.receive()).await {
        Ok(Ok(message)) => message,
        _ => return End(1008, "Invalid terminal request"),
    };
    // Go accepts an opening JSON object in either WS data-frame kind.
    let Some(open) = Control::decode(&first.into_data()) else {
        return End(1008, "Invalid terminal request");
    };
    if open.kind != "open" || !open.session.is_valid() || !geometry(&open) {
        return End(1008, "Invalid terminal request");
    }
    let Some(generation) = hub.snapshot().generation else {
        return hub_error(hub::Error::Offline);
    };
    let mut view = match timeout(
        OPEN_TIMEOUT,
        hub.open_view(
            generation,
            p::TerminalOpen {
                id: String::new(),
                session: Some(p::Session {
                    id: open.session.id,
                    created_at: open.session.created_at,
                }),
                cols: open.cols.into(),
                rows: open.rows.into(),
                capabilities: Vec::new(),
            },
        ),
    )
    .await
    {
        Ok(Ok(view)) => view,
        Ok(Err(error)) => return hub_error(error),
        Err(_) => return End(1008, "Terminal unavailable"),
    };
    let browser_flow = view.output_flow() && open.output_flow;
    if sender
        .send(false, if browser_flow { FLOW_READY } else { READY })
        .await
        .is_err()
    {
        return End(1000, "Terminal transport ended");
    }
    let mut ticks = interval_at(Instant::now() + TICK, TICK);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            // Fair selection prevents a busy input or output side starving the
            // five-second access check and application heartbeat.
            incoming = reader.receive() => {
                let incoming = match incoming { Ok(frame) => frame, Err(_) => return End(1000, "Terminal transport ended") };
                let mut control = None;
                let data = match incoming {
                    Message::Binary(data) if data.len() <= wire::MAX_DATA => Some(data),
                    Message::Text(text) => {
                        let Some(frame) = Control::decode(text.as_bytes()) else { return End(1002, "Invalid terminal control") };
                        if !matches!(frame.kind.as_str(), "output-ack" | "resize" | "refresh")
                            || (frame.kind == "resize" && !geometry(&frame))
                            || (frame.kind == "output-ack" && !browser_flow) {
                            return End(1002, "Invalid terminal control");
                        }
                        control = Some(frame);
                        None
                    },
                    _ => return End(1002, "Invalid terminal input"),
                };
                let touch = control.as_ref().is_none_or(|m| m.kind != "output-ack");
                match auth.access(token, touch, SystemTime::now().into()).await {
                    Ok(Some(_)) => {},
                    Ok(None) => return End(1008, "Session ended"),
                    Err(_) => return End(1013, "Authentication unavailable"),
                }
                let result = if let Some(data) = data { view.input(data).await } else {
                    let frame = control.expect("validated control");
                    match frame.kind.as_str() {
                        "output-ack" => {
                            match view.acknowledge(frame.received).await {
                                Err(hub::Error::Invalid) => return End(1002, "Invalid output acknowledgement"),
                                result => result,
                            }
                        },
                        "resize" => view.resize(frame.cols.into(), frame.rows.into()).await,
                        _ => view.refresh().await,
                    }
                };
                if let Err(error) = result { return hub_error(error); }
            },
            event = view.receive() => {
                match event {
                    Err(error) => return hub_error(error),
                    Ok(ViewEvent::Exit {error}) => return if error == "output-stalled" {
                        End(wire::OUTPUT_FULL, "Terminal rendering stalled")
                    } else { End(wire::VIEW_EXITED, "Terminal view ended") },
                    Ok(ViewEvent::RefreshResult {ok}) => {
                        let reply: &[u8] = if ok {br#"{"type":"refresh-result","ok":true}"#} else {br#"{"type":"refresh-result","ok":false}"#};
                        if sender.send(false, reply).await.is_err() { return End(1000, "Terminal transport ended"); }
                    },
                    Ok(ViewEvent::Data(data)) => {
                        if sender.send(true, &data).await.is_err() { return End(1000, "Terminal transport ended"); }
                        // Older browsers pace Home at completed socket writes.
                        if view.output_flow() && !browser_flow {
                            if let Err(error) = view.acknowledge(data.len() as i64).await { return hub_error(error); }
                        }
                    },
                }
            },
            _ = ticks.tick() => {
                if view.stalled() { return End(wire::OUTPUT_FULL, "Terminal rendering stalled"); }
                match auth.access(token, false, SystemTime::now().into()).await {
                    Ok(Some(_)) => {},
                    Ok(None) => return End(1008, "Session ended"),
                    Err(_) => return End(1013, "Authentication unavailable"),
                }
                if sender.send(false, br#"{"type":"heartbeat"}"#).await.is_err() { return End(1000, "Terminal transport ended"); }
            },
        }
    }
}
