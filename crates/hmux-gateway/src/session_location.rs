//! Optional login display metadata. No location result participates in auth.
//! Two in-flight addresses and 256 cached labels; no resident task or idle pool.
use crate::{
    auth_store::SessionInfo,
    push_transport::{Client, Error},
};
use futures_util::future::BoxFuture;
use serde::Deserialize;
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::watch,
    time::{timeout, Instant},
};
use tokio_util::sync::CancellationToken;

const LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);
const INTERNAL: &str = "내부·예약 네트워크";
type Fetch = dyn Fn(IpAddr) -> BoxFuture<'static, Result<Vec<u8>, Error>> + Send + Sync;
struct Entry {
    label: String,
    expires: Instant,
}
#[derive(Default)]
struct State {
    cache: HashMap<IpAddr, Entry>,
    pending: HashMap<IpAddr, watch::Receiver<()>>,
}
struct Inner {
    state: Mutex<State>,
    fetch: Arc<Fetch>,
    stopped: CancellationToken,
}
#[derive(Clone)]
pub struct Locator(Arc<Inner>);

// Owns completion even if an HTTP request is dropped during DNS/TLS/read.
// Cancelled work removes admission and wakes waiters without negative caching.
struct Pending {
    inner: Arc<Inner>,
    address: IpAddr,
    _notify: watch::Sender<()>,
    result: Option<String>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(label) = self.result.take() {
            if state.cache.len() >= 256 {
                if let Some(oldest) = state
                    .cache
                    .iter()
                    .min_by_key(|(_, e)| e.expires)
                    .map(|(ip, _)| *ip)
                {
                    state.cache.remove(&oldest);
                }
            }
            let hours = if label.is_empty() { 1 } else { 24 };
            state.cache.insert(
                self.address,
                Entry {
                    label,
                    expires: Instant::now() + Duration::from_secs(hours * 3600),
                },
            );
        }
        state.pending.remove(&self.address);
    }
}
impl Locator {
    pub fn new(client: Client) -> Self {
        Self::with_fetch(Arc::new(move |ip| {
            let client = client.clone();
            Box::pin(async move { client.location(ip).await })
        }))
    }
    fn with_fetch(fetch: Arc<Fetch>) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            fetch,
            stopped: CancellationToken::new(),
        }))
    }
    pub fn shutdown(&self) {
        self.0.stopped.cancel();
    }

    pub async fn enrich(&self, sessions: &mut [SessionInfo]) {
        // Go labels local addresses even after the shared lookup deadline.
        // Classify these first so stalled public lookups cannot hide metadata
        // that needs no external request or cache admission.
        for session in sessions.iter_mut() {
            if session.ip.parse().is_ok_and(|ip| !public_ip(ip)) {
                session.location = INTERNAL.into();
            }
        }
        let deadline = Instant::now() + LOOKUP_TIMEOUT;
        let (first, second) = sessions.split_at_mut(sessions.len().div_ceil(2));
        let work = async {
            tokio::join!(
                self.enrich_slice(first, deadline),
                self.enrich_slice(second, deadline)
            );
        };
        let _ = timeout(LOOKUP_TIMEOUT, work).await;
    }
    async fn enrich_slice(&self, sessions: &mut [SessionInfo], deadline: Instant) {
        for session in sessions {
            if Instant::now() >= deadline {
                break;
            }
            session.location = self.lookup(&session.ip).await;
        }
    }
    async fn lookup(&self, value: &str) -> String {
        let Ok(address) = value.parse::<IpAddr>() else {
            return String::new();
        };
        let address = address.to_canonical();
        if !public_ip(address) {
            return INTERNAL.into();
        }
        tokio::select! {
            biased;
            _ = self.0.stopped.cancelled() => String::new(),
            result = timeout(LOOKUP_TIMEOUT, self.lookup_inner(address)) => result.unwrap_or_default(),
        }
    }
    async fn lookup_inner(&self, address: IpAddr) -> String {
        loop {
            let (wait, owner) = {
                let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(entry) = state
                    .cache
                    .get(&address)
                    .filter(|e| Instant::now() < e.expires)
                {
                    return entry.label.clone();
                }
                if let Some(pending) = state.pending.get(&address) {
                    (Some(pending.clone()), None)
                } else {
                    if state.pending.len() >= 2 {
                        return String::new();
                    }
                    let (notify, wait) = watch::channel(());
                    state.pending.insert(address, wait);
                    (
                        None,
                        Some(Pending {
                            inner: self.0.clone(),
                            address,
                            _notify: notify,
                            result: None,
                        }),
                    )
                }
            };
            if let Some(mut wait) = wait {
                let _ = wait.changed().await;
                continue;
            }
            let mut owner = owner.expect("lookup owns completion or waits");
            let result = (self.0.fetch)(address).await;
            let label = result
                .as_ref()
                .ok()
                .map(|raw| label(raw))
                .unwrap_or_default();
            if !matches!(result, Err(Error::Cancelled | Error::Timeout | Error::Busy)) {
                owner.result = Some(label.clone());
            }
            return label;
        }
    }
}

fn label(raw: &[u8]) -> String {
    #[derive(Deserialize)]
    struct Response {
        #[serde(default)]
        success: Option<bool>,
        #[serde(default)]
        city: Option<String>,
        #[serde(default)]
        region: Option<String>,
        #[serde(default)]
        country: Option<String>,
    }
    if raw.len() > 8192 {
        return String::new();
    }
    let Ok(response) = serde_json::from_slice::<Response>(raw) else {
        return String::new();
    };
    if response.success != Some(true) {
        return String::new();
    }
    let mut parts = Vec::new();
    for part in [&response.city, &response.region, &response.country] {
        let part = part.as_deref().unwrap_or_default().trim();
        if part.is_empty() || part.len() > 160 || part.contains(['\r', '\n', '\0']) {
            continue;
        }
        if parts.last() != Some(&part) {
            parts.push(part);
        }
    }
    parts.join(", ")
}

/// Go session_location.go policy (distinct from the stricter push destination
/// policy). IPv4-mapped input is canonicalized before classification and lookup.
pub(crate) fn public_ip(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v) => ![
            (0x00000000, 8),
            (0x0a000000, 8),
            (0x64400000, 10),
            (0x7f000000, 8),
            (0xa9fe0000, 16),
            (0xac100000, 12),
            (0xc0000000, 24),
            (0xc0000200, 24),
            (0xc0586300, 24),
            (0xc0a80000, 16),
            (0xc6120000, 15),
            (0xc6336400, 24),
            (0xcb007100, 24),
            (0xe0000000, 4),
            (0xf0000000, 4),
        ]
        .into_iter()
        .any(|(base, bits)| u32::from(v) >> (32 - bits) == base >> (32 - bits)),
        IpAddr::V6(v) => ![
            (0, 96),
            (0x0064ff9b000000000000000000000000, 96),
            (0x0064ff9b000100000000000000000000, 48),
            (0x01000000000000000000000000000000, 64),
            (0x20010000000000000000000000000000, 23),
            (0x20010db8000000000000000000000000, 32),
            (0x20020000000000000000000000000000, 16),
            (0x3fff0000000000000000000000000000, 20),
            (0x5f000000000000000000000000000000, 16),
            (0xfc000000000000000000000000000000, 7),
            (0xfe800000000000000000000000000000, 10),
            (0xff000000000000000000000000000000, 8),
        ]
        .into_iter()
        .any(|(base, bits)| u128::from(v) >> (128 - bits) == base >> (128 - bits)),
    }
}

#[cfg(test)]
#[path = "session_location_tests.rs"]
mod tests;
