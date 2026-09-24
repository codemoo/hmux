use bytes::Bytes;
use futures_util::{task::AtomicWaker, SinkExt, StreamExt};
use hmux_gateway::hub::{Error, Hub, UploadEvent, ViewEvent, MAX_PENDING};
use hmux_protocol::{
    actions,
    protobuf::{self as pb, types as p, Direction, Negotiated},
    snapshots, transport,
};
use prost::Message as _;
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

type Client = WebSocketStream<DuplexStream>;

#[derive(Default)]
struct WriteGate {
    allowed: AtomicUsize,
    entered: AtomicUsize,
    wake: AtomicWaker,
    changed: Notify,
}
impl WriteGate {
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
    inner: DuplexStream,
    gate: Arc<WriteGate>,
    written: usize,
}
impl AsyncRead for PausedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for PausedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.gate.wake.register(cx.waker());
        self.gate.entered.store(self.written + 1, Ordering::SeqCst);
        self.gate.changed.notify_waiters();
        if self.gate.allowed.load(Ordering::SeqCst) <= self.written {
            return Poll::Pending;
        }
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        if matches!(result, Poll::Ready(Ok(_))) {
            self.written += 1;
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

async fn connect(hub: &Hub) -> (hmux_gateway::hub::HomeConnection, Client) {
    connect_protocol(hub, Negotiated::ProtobufV2).await
}
async fn connect_protocol(
    hub: &Hub,
    protocol: Negotiated,
) -> (hmux_gateway::hub::HomeConnection, Client) {
    let (left, right) = tokio::io::duplex(4096);
    let server =
        WebSocketStream::from_raw_socket(left, Role::Server, Some(transport::socket_config()))
            .await;
    let client =
        WebSocketStream::from_raw_socket(right, Role::Client, Some(transport::socket_config()))
            .await;
    let connection = transport::start(server, protocol, Direction::ToGateway).unwrap();
    (hub.attach(connection).unwrap(), client)
}
async fn receive(client: &mut Client) -> p::envelope::Body {
    let message = tokio::time::timeout(Duration::from_secs(2), client.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(message.is_binary());
    pb::decode(message.into_data(), Direction::ToHome)
        .unwrap()
        .body
        .unwrap()
}
async fn send(client: &mut Client, body: p::envelope::Body) {
    let raw = pb::encode(
        &p::Envelope {
            version: pb::VERSION,
            body: Some(body),
        },
        Direction::ToGateway,
    )
    .unwrap();
    client.send(Message::Binary(raw)).await.unwrap();
}
async fn receive_codec(client: &mut Client, protocol: Negotiated) -> p::envelope::Body {
    let frame = tokio::time::timeout(Duration::from_secs(2), client.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let raw = frame.into_data();
    match protocol {
        Negotiated::ProtobufV2 => pb::decode(raw, Direction::ToHome).unwrap(),
        Negotiated::JsonV1 => hmux_protocol::legacy::from_json(
            hmux_protocol::wire::Message::decode(&raw).unwrap(),
            Direction::ToHome,
        )
        .unwrap(),
    }
    .body
    .unwrap()
}
async fn send_codec(client: &mut Client, protocol: Negotiated, body: p::envelope::Body) {
    let envelope = p::Envelope {
        version: pb::VERSION,
        body: Some(body),
    };
    let frame = match protocol {
        Negotiated::ProtobufV2 => {
            Message::Binary(pb::encode(&envelope, Direction::ToGateway).unwrap())
        }
        Negotiated::JsonV1 => Message::Text(
            String::from_utf8(
                hmux_protocol::legacy::to_json(envelope, Direction::ToGateway)
                    .unwrap()
                    .encode()
                    .unwrap(),
            )
            .unwrap()
            .into(),
        ),
    };
    client.send(frame).await.unwrap();
}
fn request() -> p::Request {
    p::Request {
        id: String::new(),
        operation: p::Operation::Profiles as i32,
        session: None,
        payload: Some(p::request::Payload::Empty(p::Empty {})),
    }
}

#[tokio::test]
async fn transport_observation_reports_failure_and_cancel_without_remote_data() {
    use hmux_gateway::observation::{Event, Stage};
    let (events, mut observed) = tokio::sync::mpsc::channel::<Event>(16);
    let (hub, _completions) = Hub::with_reporter(Some(Arc::new(move |event| {
        let _ = events.try_send(event);
    })));
    let (connection, mut client) = connect(&hub).await;
    assert!(matches!(
        observed.recv().await.unwrap().stage,
        Stage::HomeConnected
    ));
    let generation = hub.snapshot().generation.unwrap();
    let request_hub = hub.clone();
    let task = tokio::spawn(async move { request_hub.request(generation, request()).await });
    let p::envelope::Body::Request(message) = receive(&mut client).await else {
        panic!("request expected")
    };
    let mut reply = response(message.id);
    reply.error = "private-remote-error-MUST-NOT-APPEAR".into();
    reply.result = None;
    send(&mut client, p::envelope::Body::Response(reply)).await;
    assert!(!task.await.unwrap().unwrap().error.is_empty());
    let event = observed.recv().await.unwrap();
    assert!(matches!(event.stage, Stage::RequestComplete));
    assert_eq!(
        event.reason,
        Some(hmux_gateway::hub::Error::RemoteOperation)
    );
    assert!(!event.to_string().contains("private-remote"));
    let request_hub = hub.clone();
    let task = tokio::spawn(async move { request_hub.request(generation, request()).await });
    let _ = receive(&mut client).await;
    task.abort();
    assert!(task.await.err().is_some_and(|error| error.is_cancelled()));
    let event = observed.recv().await.unwrap();
    assert_eq!(event.reason, Some(hmux_gateway::hub::Error::Cancelled));
    drop(client);
    connection.wait().await;
    assert!(matches!(
        observed.recv().await.unwrap().stage,
        Stage::HomeDisconnected
    ));
}
fn response(id: String) -> p::Response {
    p::Response {
        id,
        error: String::new(),
        result: Some(p::response::Result::Profiles(p::ProfilesResult {
            items: vec![p::Profile {
                id: "shell".into(),
                label: "Shell".into(),
            }],
        })),
    }
}
fn ok_response(id: String) -> p::Response {
    p::Response {
        id,
        error: String::new(),
        result: Some(p::response::Result::Ok(p::Empty {})),
    }
}
fn open() -> p::TerminalOpen {
    p::TerminalOpen {
        id: String::new(),
        session: Some(p::Session {
            id: "$1".into(),
            created_at: 42,
        }),
        cols: 80,
        rows: 24,
        capabilities: Vec::new(),
    }
}
async fn open_view(
    hub: &Hub,
    generation: hmux_gateway::hub::Generation,
    client: &mut Client,
) -> hmux_gateway::hub::ViewLease {
    let hub = hub.clone();
    let opening = tokio::spawn(async move { hub.open_view(generation, open()).await });
    let p::envelope::Body::TerminalOpen(frame) = receive(client).await else {
        panic!("not open")
    };
    send(client, p::envelope::Body::Response(ok_response(frame.id))).await;
    opening.await.unwrap().unwrap()
}

#[tokio::test]
async fn dropped_request_sends_cancel_and_does_not_block_next_request() {
    let (hub, _events) = Hub::new();
    let (home, mut client) = connect(&hub).await;
    let generation = home.generation();
    let first = tokio::spawn({
        let hub = hub.clone();
        async move { hub.request(generation, request()).await }
    });
    let p::envelope::Body::Request(sent) = receive(&mut client).await else {
        panic!("not request")
    };
    first.abort();
    assert!(matches!(first.await, Err(error) if error.is_cancelled()));
    let p::envelope::Body::Cancel(cancel) = receive(&mut client).await else {
        panic!("not cancel")
    };
    assert_eq!(cancel.id, sent.id);
    let second = tokio::spawn({
        let hub = hub.clone();
        async move { hub.request(generation, request()).await }
    });
    let p::envelope::Body::Request(sent) = receive(&mut client).await else {
        panic!("not request")
    };
    send(&mut client, p::envelope::Body::Response(response(sent.id))).await;
    assert!(matches!(
        second.await.unwrap().unwrap().result.as_ref(),
        Some(p::response::Result::Profiles(_))
    ));
    drop(home);
}

#[tokio::test]
async fn old_generation_reply_and_lease_cannot_reach_replacement() {
    let (hub, _events) = Hub::new();
    let (first_home, mut first_client) = connect(&hub).await;
    let old = first_home.generation();
    let pending = tokio::spawn({
        let hub = hub.clone();
        async move { hub.request(old, request()).await }
    });
    let p::envelope::Body::Request(old_request) = receive(&mut first_client).await else {
        panic!("not request")
    };
    let old_view = open_view(&hub, old, &mut first_client).await;
    drop(first_home);
    assert!(matches!(
        pending.await.unwrap(),
        Err(Error::Offline) | Err(Error::Stale)
    ));
    let (second_home, mut second_client) = connect(&hub).await;
    let new = second_home.generation();
    assert_ne!(old, new);
    drop(old_view);
    assert!(matches!(
        hub.request(old, request()).await,
        Err(Error::Stale)
    ));
    let next = tokio::spawn({
        let hub = hub.clone();
        async move { hub.request(new, request()).await }
    });
    let p::envelope::Body::Request(new_request) = receive(&mut second_client).await else {
        panic!("not request")
    };
    send(
        &mut second_client,
        p::envelope::Body::Response(response(old_request.id)),
    )
    .await;
    assert!(!next.is_finished());
    send(
        &mut second_client,
        p::envelope::Body::Response(response(new_request.id)),
    )
    .await;
    assert!(next.await.unwrap().is_ok());
    drop(second_home);
}

#[tokio::test]
async fn pending_requests_are_bounded_and_release_on_drop() {
    let (hub, _events) = Hub::new();
    let (home, mut client) = connect(&hub).await;
    let generation = home.generation();
    let mut pending = Vec::new();
    for _ in 0..MAX_PENDING {
        pending.push(tokio::spawn({
            let hub = hub.clone();
            async move { hub.request(generation, request()).await }
        }));
        assert!(matches!(
            receive(&mut client).await,
            p::envelope::Body::Request(_)
        ));
    }
    assert!(matches!(
        hub.request(generation, request()).await,
        Err(Error::Busy)
    ));
    for task in pending {
        task.abort();
        let _ = task.await;
    }
    drop(home);
}

#[tokio::test]
async fn slow_view_is_closed_without_blocking_another_view() {
    let (hub, _events) = Hub::new();
    let (home, mut client) = connect(&hub).await;
    let generation = home.generation();
    send(
        &mut client,
        p::envelope::Body::Hello(p::Hello {
            capabilities: vec!["terminal-output-flow-v1".into()],
        }),
    )
    .await;
    let mut slow = open_view(&hub, generation, &mut client).await;
    let mut fast = open_view(&hub, generation, &mut client).await;
    assert!(fast.output_flow());
    for _ in 0..=hmux_protocol::flow::FRAMES {
        send(
            &mut client,
            p::envelope::Body::TerminalOutput(p::Data {
                id: slow.id().into(),
                data: Bytes::from_static(b"x"),
            }),
        )
        .await;
    }
    send(
        &mut client,
        p::envelope::Body::TerminalOutput(p::Data {
            id: fast.id().into(),
            data: Bytes::from_static(b"ok"),
        }),
    )
    .await;
    let ViewEvent::Data(data) = tokio::time::timeout(Duration::from_secs(2), fast.receive())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("fast view stalled")
    };
    assert_eq!(data, b"ok"[..]);
    assert!(matches!(slow.receive().await, Err(Error::OutputFull)));
    let p::envelope::Body::Close(close) = receive(&mut client).await else {
        panic!("no disposable-view close")
    };
    assert_eq!(close.id, slow.id());
    drop(home);
}

#[tokio::test]
async fn view_drop_sends_close_and_disconnect_clears_caches_and_events() {
    let (hub, mut completions) = Hub::new();
    let (home, mut client) = connect(&hub).await;
    let generation = home.generation();
    send(
        &mut client,
        p::envelope::Body::Hello(p::Hello {
            capabilities: vec!["terminal-output-flow-v1".into(), "web-upload-v1".into()],
        }),
    )
    .await;
    let view = open_view(&hub, generation, &mut client).await;
    let id = view.id().to_owned();
    drop(view);
    let p::envelope::Body::Close(close) = receive(&mut client).await else {
        panic!("no close")
    };
    assert_eq!(close.id, id);
    send(
        &mut client,
        p::envelope::Body::Catalog(Box::new(
            snapshots::catalog_from_json(br#"{"sessions":[]}"#).unwrap(),
        )),
    )
    .await;
    send(
        &mut client,
        p::envelope::Body::Usage(Box::new(
            snapshots::usage_to_proto(hmux_usage::Snapshot::degraded(
                hmux_usage::Provider::Codex,
                1,
                "2026-09-24T00:00:00Z".parse().unwrap(),
                "ok",
            ))
            .unwrap(),
        )),
    )
    .await;
    send(
        &mut client,
        p::envelope::Body::TaskComplete(p::Completion {
            id: "a".repeat(64),
            session: Some(p::Session {
                id: "$1".into(),
                created_at: 42,
            }),
            completed_at: chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
                .to_rfc3339(),
        }),
    )
    .await;
    let event = tokio::time::timeout(Duration::from_secs(2), completions.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.id, "a".repeat(64));
    let snapshot = hub.snapshot();
    assert!(snapshot.online && snapshot.output_flow && snapshot.upload);
    assert!(snapshot.catalog.is_some() && snapshot.codex_usage.is_some());
    drop(home);
    let snapshot = hub.snapshot();
    assert!(
        !snapshot.connected
            && !snapshot.online
            && snapshot.catalog.is_none()
            && snapshot.codex_usage.is_none()
    );
}

#[tokio::test]
async fn upload_events_are_bounded_and_generation_scoped() {
    let (hub, _events) = Hub::new();
    let (home, mut client) = connect(&hub).await;
    let generation = home.generation();
    send(
        &mut client,
        p::envelope::Body::Hello(p::Hello {
            capabilities: vec!["web-upload-v1".into()],
        }),
    )
    .await;
    // A catalog barrier ensures the capability hello has been processed.
    send(
        &mut client,
        p::envelope::Body::Catalog(Box::new(snapshots::catalog_from_json(b"{}").unwrap())),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while !hub.snapshot().upload {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let header = p::UploadHeader {
        protocol_version: 1,
        request_id: String::new(),
        session: Some(p::Session {
            id: "$1".into(),
            created_at: 42,
        }),
        file_count: 1,
        total_bytes: 1,
        files: vec![p::FileHeader {
            index: 0,
            size: 1,
            extension: "txt".into(),
        }],
    };
    let opening = tokio::spawn({
        let hub = hub.clone();
        async move {
            hub.open_upload(
                generation,
                p::UploadStart {
                    id: String::new(),
                    header: Some(header),
                },
            )
            .await
        }
    });
    let p::envelope::Body::UploadStart(start) = receive(&mut client).await else {
        panic!("not upload start")
    };
    let mut upload = opening.await.unwrap().unwrap();
    assert_eq!(upload.id(), start.id);
    send(
        &mut client,
        p::envelope::Body::UploadReady(p::Reference {
            id: start.id.clone(),
        }),
    )
    .await;
    assert!(matches!(
        upload.receive().await.unwrap(),
        UploadEvent::Ready
    ));
    drop(upload);
    let p::envelope::Body::UploadCancel(cancel) = receive(&mut client).await else {
        panic!("not upload cancel")
    };
    assert_eq!(cancel.id, start.id);
    drop(home);
}

#[tokio::test]
async fn queued_upload_progress_preserves_terminal_result() {
    for is_error in [false, true] {
        let (hub, _events) = Hub::new();
        let (home, mut client) = connect(&hub).await;
        let generation = home.generation();
        send(
            &mut client,
            p::envelope::Body::Hello(p::Hello {
                capabilities: vec!["web-upload-v1".into()],
            }),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while !hub.snapshot().upload {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let opening = tokio::spawn({
            let hub = hub.clone();
            async move {
                hub.open_upload(
                    generation,
                    p::UploadStart {
                        id: String::new(),
                        header: Some(p::UploadHeader {
                            protocol_version: 1,
                            request_id: String::new(),
                            session: Some(p::Session {
                                id: "$1".into(),
                                created_at: 42,
                            }),
                            file_count: 1,
                            total_bytes: 1,
                            files: vec![p::FileHeader {
                                index: 0,
                                size: 1,
                                extension: "txt".into(),
                            }],
                        }),
                    },
                )
                .await
            }
        });
        let p::envelope::Body::UploadStart(start) = receive(&mut client).await else {
            panic!("not upload start")
        };
        send(
            &mut client,
            p::envelope::Body::UploadReady(p::Reference {
                id: start.id.clone(),
            }),
        )
        .await;
        send(
            &mut client,
            p::envelope::Body::UploadAck(p::Ack {
                id: start.id.clone(),
                received: 1,
            }),
        )
        .await;
        let result = p::Response {
            id: start.id,
            error: if is_error {
                "upload failed".into()
            } else {
                String::new()
            },
            result: if is_error {
                None
            } else {
                Some(p::response::Result::Staged(Box::new(p::StageResult {
                    protocol_version: 1,
                    request_id: "request".into(),
                    stage_id: "stage".into(),
                    session: Some(p::Session {
                        id: "$1".into(),
                        created_at: 42,
                    }),
                    expires_at_unix: 1_800_000_000,
                    files: vec![p::StageFile {
                        index: 0,
                        path: "/tmp/staged".into(),
                        size: 1,
                        sha256: "a".repeat(64),
                    }],
                })))
            },
        };
        send(
            &mut client,
            if is_error {
                p::envelope::Body::UploadError(result)
            } else {
                p::envelope::Body::UploadComplete(result)
            },
        )
        .await;
        // The catalog serves as an ordered barrier so all three upload events
        // are queued before the consumer starts receiving them.
        send(
            &mut client,
            p::envelope::Body::Catalog(Box::new(snapshots::catalog_from_json(b"{}").unwrap())),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while !hub.snapshot().online {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut upload = opening.await.unwrap().unwrap();
        assert!(matches!(
            upload.receive().await.unwrap(),
            UploadEvent::Ready
        ));
        assert!(matches!(
            upload.receive().await.unwrap(),
            UploadEvent::Ack(1)
        ));
        let terminal = upload.receive().await.unwrap();
        assert!(matches!(
            (is_error, terminal),
            (false, UploadEvent::Complete(_)) | (true, UploadEvent::Error(_))
        ));
        assert!(matches!(upload.receive().await, Err(Error::Stale)));
        assert!(matches!(upload.finish().await, Err(Error::Stale)));
        drop(upload);
        drop(home);
    }
}

#[tokio::test]
async fn active_home_rejects_second_peer() {
    let (hub, _events) = Hub::new();
    let (home, _client) = connect(&hub).await;
    let (left, right) = tokio::io::duplex(4096);
    let server =
        WebSocketStream::from_raw_socket(left, Role::Server, Some(transport::socket_config()))
            .await;
    let second_client =
        WebSocketStream::from_raw_socket(right, Role::Client, Some(transport::socket_config()))
            .await;
    let second = transport::start(server, Negotiated::ProtobufV2, Direction::ToGateway).unwrap();
    let Err(rejected) = hub.attach(second) else {
        panic!("second Home accepted")
    };
    assert_eq!(rejected.reason, Error::Busy);
    rejected.close().await;
    assert_eq!(hub.snapshot().generation, Some(home.generation()));
    drop(second_client);
    drop(home);
}

#[tokio::test]
async fn completion_overflow_or_absent_observer_does_not_disconnect_or_stall_home() {
    let (hub, mut events) = Hub::new();
    let (home, mut client) = connect(&hub).await;
    let generation = home.generation();
    let now = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now());
    let completion = |index: usize| {
        p::envelope::Body::TaskComplete(p::Completion {
            id: format!("{index:064x}"),
            session: Some(p::Session {
                id: "$1".into(),
                created_at: 42,
            }),
            completed_at: now.to_rfc3339(),
        })
    };
    for _ in 0..hmux_gateway::hub::COMPLETION_EVENTS + 8 {
        send(&mut client, completion(0)).await;
        for (id, at) in [
            ("b".repeat(64), now - chrono::Duration::seconds(121)),
            ("c".repeat(64), now + chrono::Duration::seconds(120)),
            ("not-a-valid-event-id".into(), now),
        ] {
            send(
                &mut client,
                p::envelope::Body::TaskComplete(p::Completion {
                    id,
                    session: Some(p::Session {
                        id: "$1".into(),
                        created_at: 42,
                    }),
                    completed_at: at.to_rfc3339(),
                }),
            )
            .await;
        }
    }
    for index in 0..hmux_gateway::hub::COMPLETION_EVENTS + 8 {
        send(&mut client, completion(index)).await;
    }
    // A response behind the burst is a shared-reader barrier: the overfull
    // observer must neither disconnect Home nor prevent unrelated replies.
    async fn round_trip(hub: &Hub, client: &mut Client) {
        let generation = hub.snapshot().generation.unwrap();
        let request_hub = hub.clone();
        let reply = tokio::spawn(async move { request_hub.request(generation, request()).await });
        let p::envelope::Body::Request(frame) = receive(client).await else {
            panic!("missing unrelated request")
        };
        send(client, p::envelope::Body::Response(response(frame.id))).await;
        let response = tokio::time::timeout(Duration::from_secs(2), reply)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            response.result.as_ref(),
            Some(p::response::Result::Profiles(_))
        ));
    }
    round_trip(&hub, &mut client).await;
    assert_eq!(hub.snapshot().generation, Some(generation));
    assert_eq!(events.len(), hmux_gateway::hub::COMPLETION_EVENTS);
    for index in 0..hmux_gateway::hub::COMPLETION_EVENTS {
        assert_eq!(events.try_recv().unwrap().id, format!("{index:064x}"));
    }
    assert!(events.try_recv().is_err());
    send(&mut client, completion(100)).await;
    let next = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.id, format!("{:064x}", 100));

    drop(events);
    send(&mut client, completion(101)).await;
    round_trip(&hub, &mut client).await;
    // Terminal bytes also keep flowing when notifications are disabled.
    let mut view = open_view(&hub, generation, &mut client).await;
    send(
        &mut client,
        p::envelope::Body::TerminalOutput(p::Data {
            id: view.id().into(),
            data: Bytes::from_static(b"still connected"),
        }),
    )
    .await;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), view.receive()).await.unwrap().unwrap(),
        ViewEvent::Data(data) if data.as_ref() == b"still connected"
    ));
    assert_eq!(hub.snapshot().generation, Some(generation));
    drop(view);
    drop(client);
    home.wait().await;
}

#[tokio::test]
async fn final_output_can_be_acknowledged_before_terminal_exit() {
    let (hub, _events) = Hub::new();
    let (home, mut client) = connect(&hub).await;
    let generation = home.generation();
    send(
        &mut client,
        p::envelope::Body::Hello(p::Hello {
            capabilities: vec!["terminal-output-flow-v1".into()],
        }),
    )
    .await;
    let mut view = open_view(&hub, generation, &mut client).await;
    send(
        &mut client,
        p::envelope::Body::TerminalOutput(p::Data {
            id: view.id().into(),
            data: Bytes::from_static(b"end"),
        }),
    )
    .await;
    send(
        &mut client,
        p::envelope::Body::TerminalExit(p::Response {
            id: view.id().into(),
            error: String::new(),
            result: None,
        }),
    )
    .await;
    let ViewEvent::Data(data) = view.receive().await.unwrap() else {
        panic!("final output lost")
    };
    assert_eq!(data, b"end"[..]);
    view.acknowledge(3).await.unwrap();
    let p::envelope::Body::OutputAck(ack) = receive(&mut client).await else {
        panic!("final output ACK lost")
    };
    assert_eq!(ack.id, view.id());
    assert_eq!(ack.received, 3);
    assert!(matches!(view.receive().await, Ok(ViewEvent::Exit { .. })));
    assert!(matches!(view.receive().await, Err(Error::Stale)));
    let another = open_view(&hub, generation, &mut client).await;
    drop(another);
    drop(home);
}

#[tokio::test]
async fn full_output_credit_window_keeps_refresh_and_normal_exit_in_order() {
    let (hub, _events) = Hub::new();
    let (home, mut client) = connect(&hub).await;
    send(
        &mut client,
        p::envelope::Body::Hello(p::Hello {
            capabilities: vec![hmux_protocol::flow::CAPABILITY.into()],
        }),
    )
    .await;
    let mut view = open_view(&hub, home.generation(), &mut client).await;
    for i in 0..hmux_protocol::flow::FRAMES {
        send(
            &mut client,
            p::envelope::Body::TerminalOutput(p::Data {
                id: view.id().into(),
                data: vec![i as u8].into(),
            }),
        )
        .await;
    }
    send(
        &mut client,
        p::envelope::Body::RefreshResult(p::Response {
            id: view.id().into(),
            ..Default::default()
        }),
    )
    .await;
    send(
        &mut client,
        p::envelope::Body::TerminalExit(p::Response {
            id: view.id().into(),
            ..Default::default()
        }),
    )
    .await;
    // A later shared-reader event is a barrier proving all output/control
    // events were dispatched before the consumer drains even one frame.
    send(
        &mut client,
        p::envelope::Body::Catalog(Box::new(
            snapshots::catalog_from_json(br#"{"sessions":[]}"#).unwrap(),
        )),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while !hub.snapshot().online {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for i in 0..hmux_protocol::flow::FRAMES {
        let ViewEvent::Data(data) = view.receive().await.unwrap() else {
            panic!("queued output lost")
        };
        assert_eq!(data.as_ref(), [i as u8]);
        view.acknowledge(1).await.unwrap();
        let p::envelope::Body::OutputAck(ack) = receive(&mut client).await else {
            panic!("missing FIFO ACK")
        };
        assert_eq!(ack.id, view.id());
        assert_eq!(ack.received, 1);
    }
    assert!(matches!(
        view.receive().await,
        Ok(ViewEvent::RefreshResult { ok: true })
    ));
    assert!(matches!(view.receive().await, Ok(ViewEvent::Exit {error}) if error.is_empty()));
    assert!(hub.snapshot().connected);
    drop(view);
    let p::envelope::Body::Close(_) = receive(&mut client).await else {
        panic!("missing disposable cleanup")
    };
    drop(home);
}

#[tokio::test]
async fn json_v1_uses_legacy_adapter_and_text_frames() {
    use hmux_protocol::wire;
    let (hub, _events) = Hub::new();
    let (left, right) = tokio::io::duplex(4096);
    let server =
        WebSocketStream::from_raw_socket(left, Role::Server, Some(transport::socket_config()))
            .await;
    let mut client =
        WebSocketStream::from_raw_socket(right, Role::Client, Some(transport::socket_config()))
            .await;
    let connection = transport::start(server, Negotiated::JsonV1, Direction::ToGateway).unwrap();
    let home = hub.attach(connection).unwrap();
    let generation = home.generation();
    let pending = tokio::spawn({
        let hub = hub.clone();
        async move { hub.request(generation, request()).await }
    });
    let frame = tokio::time::timeout(Duration::from_secs(2), client.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(frame.is_text());
    let sent = wire::Message::decode(frame.into_text().unwrap().as_bytes()).unwrap();
    assert_eq!(sent.kind, "request");
    assert_eq!(sent.operation, "profiles");
    let reply = wire::Message {
        kind: "response".into(),
        id: sent.id,
        payload: Some(serde_json::value::RawValue::from_string(r#"[]"#.into()).unwrap()),
        ..wire::Message::default()
    };
    client
        .send(Message::Text(
            String::from_utf8(reply.encode().unwrap()).unwrap().into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        pending.await.unwrap().unwrap().result.as_ref(),
        Some(p::response::Result::Profiles(_))
    ));
    drop(home);
}

#[tokio::test]
async fn caller_abort_after_write_starts_finishes_frame_and_keeps_home_alive() {
    let (hub, _events) = Hub::new();
    let (left, right) = tokio::io::duplex(4096);
    let gate = Arc::new(WriteGate::default());
    let server = WebSocketStream::from_raw_socket(
        PausedIo {
            inner: left,
            gate: gate.clone(),
            written: 0,
        },
        Role::Server,
        Some(transport::socket_config()),
    )
    .await;
    let mut client =
        WebSocketStream::from_raw_socket(right, Role::Client, Some(transport::socket_config()))
            .await;
    let connection =
        transport::start(server, Negotiated::ProtobufV2, Direction::ToGateway).unwrap();
    let home = hub.attach(connection).unwrap();
    let generation = home.generation();
    let pending = tokio::spawn({
        let hub = hub.clone();
        async move { hub.request(generation, request()).await }
    });
    gate.entered(1).await;
    pending.abort();
    assert!(matches!(pending.await, Err(error) if error.is_cancelled()));
    gate.release();
    let p::envelope::Body::Request(first) = receive(&mut client).await else {
        panic!("first frame interrupted")
    };
    gate.release();
    let p::envelope::Body::Cancel(cancel) = receive(&mut client).await else {
        panic!("no cleanup")
    };
    assert_eq!(cancel.id, first.id);
    assert!(hub.snapshot().connected);
    gate.release();
    let next = tokio::spawn({
        let hub = hub.clone();
        async move { hub.request(generation, request()).await }
    });
    let p::envelope::Body::Request(next_frame) = receive(&mut client).await else {
        panic!("next frame missing")
    };
    send(
        &mut client,
        p::envelope::Body::Response(response(next_frame.id)),
    )
    .await;
    assert!(next.await.unwrap().is_ok());
    drop(home);
}

#[tokio::test]
async fn both_codecs_match_typed_reply_to_pending_operation() {
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let (hub, _) = Hub::new();
        let (home, mut client) = connect_protocol(&hub, protocol).await;
        let generation = home.generation();
        let pending = tokio::spawn({
            let hub = hub.clone();
            async move { hub.request(generation, request()).await }
        });
        let p::envelope::Body::Request(sent) = receive_codec(&mut client, protocol).await else {
            panic!("profiles request expected")
        };
        assert_eq!(sent.operation, p::Operation::Profiles as i32);
        assert!(matches!(sent.payload, Some(p::request::Payload::Empty(_))));
        send_codec(
            &mut client,
            protocol,
            p::envelope::Body::Response(response(sent.id)),
        )
        .await;
        let reply = pending.await.unwrap().unwrap();
        assert!(
            matches!(reply.result.as_ref(), Some(p::response::Result::Profiles(v)) if v.items[0].id == "shell")
        );
        assert!(hub.snapshot().connected);
        drop(reply);
        drop(home);
    }
}

#[tokio::test]
async fn malformed_typed_request_is_rejected_before_home_action() {
    let (hub, _) = Hub::new();
    let (home, mut client) = connect(&hub).await;
    let generation = home.generation();
    let mut invalid = request();
    invalid.payload = Some(p::request::Payload::Create(p::CreateRequest {
        profile: Some("shell".into()),
        name: None,
    }));
    assert!(matches!(
        hub.request(generation, invalid).await,
        Err(Error::Invalid)
    ));
    let pending = tokio::spawn({
        let hub = hub.clone();
        async move { hub.request(generation, request()).await }
    });
    let p::envelope::Body::Request(sent) = receive(&mut client).await else {
        panic!("valid request expected")
    };
    assert!(matches!(sent.payload, Some(p::request::Payload::Empty(_))));
    send(&mut client, p::envelope::Body::Response(response(sent.id))).await;
    assert!(pending.await.unwrap().is_ok());
    assert!(hub.snapshot().connected);
    drop(home);
}

#[tokio::test]
async fn wrong_reply_kind_disconnects_home_for_both_codecs() {
    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let (hub, _) = Hub::new();
        let (home, mut client) = connect_protocol(&hub, protocol).await;
        let generation = home.generation();
        let pending = tokio::spawn({
            let hub = hub.clone();
            async move { hub.request(generation, request()).await }
        });
        let p::envelope::Body::Request(sent) = receive_codec(&mut client, protocol).await else {
            panic!("profiles request expected")
        };
        send_codec(
            &mut client,
            protocol,
            p::envelope::Body::Response(p::Response {
                id: sent.id,
                error: String::new(),
                result: Some(p::response::Result::Created(p::CreatedResult {
                    id: "$1".into(),
                    created_at: 42,
                    reused: false,
                })),
            }),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(2), home.wait())
            .await
            .unwrap();
        assert!(matches!(
            pending.await.unwrap(),
            Err(Error::Offline) | Err(Error::Stale)
        ));
        assert!(!hub.snapshot().connected);
    }
}

#[tokio::test]
async fn cancelled_and_unmatched_legacy_replies_are_discarded_without_reconnect() {
    let (hub, _) = Hub::new();
    let (home, mut client) = connect_protocol(&hub, Negotiated::JsonV1).await;
    let generation = home.generation();
    let pending = tokio::spawn({
        let hub = hub.clone();
        async move { hub.request(generation, request()).await }
    });
    let p::envelope::Body::Request(first) = receive_codec(&mut client, Negotiated::JsonV1).await
    else {
        panic!("request expected")
    };
    pending.abort();
    assert!(matches!(pending.await, Err(error) if error.is_cancelled()));
    let p::envelope::Body::Cancel(cancel) = receive_codec(&mut client, Negotiated::JsonV1).await
    else {
        panic!("cancel expected")
    };
    assert_eq!(cancel.id, first.id);
    for id in [first.id, "never-issued".into()] {
        let late = hmux_protocol::wire::Message {
            kind: "response".into(),
            id,
            payload: Some(
                serde_json::value::RawValue::from_string("{\"unexpected\":true}".into()).unwrap(),
            ),
            ..Default::default()
        };
        client
            .send(Message::Text(
                String::from_utf8(late.encode().unwrap()).unwrap().into(),
            ))
            .await
            .unwrap();
    }
    let next = tokio::spawn({
        let hub = hub.clone();
        async move { hub.request(generation, request()).await }
    });
    let p::envelope::Body::Request(sent) = receive_codec(&mut client, Negotiated::JsonV1).await
    else {
        panic!("next request expected")
    };
    send_codec(
        &mut client,
        Negotiated::JsonV1,
        p::envelope::Body::Response(response(sent.id)),
    )
    .await;
    assert!(next.await.unwrap().is_ok());
    assert_eq!(hub.snapshot().generation, Some(generation));
    drop(home);
}

#[tokio::test]
async fn typed_reply_budget_follows_external_clones_across_reconnect() {
    let (hub, _) = Hub::new();
    let (home, mut client) = connect(&hub).await;
    let generation = home.generation();
    let pending = tokio::spawn({
        let hub = hub.clone();
        async move { hub.request(generation, request()).await }
    });
    let p::envelope::Body::Request(sent) = receive(&mut client).await else {
        panic!("request expected")
    };
    send(&mut client, p::envelope::Body::Response(response(sent.id))).await;
    let reply = pending.await.unwrap().unwrap();
    let clone = reply.clone();
    let charged = hub.retained_payload_bytes();
    assert!(charged > 0);
    drop(reply);
    drop(home);
    assert_eq!(hub.retained_payload_bytes(), charged);
    let (replacement, mut client) = connect(&hub).await;
    let generation = replacement.generation();
    let pending = tokio::spawn({
        let hub = hub.clone();
        async move { hub.request(generation, request()).await }
    });
    let p::envelope::Body::Request(sent) = receive(&mut client).await else {
        panic!("reconnect request expected")
    };
    send(&mut client, p::envelope::Body::Response(response(sent.id))).await;
    drop(pending.await.unwrap().unwrap());
    assert_eq!(hub.retained_payload_bytes(), charged);
    drop(clone);
    assert_eq!(hub.retained_payload_bytes(), 0);
    drop(replacement);
}

#[tokio::test]
async fn paused_response_consumers_share_a_byte_budget_across_generations() {
    use hmux_gateway::hub::RETAINED_PAYLOAD_BYTES;
    let (hub, _events) = Hub::new();
    let (home, mut client) = connect(&hub).await;
    let generation = home.generation();
    let mut pending = Vec::new();
    let mut ids = Vec::new();
    // Poll only until each request is sent; never poll the response consumers.
    for _ in 0..MAX_PENDING {
        let mut request = request();
        request.operation = p::Operation::Conversation as i32;
        request.session = Some(p::Session {
            id: "$1".into(),
            created_at: 42,
        });
        let mut future = Box::pin(hub.request(generation, request));
        assert!(futures_util::poll!(future.as_mut()).is_pending());
        let p::envelope::Body::Request(sent) = receive(&mut client).await else {
            panic!("missing request")
        };
        ids.push(sent.id);
        pending.push(future);
    }
    let large_result = p::response::Result::Conversation(Box::new(p::ConversationResult {
        provider: "codex".into(),
        session_id: "$1".into(),
        created_at: 42,
        status: "ready".into(),
        messages: Some(p::ConversationMessages {
            items: vec![p::ConversationMessage {
                id: "message-0".into(),
                role: "assistant".into(),
                text: "x".repeat(512 << 10),
            }],
        }),
        truncated: false,
    }));
    let mut accepted = 0;
    let mut charged = 0;
    for id in ids {
        let reply = p::Response {
            id,
            error: String::new(),
            result: Some(large_result.clone()),
        };
        assert!(
            actions::validate_response(&reply).is_ok(),
            "large typed response invalid: {} bytes",
            actions::response_retained_bytes(&reply)
        );
        send(&mut client, p::envelope::Body::Response(reply)).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if !hub.snapshot().connected || hub.retained_payload_bytes() > charged {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if !hub.snapshot().connected {
            break;
        }
        accepted += 1;
        charged = hub.retained_payload_bytes();
        assert!(charged <= RETAINED_PAYLOAD_BYTES);
    }
    assert!(accepted > 0);
    assert!(
        !hub.snapshot().connected,
        "typed replies must exhaust the shared budget"
    );
    assert_eq!(hub.retained_payload_bytes(), charged);
    home.wait().await;

    // A reconnect must not reset accounting while old consumers pin responses.
    let (replacement, mut client) = connect(&hub).await;
    let replacement_generation = replacement.generation();
    let next = tokio::spawn({
        let hub = hub.clone();
        async move { hub.request(replacement_generation, request()).await }
    });
    let p::envelope::Body::Request(next_request) = receive(&mut client).await else {
        panic!("missing reconnect request")
    };
    send(
        &mut client,
        p::envelope::Body::Response(response(next_request.id)),
    )
    .await;
    assert!(next.await.unwrap().is_ok());
    assert_eq!(hub.retained_payload_bytes(), charged);
    drop(pending);
    assert_eq!(hub.retained_payload_bytes(), 0);
    drop(replacement);
}

#[tokio::test]
async fn usage_public_allowlist_is_enforced_for_both_codecs() {
    fn varint(mut value: usize, out: &mut Vec<u8>) {
        while value >= 128 {
            out.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }
    for protocol in [Negotiated::ProtobufV2, Negotiated::JsonV1] {
        for field in ["extras", "accounts"] {
            let (hub, _) = Hub::new();
            let (home, mut client) = connect_protocol(&hub, protocol).await;
            let now = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now());
            let valid = hmux_usage::Snapshot::degraded(hmux_usage::Provider::Codex, 1, now, "ok");
            let typed = snapshots::usage_to_proto(valid.clone()).unwrap();
            let envelope = p::Envelope {
                version: pb::VERSION,
                body: Some(p::envelope::Body::Usage(Box::new(typed.clone()))),
            };
            let accepted = match protocol {
                Negotiated::ProtobufV2 => {
                    Message::Binary(pb::encode(&envelope, Direction::ToGateway).unwrap())
                }
                Negotiated::JsonV1 => Message::Text(
                    String::from_utf8(
                        hmux_protocol::legacy::to_json(envelope, Direction::ToGateway)
                            .unwrap()
                            .encode()
                            .unwrap(),
                    )
                    .unwrap()
                    .into(),
                ),
            };
            client.send(accepted).await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while hub.snapshot().codex_usage.is_none() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let rejected = match protocol {
                Negotiated::ProtobufV2 => {
                    let mut usage = typed;
                    if field == "accounts" {
                        usage.accounts.push(p::UsageAccount {
                            number: 1,
                            email: "private@example.invalid".into(),
                            ..Default::default()
                        });
                    }
                    let mut raw_usage = usage.encode_to_vec();
                    if field == "extras" {
                        // Unknown nested field 127 carries synthetic private data.
                        raw_usage.extend_from_slice(&[0xfa, 0x07]);
                        varint(17, &mut raw_usage);
                        raw_usage.extend_from_slice(b"synthetic-private");
                    }
                    // Raw prost bypasses the outbound validator; receiver must reject.
                    let mut raw = p::Envelope {
                        version: pb::VERSION,
                        body: None,
                    }
                    .encode_to_vec();
                    raw.extend_from_slice(&[0xa2, 0x02]); // Envelope.usage, tag 36
                    varint(raw_usage.len(), &mut raw);
                    raw.extend_from_slice(&raw_usage);
                    Message::Binary(raw.into())
                }
                Negotiated::JsonV1 => {
                    let mut raw = serde_json::to_value(&valid).unwrap();
                    raw[field] = if field == "extras" {
                        serde_json::json!({"access_token":"synthetic-private"})
                    } else {
                        serde_json::json!([{"number":1,"email":"private@example.invalid"}])
                    };
                    Message::Text(
                        serde_json::json!({"type":"usage","payload":raw})
                            .to_string()
                            .into(),
                    )
                }
            };
            client.send(rejected).await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), home.wait())
                .await
                .unwrap();
            assert!(!hub.snapshot().connected);
            assert!(hub.snapshot().codex_usage.is_none());
        }
    }
}

#[tokio::test]
async fn catalog_renewal_releases_its_old_slot_but_charges_external_clones() {
    use hmux_gateway::hub::RETAINED_PAYLOAD_BYTES;
    let (hub, _) = Hub::new();
    let (home, mut client) = connect(&hub).await;
    for provider in [hmux_usage::Provider::Claude, hmux_usage::Provider::Codex] {
        let mut usage = hmux_usage::Snapshot::degraded(
            provider,
            1,
            "2026-09-24T00:00:00Z".parse().unwrap(),
            "ok",
        );
        usage.accounts = (1..=128)
            .map(|number| hmux_usage::Account {
                number,
                display_name: "x".repeat(256),
                ..Default::default()
            })
            .collect();
        send(
            &mut client,
            p::envelope::Body::Usage(Box::new(snapshots::usage_to_proto(usage).unwrap())),
        )
        .await;
    }
    let mut catalog = hmux_model::Catalog {
        protocol_version: 1,
        generated_at: "2026-09-24T00:00:00Z".into(),
        sessions: Some(
            (0..1024)
                .map(|i| hmux_model::Session {
                    id: format!("${i}"),
                    created_at: 42,
                    ..Default::default()
                })
                .collect(),
        ),
        ..Default::default()
    };
    let target = hmux_protocol::wire::MAX_MESSAGE - 1024;
    let mut remaining = target - serde_json::to_vec(&catalog).unwrap().len();
    for session in catalog.sessions.as_mut().unwrap() {
        let count = remaining.min(snapshots::MAX_CATALOG_TEXT);
        session.name = "x".repeat(count);
        remaining -= count;
    }
    assert_eq!(remaining, 0);
    for second in 0..3 {
        catalog.generated_at = format!("2026-09-24T00:00:0{second}Z");
        let typed = snapshots::catalog_to_proto(catalog.clone()).unwrap();
        let expected = hmux_protocol::legacy::catalog_payload(typed.clone()).unwrap();
        assert_eq!(expected.len(), target);
        send(&mut client, p::envelope::Body::Catalog(Box::new(typed))).await;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let state = hub.snapshot();
                assert!(state.connected, "normal catalog renewal disconnected Home");
                if state.catalog.as_ref() == Some(&expected) {
                    break;
                }
                drop(state);
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(hub.retained_payload_bytes() > target + 2048);
        assert!(hub.retained_payload_bytes() < RETAINED_PAYLOAD_BYTES);
    }
    // An external reader still retaining the previous catalog cannot bypass
    // the same budget. Disconnect frees current slots but not this allocation.
    let retained = hub.snapshot().catalog.unwrap();
    catalog.generated_at = "2026-09-24T00:00:03Z".into();
    send(
        &mut client,
        p::envelope::Body::Catalog(Box::new(snapshots::catalog_to_proto(catalog).unwrap())),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), home.wait())
        .await
        .unwrap();
    assert!(!hub.snapshot().connected);
    assert_eq!(hub.retained_payload_bytes(), retained.len());
    drop(retained);
    assert_eq!(hub.retained_payload_bytes(), 0);
}
