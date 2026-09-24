//! One WebSocket client upgrade on an already connected and verified stream.
//! The caller must bind `authority` to the stream's certificate hostname and
//! perform DNS/TLS validation. This module neither dials nor retries a failed
//! upgrade; a successful 101 without a selected protocol is legacy JSON v1.
use bytes::Bytes;
use hmux_protocol::{
    protobuf::{self, Direction, Negotiated},
    transport,
};
use http::{header, HeaderMap, HeaderValue, Request, Response, StatusCode, Version};
use http_body_util::Empty;
use hyper::{body::Incoming, client::conn::http1, upgrade::Upgraded};
use hyper_util::rt::TokioIo;
use std::{future::Future, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{timeout_at, Instant},
};
use tokio_tungstenite::{
    tungstenite::{
        handshake::{client::generate_key, derive_accept_key},
        protocol::Role,
    },
    WebSocketStream,
};
use tokio_util::sync::CancellationToken;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_AUTHORITY: usize = 255;
const MAX_TOKEN: usize = 1024;
const MAX_HEADERS: usize = 32;
const MAX_HEADER_BYTES: usize = 8192;

/// Fixed categories only. Neither bearer token nor remote response is retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidInput,
    Rejected { http_status: Option<u16> },
    InvalidResponse,
    Timeout,
    Cancelled,
    Transport,
}

pub(crate) fn valid_authority(raw: &str) -> bool {
    if raw.is_empty()
        || raw.len() > MAX_AUTHORITY
        || !raw.is_ascii()
        || raw.bytes().any(|b| {
            b <= 0x20 || b >= 0x7f || matches!(b, b'@' | b'/' | b'?' | b'#' | b'%' | b'\\')
        })
    {
        return false;
    }
    let Ok(authority) = raw.parse::<http::uri::Authority>() else {
        return false;
    };
    if authority.as_str() != raw || authority.host().is_empty() {
        return false;
    }
    // `Authority` accepts raw suffixes whose port() is None, including :abc,
    // :99999, and [::1]garbage. Require an optional, valid numeric port.
    let Some(suffix) = raw.strip_prefix(authority.host()) else {
        return false;
    };
    if suffix.is_empty() {
        return true;
    }
    suffix
        .strip_prefix(':')
        .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        && authority.port().is_some_and(|port| port.as_u16() != 0)
}
pub(crate) fn valid_bearer(raw: &str) -> bool {
    if raw.is_empty() || raw.len() > MAX_TOKEN || !raw.is_ascii() {
        return false;
    }
    let mut padding = false;
    for byte in raw.bytes() {
        if byte == b'=' {
            padding = true;
        } else if padding
            || !(byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/'))
        {
            return false;
        }
    }
    true
}
fn one<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a HeaderValue, Error> {
    let mut values = headers.get_all(name).iter();
    let value = values.next().ok_or(Error::InvalidResponse)?;
    if values.next().is_some() {
        return Err(Error::InvalidResponse);
    }
    Ok(value)
}
fn optional_one<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a HeaderValue>, Error> {
    let mut values = headers.get_all(name).iter();
    let value = values.next();
    if values.next().is_some() {
        return Err(Error::InvalidResponse);
    }
    Ok(value)
}
fn token_header(value: &HeaderValue, token: &str) -> bool {
    let Ok(text) = value.to_str() else {
        return false;
    };
    let mut found = false;
    for field in text.split(',') {
        let field = field.trim_matches([' ', '\t']);
        if field.is_empty()
            || !field.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#'
                            | b'$'
                            | b'%'
                            | b'&'
                            | b'\''
                            | b'*'
                            | b'+'
                            | b'-'
                            | b'.'
                            | b'^'
                            | b'_'
                            | b'`'
                            | b'|'
                            | b'~'
                    )
            })
        {
            return false;
        }
        if field.eq_ignore_ascii_case(token) {
            if found {
                return false;
            }
            found = true;
        }
    }
    found
}
fn validate(response: &Response<Incoming>, key: &str) -> Result<Negotiated, Error> {
    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
        let status = response.status().as_u16();
        return Err(Error::Rejected {
            http_status: (100..=599).contains(&status).then_some(status),
        });
    }
    if response.version() != Version::HTTP_11 {
        return Err(Error::InvalidResponse);
    }
    let h = response.headers();
    if !token_header(one(h, header::CONNECTION.as_str())?, "upgrade")
        || !one(h, header::UPGRADE.as_str())?
            .as_bytes()
            .eq_ignore_ascii_case(b"websocket")
        || one(h, "sec-websocket-accept")?.as_bytes()
            != derive_accept_key(key.as_bytes()).as_bytes()
        || h.get_all("sec-websocket-extensions")
            .iter()
            .next()
            .is_some()
        || h.contains_key(header::CONTENT_LENGTH)
        || h.contains_key(header::TRANSFER_ENCODING)
    {
        return Err(Error::InvalidResponse);
    }
    let selected = optional_one(h, "sec-websocket-protocol")?;
    let selected = selected
        .map(|value| value.to_str().map_err(|_| Error::InvalidResponse))
        .transpose()?;
    if selected == Some("") {
        return Err(Error::InvalidResponse);
    }
    protobuf::negotiate(selected).map_err(|_| Error::InvalidResponse)
}

async fn drive<F, T>(
    work: F,
    connection: impl Future<Output = Result<(), hyper::Error>>,
) -> Result<T, Error>
where
    F: Future<Output = Result<T, Error>>,
{
    tokio::pin!(work);
    tokio::pin!(connection);
    tokio::select! {
        result = &mut work => {
            let value = result?;
            connection.await.map_err(|_| Error::Transport)?;
            Ok(value)
        }
        result = &mut connection => {
            result.map_err(|_| Error::Transport)?;
            work.await
        }
    }
}

async fn handshake<S>(
    stream: S,
    authority: &str,
    bearer: &str,
) -> Result<(TokioIo<Upgraded>, Negotiated), Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let key = generate_key();
    let mut authorization =
        HeaderValue::from_str(&format!("Bearer {bearer}")).map_err(|_| Error::InvalidInput)?;
    authorization.set_sensitive(true);
    let request = Request::builder()
        .method("GET")
        .uri("/connect")
        .version(Version::HTTP_11)
        .header(header::HOST, authority)
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", key.as_str())
        .header("sec-websocket-protocol", protobuf::SUBPROTOCOL)
        .header(header::AUTHORIZATION, authorization)
        .body(Empty::<Bytes>::new())
        .map_err(|_| Error::InvalidInput)?;
    let mut builder = http1::Builder::new();
    builder
        .max_headers(MAX_HEADERS)
        .max_buf_size(MAX_HEADER_BYTES);
    let (mut sender, connection) = builder
        .handshake(TokioIo::new(stream))
        .await
        .map_err(|_| Error::Transport)?;
    let work = async {
        let response = sender
            .send_request(request)
            .await
            .map_err(|_| Error::Transport)?;
        let protocol = validate(&response, &key)?;
        let upgraded = hyper::upgrade::on(response)
            .await
            .map_err(|_| Error::Transport)?;
        Ok((TokioIo::new(upgraded), protocol))
    };
    drive(work, connection.with_upgrades()).await
}

/// Offer v2 once over the supplied verified stream. A valid HTTP/1.1 101 with
/// no subprotocol header selects JSON v1. Errors never trigger a second request.
/// The returned `Connection` owns the sole resident WebSocket I/O task. Dropping
/// this upgrade future before success drops the socket; no HTTP task is spawned.
pub async fn upgrade<S>(
    stream: S,
    authority: &str,
    bearer: &str,
    cancelled: &CancellationToken,
    parent_deadline: Option<Instant>,
) -> Result<transport::Connection, Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    if !valid_authority(authority) || !valid_bearer(bearer) {
        return Err(Error::InvalidInput);
    }
    if cancelled.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let mut deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    if let Some(parent) = parent_deadline {
        deadline = deadline.min(parent);
    }
    if deadline <= Instant::now() {
        return Err(Error::Timeout);
    }
    let (upgraded, protocol) = tokio::select! {
        biased;
        _ = cancelled.cancelled() => return Err(Error::Cancelled),
        result = timeout_at(deadline, handshake(stream, authority, bearer)) => result.map_err(|_| Error::Timeout)??,
    };
    let socket = tokio::select! {
        biased;
        _ = cancelled.cancelled() => return Err(Error::Cancelled),
        result = timeout_at(deadline, WebSocketStream::from_raw_socket(upgraded, Role::Client, Some(transport::socket_config()))) => result.map_err(|_| Error::Timeout)?,
    };
    if cancelled.is_cancelled() {
        return Err(Error::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(Error::Timeout);
    }
    transport::start(socket, protocol, Direction::ToHome).map_err(|_| Error::Transport)
}
