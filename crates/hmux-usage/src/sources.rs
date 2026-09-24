//! Source projection helpers. Callers merge activity while owning publication
//! state, after quota I/O completes, so stale worker copies cannot roll it back.
use crate::{cswap::Parsed, model::*, transport, Error};
use chrono::{DateTime, Utc};

pub fn copy_activity(target: &mut Snapshot, activity: &Snapshot) {
    target.burn_rate_per_min = activity.burn_rate_per_min;
    target.burn_state.clone_from(&activity.burn_state);
    target.today_total_tokens = activity.today_total_tokens;
    target.today_sessions = activity.today_sessions;
    if matches!(
        activity.status.data_source.as_str(),
        "api+jsonl" | "jsonl_only"
    ) {
        target.status.data_source = "api+jsonl".into();
    }
}

pub fn claude_swap(cli: &Snapshot, parsed: Parsed, now: DateTime<Utc>) -> Result<Snapshot, Error> {
    if cli.provider != Provider::Claude {
        return Err(Error::Invalid);
    }
    transport::validate(cli)?;
    let mut s = Snapshot::degraded(Provider::Claude, cli.seq, now, "networkError");
    s.status.quota_source = "claude_swap".into();
    s.accounts = parsed.accounts;
    s.accounts_updated_at = parsed.updated_at;
    copy_activity(&mut s, cli);
    // The command parser clears ambiguous active bindings; still fail closed
    // for a manually constructed Parsed value passed through this public API.
    if s.accounts.iter().filter(|a| a.active).count() > 1 {
        return Err(Error::Invalid);
    }
    if let Some(a) = s.accounts.iter().find(|a| a.active) {
        let window = |w: &AccountWindow| Window {
            used_pct: w.used_pct,
            remaining_seconds: remaining(w.resets_at.as_deref().and_then(parse_time), now),
            resets_at: w.resets_at.clone(),
        };
        if let Some(w) = &a.five_hour {
            s.rolling_5h = window(w);
            s.rolling_5h_observed = true;
        }
        if let Some(w) = &a.seven_day {
            s.weekly = window(w);
            s.weekly_observed = true;
        }
        s.status.quota_observed_at.clone_from(&a.last_refresh_at);
        s.status.stale = a.status != "ok";
        if matches!(a.status.as_str(), "ok" | "stale") {
            s.status.state = "ok".into();
        }
    }
    transport::validate(&s)?;
    Ok(s)
}

/// Build one provider's public view from fixed CLI and optional-source slots.
/// There is no recursive source map or unbounded source-name registry.
/// Claude summary is CLI; Codex summary uses LB only when it is configured.
/// Account preferences remain gateway/account scoped and choose among sources.
pub fn bundle(
    cli: &Snapshot,
    secondary: &Snapshot,
    codex_lb_selected: bool,
) -> Result<Snapshot, Error> {
    if cli.provider != secondary.provider
        || !cli.sources.is_empty()
        || !secondary.sources.is_empty()
    {
        return Err(Error::Invalid);
    }
    transport::validate(cli)?;
    transport::validate(secondary)?;
    let mut source = secondary.clone();
    copy_activity(&mut source, cli);
    let mut s = if cli.provider == Provider::Codex && codex_lb_selected {
        source.clone()
    } else {
        cli.clone()
    };
    s.sources.insert("cli".into(), cli.clone());
    s.sources.insert(
        if cli.provider == Provider::Claude {
            "cswap"
        } else {
            "codex-lb"
        }
        .into(),
        source,
    );
    transport::validate(&s)?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn now() -> DateTime<Utc> {
        parse_time("2026-09-24T00:00:00Z").unwrap()
    }
    #[test]
    fn pending_quota_cannot_roll_back_latest_activity() {
        let mut cli = Snapshot::degraded(Provider::Codex, 10, now(), "ok");
        cli.today_total_tokens = 500;
        cli.status.data_source = "jsonl_only".into();
        let mut old = Snapshot::degraded(Provider::Codex, 9, now(), "ok");
        old.today_total_tokens = 50;
        old.status.quota_source = "codex_lb".into();
        old.rolling_5h_observed = true;
        old.rolling_5h.used_pct = 0.5;
        old.accounts.push(Account {
            number: 1,
            active: true,
            ..Default::default()
        });
        let s = bundle(&cli, &old, true).unwrap();
        assert_eq!(s.today_total_tokens, 500);
        assert_eq!(s.sources["codex-lb"].today_total_tokens, 500);
        assert_eq!(s.status.data_source, "api+jsonl");
        assert_eq!(s.rolling_5h.used_pct, 0.5);
        // UI's existing active-account missing-five-hour rule still has the
        // absent per-account window. It must never be synthesized from summary.
        assert!(s.accounts[0].five_hour.is_none());
        assert_eq!(
            bundle(&cli, &old, false).unwrap().status.quota_source,
            "none"
        );
    }
    #[test]
    fn cswap_decision_status_and_original_time_remain_authoritative() {
        let mut cli = Snapshot::degraded(Provider::Claude, 2, now(), "authExpired");
        cli.today_total_tokens = 500;
        cli.status.data_source = "jsonl_only".into();
        let oracles: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../../tests/fixtures/usage-transport-v1/sources-go-oracle.json"
        ))
        .unwrap();
        for (status, oracle) in ["ok", "stale", "keychain_unavailable", "token_expired"]
            .into_iter()
            .zip(oracles)
        {
            let parsed = Parsed {
                updated_at: Some("2026-09-23T23:50:00Z".into()),
                accounts: vec![Account {
                    number: 1,
                    email: "test@example.invalid".into(),
                    active: true,
                    status: status.into(),
                    last_refresh_at: Some("2026-09-23T23:50:00Z".into()),
                    seven_day: Some(AccountWindow {
                        used_pct: 0.4,
                        resets_at: Some("2026-09-25T00:00:00Z".into()),
                    }),
                    ..Default::default()
                }],
            };
            let s = claude_swap(&cli, parsed, now()).unwrap();
            assert!(!s.rolling_5h_observed);
            assert!(s.weekly_observed);
            assert_eq!(s.weekly.remaining_seconds, 86400);
            assert_eq!(s.status.stale, status != "ok");
            assert_eq!(
                s.status.state,
                if matches!(status, "ok" | "stale") {
                    "ok"
                } else {
                    "networkError"
                }
            );
            assert_eq!(
                s.status.quota_observed_at.as_deref(),
                Some("2026-09-23T23:50:00Z")
            );
            let summary = bundle(&cli, &s, true).unwrap();
            assert_eq!(
                summary,
                transport::decode(&serde_json::to_vec(&oracle).unwrap()).unwrap()
            );
            assert_eq!(summary.status.state, "authExpired");
            assert_eq!(summary.sources["cswap"].accounts[0].status, status);
            assert_eq!(
                transport::decode(&transport::encode(&summary).unwrap()).unwrap(),
                summary
            );
        }
    }
    #[test]
    fn both_activity_source_values_and_ambiguous_active_binding() {
        for input in ["api+jsonl", "jsonl_only"] {
            let mut cli = Snapshot::degraded(Provider::Codex, 1, now(), "ok");
            cli.status.data_source = input.into();
            let mut secondary = cli.clone();
            secondary.status.data_source = "api_only".into();
            secondary.status.quota_source = "codex_lb".into();
            let s = bundle(&cli, &secondary, true).unwrap();
            let decoded = transport::decode(&transport::encode(&s).unwrap()).unwrap();
            assert_eq!(decoded.status.data_source, "api+jsonl");
            assert_eq!(decoded.sources["codex-lb"].status.data_source, "api+jsonl");
        }
        let cli = Snapshot::degraded(Provider::Claude, 1, now(), "ok");
        let parsed = Parsed {
            updated_at: None,
            accounts: vec![
                Account {
                    number: 1,
                    active: true,
                    ..Default::default()
                },
                Account {
                    number: 2,
                    active: true,
                    ..Default::default()
                },
            ],
        };
        assert_eq!(claude_swap(&cli, parsed, now()), Err(Error::Invalid));
    }
}
