//! Bounded verified WSS dial for the candidate Home connector. The configured
//! Home may be private, loopback, or on a custom port.
//! WSS uses bounded HTTPS_PROXY/NO_PROXY selection and HTTP(S) CONNECT
//! or SOCKS5; unlike Go's WebSocket client, this dialer does not follow redirects.
//! DNS, certificate validation, and Host use one parsed authority; no TLS or
//! authentication failure falls back to plaintext or a second HTTP request.
use crate::{proxy, upgrade};
use hmux_protocol::transport;
use http::uri::Authority;
use rustls::{pki_types::ServerName, ClientConfig, RootCertStore};
use std::{
    fmt, io,
    net::{SocketAddr, ToSocketAddrs},
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpStream,
    sync::{oneshot, OwnedSemaphorePermit, Semaphore},
    time::{timeout, timeout_at, Instant},
};
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;

const MAX_AUTHORITY: usize = 255;
const MAX_ADDRESSES: usize = 32;
const MAX_ROOTS: usize = 1024;
const MAX_ROOT_BYTES: usize = 2 << 20;
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(15);
const PHASE_TIMEOUT: Duration = Duration::from_secs(5);
const ROOT_LOAD_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONNECT_HEADERS: usize = 8192;
const MAX_CONNECT_FIELDS: usize = 32;
static ROOT_LOAD_SLOT: OnceLock<Arc<Semaphore>> = OnceLock::new();
static NATIVE_TLS: OnceLock<Arc<ClientConfig>> = OnceLock::new();
static OUTBOUND_SLOT: OnceLock<Arc<Semaphore>> = OnceLock::new();
// Usage must never contend with the persistent Gateway socket.
static USAGE_SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
static LB_SLOT: OnceLock<Arc<Semaphore>> = OnceLock::new();
static ENV_PROXY: OnceLock<Arc<proxy::Policy>> = OnceLock::new();

type Resolver = dyn Fn(&str, u16) -> Result<Vec<SocketAddr>, Error> + Send + Sync;
pub(crate) trait AsyncIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncIo for T {}
pub(crate) type BoxedStream = Box<dyn AsyncIo>;

/// Fixed errors never include the endpoint, certificate, bearer, or response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Busy,
    Cancelled,
    Timeout,
    Resolve,
    Connect,
    Tls,
    Unavailable,
    Proxy,
    UnsupportedProxy,
    Upgrade(upgrade::Error),
}

/// A validated exact `wss://authority/connect` endpoint. Debug is redacted.
#[derive(Clone)]
pub struct Endpoint {
    authority: String,
    host: String,
    port: u16,
    server_name: ServerName<'static>,
}
impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Endpoint([redacted])")
    }
}
impl Endpoint {
    pub fn parse(raw: &str) -> Result<Self, Error> {
        if raw.len() > 6 + MAX_AUTHORITY + 8 {
            return Err(Error::Invalid);
        }
        let authority = raw
            .strip_prefix("wss://")
            .and_then(|value| value.strip_suffix("/connect"))
            .ok_or(Error::Invalid)?;
        if !upgrade::valid_authority(authority) {
            return Err(Error::Invalid);
        }
        let parsed: Authority = authority.parse().map_err(|_| Error::Invalid)?;
        let host = parsed.host().trim_start_matches('[').trim_end_matches(']');
        if host.is_empty() {
            return Err(Error::Invalid);
        }
        let server_name = ServerName::try_from(host.to_owned()).map_err(|_| Error::Invalid)?;
        Ok(Self {
            authority: authority.to_owned(),
            host: host.to_owned(),
            port: parsed.port_u16().unwrap_or(443),
            server_name,
        })
    }
}

/// Keeps the one process-wide outbound slot for the actual socket lifetime.
/// Hyper upgrade and WebSocket transport move this wrapper without releasing it.
struct PermitStream<S> {
    stream: S,
    _permit: OwnedSemaphorePermit,
}
impl<S: AsyncRead + Unpin> AsyncRead for PermitStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for PermitStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

struct Inner {
    tls: Arc<ClientConfig>,
    resolver: Arc<Resolver>,
    slot: Arc<Semaphore>,
    proxy: Arc<proxy::Policy>,
}
/// Cloneable WSS dialer. Native TLS trust and environment proxy policy are
/// captured once per process, including across reconnect attempts.
#[derive(Clone)]
pub struct Client(Arc<Inner>);
impl Client {
    /// Load and verify system roots once. A dropped caller cannot release the
    /// single loader admission slot before the blocking OS loader has exited.
    pub async fn new() -> Result<Self, Error> {
        let tls = load_tls(&NATIVE_TLS, &ROOT_LOAD_SLOT, ROOT_LOAD_TIMEOUT, || {
            let loaded = rustls_native_certs::load_native_certs();
            if !loaded.errors.is_empty() {
                return Err(Error::Unavailable);
            }
            config_from_roots(loaded.certs)
        })
        .await?;
        let policy = ENV_PROXY
            .get_or_init(|| Arc::new(proxy::Policy::from_env()))
            .clone();
        Ok(Self::with_config_and_policy(
            tls,
            Arc::new(native_resolve),
            policy,
        ))
    }

    #[cfg(test)]
    pub(crate) fn with_config(tls: Arc<ClientConfig>, resolver: Arc<Resolver>) -> Self {
        Self::with_config_and_policy(tls, resolver, Arc::new(proxy::Policy::direct()))
    }

    fn with_config_and_policy(
        tls: Arc<ClientConfig>,
        resolver: Arc<Resolver>,
        proxy: Arc<proxy::Policy>,
    ) -> Self {
        Self(Arc::new(Inner {
            tls,
            resolver,
            proxy,
            slot: OUTBOUND_SLOT
                .get_or_init(|| Arc::new(Semaphore::new(1)))
                .clone(),
        }))
    }

    /// One absolute 15-second attempt, bounded by the caller's earlier deadline.
    /// DNS workers only resolve; a cancelled worker cannot later open a socket.
    pub async fn connect(
        &self,
        endpoint: &str,
        bearer: &str,
        cancelled: &CancellationToken,
        parent_deadline: Option<Instant>,
    ) -> Result<transport::Connection, Error> {
        let endpoint = Endpoint::parse(endpoint)?;
        let route =
            self.0
                .proxy
                .select(&endpoint.host, endpoint.port)
                .map_err(|error| match error {
                    proxy::Error::Invalid => Error::Proxy,
                    proxy::Error::Unsupported => Error::UnsupportedProxy,
                })?;
        if !upgrade::valid_bearer(bearer) {
            return Err(Error::Invalid);
        }
        if cancelled.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut deadline = Instant::now() + ATTEMPT_TIMEOUT;
        if let Some(parent) = parent_deadline {
            deadline = deadline.min(parent);
        }
        if deadline <= Instant::now() {
            return Err(Error::Timeout);
        }
        tokio::select! {
            biased;
            _ = cancelled.cancelled() => Err(Error::Cancelled),
            result = timeout_at(deadline, self.connect_inner(endpoint, route, bearer, cancelled, deadline)) => {
                result.map_err(|_| Error::Timeout)?
            }
        }
    }

    async fn connect_inner(
        &self,
        endpoint: Endpoint,
        route: Option<proxy::Proxy>,
        bearer: &str,
        cancelled: &CancellationToken,
        deadline: Instant,
    ) -> Result<transport::Connection, Error> {
        let stream = self.open_tls(&endpoint, route, self.0.slot.clone()).await?;
        if cancelled.is_cancelled() {
            return Err(Error::Cancelled);
        }
        upgrade::upgrade(
            stream,
            &endpoint.authority,
            bearer,
            cancelled,
            Some(deadline),
        )
        .await
        .map_err(|error| match error {
            upgrade::Error::Timeout => Error::Timeout,
            upgrade::Error::Cancelled => Error::Cancelled,
            other => Error::Upgrade(other),
        })
    }

    /// Two shared usage sockets maximum, independent of the Gateway socket.
    /// The caller owns the deadline and stream; dropping it cancels every async
    /// phase. An outstanding blocking DNS lookup retains its admission permit.
    pub(crate) async fn usage_tls(&self, authority: &str) -> Result<BoxedStream, Error> {
        let endpoint = Endpoint::parse(&format!("wss://{authority}/connect"))?;
        let route =
            self.0
                .proxy
                .select(&endpoint.host, endpoint.port)
                .map_err(|error| match error {
                    proxy::Error::Invalid => Error::Proxy,
                    proxy::Error::Unsupported => Error::UnsupportedProxy,
                })?;
        self.open_tls(
            &endpoint,
            route,
            USAGE_SLOTS
                .get_or_init(|| Arc::new(Semaphore::new(2)))
                .clone(),
        )
        .await
    }

    // A third fixed source gets its own single slot, so concurrent OAuth
    // refreshes cannot starve LB or the long-lived Gateway connection.
    pub(crate) async fn lb_tls(&self, authority: &str) -> Result<BoxedStream, Error> {
        let endpoint = Endpoint::parse(&format!("wss://{authority}/connect"))?;
        let route = self
            .0
            .proxy
            .select(&endpoint.host, endpoint.port)
            .map_err(|_| Error::Proxy)?;
        self.open_tls(
            &endpoint,
            route,
            LB_SLOT.get_or_init(|| Arc::new(Semaphore::new(1))).clone(),
        )
        .await
    }
    pub(crate) async fn lb_loopback(&self, address: SocketAddr) -> Result<BoxedStream, Error> {
        if !address.ip().is_loopback() || address.port() == 0 {
            return Err(Error::Invalid);
        }
        let permit = LB_SLOT
            .get_or_init(|| Arc::new(Semaphore::new(1)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let stream = TcpStream::connect(address)
            .await
            .map_err(|_| Error::Connect)?;
        stream.set_nodelay(true).map_err(|_| Error::Connect)?;
        Ok(Box::new(PermitStream {
            stream,
            _permit: permit,
        }))
    }

    async fn open_tls(
        &self,
        endpoint: &Endpoint,
        route: Option<proxy::Proxy>,
        slots: Arc<Semaphore>,
    ) -> Result<BoxedStream, Error> {
        let permit = slots.try_acquire_owned().map_err(|_| Error::Busy)?;
        let (tx, rx) = oneshot::channel();
        let resolver = self.0.resolver.clone();
        let (host, port) = route
            .as_ref()
            .map_or((endpoint.host.clone(), endpoint.port), |proxy| {
                (proxy.host.clone(), proxy.port)
            });
        tokio::task::spawn_blocking(move || {
            let addresses = resolver(&host, port).and_then(|found| validate_addresses(found, port));
            // If the timed-out or cancelled caller dropped rx, this also drops
            // the permit. An OS resolver that never returns retains admission.
            let _ = tx.send((permit, addresses));
        });
        let (permit, addresses) = timeout(PHASE_TIMEOUT, rx)
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(|_| Error::Resolve)?;
        let addresses = addresses?;
        let mut last = Error::Connect;
        for address in addresses {
            match timeout(PHASE_TIMEOUT, TcpStream::connect(address)).await {
                Ok(Ok(socket)) => {
                    // Match Go's TCP default and the Gateway accept path. Small
                    // terminal/ACK frames must not wait for Nagle + delayed ACK,
                    // including when TLS or a proxy wraps this first-hop socket.
                    socket.set_nodelay(true).map_err(|_| Error::Connect)?;
                    let socket: BoxedStream = Box::new(PermitStream {
                        stream: socket,
                        _permit: permit,
                    });
                    let socket = if let Some(proxy) = route {
                        let mut first_hop: BoxedStream = if proxy.scheme == proxy::Scheme::Https {
                            Box::new(
                                timeout(
                                    PHASE_TIMEOUT,
                                    TlsConnector::from(self.0.tls.clone())
                                        .connect(proxy.server_name, socket),
                                )
                                .await
                                .map_err(|_| Error::Timeout)?
                                .map_err(|_| Error::Tls)?,
                            )
                        } else {
                            socket
                        };
                        let tunnel = async {
                            if proxy.scheme == proxy::Scheme::Socks5 {
                                connect_socks5(&mut first_hop, endpoint, proxy.socks_auth.as_ref())
                                    .await
                            } else {
                                connect_tunnel(
                                    &mut first_hop,
                                    endpoint,
                                    proxy.basic_auth.as_deref(),
                                )
                                .await
                            }
                        };
                        timeout(PHASE_TIMEOUT, tunnel)
                            .await
                            .map_err(|_| Error::Timeout)??;
                        first_hop
                    } else {
                        socket
                    };
                    let tls = timeout(
                        PHASE_TIMEOUT,
                        TlsConnector::from(self.0.tls.clone())
                            .connect(endpoint.server_name.clone(), socket),
                    )
                    .await
                    .map_err(|_| Error::Timeout)?
                    .map_err(|_| Error::Tls)?;
                    return Ok(Box::new(tls));
                }
                Ok(Err(_)) => last = Error::Connect,
                Err(_) => last = Error::Timeout,
            }
        }
        Err(last)
    }
}

/// Both Go SOCKS schemes delegate name resolution to the proxy. Read exactly
/// the reply so target TLS starts at the next byte, with no unbounded buffer.
async fn connect_socks5(
    stream: &mut BoxedStream,
    endpoint: &Endpoint,
    auth: Option<&(Vec<u8>, Vec<u8>)>,
) -> Result<(), Error> {
    let methods: &[u8] = if auth.is_some() {
        &[5, 2, 0, 2]
    } else {
        &[5, 1, 0]
    };
    stream.write_all(methods).await.map_err(|_| Error::Proxy)?;
    let mut selected = [0u8; 2];
    stream
        .read_exact(&mut selected)
        .await
        .map_err(|_| Error::Proxy)?;
    if selected[0] != 5 {
        return Err(Error::Proxy);
    }
    match selected[1] {
        0 => {}
        2 if auth.is_some() => {
            let (user, pass) = auth.ok_or(Error::Proxy)?;
            if user.is_empty() || user.len() > 255 || pass.len() > 255 {
                return Err(Error::Proxy);
            }
            let mut request = Vec::with_capacity(3 + user.len() + pass.len());
            request.extend([1, user.len() as u8]);
            request.extend(user);
            request.push(pass.len() as u8);
            request.extend(pass);
            stream.write_all(&request).await.map_err(|_| Error::Proxy)?;
            let mut response = [0u8; 2];
            stream
                .read_exact(&mut response)
                .await
                .map_err(|_| Error::Proxy)?;
            if response != [1, 0] {
                return Err(Error::Proxy);
            }
        }
        _ => return Err(Error::Proxy),
    }
    let mut request = Vec::with_capacity(6 + endpoint.host.len());
    request.extend([5, 1, 0]);
    if let Ok(ip) = endpoint.host.parse::<std::net::IpAddr>() {
        match ip.to_canonical() {
            std::net::IpAddr::V4(ip) => {
                request.push(1);
                request.extend(ip.octets());
            }
            std::net::IpAddr::V6(ip) => {
                request.push(4);
                request.extend(ip.octets());
            }
        }
    } else {
        if endpoint.host.len() > 255 || !endpoint.host.is_ascii() {
            return Err(Error::Proxy);
        }
        request.extend([3, endpoint.host.len() as u8]);
        request.extend(endpoint.host.as_bytes());
    }
    request.extend(endpoint.port.to_be_bytes());
    stream.write_all(&request).await.map_err(|_| Error::Proxy)?;
    let mut head = [0u8; 4];
    stream
        .read_exact(&mut head)
        .await
        .map_err(|_| Error::Proxy)?;
    if head[0] != 5 || head[1] != 0 || head[2] != 0 {
        return Err(Error::Proxy);
    }
    let tail_len = match head[3] {
        1 => 4 + 2,
        4 => 16 + 2,
        3 => {
            let mut length = [0u8; 1];
            stream
                .read_exact(&mut length)
                .await
                .map_err(|_| Error::Proxy)?;
            usize::from(length[0]) + 2
        }
        _ => return Err(Error::Proxy),
    };
    let mut tail = [0u8; 257];
    stream
        .read_exact(&mut tail[..tail_len])
        .await
        .map_err(|_| Error::Proxy)?;
    Ok(())
}

/// CONNECT is sent only to the proxy. The Home bearer is introduced later,
/// after the target certificate has been checked inside this tunnel.
async fn connect_tunnel(
    stream: &mut BoxedStream,
    endpoint: &Endpoint,
    proxy_auth: Option<&str>,
) -> Result<(), Error> {
    let target = if endpoint.host.contains(':') {
        format!("[{}]:{}", endpoint.host, endpoint.port)
    } else {
        format!("{}:{}", endpoint.host, endpoint.port)
    };
    let auth = proxy_auth.map_or(String::new(), |value| {
        format!("Proxy-Authorization: {value}\r\n")
    });
    let request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n{auth}\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|_| Error::Proxy)?;
    stream.flush().await.map_err(|_| Error::Proxy)?;

    // Read exactly the CONNECT header bytes. Any buffered TLS bytes from the
    // Home must remain on the stream for the next handshake.
    let mut response = [0u8; MAX_CONNECT_HEADERS];
    let mut length = 0;
    loop {
        if length == response.len() {
            return Err(Error::Proxy);
        }
        stream
            .read_exact(&mut response[length..length + 1])
            .await
            .map_err(|_| Error::Proxy)?;
        length += 1;
        if response[..length].ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let header = std::str::from_utf8(&response[..length]).map_err(|_| Error::Proxy)?;
    let mut lines = header.split("\r\n");
    let status = lines.next().ok_or(Error::Proxy)?;
    let mut parts = status.splitn(3, ' ');
    let version = parts.next().ok_or(Error::Proxy)?;
    let code = parts.next().ok_or(Error::Proxy)?;
    let reason = parts.next().unwrap_or("");
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1")
        || code != "200"
        || !reason.bytes().all(|b| (0x20..=0x7e).contains(&b))
    {
        return Err(Error::Proxy);
    }
    let mut fields = 0;
    for line in lines {
        if line.is_empty() {
            break;
        }
        fields += 1;
        if fields > MAX_CONNECT_FIELDS || !valid_connect_header_line(line) {
            return Err(Error::Proxy);
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("transfer-encoding")
                || (name.eq_ignore_ascii_case("content-length") && value.trim() != "0")
            {
                return Err(Error::Proxy);
            }
        }
    }
    Ok(())
}

fn valid_connect_header_line(line: &str) -> bool {
    let Some((name, value)) = line.split_once(':') else {
        return false;
    };
    if name.is_empty()
        || !name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
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
    value
        .bytes()
        .all(|b| b == b'\t' || (0x20..=0x7e).contains(&b))
}

async fn load_tls(
    cache: &'static OnceLock<Arc<ClientConfig>>,
    admission: &'static OnceLock<Arc<Semaphore>>,
    limit: Duration,
    loader: impl FnOnce() -> Result<Arc<ClientConfig>, Error> + Send + 'static,
) -> Result<Arc<ClientConfig>, Error> {
    if let Some(tls) = cache.get() {
        return Ok(tls.clone());
    }
    let slot = admission
        .get_or_init(|| Arc::new(Semaphore::new(1)))
        .clone();
    let permit = slot.try_acquire_owned().map_err(|_| Error::Busy)?;
    // Another loader may have published between the first cache read and slot.
    if let Some(tls) = cache.get() {
        return Ok(tls.clone());
    }
    let (tx, rx) = oneshot::channel();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let result = loader();
        if let Ok(ref tls) = result {
            let _ = cache.set(tls.clone());
        }
        let _ = tx.send(result);
    });
    timeout(limit, rx)
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|_| Error::Unavailable)?
}

fn config_from_roots(
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
) -> Result<Arc<ClientConfig>, Error> {
    if certs.is_empty()
        || certs.len() > MAX_ROOTS
        || certs
            .iter()
            .try_fold(0usize, |n, cert| n.checked_add(cert.as_ref().len()))
            .is_none_or(|total| total > MAX_ROOT_BYTES)
    {
        return Err(Error::Unavailable);
    }
    let mut roots = RootCertStore::empty();
    for cert in certs {
        roots.add(cert).map_err(|_| Error::Unavailable)?;
    }
    let mut tls =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| Error::Unavailable)?
            .with_root_certificates(roots)
            .with_no_client_auth();
    tls.alpn_protocols.clear(); // HTTP/1 only.
    tls.resumption = rustls::client::Resumption::in_memory_sessions(4);
    Ok(Arc::new(tls))
}

fn native_resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>, Error> {
    (host, port)
        .to_socket_addrs()
        .map_err(|_| Error::Resolve)
        .map(|addresses| addresses.take(MAX_ADDRESSES + 1).collect())
}
fn validate_addresses(addresses: Vec<SocketAddr>, port: u16) -> Result<Vec<SocketAddr>, Error> {
    if addresses.is_empty()
        || addresses.len() > MAX_ADDRESSES
        || addresses.iter().any(|address| address.port() != port)
    {
        return Err(Error::Invalid);
    }
    Ok(addresses)
}

#[cfg(test)]
#[path = "dial_tests.rs"]
mod tests;
