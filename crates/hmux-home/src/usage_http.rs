//! Read-only OAuth usage requests. Reuses Home's TLS trust/proxy stack, with
//! two process-wide usage socket slots, no redirect/retry, idle pool or task.
//! Deadline/cancellation drops the HTTP driver and socket together. Error
//! categories never contain provider responses, request headers or addresses.
use crate::dial;
use bytes::Bytes;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use hmux_usage::{oauth, quota_state::Failure, Provider, Snapshot};
use http::{header, HeaderValue, Request};
use http_body_util::{BodyExt, Empty};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::{timeout_at, Instant};
use tokio_util::sync::CancellationToken;

const MAX_BODY: usize = 64 * 1024;
const MAX_HEADERS: usize = 64;
const MAX_HEADER_BYTES: usize = 16 * 1024;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Clone)]
pub struct Client {
    pub(crate) dial: dial::Client,
}
impl Client {
    pub fn new(dial: dial::Client) -> Self {
        Self { dial }
    }

    /// Fixed provider endpoints only. Credentials stay on this host; redirects
    /// are contract errors, including redirects to another provider hostname.
    pub async fn fetch(
        &self,
        provider: Provider,
        access_token: &str,
        account_id: &str,
        seq: i64,
        now: DateTime<Utc>,
        cancel: &CancellationToken,
    ) -> Result<Snapshot, Failure> {
        let request = request(provider, access_token, account_id)?;
        let authority = endpoint(provider).0;
        let work = async {
            let stream = self
                .dial
                .usage_tls(authority)
                .await
                .map_err(|_| Failure::Network)?;
            let body = exchange_limited(stream, request, now, MAX_BODY, false).await?;
            oauth::normalize(provider, &body, seq, now).map_err(|_| Failure::Contract)
        };
        bounded(work, cancel, Instant::now() + REQUEST_TIMEOUT).await
    }
}

fn endpoint(provider: Provider) -> (&'static str, &'static str) {
    match provider {
        Provider::Claude => ("api.anthropic.com", "/api/oauth/usage"),
        Provider::Codex => ("chatgpt.com", "/backend-api/wham/usage"),
    }
}
pub(crate) fn secret_header(value: &str, cap: usize) -> Result<HeaderValue, Failure> {
    if value.is_empty() || value.len() > cap || !value.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(Failure::CredentialMalformed);
    }
    let mut value = HeaderValue::from_str(value).map_err(|_| Failure::CredentialMalformed)?;
    value.set_sensitive(true);
    Ok(value)
}
fn request(
    provider: Provider,
    token: &str,
    account: &str,
) -> Result<Request<Empty<Bytes>>, Failure> {
    let token = secret_header(token, 16 * 1024)?;
    let mut authorization = HeaderValue::from_bytes(&[b"Bearer ", token.as_bytes()].concat())
        .map_err(|_| Failure::CredentialMalformed)?;
    authorization.set_sensitive(true);
    let (authority, path) = endpoint(provider);
    let mut request = Request::builder()
        .method("GET")
        .uri(path)
        .header(header::HOST, authority)
        .header(header::AUTHORIZATION, authorization)
        .header(header::ACCEPT, "application/json")
        .header(header::CONNECTION, "close");
    match provider {
        Provider::Claude => request = request.header("anthropic-beta", "oauth-2025-04-20"),
        Provider::Codex if !account.is_empty() => {
            request = request.header("chatgpt-account-id", secret_header(account, 1024)?)
        }
        Provider::Codex => (),
    }
    request.body(Empty::new()).map_err(|_| Failure::Contract)
}

pub(crate) async fn bounded<T>(
    work: impl std::future::Future<Output = Result<T, Failure>>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<T, Failure> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Failure::Network),
        result = timeout_at(deadline, work) => result.map_err(|_| Failure::Network)?,
    }
}

pub(crate) async fn exchange_limited<S>(
    stream: S,
    request: Request<Empty<Bytes>>,
    now: DateTime<Utc>,
    limit: usize,
    lb: bool,
) -> Result<Vec<u8>, Failure>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut builder = http1::Builder::new();
    builder
        .max_headers(MAX_HEADERS)
        .max_buf_size(MAX_HEADER_BYTES);
    let (mut sender, connection) = builder
        .handshake(TokioIo::new(stream))
        .await
        .map_err(|_| Failure::Network)?;
    let work = async {
        let response = sender
            .send_request(request)
            .await
            .map_err(|_| Failure::Network)?;
        let status = response.status().as_u16();
        if lb && !(200..=299).contains(&status) {
            return Err(Failure::Network);
        }
        if matches!(status, 401 | 403) {
            return Err(Failure::Unauthorized);
        }
        let retry = response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| retry_after(v, now));
        // Reject compressed bodies instead of adding an unbounded decoder. No
        // Accept-Encoding is advertised, matching the bounded plain contract.
        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|v| v != "identity")
        {
            return Err(Failure::Contract);
        }
        let mut body = response.into_body();
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| Failure::Network)?;
            if let Some(data) = frame.data_ref() {
                if data.len() > limit - bytes.len() {
                    return Err(if lb {
                        Failure::Network
                    } else {
                        Failure::Contract
                    });
                }
                bytes.extend_from_slice(data);
            }
        }
        match status {
            200..=299 => Ok(bytes),
            429 => Err(Failure::RateLimited { retry_after: retry }),
            408 | 425 | 500..=599 => Err(Failure::Server),
            _ => Err(Failure::Contract),
        }
    };
    // No spawned driver. Once work completes the connection is dropped; even
    // a peer ignoring Connection: close cannot retain a task or admission slot.
    tokio::pin!(work);
    tokio::pin!(connection);
    tokio::select! {
        biased;
        result = &mut work => result,
        result = &mut connection => {
            result.map_err(|_| Failure::Network)?;
            work.await
        }
    }
}

fn retry_after(raw: &str, now: DateTime<Utc>) -> Option<ChronoDuration> {
    let raw = raw.trim();
    let delay = if let Ok(seconds) = raw.parse::<i64>() {
        if seconds < 0 {
            return None;
        }
        ChronoDuration::seconds(seconds.min(86400))
    } else {
        let at: DateTime<Utc> = httpdate::parse_http_date(raw).ok()?.into();
        at.signed_duration_since(now).max(ChronoDuration::zero())
    };
    Some(delay.min(ChronoDuration::hours(24)))
}

#[cfg(test)]
#[path = "usage_http_tests.rs"]
mod tests;

#[cfg(test)]
async fn exchange<S>(
    stream: S,
    request: Request<Empty<Bytes>>,
    now: DateTime<Utc>,
) -> Result<Vec<u8>, Failure>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    exchange_limited(stream, request, now, MAX_BODY, false).await
}
