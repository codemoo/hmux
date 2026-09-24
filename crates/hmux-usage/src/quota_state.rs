//! Pure OAuth quota cache decisions for one provider. Home owns credentials,
//! HTTP, activity and publication. This holds at most a live and last-good
//! bounded snapshot; it creates no worker, timer, or credential write.
use crate::{model::format_time, transport, Provider, Snapshot};
use chrono::{DateTime, Duration, Utc};
use std::fmt;

const CACHE_TTL: Duration = Duration::seconds(60);
const STICKY_TTL: Duration = Duration::seconds(600);
const FALLBACK_BACKOFF: Duration = Duration::seconds(300);
const MAX_BACKOFF: Duration = Duration::hours(24);

/// A digest of Go's account identity precedence (account ID, email, then
/// access-token tail). Home must compute this before calling `begin` and
/// discard the raw identity. No secret or digest bytes appear in Debug.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AccountKey([u8; 32]);
impl AccountKey {
    pub const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }
}
impl fmt::Debug for AccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccountKey([redacted])")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FetchTicket {
    id: u64,
    account: Option<AccountKey>,
    started_at: DateTime<Utc>,
    seq: i64,
}
impl fmt::Debug for FetchTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetchTicket")
            .field("id", &self.id)
            .field("account", &self.account)
            .field("started_at", &self.started_at)
            .field("seq", &self.seq)
            .finish()
    }
}
impl FetchTicket {
    pub fn sequence(self) -> i64 {
        self.seq
    }
}

/// `InFlight` means the caller should coalesce/wait for the existing fetch.
/// All cached snapshots contain quota only; Home adds current activity.
pub enum Begin {
    Fetch(FetchTicket),
    Cached(Box<Snapshot>),
    InFlight,
}

/// Typed, redacted result of credential loading or upstream fetch/recovery.
/// `Unauthorized` is a confirmed upstream rejection after read-only disk
/// reload was unable to recover. It is distinct from absent credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    CredentialMissing,
    CredentialMalformed,
    CredentialIo,
    Unauthorized,
    RateLimited { retry_after: Option<Duration> },
    Network,
    Server,
    Contract,
}

pub enum Finish {
    Applied(Snapshot),
    /// A purported success failed the OAuth quota boundary; the contained
    /// public snapshot is a contract-error degradation, never the raw input.
    RejectedSnapshot(Snapshot),
    /// The ticket was superseded by an account switch or a newer request.
    Discarded,
}

/// All fields are privacy-safe categories and timestamps; no account key is
/// exposed. Activity observation is deliberately outside this state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostics {
    pub observed: bool,
    pub state: Option<&'static str>,
    pub last_quota_attempt_at: Option<String>,
    pub last_quota_success_at: Option<String>,
    pub last_quota_error_at: Option<String>,
    pub last_quota_error_kind: Option<&'static str>,
}

pub struct QuotaState {
    provider: Provider,
    account: Option<AccountKey>,
    seq: i64,
    ticket_id: u64,
    flight: Option<FetchTicket>,
    latest: Option<Snapshot>,
    last_fetch_at: Option<DateTime<Utc>>,
    last_good: Option<(Snapshot, DateTime<Utc>)>,
    suspended_until: Option<DateTime<Utc>>,
    last_attempt: Option<DateTime<Utc>>,
    last_success: Option<DateTime<Utc>>,
    last_error: Option<DateTime<Utc>>,
    last_error_kind: Option<&'static str>,
    last_state: Option<&'static str>,
}

impl QuotaState {
    pub fn new(provider: Provider) -> Self {
        Self {
            provider,
            account: None,
            seq: 0,
            ticket_id: 0,
            flight: None,
            latest: None,
            last_fetch_at: None,
            last_good: None,
            suspended_until: None,
            last_attempt: None,
            last_success: None,
            last_error: None,
            last_error_kind: None,
            last_state: None,
        }
    }

    /// Decide whether Home should fetch. A key change immediately invalidates
    /// all quota, suspension and in-flight state from the prior account.
    /// Home must finish every issued ticket, including I/O error and canceled
    /// requests, with a typed failure; otherwise later begins remain InFlight.
    pub fn begin(&mut self, key: Option<AccountKey>, now: DateTime<Utc>) -> Begin {
        if self.account != key {
            self.account = key;
            self.flight = None;
            self.latest = None;
            self.last_fetch_at = None;
            self.last_good = None;
            self.suspended_until = None;
            self.last_state = None;
        }
        if self.flight.is_some() {
            return Begin::InFlight;
        }
        let suspended = key.is_some() && self.suspended_until.is_some_and(|until| now < until);
        let expired_suspension = self.suspended_until.is_some() && !suspended;
        if expired_suspension {
            self.suspended_until = None;
        }
        let cache_valid = key.is_some()
            && !expired_suspension
            && self
                .last_fetch_at
                .is_some_and(|at| now.signed_duration_since(at) < CACHE_TTL);
        if cache_valid {
            if let Some(snap) = self.latest.clone() {
                return Begin::Cached(Box::new(self.emit(snap, now)));
            }
        }
        if suspended {
            let retry_at = self.suspended_until.and_then(|until| retry_at(until, now));
            let snap = if let Some((good, at)) = &self.last_good {
                if now.signed_duration_since(*at) < STICKY_TTL {
                    let mut good = good.clone();
                    good.status.stale = true;
                    good.status.retry_at = retry_at;
                    good
                } else {
                    self.degraded("rateLimited", now, retry_at)
                }
            } else {
                self.degraded("rateLimited", now, retry_at)
            };
            return Begin::Cached(Box::new(self.emit(snap, now)));
        }
        self.seq = self.seq.saturating_add(1);
        self.ticket_id = self.ticket_id.wrapping_add(1);
        let ticket = FetchTicket {
            id: self.ticket_id,
            account: key,
            started_at: now,
            seq: self.seq,
        };
        self.flight = Some(ticket);
        self.last_fetch_at = Some(now);
        self.last_attempt = Some(now);
        Begin::Fetch(ticket)
    }

    /// Finish only the current account's active ticket. `now` is the logical
    /// refresh time; pass the same clock instant used by `begin` for exact Go
    /// timing parity. Delayed completions cannot replace a newer account.
    pub fn finish(
        &mut self,
        ticket: FetchTicket,
        result: Result<Snapshot, Failure>,
        now: DateTime<Utc>,
    ) -> Finish {
        if self.flight != Some(ticket) || self.account != ticket.account {
            return Finish::Discarded;
        }
        self.flight = None;
        match result {
            Ok(mut raw) => {
                if !self.valid_success(&raw) {
                    return Finish::RejectedSnapshot(self.failure(Failure::Contract, now));
                }
                if raw.status.quota_observed_at.is_none() {
                    raw.status.quota_observed_at = Some(raw.generated_at_utc.clone());
                }
                raw.seq = ticket.seq;
                raw.generated_at_utc = format_time(now);
                raw.status.retry_at = None;
                self.latest = Some(raw.clone());
                self.last_good = Some((raw.clone(), now));
                self.suspended_until = None;
                self.last_success = Some(now);
                self.last_state = Some("ok");
                Finish::Applied(raw)
            }
            Err(failure) => Finish::Applied(self.failure(failure, now)),
        }
    }

    pub fn diagnostics(&self) -> Diagnostics {
        Diagnostics {
            observed: self.latest.is_some(),
            state: self.last_state,
            last_quota_attempt_at: self.last_attempt.map(format_time),
            last_quota_success_at: self.last_success.map(format_time),
            last_quota_error_at: self.last_error.map(format_time),
            last_quota_error_kind: self.last_error_kind,
        }
    }

    fn valid_success(&self, raw: &Snapshot) -> bool {
        raw.provider == self.provider
            && raw.status.state == "ok"
            && !raw.status.stale
            && raw.status.quota_source == "oauth_api"
            && raw.status.data_source == "api_only"
            && raw.status.retry_at.is_none()
            && raw.accounts.is_empty()
            && raw.accounts_updated_at.is_none()
            && raw.sources.is_empty()
            && raw.burn_rate_per_min == 0.0
            && raw.burn_state == "idle"
            && raw.today_total_tokens == 0
            && raw.today_sessions == 0
            && transport::validate(raw).is_ok()
    }

    fn emit(&mut self, mut snap: Snapshot, now: DateTime<Utc>) -> Snapshot {
        self.seq = self.seq.saturating_add(1);
        snap.seq = self.seq;
        snap.generated_at_utc = format_time(now);
        self.latest = Some(snap.clone());
        snap
    }

    fn degraded(
        &mut self,
        state: &'static str,
        now: DateTime<Utc>,
        retry: Option<String>,
    ) -> Snapshot {
        let mut snap = Snapshot::degraded(self.provider, self.seq, now, state);
        snap.status.retry_at = retry;
        self.last_state = Some(state);
        snap
    }

    fn failure(&mut self, failure: Failure, now: DateTime<Utc>) -> Snapshot {
        let (state, kind, transient) = match failure {
            Failure::CredentialMissing => (auth_state(self.provider), "credential_missing", false),
            Failure::CredentialMalformed => ("quotaEndpointChanged", "credential_contract", false),
            Failure::CredentialIo => ("networkError", "credential_io", true),
            Failure::Unauthorized => (auth_state(self.provider), "auth_rejected", false),
            Failure::RateLimited { .. } => ("rateLimited", "rate_limited", true),
            Failure::Network => ("networkError", "network", true),
            Failure::Server => ("networkError", "upstream_server", true),
            Failure::Contract => ("quotaEndpointChanged", "upstream_contract", false),
        };
        self.last_error = Some(now);
        self.last_error_kind = Some(kind);
        let retry = if let Failure::RateLimited { retry_after } = failure {
            let delay = retry_after
                .filter(|d| *d > Duration::zero())
                .map(|d| d.min(MAX_BACKOFF))
                .unwrap_or(FALLBACK_BACKOFF);
            let until = now + delay;
            self.suspended_until = self.account.map(|_| until);
            retry_at(until, now)
        } else {
            None
        };
        let mut snap = if transient && self.account.is_some() {
            if let Some((good, at)) = &self.last_good {
                if now.signed_duration_since(*at) < STICKY_TTL {
                    let mut good = good.clone();
                    good.status.stale = true;
                    good.status.retry_at = retry;
                    self.last_state = Some("ok");
                    good
                } else {
                    self.degraded(state, now, retry)
                }
            } else {
                self.degraded(state, now, retry)
            }
        } else {
            self.degraded(state, now, retry)
        };
        snap.seq = self.seq;
        snap.generated_at_utc = format_time(now);
        self.latest = Some(snap.clone());
        snap
    }
}

fn auth_state(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "authExpired",
        Provider::Codex => "codexLoggedOut",
    }
}

/// Omit a positive deadline if fixed millisecond wire precision collapses it
/// to the generated timestamp, matching Go's `retryAtPointer`.
pub fn retry_at(deadline: DateTime<Utc>, generated_at: DateTime<Utc>) -> Option<String> {
    if deadline <= generated_at {
        return None;
    }
    let deadline = format_time(deadline);
    (deadline > format_time(generated_at)).then_some(deadline)
}

#[cfg(test)]
#[path = "quota_state_tests.rs"]
mod tests;
