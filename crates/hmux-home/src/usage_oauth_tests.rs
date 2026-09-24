use super::*;
use hmux_usage::{credentials, oauth};
use std::{
    cell::Cell,
    future::{pending, ready},
};
fn now() -> DateTime<Utc> {
    "2026-09-24T00:00:00Z".parse().unwrap()
}
fn credential(token: &str, account: &str) -> Arc<Credential> {
    Arc::new(
        credentials::parse(
            Provider::Codex,
            format!(r#"{{"tokens":{{"access_token":"{token}","account_id":"{account}"}}}}"#)
                .as_bytes(),
        )
        .unwrap(),
    )
}
fn success(seq: i64, time: DateTime<Utc>) -> Snapshot {
    oauth::normalize(
        Provider::Codex,
        br#"{"rate_limit":{"primary_window":{"used_percent":25}}}"#,
        seq,
        time,
    )
    .unwrap()
}

#[tokio::test]
async fn cache_sticky_account_switch_and_missing_credentials() {
    let mut state = QuotaState::new(Provider::Codex);
    let cancel = CancellationToken::new();
    let calls = Cell::new(0);
    let first = credential("token-a", "account-a");
    let snap = refresh_with(
        &mut state,
        Provider::Codex,
        now(),
        &cancel,
        |_| ready(Ok(first.clone())),
        |_, seq| {
            calls.set(calls.get() + 1);
            ready(Ok(success(seq, now())))
        },
    )
    .await;
    assert_eq!(snap.status.state, "ok");
    let cached = refresh_with(
        &mut state,
        Provider::Codex,
        now() + chrono::Duration::seconds(30),
        &cancel,
        |_| ready(Ok(first.clone())),
        |_, _| {
            calls.set(calls.get() + 1);
            ready(Err(Failure::Network))
        },
    )
    .await;
    assert_eq!(cached.status.state, "ok");
    assert!(!cached.status.stale);
    assert_eq!(calls.get(), 1);
    let stale = refresh_with(
        &mut state,
        Provider::Codex,
        now() + chrono::Duration::seconds(61),
        &cancel,
        |_| ready(Ok(first.clone())),
        |_, _| ready(Err(Failure::Network)),
    )
    .await;
    assert_eq!(stale.status.state, "ok");
    assert!(stale.status.stale);
    let other = credential("token-b", "account-b");
    let switched = refresh_with(
        &mut state,
        Provider::Codex,
        now() + chrono::Duration::seconds(62),
        &cancel,
        |_| ready(Ok(other.clone())),
        |_, _| ready(Err(Failure::Network)),
    )
    .await;
    assert_eq!(switched.status.state, "networkError");
    assert!(!switched.rolling_5h_observed);
    let missing = refresh_with(
        &mut state,
        Provider::Codex,
        now() + chrono::Duration::seconds(63),
        &cancel,
        |_| ready(Err(Failure::CredentialMissing)),
        |_, _| ready(Err(Failure::Network)),
    )
    .await;
    assert_eq!(missing.status.state, "codexLoggedOut");
}

#[tokio::test]
async fn rejected_token_reloads_only_once_and_binds_new_account() {
    for changed in [false, true] {
        let mut state = QuotaState::new(Provider::Codex);
        let old = credential("old-token", "old-account");
        let new = if changed {
            credential("new-token", "new-account")
        } else {
            old.clone()
        };
        let reads = Cell::new(0);
        let calls = Cell::new(0);
        let snap = refresh_with(
            &mut state,
            Provider::Codex,
            now(),
            &CancellationToken::new(),
            |force| {
                reads.set(reads.get() + 1);
                ready(Ok(if force { new.clone() } else { old.clone() }))
            },
            |cred, seq| {
                calls.set(calls.get() + 1);
                ready(if cred.same_token(&old) {
                    Err(Failure::Unauthorized)
                } else {
                    Ok(success(seq, now()))
                })
            },
        )
        .await;
        assert_eq!(reads.get(), 2);
        assert_eq!(calls.get(), if changed { 2 } else { 1 });
        assert_eq!(
            snap.status.state,
            if changed { "ok" } else { "codexLoggedOut" }
        );
        let next = state.begin(
            Some(new.account_key()),
            now() + chrono::Duration::seconds(1),
        );
        assert!(matches!(next, Begin::Cached(_)));
    }
}

#[tokio::test]
async fn dropped_refresh_finishes_ticket() {
    let mut state = QuotaState::new(Provider::Codex);
    let cred = credential("synthetic", "account");
    let cancel = CancellationToken::new();
    let called = Cell::new(false);
    {
        let mut refresh = Box::pin(refresh_with(
            &mut state,
            Provider::Codex,
            now(),
            &cancel,
            |_| ready(Ok(cred.clone())),
            |_, _| {
                called.set(true);
                pending()
            },
        ));
        tokio::select! { biased; _ = &mut refresh => panic!("pending HTTP finished"), _ = tokio::task::yield_now() => () }
        assert!(called.get());
    }
    assert_eq!(state.diagnostics().last_quota_error_kind, Some("network"));
    assert!(matches!(
        state.begin(
            Some(cred.account_key()),
            now() + chrono::Duration::seconds(61)
        ),
        Begin::Fetch(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn absolute_timeout_and_cancel_never_leave_inflight_state() {
    let mut state = QuotaState::new(Provider::Codex);
    let cred = credential("synthetic", "account");
    let result = refresh_with(
        &mut state,
        Provider::Codex,
        now(),
        &CancellationToken::new(),
        |_| ready(Ok(cred.clone())),
        |_, _| pending(),
    )
    .await;
    assert_eq!(result.status.state, "networkError");
    let cancel = CancellationToken::new();
    let result = refresh_with(
        &mut state,
        Provider::Codex,
        now() + chrono::Duration::seconds(61),
        &cancel,
        |_| ready(Ok(cred.clone())),
        |_, _| {
            cancel.cancel();
            pending()
        },
    )
    .await;
    assert_eq!(result.status.state, "networkError");
    assert!(matches!(
        state.begin(
            Some(cred.account_key()),
            now() + chrono::Duration::seconds(122)
        ),
        Begin::Fetch(_)
    ));
}

#[tokio::test]
async fn same_token_new_account_header_recovers_once() {
    let mut state = QuotaState::new(Provider::Codex);
    let old = credential("unchanged-token", "old-account");
    let new = credential("unchanged-token", "new-account");
    let calls = Cell::new(0);
    let snap = refresh_with(
        &mut state,
        Provider::Codex,
        now(),
        &CancellationToken::new(),
        |force| ready(Ok(if force { new.clone() } else { old.clone() })),
        |cred, seq| {
            calls.set(calls.get() + 1);
            ready(if cred.account_id() == "new-account" {
                Ok(success(seq, now()))
            } else {
                Err(Failure::Unauthorized)
            })
        },
    )
    .await;
    assert_eq!(calls.get(), 2);
    assert_eq!(snap.status.state, "ok");
    assert!(matches!(
        state.begin(
            Some(new.account_key()),
            now() + chrono::Duration::seconds(1)
        ),
        Begin::Cached(_)
    ));
}
