use futures_util::{task::AtomicWaker, SinkExt, StreamExt};
use hmux_protocol::{
    protobuf::{self, Direction, Negotiated},
    transport::{self, Error},
};
use std::{
    io,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf},
    sync::Notify,
};
use tokio_tungstenite::{
    tungstenite::{protocol::Role, Message},
    WebSocketStream,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Gate {
    allowed: AtomicUsize,
    entered: AtomicUsize,
    wake: AtomicWaker,
    changed: Notify,
}
impl Gate {
    async fn entered(&self, count: usize) {
        loop {
            let changed = self.changed.notified();
            if self.entered.load(Ordering::SeqCst) >= count {
                return;
            }
            changed.await;
        }
    }
    fn release(&self) {
        self.allowed.fetch_add(1, Ordering::SeqCst);
        self.wake.wake();
    }
}
struct PausedIo {
    io: DuplexStream,
    gate: Arc<Gate>,
    write_count: usize,
}
impl AsyncRead for PausedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}
impl AsyncWrite for PausedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.gate.wake.register(cx.waker());
        self.gate
            .entered
            .store(self.write_count + 1, Ordering::SeqCst);
        self.gate.changed.notify_waiters();
        if self.gate.allowed.load(Ordering::SeqCst) <= self.write_count {
            return Poll::Pending;
        }
        let result = Pin::new(&mut self.io).poll_write(cx, buf);
        if matches!(result, Poll::Ready(Ok(_))) {
            self.write_count += 1;
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}
async fn paused_pair() -> (
    WebSocketStream<PausedIo>,
    WebSocketStream<DuplexStream>,
    Arc<Gate>,
) {
    let (left, right) = tokio::io::duplex(4096);
    let gate = Arc::new(Gate::default());
    let server = WebSocketStream::from_raw_socket(
        PausedIo {
            io: left,
            gate: gate.clone(),
            write_count: 0,
        },
        Role::Server,
        Some(transport::socket_config()),
    )
    .await;
    let client =
        WebSocketStream::from_raw_socket(right, Role::Client, Some(transport::socket_config()))
            .await;
    (server, client, gate)
}
async fn pair() -> (WebSocketStream<DuplexStream>, WebSocketStream<DuplexStream>) {
    let (left, right) = tokio::io::duplex(4096);
    let server =
        WebSocketStream::from_raw_socket(left, Role::Server, Some(transport::socket_config()))
            .await;
    let client =
        WebSocketStream::from_raw_socket(right, Role::Client, Some(transport::socket_config()))
            .await;
    (server, client)
}
fn enqueue(sender: &transport::Sender, raw: &[u8], token: CancellationToken) -> transport::Receipt {
    sender
        .try_reserve(raw.len())
        .unwrap()
        .submit(raw, token)
        .unwrap()
}

#[tokio::test]
async fn detached_cleanup_keeps_fifo_without_a_receipt_or_extra_waiter() {
    let (server, mut client, gate) = paused_pair().await;
    let connection = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    let receipt = enqueue(&connection.sender, b"first", CancellationToken::new());
    gate.entered(1).await;
    connection
        .sender
        .try_reserve(7)
        .unwrap()
        .submit_detached(b"cleanup", CancellationToken::new())
        .unwrap();
    assert_eq!(connection.sender.retained_frames(), 2);
    assert_eq!(connection.sender.retained_bytes(), 12);
    gate.release();
    receipt.wait().await.unwrap();
    assert_eq!(client.next().await.unwrap().unwrap().into_data(), "first");
    gate.entered(2).await;
    gate.release();
    assert_eq!(client.next().await.unwrap().unwrap().into_data(), "cleanup");
    connection.sender.close();
    connection.task.await.unwrap();
    assert_eq!(connection.sender.retained_frames(), 0);
}

#[tokio::test]
async fn cancelled_detached_control_tears_down_instead_of_leaking_remote_work() {
    let (server, mut client, gate) = paused_pair().await;
    let connection = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    let receipt = enqueue(&connection.sender, b"first", CancellationToken::new());
    gate.entered(1).await;
    let cancelled = CancellationToken::new();
    connection
        .sender
        .try_reserve(7)
        .unwrap()
        .submit_detached(b"cleanup", cancelled.clone())
        .unwrap();
    cancelled.cancel();
    gate.release();
    receipt.wait().await.unwrap();
    assert_eq!(client.next().await.unwrap().unwrap().into_data(), "first");
    connection.task.await.unwrap();
    assert!(connection.sender.is_closed());
    assert_eq!(connection.sender.retained_bytes(), 0);
}

#[tokio::test]
async fn in_flight_write_survives_caller_cancellation_and_dropped_receipt() {
    let (server, mut client, gate) = paused_pair().await;
    let transport::Connection {
        sender,
        reader: _reader,
        task: writer,
    } = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    let token = CancellationToken::new();
    let first = enqueue(
        &sender,
        br#"{"type":"request","id":"first"}"#,
        token.clone(),
    );
    gate.entered(1).await;
    token.cancel();
    drop(first);
    gate.release();
    assert_eq!(
        client.next().await.unwrap().unwrap().into_text().unwrap(),
        r#"{"type":"request","id":"first"}"#
    );
    gate.release();
    let next = enqueue(
        &sender,
        br#"{"type":"request","id":"next"}"#,
        CancellationToken::new(),
    );
    next.wait().await.unwrap();
    assert_eq!(
        client.next().await.unwrap().unwrap().into_text().unwrap(),
        r#"{"type":"request","id":"next"}"#
    );
    assert!(!sender.is_closed());
    assert_eq!(sender.retained_frames(), 0);
    assert_eq!(sender.retained_bytes(), 0);
    sender.close();
    writer.await.unwrap();
}

#[tokio::test]
async fn queued_cancellation_discards_only_its_frame() {
    let (server, mut client, gate) = paused_pair().await;
    let transport::Connection {
        sender,
        reader: _reader,
        task: writer,
    } = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    let first = enqueue(&sender, br#"{"type":"first"}"#, CancellationToken::new());
    gate.entered(1).await;
    let cancelled = CancellationToken::new();
    let second = enqueue(&sender, br#"{"type":"cancelled"}"#, cancelled.clone());
    let dropped = enqueue(&sender, br#"{"type":"dropped"}"#, CancellationToken::new());
    let last = enqueue(&sender, br#"{"type":"last"}"#, CancellationToken::new());
    cancelled.cancel();
    drop(dropped);
    gate.release();
    gate.release();
    first.wait().await.unwrap();
    assert_eq!(second.wait().await, Err(Error::Cancelled));
    last.wait().await.unwrap();
    for expected in [r#"{"type":"first"}"#, r#"{"type":"last"}"#] {
        assert_eq!(
            client.next().await.unwrap().unwrap().into_text().unwrap(),
            expected
        );
    }
    sender.close();
    writer.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn queue_time_does_not_consume_the_active_write_deadline() {
    let (server, mut client, gate) = paused_pair().await;
    let transport::Connection {
        sender,
        reader: _reader,
        task: writer,
    } = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    let first = enqueue(&sender, b"{}", CancellationToken::new());
    gate.entered(1).await;
    let second = enqueue(&sender, b"{}", CancellationToken::new());
    tokio::time::advance(Duration::from_millis(4500)).await;
    gate.release();
    first.wait().await.unwrap();
    gate.entered(2).await;
    tokio::time::advance(Duration::from_millis(750)).await;
    gate.release();
    second.wait().await.unwrap();
    for _ in 0..2 {
        assert!(client.next().await.unwrap().unwrap().is_text());
    }
    assert!(!sender.is_closed());
    sender.close();
    writer.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn transport_timeout_closes_peer_and_releases_all_queued_bytes() {
    let (server, mut client, gate) = paused_pair().await;
    let transport::Connection {
        sender,
        mut reader,
        task: writer,
    } = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    let first = enqueue(&sender, b"{}", CancellationToken::new());
    gate.entered(1).await;
    let second = enqueue(&sender, b"{}", CancellationToken::new());
    tokio::time::advance(transport::WRITE_DEADLINE + Duration::from_millis(1)).await;
    assert_eq!(first.wait().await, Err(Error::WriteTimeout));
    assert_eq!(second.wait().await, Err(Error::Closed));
    writer.await.unwrap();
    assert!(sender.is_closed());
    assert_eq!(sender.retained_bytes(), 0);
    assert_eq!(sender.retained_frames(), 0);
    // Do not poll/drop Reader to make peer closure happen.
    assert!(client.next().await.is_none_or(|result| result.is_err()));
    assert!(matches!(reader.receive().await, Err(Error::Closed)));
}

#[tokio::test]
async fn admission_bounds_counts_bytes_and_encoding_before_queueing() {
    let (server, _client, _) = paused_pair().await;
    let transport::Connection {
        sender,
        reader: _reader,
        task: writer,
    } = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    let reservation = sender.try_reserve(transport::QUEUED_BYTES).unwrap();
    assert!(matches!(sender.try_reserve(1), Err(Error::Busy)));
    assert_eq!(sender.retained_bytes(), transport::QUEUED_BYTES);
    drop(reservation);
    let mut reservations = Vec::new();
    for _ in 0..transport::QUEUED_FRAMES {
        reservations.push(sender.try_reserve(1).unwrap());
    }
    assert!(matches!(sender.try_reserve(1), Err(Error::Busy)));
    drop(reservations);
    assert!(matches!(sender.try_reserve(0), Err(Error::Size)));
    assert!(matches!(
        sender.try_reserve(transport::QUEUED_BYTES + 1),
        Err(Error::Size)
    ));
    assert!(matches!(
        sender
            .try_reserve(1)
            .unwrap()
            .submit(b"{}", CancellationToken::new()),
        Err(Error::Size)
    ));
    assert_eq!(sender.retained_bytes(), 0);
    assert_eq!(sender.retained_frames(), 0);
    sender.close();
    writer.await.unwrap();
}

#[tokio::test]
async fn protobuf_socket_does_not_silently_downgrade_to_json() {
    let (server, mut client, _) = paused_pair().await;
    let transport::Connection {
        sender,
        mut reader,
        task: writer,
    } = transport::start(server, Negotiated::ProtobufV2, Direction::ToGateway).unwrap();
    client
        .send(Message::Text(r#"{"type":"hello"}"#.into()))
        .await
        .unwrap();
    assert!(matches!(reader.receive().await, Err(Error::Protocol)));
    writer.await.unwrap();
    assert!(sender.is_closed());
}

#[tokio::test]
async fn protobuf_binary_round_trip_and_terminal_exit_reason() {
    let (server, mut client, gate) = paused_pair().await;
    let transport::Connection {
        sender,
        mut reader,
        task: writer,
    } = transport::start(server, Negotiated::ProtobufV2, Direction::ToGateway).unwrap();
    let message = protobuf::types::Envelope {
        version: protobuf::VERSION,
        body: Some(protobuf::types::envelope::Body::TerminalExit(
            protobuf::types::Response {
                id: "synthetic-view".into(),
                error: "output-stalled".into(),
                result: None,
            },
        )),
    };
    let raw = protobuf::encode(&message, Direction::ToGateway).unwrap();
    client.send(Message::Binary(raw.clone())).await.unwrap();
    let transport::Incoming::Protobuf(decoded) = reader.receive().await.unwrap() else {
        panic!("not protobuf");
    };
    assert!(decoded == message);
    gate.release();
    let receipt = enqueue(&sender, &raw, CancellationToken::new());
    receipt.wait().await.unwrap();
    assert_eq!(client.next().await.unwrap().unwrap().into_data(), raw);
    sender.close();
    writer.await.unwrap();
}

#[tokio::test]
async fn unbounded_socket_config_is_rejected_before_start() {
    let (io, _other) = tokio::io::duplex(4096);
    let socket = WebSocketStream::from_raw_socket(io, Role::Server, None).await;
    assert!(matches!(
        transport::start(socket, Negotiated::JsonV1, Direction::ToGateway),
        Err(Error::Protocol)
    ));
}

#[tokio::test]
async fn aborting_an_unpolled_writer_closes_its_reader() {
    let (server, mut client, _) = paused_pair().await;
    let connection = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    connection.task.abort();
    assert!(connection.task.await.unwrap_err().is_cancelled());
    assert!(connection.sender.is_closed());
    assert!(client.next().await.is_none_or(|result| result.is_err()));
    let mut reader = connection.reader;
    assert!(matches!(reader.receive().await, Err(Error::Closed)));
}

#[tokio::test]
async fn idle_reader_and_full_handoff_cannot_prevent_socket_teardown() {
    let (server, mut client, _) = paused_pair().await;
    let connection = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    for _ in 0..3 {
        client.send(Message::Text("{}".into())).await.unwrap();
    }
    tokio::task::yield_now().await;
    connection.sender.close();
    connection.task.await.unwrap();
    // Keep the unused Reader alive until after the remote endpoint sees EOF.
    let closed = tokio::time::timeout(Duration::from_secs(1), client.next())
        .await
        .unwrap();
    assert!(closed.is_none_or(|result| result.is_err()));
    drop(connection.reader);
}

#[tokio::test(start_paused = true)]
async fn heartbeat_sends_ping_and_accepts_exact_pong() {
    let (server, mut client) = pair().await;
    let connection = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(transport::HEARTBEAT_INTERVAL).await;
    let Some(Ok(Message::Ping(first))) = client.next().await else {
        panic!("first heartbeat ping missing")
    };
    assert_eq!(first.len(), 8);
    client.send(Message::Pong(first.clone())).await.unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(transport::HEARTBEAT_INTERVAL).await;
    let Some(Ok(Message::Ping(second))) = client.next().await else {
        panic!("second heartbeat ping missing")
    };
    assert_ne!(first, second);
    assert!(!connection.sender.is_closed());
    connection.sender.close();
    connection.task.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn busy_data_and_wrong_pong_do_not_satisfy_heartbeat() {
    let (server, mut client) = pair().await;
    let mut connection =
        transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(transport::HEARTBEAT_INTERVAL).await;
    // Do not read the Ping: Tungstenite would automatically queue its matching
    // Pong. Unsolicited wrong Pong and application traffic must not count.
    client
        .send(Message::Pong(b"wrong".as_slice().into()))
        .await
        .unwrap();
    for _ in 0..3 {
        client.send(Message::Text("{}".into())).await.unwrap();
        assert!(matches!(
            connection.reader.receive().await,
            Ok(transport::Incoming::Json(_))
        ));
    }
    tokio::time::advance(transport::HEARTBEAT_DEADLINE + Duration::from_millis(1)).await;
    connection.task.await.unwrap();
    assert!(connection.sender.is_closed());
}

#[tokio::test(start_paused = true)]
async fn peer_ping_is_flushed_without_application_writes() {
    let (server, mut client) = pair().await;
    let connection = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    client
        .send(Message::Ping(b"peer".as_slice().into()))
        .await
        .unwrap();
    let Some(Ok(Message::Pong(payload))) = client.next().await else {
        panic!("automatic pong was not flushed")
    };
    assert_eq!(payload, b"peer"[..]);
    connection.sender.close();
    connection.task.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn shutdown_interrupts_stalled_heartbeat_write() {
    let (server, _client, gate) = paused_pair().await;
    let connection = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(transport::HEARTBEAT_INTERVAL).await;
    gate.entered(1).await;
    connection.sender.close();
    tokio::time::timeout(Duration::from_secs(1), connection.task)
        .await
        .expect("stalled heartbeat prevented shutdown")
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn matching_pong_preserves_write_across_heartbeat_deadline() {
    let (server, mut client, gate) = paused_pair().await;
    let connection = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    gate.release();
    tokio::task::yield_now().await;
    tokio::time::advance(transport::HEARTBEAT_INTERVAL).await;
    let Some(Ok(Message::Ping(nonce))) = client.next().await else {
        panic!("heartbeat ping missing")
    };
    tokio::time::advance(Duration::from_secs(9)).await;
    let receipt = enqueue(&connection.sender, b"{}", CancellationToken::new());
    gate.entered(2).await;
    client.send(Message::Pong(nonce)).await.unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(1) + Duration::from_millis(1)).await;
    gate.release();
    receipt.wait().await.unwrap();
    assert!(!connection.sender.is_closed());
    connection.sender.close();
    connection.task.await.unwrap();
}

#[tokio::test]
async fn browser_frames_preserve_binary_text_and_close_codes_with_smaller_limits() {
    let (left, right) = tokio::io::duplex(4096);
    let config = transport::socket_config()
        .max_message_size(Some(65536))
        .max_frame_size(Some(65536))
        .max_write_buffer_size(65536 + 4096);
    let server = WebSocketStream::from_raw_socket(left, Role::Server, Some(config)).await;
    let mut client = WebSocketStream::from_raw_socket(right, Role::Client, Some(config)).await;
    let mut frames = transport::start_frames(server, 65536).unwrap();
    client
        .send(Message::Binary(vec![0, 255, 1].into()))
        .await
        .unwrap();
    assert!(
        matches!(frames.reader.receive().await.unwrap(), Message::Binary(data) if data.as_ref() == [0,255,1])
    );
    frames
        .sender
        .send(false, br#"{"type":"ready"}"#)
        .await
        .unwrap();
    assert!(
        matches!(client.next().await, Some(Ok(Message::Text(raw))) if raw.as_str() == r#"{"type":"ready"}"#)
    );
    assert_eq!(
        frames.sender.send(true, &vec![0; 65537]).await,
        Err(transport::Error::Size)
    );
    frames
        .sender
        .close_with(4002, "Terminal rendering stalled")
        .await;
    assert!(
        matches!(client.next().await, Some(Ok(Message::Close(Some(frame)))) if u16::from(frame.code) == 4002)
    );
    frames.task.await.unwrap();
}

#[tokio::test]
async fn dropping_a_pending_close_still_tears_down_the_transport() {
    let (server, _client, gate) = paused_pair().await;
    let connection = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    let sender = connection.sender.clone();
    let closing = tokio::spawn(async move { sender.close_with(1008, "Rejected").await });
    gate.entered(1).await;
    closing.abort();
    let _ = closing.await;
    tokio::time::timeout(Duration::from_secs(1), connection.task)
        .await
        .unwrap()
        .unwrap();
    assert!(connection.sender.is_closed());
}

#[tokio::test(start_paused = true)]
async fn stalled_close_is_joined_before_http_cleanup_deadline() {
    let (server, _client, gate) = paused_pair().await;
    let connection = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    let sender = connection.sender.clone();
    let closing = tokio::spawn(async move { sender.close_with(1001, "Gateway stopping").await });
    gate.entered(1).await;
    tokio::time::advance(transport::CLOSE_DEADLINE).await;
    tokio::time::timeout(Duration::from_millis(1), async {
        closing.await.unwrap();
        connection.task.await.unwrap();
    })
    .await
    .expect("close exceeded its cleanup budget");
    assert!(connection.sender.is_closed());
    assert_eq!(connection.sender.retained_bytes(), 0);
    assert_eq!(connection.sender.retained_frames(), 0);
}
