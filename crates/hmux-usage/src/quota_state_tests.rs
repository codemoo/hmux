use super::*;
use crate::model::parse_time;
use serde_json::{json, Value};

fn at() -> DateTime<Utc> {
    parse_time("2026-09-08T04:05:06.123456789Z").unwrap()
}
fn key(n: u8) -> AccountKey {
    AccountKey::from_digest([n; 32])
}
fn fetch(state: &mut QuotaState, key: Option<AccountKey>, now: DateTime<Utc>) -> FetchTicket {
    match state.begin(key, now) {
        Begin::Fetch(t) => t,
        _ => panic!("expected fetch"),
    }
}
fn applied(result: Finish) -> Snapshot {
    match result {
        Finish::Applied(s) => s,
        _ => panic!("expected applied"),
    }
}
fn good(provider: Provider, now: DateTime<Utc>) -> Snapshot {
    let mut s = Snapshot::degraded(provider, 0, now, "ok");
    s.status.stale = false;
    s.status.quota_source = "oauth_api".into();
    s.status.quota_observed_at = Some(format_time(now));
    s.rolling_5h.used_pct = 0.25;
    s.rolling_5h_observed = true;
    s
}
fn visible(s: &Snapshot) -> Value {
    json!({"state": s.status.state, "stale": s.status.stale,
        "retry_at": s.status.retry_at, "quota_observed_at": s.status.quota_observed_at})
}

#[test]
fn go_oracle_states_sticky_and_retry() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/usage-state-v1/go.json"
    ))
    .unwrap();
    for provider in [Provider::Claude, Provider::Codex] {
        let prefix = provider.as_str();
        for (name, failure) in [
            ("missing", Failure::CredentialMissing),
            ("malformed", Failure::CredentialMalformed),
            ("unauthorized", Failure::Unauthorized),
        ] {
            let mut state = QuotaState::new(provider);
            let ticket = fetch(&mut state, Some(key(1)), at());
            let snap = applied(state.finish(ticket, Err(failure), at()));
            assert_eq!(
                snap.status.state,
                oracle[format!("{prefix}_{name}")].as_str().unwrap()
            );
        }
    }
    let mut sticky = QuotaState::new(Provider::Claude);
    let ticket = fetch(&mut sticky, Some(key(1)), at());
    applied(sticky.finish(ticket, Ok(good(Provider::Claude, at())), at()));
    let later = at() + Duration::seconds(61);
    let ticket = fetch(&mut sticky, Some(key(1)), later);
    let snap = applied(sticky.finish(ticket, Err(Failure::Network), later));
    assert_eq!(visible(&snap), oracle["sticky_network"]);

    let mut expired = QuotaState::new(Provider::Claude);
    let ticket = fetch(&mut expired, Some(key(1)), at());
    applied(expired.finish(ticket, Ok(good(Provider::Claude, at())), at()));
    let later = at() + Duration::seconds(600);
    let ticket = fetch(&mut expired, Some(key(1)), later);
    let snap = applied(expired.finish(ticket, Err(Failure::Network), later));
    assert_eq!(visible(&snap), oracle["expired_network"]);

    let mut limited = QuotaState::new(Provider::Claude);
    let ticket = fetch(&mut limited, Some(key(1)), at());
    let snap = applied(limited.finish(
        ticket,
        Err(Failure::RateLimited {
            retry_after: Some(Duration::seconds(2307)),
        }),
        at(),
    ));
    assert_eq!(visible(&snap), oracle["rate_limited"]);
    assert_eq!(
        retry_at(at() + Duration::microseconds(100), at()),
        oracle["collapsed_retry"].as_str().map(str::to_owned)
    );
    assert_eq!(
        retry_at(at() + Duration::milliseconds(1), at()),
        oracle["visible_retry"].as_str().map(str::to_owned)
    );
    assert_eq!(oracle["delay_seconds"], json!([2307, 300, 86400]));
}

#[test]
fn cache_sticky_suspension_and_expiry() {
    let mut state = QuotaState::new(Provider::Claude);
    let ticket = fetch(&mut state, Some(key(1)), at());
    applied(state.finish(ticket, Ok(good(Provider::Claude, at())), at()));
    match state.begin(Some(key(1)), at() + Duration::seconds(59)) {
        Begin::Cached(s) => {
            assert_eq!(s.status.state, "ok");
            assert!(!s.status.stale);
            assert_eq!(s.seq, 2);
        }
        _ => panic!("expected 60s cache"),
    }
    let later = at() + Duration::seconds(60);
    let ticket = fetch(&mut state, Some(key(1)), later);
    let snap = applied(state.finish(
        ticket,
        Err(Failure::RateLimited {
            retry_after: Some(Duration::seconds(2307)),
        }),
        later,
    ));
    assert!(snap.status.stale);
    let retry = snap.status.retry_at.clone();
    match state.begin(Some(key(1)), later + Duration::seconds(61)) {
        Begin::Cached(s) => {
            assert!(s.status.stale);
            assert_eq!(s.status.retry_at, retry);
        }
        _ => panic!("expected suspension"),
    }
    match state.begin(Some(key(1)), at() + Duration::seconds(601)) {
        Begin::Cached(s) => {
            assert_eq!(s.status.state, "rateLimited");
            assert!(s.status.stale);
            assert_eq!(s.status.retry_at, retry);
        }
        _ => panic!("expected expired sticky under suspension"),
    }
    let resumed = fetch(&mut state, Some(key(1)), at() + Duration::seconds(2400));
    let snap = applied(state.finish(
        resumed,
        Err(Failure::Network),
        at() + Duration::seconds(2400),
    ));
    assert_eq!(snap.status.state, "networkError");
    assert!(snap.status.retry_at.is_none());
}

#[test]
fn account_rotation_and_delayed_completion_are_isolated() {
    let mut state = QuotaState::new(Provider::Codex);
    let old = fetch(&mut state, Some(key(1)), at());
    assert!(matches!(state.begin(Some(key(1)), at()), Begin::InFlight));
    let new = fetch(&mut state, Some(key(2)), at() + Duration::seconds(1));
    assert!(matches!(
        state.finish(old, Ok(good(Provider::Codex, at())), at()),
        Finish::Discarded
    ));
    let fresh = applied(state.finish(
        new,
        Ok(good(Provider::Codex, at())),
        at() + Duration::seconds(1),
    ));
    assert_eq!(fresh.seq, new.sequence());
    assert_eq!(fresh.status.state, "ok");
    assert!(matches!(
        state.begin(Some(key(1)), at() + Duration::seconds(2)),
        Begin::Fetch(_)
    ));
    assert!(!format!("{old:?}").contains(&"01".repeat(32)));
    assert_eq!(format!("{:?}", key(1)), "AccountKey([redacted])");
}

#[test]
fn wrong_source_or_nested_snapshot_is_rejected_without_caching_it() {
    let mut state = QuotaState::new(Provider::Codex);
    let ticket = fetch(&mut state, Some(key(1)), at());
    let mut bad = good(Provider::Claude, at());
    let snap = match state.finish(ticket, Ok(bad.clone()), at()) {
        Finish::RejectedSnapshot(s) => s,
        _ => panic!("expected rejection"),
    };
    assert_eq!(snap.status.state, "quotaEndpointChanged");
    assert_eq!(
        state.diagnostics().last_quota_error_kind,
        Some("upstream_contract")
    );
    let ticket = fetch(&mut state, Some(key(1)), at() + Duration::seconds(61));
    bad.provider = Provider::Codex;
    bad.sources
        .insert("cli".into(), good(Provider::Codex, at()));
    assert!(matches!(
        state.finish(ticket, Ok(bad), at()),
        Finish::RejectedSnapshot(_)
    ));
}

#[test]
fn missing_credentials_have_no_sticky_and_rate_limit_is_bounded() {
    let mut state = QuotaState::new(Provider::Claude);
    let ticket = fetch(&mut state, Some(key(1)), at());
    applied(state.finish(ticket, Ok(good(Provider::Claude, at())), at()));
    let later = at() + Duration::seconds(61);
    let ticket = fetch(&mut state, Some(key(1)), later);
    let snap = applied(state.finish(ticket, Err(Failure::CredentialMissing), later));
    assert_eq!(snap.status.state, "authExpired");
    assert!(snap.status.quota_observed_at.is_none());
    let mut empty = QuotaState::new(Provider::Codex);
    let ticket = fetch(&mut empty, None, at());
    let snap = applied(empty.finish(ticket, Err(Failure::CredentialMalformed), at()));
    assert_eq!(snap.status.state, "quotaEndpointChanged");
    assert!(matches!(
        empty.begin(None, at() + Duration::seconds(1)),
        Begin::Fetch(_)
    ));
    let mut rate = QuotaState::new(Provider::Claude);
    let ticket = fetch(&mut rate, Some(key(1)), at());
    let snap = applied(rate.finish(
        ticket,
        Err(Failure::RateLimited {
            retry_after: Some(Duration::hours(48)),
        }),
        at(),
    ));
    assert_eq!(
        snap.status.retry_at,
        Some(format_time(at() + Duration::hours(24)))
    );
    let ticket = fetch(&mut rate, Some(key(2)), at() + Duration::seconds(1));
    let snap = applied(rate.finish(ticket, Err(Failure::Network), at() + Duration::seconds(1)));
    assert!(snap.status.retry_at.is_none());
}
