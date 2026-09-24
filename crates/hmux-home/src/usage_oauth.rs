//! Single-owner OAuth refresh. One owner per provider belongs to the shared
//! Home usage collector, never to a browser/tab. All work is borrowed futures;
//! dropping a refresh completes its ticket with a redacted network failure.
use crate::{usage_credentials::Store, usage_http::Client};
use chrono::{DateTime, Utc};
use hmux_usage::{
    credentials::Credential,
    quota_state::{AccountKey, Begin, Failure, FetchTicket, Finish, QuotaState},
    Provider, Snapshot,
};
use std::{future::Future, sync::Arc};
use tokio::time::{timeout_at, Instant};
use tokio_util::sync::CancellationToken;

pub struct Owner {
    provider: Provider,
    state: QuotaState,
    store: Store,
    client: Client,
    force_credentials: bool,
}
impl Owner {
    pub fn new(provider: Provider, store: Store, client: Client) -> Self {
        Self {
            provider,
            state: QuotaState::new(provider),
            store,
            client,
            force_credentials: false,
        }
    }
    pub fn invalidate(&mut self) {
        self.state = QuotaState::new(self.provider);
        self.force_credentials = true;
    }
    pub fn diagnostics(&self) -> hmux_usage::quota_state::Diagnostics {
        self.state.diagnostics()
    }
    pub async fn refresh(&mut self, now: DateTime<Utc>, cancel: &CancellationToken) -> Snapshot {
        let store = &self.store;
        let client = &self.client;
        let provider = self.provider;
        let force_credentials = std::mem::take(&mut self.force_credentials);
        refresh_with(
            &mut self.state,
            provider,
            now,
            cancel,
            |force| store.load(provider, force || force_credentials, cancel),
            |credential: Arc<Credential>, seq| async move {
                client
                    .fetch(
                        provider,
                        credential.access_token(),
                        credential.account_id(),
                        seq,
                        now,
                        cancel,
                    )
                    .await
            },
        )
        .await
    }
}

struct Lease<'a> {
    state: &'a mut QuotaState,
    ticket: Option<FetchTicket>,
    now: DateTime<Utc>,
    provider: Provider,
}
impl Lease<'_> {
    fn finish(mut self, result: Result<Snapshot, Failure>) -> Snapshot {
        let ticket = self.ticket.take().expect("active refresh ticket");
        match self.state.finish(ticket, result, self.now) {
            Finish::Applied(snapshot) | Finish::RejectedSnapshot(snapshot) => snapshot,
            Finish::Discarded => {
                Snapshot::degraded(self.provider, ticket.sequence(), self.now, "networkError")
            }
        }
    }
    fn sequence(&self) -> i64 {
        self.ticket.expect("active refresh ticket").sequence()
    }
    fn switch_account(&mut self, key: AccountKey) {
        // Caller invokes only for a changed account, which invalidates the old
        // flight and sticky cache before any second provider request.
        if let Begin::Fetch(ticket) = self.state.begin(Some(key), self.now) {
            self.ticket = Some(ticket);
        }
    }
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            let _ = self.state.finish(ticket, Err(Failure::Network), self.now);
        }
    }
}

async fn step<T>(
    work: impl Future<Output = Result<T, Failure>>,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<T, Failure> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Failure::Network),
        result = timeout_at(deadline, work) => result.map_err(|_| Failure::Network)?,
    }
}
async fn refresh_with<L, F, LF, FF>(
    state: &mut QuotaState,
    provider: Provider,
    now: DateTime<Utc>,
    cancel: &CancellationToken,
    mut load: L,
    mut fetch: F,
) -> Snapshot
where
    L: FnMut(bool) -> LF,
    LF: Future<Output = Result<Arc<Credential>, Failure>>,
    F: FnMut(Arc<Credential>, i64) -> FF,
    FF: Future<Output = Result<Snapshot, Failure>>,
{
    let deadline = Instant::now() + crate::usage_http::REQUEST_TIMEOUT;
    let credential = step(load(false), deadline, cancel).await;
    let key = credential.as_ref().ok().map(|value| value.account_key());
    let ticket = match state.begin(key, now) {
        Begin::Cached(snapshot) => return *snapshot,
        Begin::Fetch(ticket) => ticket,
        // A private &mut state plus the lease invariant makes this unreachable
        // for Owner; fail closed rather than expose raw or unrelated state.
        Begin::InFlight => return Snapshot::degraded(provider, 0, now, "networkError"),
    };
    let mut lease = Lease {
        state,
        ticket: Some(ticket),
        now,
        provider,
    };
    let credential = match credential {
        Ok(value) => value,
        Err(error) => return lease.finish(Err(error)),
    };
    let result = step(
        fetch(credential.clone(), lease.sequence()),
        deadline,
        cancel,
    )
    .await;
    if result != Err(Failure::Unauthorized) {
        return lease.finish(result);
    }
    // Read-only recovery: retry once when the externally changed token or
    // account header changes the effective authenticated request.
    // Reload errors/unchanged tokens preserve the confirmed auth rejection.
    let latest = step(load(true), deadline, cancel).await;
    if let Ok(latest) = latest {
        if latest.account_key() != credential.account_key() {
            lease.switch_account(latest.account_key());
        }
        if !latest.same_token(&credential) || latest.account_id() != credential.account_id() {
            let result = step(fetch(latest, lease.sequence()), deadline, cancel).await;
            return lease.finish(result);
        }
    }
    lease.finish(Err(Failure::Unauthorized))
}

#[cfg(test)]
#[path = "usage_oauth_tests.rs"]
mod tests;
