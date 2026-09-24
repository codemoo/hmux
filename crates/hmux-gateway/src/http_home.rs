//! Candidate Home route, enabled by the separate full gateway executable.

use crate::{
    http_boundary::{error, Policy, Reply, RequestContext},
    http_upgrade::home_handshake,
    hub::Hub,
};
use hmux_protocol::{protobuf::Direction, transport};
use http::{Request, StatusCode};
use hyper::body::Incoming;
use tokio_tungstenite::{tungstenite::protocol::Role, WebSocketStream};

pub fn connect(
    policy: &Policy,
    hub: &Hub,
    mut request: Request<Incoming>,
    context: RequestContext,
) -> Reply {
    let handshake = match home_handshake(policy, &request) {
        Ok(handshake) => handshake,
        Err(status) => return error(status),
    };
    let protocol = handshake.protocol;
    let hub = hub.clone();
    if context
        .spawn_upgrade_graceful(&mut request, move |socket, shutdown| async move {
            let socket = WebSocketStream::from_raw_socket(
                socket,
                Role::Server,
                Some(transport::socket_config()),
            )
            .await;
            let Ok(connection) = transport::start(socket, protocol, Direction::ToGateway) else {
                return;
            };
            let sender = connection.sender.clone();
            let home = match hub.attach(connection) {
                Ok(home) => home,
                Err(rejected) => {
                    rejected.close().await;
                    return;
                }
            };
            let finished = home.wait();
            tokio::pin!(finished);
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    sender.close();
                    finished.await;
                },
                _ = &mut finished => {},
            }
        })
        .is_err()
    {
        return error(StatusCode::SERVICE_UNAVAILABLE);
    }
    handshake.response
}
