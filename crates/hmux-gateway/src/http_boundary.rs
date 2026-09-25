//! HTTP/1 security and allocation boundary shared by the candidate routes.
//! Authentication, CSRF and route authorization still belong to the route owner.
//! No HTTP/2, gRPC, TLS terminator or per-client resident worker is added here.

use crate::auth;
use bytes::{Bytes, BytesMut};
use http::{header, HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::de::DeserializeOwned;
use std::{
    convert::Infallible,
    future::Future,
    io,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, OnceLock,
    },
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
    time::timeout,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub const MAX_CONNECTIONS: usize = 64;
pub const MAX_HEADER_BYTES: usize = 8192;
pub const MAX_JSON_BYTES: usize = 16 << 10;
// A state reply includes a 4 MiB catalog, two 1 MiB usage snapshots and
// bounded preferences/wrapper fields. Keep the aggregate retention cap separate.
pub const MAX_REPLY_BYTES: usize = (6 << 20) + (8 << 10);
pub const MAX_RETAINED_JSON_BYTES: usize = 8 << 20;
pub const HEADER_TIMEOUT: Duration = Duration::from_secs(5);
pub const BODY_TIMEOUT: Duration = Duration::from_secs(15);
// Includes request body and route execution. Before enabling streaming upload
// routes, give them an explicit separately bounded streaming policy.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub use crate::http_body::Body;
pub type Reply = Response<Body>;

pub const CSP: &str = "default-src 'none'; manifest-src 'self'; worker-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; font-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// Does not implement Debug: the connector bearer token is secret.
pub struct Policy {
    origin: String,
    host: String,
    authorization: Vec<u8>,
}

impl Policy {
    pub fn new(origin: &str, connector_token: &str) -> Result<Self, &'static str> {
        let host = origin
            .strip_prefix("https://")
            .ok_or("origin must be an exact HTTPS origin")?;
        if host.is_empty()
            || host.contains(['/', '?', '#', '@'])
            || !host.is_ascii()
            || host.bytes().any(|b| b.is_ascii_whitespace())
        {
            return Err("origin must be an exact HTTPS origin");
        }
        let authority: http::uri::Authority =
            host.parse().map_err(|_| "invalid origin authority")?;
        let suffix = &host[authority.host().len()..];
        if authority.host().is_empty()
            || (!suffix.is_empty()
                && suffix
                    .strip_prefix(':')
                    .and_then(|p| p.parse::<u16>().ok())
                    .is_none())
        {
            return Err("invalid origin authority");
        }
        if !auth::valid_token(connector_token) {
            return Err("invalid connector token");
        }
        Ok(Self {
            origin: origin.to_owned(),
            host: host.to_owned(),
            authorization: format!("Bearer {connector_token}").into_bytes(),
        })
    }

    /// Host/Origin checks precede routing, authentication, upgrade and body IO.
    /// Reject ambiguous duplicate security headers instead of choosing one.
    pub fn preflight<B>(&self, request: &Request<B>) -> Result<(), StatusCode> {
        let headers = request.headers();
        if single(headers, header::HOST.as_str()) != Some(self.host.as_bytes())
            || request
                .uri()
                .authority()
                .is_some_and(|a| a.as_str() != self.host)
        {
            return Err(StatusCode::MISDIRECTED_REQUEST);
        }
        let path = request.uri().path();
        if path == "/connect" {
            if request.method() != Method::GET
                || !absent_or_empty(headers, header::ORIGIN.as_str())
                || single(headers, header::AUTHORIZATION.as_str())
                    .is_none_or(|v| !bool::from(v.ct_eq(&self.authorization)))
            {
                return Err(StatusCode::FORBIDDEN);
            }
        } else if path.starts_with("/api/") {
            // Terminal GET upgrades are also Origin protected.
            if (request.method() != Method::GET || matches!(path, "/api/terminal" | "/api/upload"))
                && single(headers, header::ORIGIN.as_str()) != Some(self.origin.as_bytes())
            {
                return Err(StatusCode::FORBIDDEN);
            }
        } else if request.method() != Method::GET && request.method() != Method::HEAD {
            return Err(StatusCode::METHOD_NOT_ALLOWED);
        }
        Ok(())
    }
}

fn single<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a [u8]> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?;
    if values.next().is_some() {
        return None;
    }
    Some(first.as_bytes())
}

fn absent_or_empty(headers: &HeaderMap, name: &str) -> bool {
    !headers.contains_key(name) || single(headers, name) == Some(b"")
}

/// Borrow a canonical token; never log the request or this return value.
pub fn session_token(headers: &HeaderMap) -> Option<&str> {
    let mut found = None;
    for line in headers.get_all(header::COOKIE) {
        for part in line.to_str().ok()?.split(';') {
            let (name, value) = match part.trim().split_once('=') {
                Some(v) => v,
                None => continue,
            };
            if name.trim() != auth::COOKIE_NAME {
                continue;
            }
            if found.is_some() || !auth::valid_token(value) {
                return None;
            }
            found = Some(value);
        }
    }
    found
}

pub fn valid_csrf(headers: &HeaderMap, expected: &str) -> bool {
    !expected.is_empty()
        && single(headers, "x-csrf-token").is_some_and(|v| bool::from(v.ct_eq(expected.as_bytes())))
}

pub fn set_cookie(response: &mut Reply, token: Option<&str>) -> Result<(), &'static str> {
    let (value, max_age) = match token {
        Some(token) if auth::valid_token(token) => (token, auth::LOGIN_LIFETIME_SECONDS),
        Some(_) => return Err("invalid session token"),
        None => ("", 0),
    };
    let cookie = format!(
        "{}={value}; Path=/; Max-Age={max_age}; HttpOnly; Secure; SameSite=Strict",
        auth::COOKIE_NAME
    );
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| "invalid session token")?,
    );
    Ok(())
}

pub fn secure_headers(response: &mut Reply) {
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        (
            "permissions-policy",
            "camera=(), microphone=(), geolocation=()",
        ),
        ("content-security-policy", CSP),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
}

pub fn error(status: StatusCode) -> Reply {
    // Fixed messages only; parser/internal errors must not disclose credentials,
    // filesystem paths, request bodies or connector frames.
    let mut response = Response::new(Body::new(Bytes::from(format!(
        "{}\n",
        status.canonical_reason().unwrap_or("Request failed")
    ))));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    secure_headers(&mut response);
    response
}

/// Fixed retry hint for brief local admission failures and bounded deadlines.
pub fn retryable_error(status: StatusCode) -> Reply {
    debug_assert!(matches!(
        status,
        StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT
    ));
    let mut response = error(status);
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

pub fn json(value: &impl serde::Serialize) -> Reply {
    static BUDGET: OnceLock<Arc<ReplyBudget>> = OnceLock::new();
    json_with_budget(
        value,
        BUDGET
            .get_or_init(|| {
                Arc::new(ReplyBudget {
                    used: AtomicUsize::new(0),
                    limit: MAX_RETAINED_JSON_BYTES,
                })
            })
            .clone(),
    )
}

struct ReplyBudget {
    used: AtomicUsize,
    limit: usize,
}
struct JsonBuffer {
    data: Vec<u8>,
    budget: Arc<ReplyBudget>,
    charged: usize,
    busy: bool,
}
impl JsonBuffer {
    fn charge(&mut self, capacity: usize) -> io::Result<()> {
        if capacity <= self.charged {
            return Ok(());
        }
        let extra = capacity - self.charged;
        if self
            .budget
            .used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(extra)
                    .filter(|new| *new <= self.budget.limit)
            })
            .is_err()
        {
            self.busy = true;
            return Err(io::Error::other("response capacity unavailable"));
        }
        self.charged = capacity;
        Ok(())
    }
}
impl io::Write for JsonBuffer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.len() > MAX_REPLY_BYTES - self.data.len() {
            return Err(io::Error::other("response too large"));
        }
        let needed = self.data.len() + data.len();
        if needed > self.data.capacity() {
            // Admit before growing. Charge Vec capacity, not logical length:
            // the allocation remains owned while even a tiny Bytes slice lives.
            let target = needed.next_power_of_two().min(MAX_REPLY_BYTES);
            self.charge(target)?;
            self.data
                .try_reserve_exact(target - self.data.len())
                .map_err(|_| io::Error::other("response allocation failed"))?;
            if self.data.capacity() > self.charged {
                self.charge(self.data.capacity())?;
            }
        }
        self.data.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl AsRef<[u8]> for JsonBuffer {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}
impl Drop for JsonBuffer {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.data));
        self.budget.used.fetch_sub(self.charged, Ordering::Relaxed);
    }
}
fn json_with_budget(value: &impl serde::Serialize, budget: Arc<ReplyBudget>) -> Reply {
    let mut raw = JsonBuffer {
        data: Vec::new(),
        budget,
        charged: 0,
        busy: false,
    };
    match serde_json::to_writer(&mut raw, value) {
        Ok(()) => {
            // Attach accounting to the allocation itself, not the HTTP Body:
            // Hyper may drop the body while still writing its returned Bytes.
            let mut response = Response::new(Body::new(Bytes::from_owner(raw)));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            secure_headers(&mut response);
            response
        }
        Err(_) if raw.busy => error(StatusCode::SERVICE_UNAVAILABLE),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    MediaType,
    Size,
    Timeout,
    Body,
    Json,
}

/// Route DTOs must use deny_unknown_fields. Root arrays and scalar JSON are
/// rejected here because serde structs can otherwise deserialize from arrays.
pub async fn decode_json<B, T>(request: Request<B>) -> Result<T, DecodeError>
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
    T: DeserializeOwned,
{
    if single(request.headers(), header::CONTENT_TYPE.as_str())
        .is_none_or(|v| !v.starts_with(b"application/json"))
    {
        return Err(DecodeError::MediaType);
    }
    if request.body().size_hint().lower() > MAX_JSON_BYTES as u64 {
        return Err(DecodeError::Size);
    }
    let mut body = request.into_body();
    timeout(BODY_TIMEOUT, async {
        let mut data = BytesMut::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| DecodeError::Body)?;
            if let Ok(chunk) = frame.into_data() {
                if chunk.len() > MAX_JSON_BYTES - data.len() {
                    return Err(DecodeError::Size);
                }
                data.extend_from_slice(&chunk);
            } else {
                return Err(DecodeError::Body);
            }
        }
        if data.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
            return Err(DecodeError::Json);
        }
        serde_json::from_slice(&data).map_err(|_| DecodeError::Json)
    })
    .await
    .map_err(|_| DecodeError::Timeout)?
}

/// X-Real-IP is accepted only from the loopback proxy. Never trust forwarded
/// chains or allow multiple headers to select different rate-limit identities.
pub fn login_ip(peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    let ip = if peer.ip().is_loopback() {
        single(headers, "x-real-ip")
            .and_then(|v| std::str::from_utf8(v).ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(peer.ip())
    } else {
        peer.ip()
    };
    match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        _ => ip,
    }
}

pub fn login_source(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return v4.to_string();
            }
            let mut bytes = ip.octets();
            bytes[8..].fill(0);
            Ipv6Addr::from(bytes).to_string()
        }
    }
}

pub fn browser_label(ua: &str) -> String {
    let mut end = 512.min(ua.len());
    while !ua.is_char_boundary(end) {
        end -= 1;
    }
    let ua = &ua[..end];
    let os = if ua.contains("Android") {
        "Android"
    } else if ["iPhone", "iPad", "iPod"].iter().any(|s| ua.contains(s)) {
        "iOS"
    } else if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Macintosh") || ua.contains("Mac OS X") {
        "macOS"
    } else if ua.contains("CrOS") {
        "ChromeOS"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        ""
    };
    let browser = if ua.contains("Edg/") {
        "Edge"
    } else if ua.contains("CriOS") || ua.contains("Chrome/") {
        "Chrome"
    } else if ua.contains("FxiOS") || ua.contains("Firefox/") {
        "Firefox"
    } else if ua.contains("Safari/") {
        "Safari"
    } else {
        "Unknown browser"
    };
    if os.is_empty() {
        browser.to_owned()
    } else {
        format!("{browser} on {os}")
    }
}

pub fn loopback_address(value: &str) -> io::Result<SocketAddr> {
    let address: SocketAddr = value
        .parse()
        .map_err(|_| io::Error::other("listen must be a literal loopback address"))?;
    let ip = match address.ip() {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        ip => ip,
    };
    if !ip.is_loopback() {
        return Err(io::Error::other(
            "listen must be a literal loopback address",
        ));
    }
    Ok(address)
}

/// Upgraded handlers must retain this context until their socket is closed.
/// Clones share one permit, including the HTTP-to-WebSocket handoff.
#[derive(Clone)]
pub struct RequestContext {
    pub peer: SocketAddr,
    pub shutdown: CancellationToken,
    _connection: Arc<OwnedSemaphorePermit>,
    upgrades: TaskTracker,
}

impl RequestContext {
    /// The framework owns upgrade lifetime and keeps the TCP admission permit.
    /// The route must validate its WebSocket handshake/auth before calling this
    /// and return the corresponding 101 response. No new connection is opened.
    pub fn spawn_upgrade<F, Fut>(
        &self,
        request: &mut Request<Incoming>,
        handler: F,
    ) -> Result<(), &'static str>
    where
        F: FnOnce(TokioIo<hyper::upgrade::Upgraded>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.spawn_upgrade_graceful(request, |socket, shutdown| async move {
            tokio::select! { biased; _ = shutdown.cancelled() => {}, _ = handler(socket) => {} }
        })
    }

    /// Owners with socket tasks use the shutdown token to close and join them.
    /// Admission remains held through cleanup. A noncooperative owner is dropped
    /// after five seconds; this is a shutdown bound, not a normal socket timeout.
    pub fn spawn_upgrade_graceful<F, Fut>(
        &self,
        request: &mut Request<Incoming>,
        handler: F,
    ) -> Result<(), &'static str>
    where
        F: FnOnce(TokioIo<hyper::upgrade::Upgraded>, CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        if self.shutdown.is_cancelled() {
            return Err("gateway is stopping");
        }
        let upgrade = hyper::upgrade::on(request);
        let context = self.clone();
        self.upgrades.spawn(async move {
            let socket = tokio::select! {
                biased;
                _ = context.shutdown.cancelled() => return,
                upgraded = timeout(HEADER_TIMEOUT, upgrade) => match upgraded {
                    Ok(Ok(socket)) => socket,
                    _ => return,
                },
            };
            let work = handler(TokioIo::new(socket), context.shutdown.clone());
            tokio::pin!(work);
            tokio::select! {
                biased;
                _ = context.shutdown.cancelled() => {
                    let _ = timeout(HEADER_TIMEOUT, &mut work).await;
                },
                _ = &mut work => {},
            }
            drop(context);
        });
        Ok(())
    }
}

/// Finite connection tasks; capacity exhaustion drops the accepted socket before
/// allocating parser/request state. The reverse proxy can retry. Every normal
/// response passes security headers, even when the route rejects a request.
pub async fn serve<F, Fut>(
    listener: TcpListener,
    policy: Arc<Policy>,
    handler: F,
    shutdown: CancellationToken,
) -> io::Result<()>
where
    F: Fn(Request<Incoming>, RequestContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Reply> + Send + 'static,
{
    serve_with_socket_setup(listener, policy, handler, shutdown, |socket| {
        socket.set_nodelay(true)
    })
    .await
}

async fn serve_with_socket_setup<F, Fut, S>(
    listener: TcpListener,
    policy: Arc<Policy>,
    handler: F,
    shutdown: CancellationToken,
    setup: S,
) -> io::Result<()>
where
    F: Fn(Request<Incoming>, RequestContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Reply> + Send + 'static,
    S: Fn(&TcpStream) -> io::Result<()>,
{
    loopback_address(&listener.local_addr()?.to_string())?;
    let handler = Arc::new(handler);
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut connections = JoinSet::new();
    let upgrades = TaskTracker::new();
    let stopped = shutdown.child_token();
    let _cancel_on_exit = stopped.clone().drop_guard();
    let result = loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break Ok(()),
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            incoming = listener.accept() => {
                let (socket, peer) = match incoming { Ok(pair) => pair, Err(error) => break Err(error) };
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                // A socket-local setup failure must not stop the listener and
                // cancel unrelated HTTP and upgraded terminal connections.
                if setup(&socket).is_err() { continue; }
                let context = RequestContext { peer, shutdown: stopped.clone(), _connection: Arc::new(permit), upgrades: upgrades.clone() };
                let policy = policy.clone();
                let handler = handler.clone();
                let stop = stopped.clone();
                connections.spawn(async move {
                    let service = service_fn(move |request| {
                        let context = context.clone();
                        let policy = policy.clone();
                        let handler = handler.clone();
                        async move {
                            let mut response = match policy.preflight(&request) {
                                Ok(()) => match timeout(REQUEST_TIMEOUT, handler(request, context)).await {
                                    Ok(response) => response,
                                    Err(_) => closing_error(StatusCode::REQUEST_TIMEOUT),
                                },
                                Err(status) => closing_error(status),
                            };
                            secure_headers(&mut response);
                            Ok::<_, Infallible>(response)
                        }
                    });
                    let mut builder = http1::Builder::new();
                    builder.timer(TokioTimer::new()).header_read_timeout(HEADER_TIMEOUT).max_buf_size(MAX_HEADER_BYTES);
                    // HTTP/1 only. Header deadline also bounds idle keep-alive
                    // connections to five seconds in this candidate.
                    let connection = builder.serve_connection(TokioIo::new(socket), service).with_upgrades();
                    tokio::select! { _ = stop.cancelled() => {}, _ = connection => {} }
                });
            }
        }
    };
    stopped.cancel();
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    upgrades.close();
    upgrades.wait().await;
    result
}

fn closing_error(status: StatusCode) -> Reply {
    let mut reply = error(status);
    reply
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn socket_setup_failure_does_not_stop_the_gateway() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let policy = Arc::new(Policy::new("https://hmux.example", &"A".repeat(43)).unwrap());
        let stop = CancellationToken::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let attempts = calls.clone();
        let server = tokio::spawn(serve_with_socket_setup(
            listener,
            policy,
            |_, _| async { json(&true) },
            stop.clone(),
            move |socket| {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(io::Error::other("synthetic socket option failure"))
                } else {
                    socket.set_nodelay(true)
                }
            },
        ));
        timeout(Duration::from_secs(5), async {
            let mut failed = TcpStream::connect(address).await.unwrap();
            assert_eq!(failed.read(&mut [0; 1]).await.unwrap(), 0);
            let mut healthy = TcpStream::connect(address).await.unwrap();
            healthy
                .write_all(b"GET / HTTP/1.1\r\nHost: hmux.example\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut reply = String::new();
            healthy.read_to_string(&mut reply).await.unwrap();
            assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"));
            assert!(reply.ends_with("true"));
        })
        .await
        .unwrap();
        stop.cancel();
        assert!(timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn json_budget_survives_http_body_handoff_and_small_byte_slices() {
        let budget = Arc::new(ReplyBudget {
            used: AtomicUsize::new(0),
            limit: 64,
        });
        let first = json_with_budget(&"a".repeat(18), budget.clone());
        let second = json_with_budget(&"b".repeat(18), budget.clone());
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(budget.used.load(Ordering::Relaxed), 64);
        assert_eq!(
            json_with_budget(&true, budget.clone()).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        let bytes = first.into_body().collect().await.unwrap().to_bytes();
        let slice = bytes.slice(0..1);
        drop(bytes);
        assert_eq!(budget.used.load(Ordering::Relaxed), 64);
        drop(slice);
        assert_eq!(budget.used.load(Ordering::Relaxed), 32);
        assert_eq!(
            json_with_budget(&true, budget.clone()).status(),
            StatusCode::OK
        );
        drop(second);
        assert_eq!(budget.used.load(Ordering::Relaxed), 0);
        assert_eq!(
            json_with_budget(&"a".repeat(MAX_REPLY_BYTES + 1), budget.clone()).status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(budget.used.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn concurrent_json_encoders_cannot_exceed_shared_capacity() {
        let budget = Arc::new(ReplyBudget {
            used: AtomicUsize::new(0),
            limit: 64,
        });
        let ready = Arc::new(std::sync::Barrier::new(17));
        let release = Arc::new(std::sync::Barrier::new(17));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let budget = budget.clone();
                let ready = ready.clone();
                let release = release.clone();
                std::thread::spawn(move || {
                    let response = json_with_budget(&"a".repeat(18), budget);
                    let status = response.status();
                    ready.wait();
                    release.wait();
                    drop(response);
                    status
                })
            })
            .collect();
        ready.wait();
        let held = budget.used.load(Ordering::Relaxed);
        assert!(held > 0 && held <= 64);
        release.wait();
        let statuses: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == StatusCode::OK)
                .count(),
            held / 32
        );
        assert!(statuses
            .iter()
            .all(|status| *status == StatusCode::OK || *status == StatusCode::SERVICE_UNAVAILABLE));
        assert_eq!(budget.used.load(Ordering::Relaxed), 0);
    }
}
