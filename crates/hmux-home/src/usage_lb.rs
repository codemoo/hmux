//! Bounded codex-lb source, preserving aggregate selection and sticky cache.
use crate::{
    upgrade,
    usage_http::{self, Client},
};
use bytes::Bytes;
use chrono::{DateTime, Duration as Span, Utc};
use hmux_usage::{
    codex_lb, codex_pool, model::format_time, quota_state::Failure, Provider, Snapshot,
};
use http::{header, Request, Uri};
use http_body_util::Empty;
use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Endpoint {
    authority: String,
    path: String,
    fleet_path: String,
    loopback: Option<SocketAddr>,
}
impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LBEndpoint([redacted])")
    }
}
impl Endpoint {
    pub fn parse(raw: &str) -> Result<Self, Failure> {
        let raw = raw.trim();
        let raw = if raw.is_empty() {
            "http://127.0.0.1:2455"
        } else {
            raw
        };
        if raw.len() > 4096 {
            return Err(Failure::Contract);
        }
        // Go configuration normalizes away query/fragment and a trailing /v1.
        let raw = raw.split(['?', '#']).next().ok_or(Failure::Contract)?;
        let uri: Uri = raw.parse().map_err(|_| Failure::Contract)?;
        let authority = uri.authority().ok_or(Failure::Contract)?;
        if !upgrade::valid_authority(authority.as_str()) {
            return Err(Failure::Contract);
        }
        let scheme = uri.scheme_str().ok_or(Failure::Contract)?;
        let loopback = match scheme {
            "https" => None,
            "http" => {
                let host = authority
                    .host()
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                let ip = match host.as_str() {
                    "localhost" | "127.0.0.1" => IpAddr::V4(Ipv4Addr::LOCALHOST),
                    "::1" => IpAddr::V6(Ipv6Addr::LOCALHOST),
                    _ => return Err(Failure::Contract),
                };
                Some(SocketAddr::new(ip, authority.port_u16().unwrap_or(80)))
            }
            _ => return Err(Failure::Contract),
        };
        let base_path = uri.path().trim_end_matches('/');
        let path = base_path.strip_suffix("/v1").unwrap_or(base_path);
        Ok(Self {
            authority: authority.to_string(),
            path: format!("{path}/v1/usage"),
            fleet_path: format!("{path}/api/fleet/summary"),
            loopback,
        })
    }
}

pub struct Owner {
    endpoint: Endpoint,
    key: String,
    client: Client,
    last_attempt: Option<DateTime<Utc>>,
    next_attempt: Option<DateTime<Utc>>,
    good: Option<(Snapshot, DateTime<Utc>)>,
    seq: i64,
    fleet_supported: Option<bool>,
}
impl Owner {
    pub fn new(endpoint: Endpoint, key: String, client: Client) -> Result<Self, Failure> {
        usage_http::secret_header(&key, 4096)?;
        Ok(Self {
            endpoint,
            key,
            client,
            last_attempt: None,
            next_attempt: None,
            good: None,
            seq: 0,
            fleet_supported: None,
        })
    }
    pub async fn refresh(&mut self, now: DateTime<Utc>, cancel: &CancellationToken) -> Snapshot {
        self.seq = self.seq.saturating_add(1);
        if let Some((good, at)) = &self.good {
            if self.next_attempt.is_some_and(|next| now < next) && now - *at < Span::minutes(10) {
                return reemit(good, self.seq, now, true);
            }
            if self.next_attempt.is_none()
                && self
                    .last_attempt
                    .is_some_and(|at| now - at < Span::seconds(60))
            {
                return reemit(good, self.seq, now, false);
            }
        }
        self.last_attempt = Some(now);
        // Mark backoff before awaiting so caller drop cannot create a busy loop.
        self.next_attempt = Some(now + Span::seconds(30));
        let result = if self.fleet_supported != Some(false) {
            match fetch_fleet(
                &self.client,
                &self.endpoint,
                &self.key,
                self.seq,
                now,
                cancel,
            )
            .await
            {
                Ok(Some(snap)) => {
                    self.fleet_supported = Some(true);
                    Ok(Some(snap))
                }
                Ok(None) if self.fleet_supported.is_none() => {
                    self.fleet_supported = Some(false);
                    fetch(
                        &self.client,
                        &self.endpoint,
                        &self.key,
                        self.seq,
                        now,
                        cancel,
                    )
                    .await
                }
                Ok(None) => Err(Failure::Contract),
                Err(error) => Err(error),
            }
        } else {
            fetch(
                &self.client,
                &self.endpoint,
                &self.key,
                self.seq,
                now,
                cancel,
            )
            .await
        };
        match result {
            Ok(Some(snap)) => {
                self.good = Some((snap.clone(), now));
                self.next_attempt = None;
                snap
            }
            // A validated response whose empty pool supersedes key limits has
            // no usable quota. Go degrades immediately for this narrow case.
            Ok(None) => unavailable(self.seq, now, "quotaEndpointChanged"),
            Err(error) => {
                if let Some((good, at)) = &self.good {
                    if now - *at < Span::minutes(10) {
                        return reemit(good, self.seq, now, true);
                    }
                }
                unavailable(
                    self.seq,
                    now,
                    if error == Failure::Contract {
                        "quotaEndpointChanged"
                    } else {
                        "networkError"
                    },
                )
            }
        }
    }
}
pub fn unavailable(seq: i64, now: DateTime<Utc>, state: &str) -> Snapshot {
    let mut snapshot = Snapshot::degraded(Provider::Codex, seq, now, state);
    snapshot.status.quota_source = "codex_lb".into();
    snapshot
}
fn reemit(snapshot: &Snapshot, seq: i64, now: DateTime<Utc>, stale: bool) -> Snapshot {
    let mut snap = snapshot.clone();
    snap.seq = seq;
    snap.generated_at_utc = format_time(now);
    snap.status.stale |= stale;
    snap
}
async fn fetch(
    client: &Client,
    endpoint: &Endpoint,
    key: &str,
    seq: i64,
    now: DateTime<Utc>,
    cancel: &CancellationToken,
) -> Result<Option<Snapshot>, Failure> {
    let body = fetch_body(client, endpoint, key, now, cancel, false)
        .await?
        .ok_or(Failure::Contract)?;
    match codex_lb::parse_usage(&body, seq, now) {
        Ok(mut snapshot) => {
            snapshot.status.quota_weighting = "upstream".into();
            Ok(Some(snapshot))
        }
        Err(_) if empty_pool_supersedes_valid_limits(&body, seq, now) => Ok(None),
        Err(_) => Err(Failure::Contract),
    }
}
async fn fetch_fleet(
    client: &Client,
    endpoint: &Endpoint,
    key: &str,
    seq: i64,
    now: DateTime<Utc>,
    cancel: &CancellationToken,
) -> Result<Option<Snapshot>, Failure> {
    fetch_body(client, endpoint, key, now, cancel, true)
        .await?
        .map(|body| codex_pool::parse_fleet(&body, seq, now).map_err(|_| Failure::Contract))
        .transpose()
}
async fn fetch_body(
    client: &Client,
    endpoint: &Endpoint,
    key: &str,
    now: DateTime<Utc>,
    cancel: &CancellationToken,
    fleet: bool,
) -> Result<Option<Vec<u8>>, Failure> {
    let mut auth = http::HeaderValue::from_bytes(
        &[b"Bearer ", usage_http::secret_header(key, 4096)?.as_bytes()].concat(),
    )
    .map_err(|_| Failure::CredentialMalformed)?;
    auth.set_sensitive(true);
    let request = Request::builder()
        .method("GET")
        .uri(if fleet {
            &endpoint.fleet_path
        } else {
            &endpoint.path
        })
        .header(header::HOST, &endpoint.authority)
        .header(header::AUTHORIZATION, auth)
        .header(header::ACCEPT, "application/json")
        .header(header::CONNECTION, "close")
        .body(Empty::<Bytes>::new())
        .map_err(|_| Failure::Contract)?;
    let work = async {
        let stream = match endpoint.loopback {
            Some(address) => client.dial.lb_loopback(address).await,
            None => client.dial.lb_tls(&endpoint.authority).await,
        }
        .map_err(|_| Failure::Network)?;
        usage_http::exchange_optional(stream, request, now, 1 << 20, !fleet, fleet).await
    };
    usage_http::bounded(work, cancel, Instant::now() + Duration::from_secs(5)).await
}

// This is the one response shape that passes Go fetch validation but fails
// buildSnapshot: a present, empty pool suppresses otherwise valid key limits.
// Re-running the pure parser without that pool confirms the underlying limits
// are valid, so malformed JSON and invalid quota still retain last-good.
fn empty_pool_supersedes_valid_limits(body: &[u8], seq: i64, now: DateTime<Utc>) -> bool {
    // Borrow opaque subtrees; a failed response must not allocate an arbitrary
    // JSON Value tree. The first parser already bounded body bytes and depth.
    #[derive(serde::Deserialize)]
    struct Probe<'a> {
        account_pool_usage: Option<EmptyPool>,
        #[serde(borrow)]
        upstream_limits: Option<&'a serde_json::value::RawValue>,
    }
    #[derive(serde::Deserialize)]
    struct EmptyPool {
        primary: Option<f64>,
        secondary: Option<f64>,
    }
    let Ok(Probe {
        account_pool_usage: Some(pool),
        upstream_limits: Some(limits),
    }) = serde_json::from_slice(body)
    else {
        return false;
    };
    if pool.primary.is_some() || pool.secondary.is_some() {
        return false;
    }
    let mut without_pool = Vec::with_capacity(limits.get().len() + 21);
    without_pool.extend_from_slice(b"{\"upstream_limits\":");
    without_pool.extend_from_slice(limits.get().as_bytes());
    without_pool.push(b'}');
    codex_lb::parse_usage(&without_pool, seq, now).is_ok()
}

#[cfg(test)]
#[path = "usage_lb_tests.rs"]
mod tests;
