//! Bounded HTTPS delivery to validated public push services and fixed session
//! location metadata. Shared admission and trust serve the optional
//! gateway push owner. Native DNS and roots preserve host policy;
//! a native resolver call itself cannot be cancelled or joined with a time bound.
use crate::{auth_store::SessionAccess, push_crypto::Prepared, push_state};
use http::{header, HeaderValue, Request, StatusCode, Uri};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use rustls::{pki_types::ServerName, ClientConfig, RootCertStore};
use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, OnceLock,
    },
    time::{Duration, SystemTime},
};
use tokio::{
    net::TcpStream,
    sync::{oneshot, Semaphore},
    time::{timeout, timeout_at, Instant},
};
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;

const SLOTS: usize = 2;
const MAX_ADDRESSES: usize = 32;
const MAX_ROOTS: usize = 1024;
const MAX_ROOT_BYTES: usize = 2 << 20;
const MAX_BODY: usize = 4096;
const MAX_AUTH: usize = 4096;
const MAX_TOPIC: usize = 64;
const MAX_RESPONSE: usize = 4096;
static ROOT_LOAD_SLOT: OnceLock<Arc<Semaphore>> = OnceLock::new();
static NATIVE_TLS: OnceLock<Arc<ClientConfig>> = OnceLock::new();
static OUTBOUND_SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();

type Resolver = dyn Fn(&str) -> Result<Vec<SocketAddr>, Error> + Send + Sync;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Busy,
    Unauthorized,
    Cancelled,
    Timeout,
    Unavailable,
    Redirect,
}

struct Inner {
    tls: Arc<ClientConfig>,
    slots: Arc<Semaphore>,
    shutdown: CancellationToken,
    resolver: Arc<Resolver>,
    dns_jobs: AtomicUsize,
    #[cfg(test)]
    dial_override: Option<SocketAddr>,
}
struct DnsJobGuard(Arc<Inner>);
impl Drop for DnsJobGuard {
    fn drop(&mut self) {
        self.0.dns_jobs.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Cloneable, one-owner transport. Each request opens one HTTP/1 connection and
/// closes it after a bounded response; there is no resident idle TCP pool.
#[derive(Clone)]
pub struct Client(Arc<Inner>);

impl Client {
    /// Synthetic trust for isolated in-crate tests; normal TLS verification and
    /// outbound address/admission rules are unchanged.
    #[cfg(test)]
    pub(crate) fn with_test_roots(
        roots: Vec<rustls::pki_types::CertificateDer<'static>>,
    ) -> Result<Self, Error> {
        Ok(Self::with_config(
            config_from_roots(roots)?,
            Arc::new(native_resolve),
        ))
    }

    /// Load system trust once during bounded blocking startup. A dropped init
    /// waiter does not release its loader permit before the OS loader returns.
    pub async fn new() -> Result<Self, Error> {
        if let Some(tls) = NATIVE_TLS.get() {
            return Ok(Self::with_config(tls.clone(), Arc::new(native_resolve)));
        }
        let slots = ROOT_LOAD_SLOT
            .get_or_init(|| Arc::new(Semaphore::new(1)))
            .clone();
        let permit = slots.try_acquire_owned().map_err(|_| Error::Busy)?;
        // A concurrent loader may have published between the first cache read
        // and admission. Holding this permit makes the second check decisive.
        if let Some(tls) = NATIVE_TLS.get() {
            return Ok(Self::with_config(tls.clone(), Arc::new(native_resolve)));
        }
        let (tx, rx) = oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let roots = rustls_native_certs::load_native_certs();
            let result = if roots.errors.is_empty() {
                config_from_roots(roots.certs)
            } else {
                Err(Error::Unavailable)
            };
            if let Ok(ref tls) = result {
                let _ = NATIVE_TLS.set(tls.clone());
            }
            let _ = tx.send(result);
        });
        let tls = rx.await.map_err(|_| Error::Unavailable)??;
        Ok(Self::with_config(tls, Arc::new(native_resolve)))
    }

    fn with_config(tls: Arc<ClientConfig>, resolver: Arc<Resolver>) -> Self {
        Self(Arc::new(Inner {
            tls,
            slots: OUTBOUND_SLOTS
                .get_or_init(|| Arc::new(Semaphore::new(SLOTS)))
                .clone(),
            shutdown: CancellationToken::new(),
            resolver,
            dns_jobs: AtomicUsize::new(0),
            #[cfg(test)]
            dial_override: None,
        }))
    }

    /// Returns completed HTTP status. The caller owns guarded 404/410 cleanup.
    /// `check` must query current login, subscription and membership immediately
    /// before the POST. It runs under the same deadline and cancellation scope.
    pub async fn send<F, Fut>(
        &self,
        prepared: Prepared,
        access: SessionAccess,
        parent_deadline: Option<Instant>,
        check: F,
    ) -> Result<StatusCode, Error>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), Error>>,
    {
        if self.0.shutdown.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let until_expiry = SystemTime::from(access.expires_at)
            .duration_since(SystemTime::now())
            .map_err(|_| Error::Unauthorized)?;
        if *access.cancelled.borrow()
            || access.cancelled.has_changed().is_err()
            || until_expiry.is_zero()
        {
            return Err(Error::Unauthorized);
        }
        let mut deadline = Instant::now() + Duration::from_secs(10).min(until_expiry);
        if let Some(parent) = parent_deadline {
            deadline = deadline.min(parent);
        }
        if deadline <= Instant::now() {
            return Err(Error::Timeout);
        }
        let mut cancellation = access.cancelled.clone();
        let work = self.send_inner(prepared, &access, deadline, check);
        tokio::select! {
            biased;
            _ = self.0.shutdown.cancelled() => Err(Error::Cancelled),
            _ = async { while !*cancellation.borrow() && cancellation.changed().await.is_ok() {} } => Err(Error::Unauthorized),
            result = timeout_at(deadline, work) => result.map_err(|_| Error::Timeout)?,
        }
    }

    async fn send_inner<F, Fut>(
        &self,
        prepared: Prepared,
        access: &SessionAccess,
        deadline: Instant,
        check: F,
    ) -> Result<StatusCode, Error>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), Error>>,
    {
        let uri = validate_prepared(&prepared)?;
        let (tls, _permit) = self.connect(&uri).await?;
        // Authorization is checked after DNS/TLS and immediately before POST.
        check().await?;
        if *access.cancelled.borrow()
            || access.cancelled.has_changed().is_err()
            || SystemTime::from(access.expires_at) <= SystemTime::now()
        {
            return Err(Error::Unauthorized);
        }
        if self.0.shutdown.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(Error::Timeout);
        }
        post(tls, &uri, prepared).await
    }

    /// Fixed unauthenticated metadata endpoint; arbitrary URLs are not accepted.
    /// Shares native trust, DNS/socket admission and shutdown with push delivery.
    pub(crate) async fn location(&self, address: IpAddr) -> Result<Vec<u8>, Error> {
        let address = address.to_canonical();
        if !crate::session_location::public_ip(address) {
            return Err(Error::Invalid);
        }
        let uri: Uri = format!("https://ipwho.is/{address}?fields=success,country,region,city")
            .parse()
            .map_err(|_| Error::Invalid)?;
        let work = async {
            let (tls, _permit) = self.connect(&uri).await?;
            get_location(tls, &uri).await
        };
        tokio::select! {
            biased;
            _ = self.0.shutdown.cancelled() => Err(Error::Cancelled),
            result = timeout(Duration::from_secs(2), work) => result.map_err(|_| Error::Timeout)?,
        }
    }

    async fn connect(
        &self,
        uri: &Uri,
    ) -> Result<
        (
            tokio_rustls::client::TlsStream<TcpStream>,
            tokio::sync::OwnedSemaphorePermit,
        ),
        Error,
    > {
        let host = uri.host().ok_or(Error::Invalid)?.to_owned();
        let permit = self
            .0
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let (tx, rx) = oneshot::channel();
        let resolver = self.0.resolver.clone();
        let jobs = self.0.clone();
        jobs.dns_jobs.fetch_add(1, Ordering::AcqRel);
        tokio::task::spawn_blocking(move || {
            let _job = DnsJobGuard(jobs);
            // The only worker permitted to call the host resolver. A timed-out
            // caller drops rx; no later connection is created by this worker.
            let found = resolver(&host).and_then(validate_addresses);
            let _ = tx.send((permit, found));
        });
        let (permit, addresses) = rx.await.map_err(|_| Error::Unavailable)?;
        let addresses = addresses?;
        let host = uri.host().ok_or(Error::Invalid)?.to_owned();
        let mut last = Error::Unavailable;
        for address in addresses {
            #[cfg(test)]
            let dial_address = self.0.dial_override.unwrap_or(address);
            #[cfg(not(test))]
            let dial_address = address;
            match timeout(Duration::from_secs(5), TcpStream::connect(dial_address)).await {
                Ok(Ok(stream)) => {
                    let name = ServerName::try_from(host.clone()).map_err(|_| Error::Invalid)?;
                    let tls = timeout(
                        Duration::from_secs(5),
                        TlsConnector::from(self.0.tls.clone()).connect(name, stream),
                    )
                    .await
                    .map_err(|_| Error::Timeout)?
                    .map_err(|_| Error::Unavailable)?;
                    return Ok((tls, permit));
                }
                Ok(Err(_)) => last = Error::Unavailable,
                Err(_) => last = Error::Timeout,
            }
        }
        Err(last)
    }

    /// Closes admission and signals active async sends; the owner must poll/join
    /// those sends to close sockets. Returns
    /// the number of outstanding native DNS calls still running in OS workers.
    /// Such calls retain their permits until they actually return.
    pub fn shutdown(&self) -> usize {
        self.0.shutdown.cancel();
        // Admission is process-wide: closing it would disable future owners.
        // This owner's cancellation gate rejects sends while old DNS workers
        // retain the shared permits until the OS actually returns.
        self.0.dns_jobs.load(Ordering::Acquire)
    }

    pub fn outstanding_dns_jobs(&self) -> usize {
        self.0.dns_jobs.load(Ordering::Acquire)
    }
}

fn config_from_roots(
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
) -> Result<Arc<ClientConfig>, Error> {
    if certs.is_empty()
        || certs.len() > MAX_ROOTS
        || certs
            .iter()
            .try_fold(0usize, |n, cert| n.checked_add(cert.as_ref().len()))
            .is_none_or(|n| n > MAX_ROOT_BYTES)
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
    tls.alpn_protocols.clear(); // HTTP/1 only; no HTTP/2 or proxy protocol.
    tls.resumption = rustls::client::Resumption::in_memory_sessions(4);
    Ok(Arc::new(tls))
}

fn native_resolve(host: &str) -> Result<Vec<SocketAddr>, Error> {
    (host, 443)
        .to_socket_addrs()
        .map_err(|_| Error::Unavailable)
        .map(|addresses| addresses.take(MAX_ADDRESSES + 1).collect())
}

fn validate_addresses(addresses: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, Error> {
    if addresses.is_empty() || addresses.len() > MAX_ADDRESSES {
        return Err(Error::Invalid);
    }
    let mut pinned = Vec::with_capacity(addresses.len());
    for address in addresses {
        if address.port() != 443 || !public_address(address.ip()) {
            return Err(Error::Invalid);
        }
        pinned.push(address);
    }
    Ok(pinned)
}

fn in4(ip: Ipv4Addr, base: [u8; 4], bits: u32) -> bool {
    let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
    u32::from(ip) & mask == u32::from(Ipv4Addr::from(base)) & mask
}
fn in6(ip: Ipv6Addr, base: [u16; 8], bits: u32) -> bool {
    let mask = u128::MAX.checked_shl(128 - bits).unwrap_or(0);
    u128::from(ip) & mask == u128::from(Ipv6Addr::from(base)) & mask
}

/// Mirrors Go `publicPushAddress`: unmap IPv4-in-IPv6 before applying every
/// special-use exclusion. IPv6 is limited to 2000::/3 global unicast.
pub fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => public_v4(v),
        IpAddr::V6(v) => match v.to_ipv4_mapped() {
            Some(mapped) => public_v4(mapped),
            None => {
                in6(v, [0x2000, 0, 0, 0, 0, 0, 0, 0], 3)
                    && !in6(v, [0x2001, 0, 0, 0, 0, 0, 0, 0], 23)
                    && !in6(v, [0x2001, 0xdb8, 0, 0, 0, 0, 0, 0], 32)
                    && !in6(v, [0x2002, 0, 0, 0, 0, 0, 0, 0], 16)
                    && !in6(v, [0x3fff, 0, 0, 0, 0, 0, 0, 0], 20)
            }
        },
    }
}
fn public_v4(v: Ipv4Addr) -> bool {
    ![
        ([0, 0, 0, 0], 8),
        ([10, 0, 0, 0], 8),
        ([100, 64, 0, 0], 10),
        ([127, 0, 0, 0], 8),
        ([169, 254, 0, 0], 16),
        ([172, 16, 0, 0], 12),
        ([192, 0, 0, 0], 24),
        ([192, 0, 2, 0], 24),
        ([192, 88, 99, 0], 24),
        ([192, 168, 0, 0], 16),
        ([198, 18, 0, 0], 15),
        ([198, 51, 100, 0], 24),
        ([203, 0, 113, 0], 24),
        ([224, 0, 0, 0], 4),
        ([240, 0, 0, 0], 4),
    ]
    .iter()
    .any(|&(base, bits)| in4(v, base, bits))
}

fn validate_prepared(p: &Prepared) -> Result<Uri, Error> {
    let uri = push_state::validate_endpoint(&p.endpoint).map_err(|_| Error::Invalid)?;
    if p.body.len() > MAX_BODY
        || p.authorization.len() > MAX_AUTH
        || p.topic.len() > MAX_TOPIC
        || p.ttl != "120"
        || p.urgency != "normal"
        || p.content_type != "application/octet-stream"
        || p.content_encoding != "aes128gcm"
        || p.topic.len() != 32
    {
        return Err(Error::Invalid);
    }
    for value in [&p.authorization, &p.topic] {
        HeaderValue::from_str(value).map_err(|_| Error::Invalid)?;
    }
    Ok(uri)
}

async fn get_location<S>(tls: S, uri: &Uri) -> Result<Vec<u8>, Error>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut builder = http1::Builder::new();
    builder.max_buf_size(8192).max_headers(64);
    let (mut sender, conn) = builder
        .handshake(TokioIo::new(tls))
        .await
        .map_err(|_| Error::Unavailable)?;
    let request = Request::get(uri.path_and_query().ok_or(Error::Invalid)?.as_str())
        .header(header::HOST, "ipwho.is")
        .body(Full::new(bytes::Bytes::new()))
        .map_err(|_| Error::Invalid)?;
    let operation = async {
        let response = sender
            .send_request(request)
            .await
            .map_err(|_| Error::Unavailable)?;
        if response.status().is_redirection() {
            return Err(Error::Redirect);
        }
        if response.status() != StatusCode::OK {
            return Err(Error::Unavailable);
        }
        let mut body = response.into_body();
        let mut raw = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| Error::Unavailable)?;
            if let Some(data) = frame.data_ref() {
                if data.len() > 8192 - raw.len() {
                    return Err(Error::Invalid);
                }
                raw.extend_from_slice(data);
            }
        }
        Ok(raw)
    };
    tokio::pin!(conn);
    tokio::pin!(operation);
    tokio::select! {
        result = &mut operation => result,
        _ = &mut conn => operation.await,
    }
}

async fn post<S>(tls: S, uri: &Uri, p: Prepared) -> Result<StatusCode, Error>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut builder = http1::Builder::new();
    builder.max_buf_size(8192).max_headers(64);
    let (mut sender, conn) = builder
        .handshake(TokioIo::new(tls))
        .await
        .map_err(|_| Error::Unavailable)?;
    let target = uri.path_and_query().map(|v| v.as_str()).unwrap_or("/");
    let request = Request::post(target)
        .header(header::HOST, uri.host().ok_or(Error::Invalid)?)
        .header(header::AUTHORIZATION, p.authorization)
        .header(header::CONTENT_ENCODING, p.content_encoding)
        .header(header::CONTENT_TYPE, p.content_type)
        .header("TTL", p.ttl)
        .header("Urgency", p.urgency)
        .header("Topic", p.topic)
        .header(header::CONTENT_LENGTH, p.body.len())
        .body(Full::new(p.body))
        .map_err(|_| Error::Invalid)?;
    let operation = async {
        let response = timeout(Duration::from_secs(8), sender.send_request(request))
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(|_| Error::Unavailable)?;
        let status = response.status();
        if status.is_redirection() {
            return Err(Error::Redirect);
        }
        let mut body = response.into_body();
        let mut remaining = MAX_RESPONSE;
        while remaining > 0 {
            let Some(frame) = body.frame().await else {
                break;
            };
            // Go ignores a response-body copy error after receiving status.
            let Ok(frame) = frame else {
                break;
            };
            if let Some(data) = frame.data_ref() {
                remaining -= data.len().min(remaining);
            }
        }
        Ok(status)
    };
    tokio::pin!(conn);
    tokio::pin!(operation);
    tokio::select! {
        result = &mut operation => result,
        _ = &mut conn => {
            // Even a failed driver may have queued valid response headers
            // before a truncated body. Let the request path distinguish a
            // pre-header failure from Go's status-preserving body-copy error.
            operation.await
        }
    }
}

#[cfg(test)]
#[path = "push_transport_tests.rs"]
mod tests;
