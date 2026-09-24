//! Authenticated Home handshake policy. Socket ownership remains with the HTTP
//! upgrade tracker and hub; the auth-only example does not enable this route.

use crate::http_boundary::{secure_headers, Policy, Reply};
use base64::{engine::general_purpose::STANDARD, Engine};
use bytes::Bytes;
use hmux_protocol::protobuf::{Negotiated, SUBPROTOCOL};
use http::{header, HeaderValue, Request, StatusCode, Version};
use tokio_tungstenite::tungstenite::handshake::server::create_response_with_body;

pub struct HomeHandshake {
    pub response: Reply,
    pub protocol: Negotiated,
}

/// Authenticate before parsing WebSocket fields. This function can never turn a
/// bad token or browser Origin into a successful legacy fallback.
pub fn home_handshake<B>(
    policy: &Policy,
    request: &Request<B>,
) -> Result<HomeHandshake, StatusCode> {
    if request.uri().path() != "/connect" {
        return Err(StatusCode::NOT_FOUND);
    }
    policy.preflight(request)?;
    let mut response = websocket_response(request)?;
    let headers = request.headers();
    let mut selected = false;
    let mut offers = 0;
    for field in headers.get_all("sec-websocket-protocol") {
        let field = field.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
        for offer in field.split(',').map(str::trim) {
            offers += 1;
            if offers > 16 || offer.is_empty() || offer.len() > 64 || !offer.bytes().all(is_token) {
                return Err(StatusCode::BAD_REQUEST);
            }
            selected |= offer == SUBPROTOCOL;
        }
    }
    let protocol = if selected {
        response.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_static(SUBPROTOCOL),
        );
        Negotiated::ProtobufV2
    } else {
        Negotiated::JsonV1
    };
    secure_headers(&mut response);
    Ok(HomeHandshake { response, protocol })
}

/// Cookie authentication belongs to the route; this helper verifies exact
/// browser Origin/Host and the same bounded RFC6455 header shape as Home.
pub fn terminal_handshake<B>(policy: &Policy, request: &Request<B>) -> Result<Reply, StatusCode> {
    if request.uri().path() != "/api/terminal" {
        return Err(StatusCode::NOT_FOUND);
    }
    policy.preflight(request)?;
    websocket_response(request)
}

pub fn upload_handshake<B>(policy: &Policy, request: &Request<B>) -> Result<Reply, StatusCode> {
    if request.uri().path() != "/api/upload" {
        return Err(StatusCode::NOT_FOUND);
    }
    policy.preflight(request)?;
    websocket_response(request)
}

fn websocket_response<B>(request: &Request<B>) -> Result<Reply, StatusCode> {
    let headers = request.headers();
    for name in [
        "connection",
        "upgrade",
        "sec-websocket-key",
        "sec-websocket-version",
        "content-length",
    ] {
        if headers.get_all(name).iter().count() > 1 {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    if request.version() != Version::HTTP_11
        || headers.contains_key(header::TRANSFER_ENCODING)
        || headers
            .get(header::CONTENT_LENGTH)
            .is_some_and(|h| h != "0")
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let key = headers
        .get("sec-websocket-key")
        .ok_or(StatusCode::BAD_REQUEST)?;
    // Validate nonce length before the library computes its RFC6455 digest.
    if key.as_bytes().len() != 24
        || STANDARD
            .decode(key.as_bytes())
            .map_or(true, |b| b.len() != 16)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut response =
        create_response_with_body(request, || crate::http_boundary::Body::new(Bytes::new()))
            .map_err(|_| StatusCode::BAD_REQUEST)?;
    secure_headers(&mut response);
    Ok(response)
}

fn is_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

#[cfg(test)]
mod tests {
    use super::*;
    const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    fn policy() -> Policy {
        Policy::new("https://hmux.example", TOKEN).unwrap()
    }
    fn request() -> Request<()> {
        Request::builder()
            .uri("/connect")
            .header("host", "hmux.example")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("connection", "Upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .header("sec-websocket-version", "13")
            .body(())
            .unwrap()
    }
    fn status(request: &Request<()>) -> StatusCode {
        match home_handshake(&policy(), request) {
            Ok(reply) => reply.response.status(),
            Err(status) => status,
        }
    }
    #[test]
    fn retired_snapshot_offer_uses_json_without_selecting_incompatible_proto() {
        let mut request = request();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_static("hmux-home.pb.v2"),
        );
        let reply = home_handshake(&policy(), &request).unwrap();
        assert_eq!(reply.protocol, Negotiated::JsonV1);
        assert!(!reply
            .response
            .headers()
            .contains_key("sec-websocket-protocol"));
    }
    #[test]
    fn authenticated_legacy_and_protobuf_negotiate_on_the_same_socket() {
        let mut request = request();
        let legacy = home_handshake(&policy(), &request).unwrap();
        assert_eq!(legacy.protocol, Negotiated::JsonV1);
        assert_eq!(legacy.response.status(), StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(
            legacy.response.headers()["sec-websocket-accept"],
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        assert!(!legacy
            .response
            .headers()
            .contains_key("sec-websocket-protocol"));
        request.headers_mut().insert(
            "sec-websocket-protocol",
            format!("unknown-future, {SUBPROTOCOL}").parse().unwrap(),
        );
        let protobuf = home_handshake(&policy(), &request).unwrap();
        assert_eq!(protobuf.protocol, Negotiated::ProtobufV2);
        assert_eq!(
            protobuf.response.headers()["sec-websocket-protocol"],
            SUBPROTOCOL
        );
        assert_eq!(protobuf.response.headers()["cache-control"], "no-store");
        assert!(!protobuf
            .response
            .headers()
            .contains_key("sec-websocket-extensions"));
    }
    #[test]
    fn failed_auth_or_browser_origin_never_downgrades() {
        let mut request = request();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_static(SUBPROTOCOL),
        );
        request
            .headers_mut()
            .insert("authorization", HeaderValue::from_static("Bearer wrong"));
        request
            .headers_mut()
            .insert("sec-websocket-key", HeaderValue::from_static("invalid"));
        assert_eq!(status(&request), StatusCode::FORBIDDEN);
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
        request
            .headers_mut()
            .insert("origin", HeaderValue::from_static("https://hmux.example"));
        assert_eq!(status(&request), StatusCode::FORBIDDEN);
        request.headers_mut().remove("origin");
        assert_eq!(status(&request), StatusCode::BAD_REQUEST);
    }
    #[test]
    fn ambiguous_headers_invalid_offers_and_upgrade_bodies_are_rejected() {
        let mut req = request();
        req.headers_mut().append(
            "sec-websocket-key",
            HeaderValue::from_static("dGhlIHNhbXBsZSBub25jZQ=="),
        );
        assert_eq!(status(&req), StatusCode::BAD_REQUEST);
        for offer in [
            "",
            "hmux-home.pb.v2.controls1,",
            "hmux-home.pb.v2.controls1;bad",
            "quoted\"token",
        ] {
            let mut req = request();
            req.headers_mut()
                .insert("sec-websocket-protocol", offer.parse().unwrap());
            assert_eq!(status(&req), StatusCode::BAD_REQUEST);
        }
        let mut req = request();
        req.headers_mut().insert(
            "sec-websocket-protocol",
            "v1,".repeat(17).trim_end_matches(',').parse().unwrap(),
        );
        assert_eq!(status(&req), StatusCode::BAD_REQUEST);
        let mut req = request();
        req.headers_mut()
            .insert("content-length", HeaderValue::from_static("1"));
        assert_eq!(status(&req), StatusCode::BAD_REQUEST);
        let mut req = request();
        req.headers_mut()
            .insert("transfer-encoding", HeaderValue::from_static("chunked"));
        assert_eq!(status(&req), StatusCode::BAD_REQUEST);
    }
}
