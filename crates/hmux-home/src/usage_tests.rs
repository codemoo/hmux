use super::*;
use hmux_usage::activity::{ActivitySource, BurnState};

fn now() -> DateTime<Utc> {
    "2026-09-24T00:00:00Z".parse().unwrap()
}
fn sample(tokens: i64) -> usage_activity::Sample {
    let activity = ActivitySnapshot {
        rate_per_minute: 2.0,
        state: BurnState::Walk,
        today_total_tokens: tokens,
        today_sessions_count: 1,
        has_observed: true,
        activity_sources: vec![ActivitySource::Jsonl],
        sessions_capped: false,
        window_events_dropped: 0,
        total_saturated: false,
    };
    usage_activity::Sample {
        claude: activity.clone(),
        codex: activity,
        claude_diagnostics: Default::default(),
        codex_diagnostics: Default::default(),
    }
}
fn inputs() -> (
    Inputs,
    watch::Sender<Snapshot>,
    watch::Sender<Lb>,
    watch::Sender<Option<usage_activity::Sample>>,
) {
    let (_, claude) = watch::channel(degraded(Provider::Claude, now()));
    let (codex_tx, codex) = watch::channel(degraded(Provider::Codex, now()));
    let (_, swap) = watch::channel(None);
    let (lb_tx, lb) = watch::channel(Lb {
        selected: false,
        snapshot: usage_lb::unavailable(0, now(), "networkError"),
    });
    let (activity_tx, activity) = watch::channel(None);
    (
        Inputs {
            claude,
            codex,
            swap,
            lb,
            activity,
        },
        codex_tx,
        lb_tx,
        activity_tx,
    )
}
#[test]
fn publication_survives_retry_deadline_without_refreshing_quota_age() {
    for source in ["claude", "codex", "codex-lb"] {
        for state in ["rateLimited", "ok"] {
            let (mut input, codex, lb, _) = inputs();
            let mut snapshot = degraded(
                if source == "claude" {
                    Provider::Claude
                } else {
                    Provider::Codex
                },
                now(),
            );
            snapshot.status.state = state.into();
            snapshot.status.stale = true;
            snapshot.status.quota_source = if source == "codex-lb" {
                "codex_lb"
            } else {
                "oauth_api"
            }
            .into();
            snapshot.status.quota_observed_at = Some(format_time(now()));
            snapshot.status.retry_at = Some(format_time(now() + chrono::Duration::seconds(5)));
            snapshot.weekly_observed = true;
            snapshot.weekly.used_pct = 0.4;
            transport::validate(&snapshot).unwrap();
            let (claude_tx, claude) = watch::channel(snapshot.clone());
            match source {
                "claude" => input.claude = claude,
                "codex" => {
                    codex.send_replace(snapshot);
                }
                _ => {
                    lb.send_replace(Lb {
                        selected: true,
                        snapshot,
                    });
                }
            }
            let before = input.assemble(1, now()).unwrap();
            let prior = if source == "claude" {
                before.claude
            } else {
                before.codex
            };
            assert!(prior.status.retry_at.is_some());
            for seconds in [5, 6, 61] {
                let latest = input
                    .assemble(2, now() + chrono::Duration::seconds(seconds))
                    .unwrap();
                let current = if source == "claude" {
                    latest.claude
                } else {
                    latest.codex
                };
                assert!(current.status.retry_at.is_none());
                assert_eq!(current.status.quota_observed_at, Some(format_time(now())));
                assert_eq!(current.weekly.used_pct, 0.4);
                transport::validate(&current).unwrap();
            }
            drop(claude_tx);
        }
    }
}

#[test]
fn late_quota_results_merge_latest_activity_for_all_sources() {
    let (mut input, codex, lb, activity) = inputs();
    activity.send_replace(Some(sample(500)));
    let mut old = degraded(Provider::Codex, now());
    old.today_total_tokens = 1;
    old.status.quota_source = "codex_lb".into();
    old.status.quota_observed_at = Some(format_time(now()));
    old.weekly_observed = true;
    old.weekly.used_pct = 0.5;
    lb.send_replace(Lb {
        selected: true,
        snapshot: old,
    });
    codex.send_replace(degraded(Provider::Codex, now()));
    let latest = input.assemble(7, now()).unwrap();
    let decoded = latest.codex.as_ref();
    assert_eq!(decoded.today_total_tokens, 500);
    assert_eq!(decoded.seq, 7);
    assert_eq!(decoded.status.quota_source, "codex_lb");
    assert_eq!(decoded.weekly.used_pct, 0.5);
    for name in ["cli", "codex-lb"] {
        assert_eq!(decoded.sources[name].today_total_tokens, 500);
    }
    assert_eq!(
        latest.claude.as_ref().sources["cswap"].today_total_tokens,
        500
    );
    // A failed scan cannot keep the previous day's counters alive through
    // heartbeat publication; source quota remains independently available.
    activity.send_replace(None);
    let missing = input.assemble(8, now()).unwrap().codex;
    assert_eq!(missing.today_total_tokens, 0);
    assert_eq!(missing.status.data_source, "api_only");
    assert_eq!(missing.weekly.used_pct, 0.5);
    activity.send_replace(Some(sample(500)));
    assert_eq!(
        input.assemble(9, now()).unwrap().codex.today_total_tokens,
        500
    );
    // A local day rollover is authoritative even when quota I/O returns later.
    activity.send_replace(Some(sample(0)));
    assert_eq!(
        input.assemble(8, now()).unwrap().codex.today_total_tokens,
        0
    );
}

#[tokio::test(start_paused = true)]
async fn invalid_publication_withdraws_data_and_recovers_without_stopping_sources() {
    let (input, codex, _lb, _activity) = inputs();
    let (output, mut latest) = watch::channel(None);
    let stop = CancellationToken::new();
    let task_stop = stop.clone();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = events.clone();
    let reporter: observation::Reporter =
        Arc::new(move |event| observed.lock().unwrap().push(event.reason));
    let task =
        tokio::spawn(async move { publish(input, output, &task_stop, Some(reporter)).await });
    latest.changed().await.unwrap();
    assert!(latest.borrow_and_update().is_some());
    let mut invalid = degraded(Provider::Codex, now());
    invalid.status.state.clear();
    codex.send_replace(invalid);
    latest.changed().await.unwrap();
    assert!(latest.borrow_and_update().is_none());
    for _ in 0..25 {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
    assert!(!stop.is_cancelled());
    assert!(!task.is_finished());
    assert_eq!(
        *events.lock().unwrap(),
        [
            observation::Reason::Published,
            observation::Reason::Encoding
        ]
    );
    latest.borrow_and_update();
    codex.send_replace(degraded(Provider::Codex, now()));
    latest.changed().await.unwrap();
    assert!(latest.borrow_and_update().is_some());
    assert_eq!(
        *events.lock().unwrap(),
        [
            observation::Reason::Published,
            observation::Reason::Encoding,
            observation::Reason::Recovered
        ]
    );
    stop.cancel();
    assert_eq!(task.await.unwrap(), Ok(()));
}

#[tokio::test(start_paused = true)]
async fn publication_coalesces_and_heartbeats_without_refreshing_observation_age() {
    let (input, _codex, lb, activity) = inputs();
    let mut snapshot = usage_lb::unavailable(1, now(), "ok");
    snapshot.status.quota_observed_at = Some(format_time(now()));
    lb.send_replace(Lb {
        selected: true,
        snapshot,
    });
    let (output, mut latest) = watch::channel(None);
    let stop = CancellationToken::new();
    let task_stop = stop.clone();
    let task = tokio::spawn(async move { publish(input, output, &task_stop, None).await });
    latest.changed().await.unwrap();
    latest.borrow_and_update();
    for count in 1..=100 {
        activity.send_replace(Some(sample(count)));
    }
    tokio::time::advance(Duration::from_millis(900)).await;
    assert!(!latest.has_changed().unwrap());
    tokio::time::advance(Duration::from_millis(100)).await;
    latest.changed().await.unwrap();
    let current = latest.borrow_and_update().clone().unwrap();
    let decoded = current.codex.as_ref();
    assert_eq!(decoded.today_total_tokens, 100);
    let seq = decoded.seq;
    tokio::time::advance(Duration::from_secs(9)).await;
    tokio::task::yield_now().await;
    // Missed ticks are skipped, so advance individual ticks for the heartbeat.
    for _ in 0..10 {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
    assert!(latest.has_changed().unwrap());
    let current = latest.borrow_and_update().clone().unwrap();
    let decoded = current.codex.as_ref();
    assert!(decoded.seq > seq);
    assert_eq!(decoded.status.quota_observed_at, Some(format_time(now())));
    stop.cancel();
    assert_eq!(task.await.unwrap(), Ok(()));
}

#[tokio::test(start_paused = true)]
async fn explicit_refresh_fans_out_coalesces_and_survives_inflight_work() {
    let (tx, rx) = watch::channel(0);
    let handle = RefreshHandle(tx);
    let stop = CancellationToken::new();
    let mut a = rx.clone();
    let mut b = rx;
    let mut timer_a = ticker(REFRESH);
    let mut timer_b = ticker(REFRESH);
    assert_eq!(source_tick(&mut timer_a, &mut a, &stop).await, Some(false));
    assert_eq!(source_tick(&mut timer_b, &mut b, &stop).await, Some(false));
    // Both independent owners see one refresh after any number of callers, even
    // when those requests arrived while a source was performing I/O.
    for _ in 0..1000 {
        handle.request();
    }
    let (a_result, b_result) = tokio::join!(
        source_tick(&mut timer_a, &mut a, &stop),
        source_tick(&mut timer_b, &mut b, &stop)
    );
    assert_eq!((a_result, b_result), (Some(true), Some(true)));
    assert!(!a.has_changed().unwrap() && !b.has_changed().unwrap());
    handle.request();
    assert!(a.has_changed().unwrap() && b.has_changed().unwrap());
    stop.cancel();
    assert_eq!(source_tick(&mut timer_a, &mut a, &stop).await, None);
}
