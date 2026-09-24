use futures_util::{SinkExt, StreamExt};
use hmux_gateway::{http_boundary as boundary, http_upgrade::home_handshake};
use hmux_protocol::{
    legacy,
    protobuf::{self as pb, types as p, Direction, Negotiated, SUBPROTOCOL},
    transport, wire,
};
use http::{header::HeaderValue, StatusCode};
use std::{sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
    time::timeout,
};
use tokio_tungstenite::{
    client_async_with_config,
    tungstenite::{client::IntoClientRequest, protocol::Role, Error, Message},
    WebSocketStream,
};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn hello() -> p::Envelope {
    p::Envelope {
        version: pb::VERSION,
        body: Some(p::envelope::Body::Hello(p::Hello {
            capabilities: vec!["output-ack-v1".into()],
        })),
    }
}

fn encode(message: p::Envelope, protocol: Negotiated, direction: Direction) -> Message {
    match protocol {
        Negotiated::JsonV1 => Message::Text(
            String::from_utf8(
                legacy::to_json(message, direction)
                    .unwrap()
                    .encode()
                    .unwrap(),
            )
            .unwrap()
            .into(),
        ),
        Negotiated::ProtobufV2 => Message::Binary(pb::encode(&message, direction).unwrap()),
    }
}

#[tokio::test]
async fn real_http_upgrade_authenticates_and_exchanges_both_wire_versions() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let policy = Arc::new(boundary::Policy::new("https://hmux.example", TOKEN).unwrap());
    let handler_policy = policy.clone();
    let shutdown = CancellationToken::new();
    let (done, mut completions) = mpsc::channel(2);
    let server = tokio::spawn(boundary::serve(
        listener,
        policy,
        move |mut request, context| {
            let handshake = home_handshake(&handler_policy, &request);
            let done = done.clone();
            async move {
                let handshake = match handshake {
                    Ok(handshake) => handshake,
                    Err(status) => return boundary::error(status),
                };
                let protocol = handshake.protocol;
                context
                    .spawn_upgrade(&mut request, move |socket| async move {
                        let socket = WebSocketStream::from_raw_socket(
                            socket,
                            Role::Server,
                            Some(transport::socket_config()),
                        )
                        .await;
                        let transport::Connection {
                            sender,
                            mut reader,
                            task,
                        } = transport::start(socket, protocol, Direction::ToGateway).unwrap();
                        let incoming = match reader.receive().await.unwrap() {
                            transport::Incoming::Json(message) => {
                                legacy::from_json(message, Direction::ToGateway).unwrap()
                            }
                            transport::Incoming::Protobuf(message) => message,
                        };
                        assert!(incoming == hello());
                        let profiles = p::Envelope {
                            version: pb::VERSION,
                            body: Some(p::envelope::Body::Request(Box::new(p::Request {
                                id: "test-profiles".into(),
                                operation: p::Operation::Profiles as i32,
                                session: None,
                                payload: Some(p::request::Payload::Empty(p::Empty {})),
                            }))),
                        };
                        let raw = encode(profiles, protocol, Direction::ToHome).into_data();
                        sender
                            .try_reserve(raw.len())
                            .unwrap()
                            .submit(&raw, CancellationToken::new())
                            .unwrap()
                            .wait()
                            .await
                            .unwrap();
                        assert!(reader.receive().await.is_err());
                        sender.close();
                        task.await.unwrap();
                        done.send(protocol).await.unwrap();
                    })
                    .unwrap();
                handshake.response
            }
        },
        shutdown.clone(),
    ));

    // Rejected authentication cannot select either protocol or call the handler.
    let mut request = "ws://hmux.example/connect".into_client_request().unwrap();
    request
        .headers_mut()
        .insert("authorization", HeaderValue::from_static("Bearer wrong"));
    request.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(SUBPROTOCOL),
    );
    let socket = TcpStream::connect(address).await.unwrap();
    let failure = client_async_with_config(request, socket, Some(transport::socket_config()))
        .await
        .unwrap_err();
    match failure {
        Error::Http(response) => {
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert!(!response.headers().contains_key("sec-websocket-protocol"));
        }
        _ => panic!("expected HTTP rejection"),
    }
    assert!(completions.try_recv().is_err());

    for protocol in [Negotiated::JsonV1, Negotiated::ProtobufV2] {
        let mut request = "ws://hmux.example/connect".into_client_request().unwrap();
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
        if protocol == Negotiated::ProtobufV2 {
            request.headers_mut().insert(
                "sec-websocket-protocol",
                HeaderValue::from_static(SUBPROTOCOL),
            );
        }
        let socket = TcpStream::connect(address).await.unwrap();
        let (mut client, response) =
            client_async_with_config(request, socket, Some(transport::socket_config()))
                .await
                .unwrap();
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(
            pb::negotiate(
                response
                    .headers()
                    .get("sec-websocket-protocol")
                    .map(|h| h.to_str().unwrap())
            )
            .unwrap(),
            protocol
        );
        client
            .send(encode(hello(), protocol, Direction::ToGateway))
            .await
            .unwrap();
        let reply = timeout(Duration::from_secs(2), client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(reply.is_binary(), protocol == Negotiated::ProtobufV2);
        let raw = reply.into_data();
        let reply = match protocol {
            Negotiated::JsonV1 => {
                legacy::from_json(wire::Message::decode(&raw).unwrap(), Direction::ToHome).unwrap()
            }
            Negotiated::ProtobufV2 => pb::decode(raw, Direction::ToHome).unwrap(),
        };
        let Some(p::envelope::Body::Request(request)) = reply.body else {
            panic!("expected profiles request")
        };
        assert_eq!(request.id, "test-profiles");
        assert_eq!(request.operation, p::Operation::Profiles as i32);
        client.close(None).await.unwrap();
        assert_eq!(
            timeout(Duration::from_secs(2), completions.recv())
                .await
                .unwrap()
                .unwrap(),
            protocol
        );
    }
    shutdown.cancel();
    timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
