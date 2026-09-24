use super::*;
use chrono::{DateTime, Utc};
fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}
fn event(s: &str, n: i64, session: &str) -> TokenEvent {
    TokenEvent {
        provider: Provider::Claude,
        timestamp: Some(at(s)),
        tokens: n,
        model: String::new(),
        session_key: session.into(),
        account_number: 0,
        source: None,
    }
}
#[test]
fn parser_fresh_and_private() {
    let c = br#"{"type":"assistant","timestamp":"2026-09-24T00:00:00Z","sessionId":"s1","message":{"model":"sonnet","usage":{"input_tokens":100,"output_tokens":20,"cache_creation_input_tokens":5,"cache_read_input_tokens":900},"content":"PRIVATE_BODY"}}"#;
    let e = parse_line(Provider::Claude, c, "fixture.jsonl").unwrap();
    assert_eq!(e.tokens, 125);
    assert_eq!(e.session_key, "s1");
    assert!(!format!("{e:?}").contains("s1"));
    assert!(!format!("{e:?}").contains("PRIVATE_BODY"));
    let c = br#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":130,"cached_input_tokens":100,"output_tokens":40,"reasoning_output_tokens":20}}}}"#;
    assert_eq!(
        parse_line(Provider::Codex, c, "fixture.jsonl")
            .unwrap()
            .tokens,
        70
    );
    assert!(parse_line(Provider::Claude, b"bad", "x").is_none());
    assert!(parse_line(Provider::Codex, c, &"x".repeat(MAX_PATH_BYTES + 1)).is_none());
}
#[test]
fn delayed_future_backward_day_and_dwell() {
    let start = at("2026-09-23T23:59:50Z");
    let mut t = Tracker::new(start);
    let day = start.date_naive();
    let next = at("2026-09-24T00:00:00Z").date_naive();
    let e = event("2026-09-23T23:59:51Z", 600, "a");
    let s = t.ingest(&e, at("2026-09-23T23:59:51Z"), day, day);
    assert_eq!(s.state, BurnState::Walk);
    let f = event("2026-09-24T01:00:00Z", 10000, "future");
    assert_eq!(
        t.ingest(&f, at("2026-09-23T23:59:52Z"), day, next)
            .today_total_tokens,
        600
    );
    let old = event("2026-09-23T23:59:52Z", 100, "old");
    let s = t.ingest(&old, at("2026-09-24T00:00:01Z"), next, day);
    assert_eq!(s.today_total_tokens, 0);
    let e = event("2026-09-24T00:00:02Z", 3100, "new");
    let s = t.ingest(&e, at("2026-09-24T00:00:02Z"), next, next);
    assert_eq!(s.today_total_tokens, 3100);
    assert_eq!(s.state, BurnState::Jog);
    assert_eq!(
        t.snapshot(at("2026-09-23T23:59:59Z"), day)
            .today_total_tokens,
        3100
    );
}
#[test]
fn caps_and_sources() {
    let now = at("2026-09-24T12:00:00Z");
    let mut t = Tracker::new(now);
    let day = now.date_naive();
    for i in 0..=MAX_WINDOW_EVENTS {
        let mut e = event("2026-09-24T12:00:00Z", 1, &format!("s{i}"));
        e.source = Some(if i % 2 == 0 {
            ActivitySource::Jsonl
        } else {
            ActivitySource::Hermes
        });
        t.ingest(&e, now, day, day);
    }
    let s = t.snapshot(now, day);
    assert_eq!(s.today_total_tokens, (MAX_WINDOW_EVENTS + 1) as i64);
    assert_eq!(s.today_sessions_count, MAX_DAY_SESSIONS);
    assert!(s.sessions_capped);
    assert_eq!(s.window_events_dropped, 1);
    assert_eq!(
        s.activity_sources,
        vec![ActivitySource::Hermes, ActivitySource::Jsonl]
    );
}
#[test]
fn go_parser_oracle() {
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/usage-activity-v1/parser-cases.json"
    ))
    .unwrap();
    let expected: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/usage-activity-v1/parser-go-oracle.json"
    ))
    .unwrap();
    assert_eq!(cases.len(), expected.len());
    for (case, want) in cases.iter().zip(expected.iter()) {
        let provider = if case["provider"] == "claude" {
            Provider::Claude
        } else {
            Provider::Codex
        };
        let got = parse_line(
            provider,
            case["line"].as_str().unwrap().as_bytes(),
            case["path"].as_str().unwrap(),
        );
        assert_eq!(
            got.as_ref().map_or(0, |e| e.tokens),
            want["tokens"].as_i64().unwrap(),
            "{}",
            case["name"]
        );
        if let Some(got) = got {
            assert_eq!(
                got.timestamp
                    .unwrap()
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                want["timestamp"],
                "{}",
                case["name"]
            );
            assert_eq!(got.model, want["model"]);
            assert_eq!(got.session_key, want["session"]);
        }
    }
}
#[test]
fn go_burn_oracle() {
    let steps: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/usage-activity-v1/burn-steps.json"
    ))
    .unwrap();
    let expected: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/usage-activity-v1/burn-go-oracle.json"
    ))
    .unwrap();
    let mut t = Tracker::new(at(steps[0]["now"].as_str().unwrap()));
    for (step, want) in steps.iter().zip(expected.iter()) {
        let now = at(step["now"].as_str().unwrap());
        let s = if step["op"] == "ingest" {
            let mut e = event(
                step["at"].as_str().unwrap(),
                step["tokens"].as_i64().unwrap(),
                step["session"].as_str().unwrap(),
            );
            e.source = match step["source"].as_str() {
                Some("jsonl") => Some(ActivitySource::Jsonl),
                Some("hermes") => Some(ActivitySource::Hermes),
                _ => None,
            };
            let day = e.timestamp.unwrap().date_naive();
            t.ingest(&e, now, now.date_naive(), day)
        } else {
            t.snapshot(now, now.date_naive())
        };
        assert!(
            (s.rate_per_minute - want["rate"].as_f64().unwrap()).abs() < 1e-9,
            "{step:?}"
        );
        assert_eq!(s.state.as_str(), want["state"]);
        assert_eq!(s.today_total_tokens, want["total"]);
        assert_eq!(s.today_sessions_count as i64, want["sessions"]);
        assert_eq!(s.has_observed, want["observed"]);
        let sources: Vec<_> = s.activity_sources.iter().map(|v| v.as_str()).collect();
        let oracle: Vec<_> = want["sources"]
            .as_array()
            .map(|v| v.iter().map(|x| x.as_str().unwrap()).collect())
            .unwrap_or_default();
        assert_eq!(sources, oracle);
    }
}
#[test]
fn timestamp_fallback_future_boundary_and_window_eviction() {
    let now = at("2026-09-24T00:00:00Z");
    let day = now.date_naive();
    let raw =
        br#"{"type":"assistant","timestamp":"invalid","message":{"usage":{"output_tokens":5}}}"#;
    let e = parse_line(Provider::Claude, raw, "fixture.jsonl").unwrap();
    assert!(e.timestamp.is_none());
    let mut t = Tracker::new(now);
    let s = t.ingest(&e, now, day, day);
    assert_eq!(s.today_total_tokens, 5);
    let edge = event("2026-09-24T00:05:00Z", 10, "edge");
    assert_eq!(t.ingest(&edge, now, day, day).today_total_tokens, 15);
    let beyond = event("2026-09-24T00:05:01Z", 100, "beyond");
    assert_eq!(t.ingest(&beyond, now, day, day).today_total_tokens, 15);
    let later = at("2026-09-24T00:01:01Z");
    assert!(t.snapshot(later, day).rate_per_minute < 15.0);
}
#[test]
fn session_cap_resets_on_forward_day_and_rejects_long_manual_key() {
    let now = at("2026-09-24T12:00:00Z");
    let day = now.date_naive();
    let mut t = Tracker::new(now);
    let e = event("2026-09-24T12:00:00Z", 3, &"x".repeat(MAX_PATH_BYTES + 1));
    let s = t.ingest(&e, now, day, day);
    assert_eq!(s.today_total_tokens, 3);
    assert_eq!(s.today_sessions_count, 0);
    assert!(s.sessions_capped);
    let next = at("2026-09-25T12:00:00Z");
    let s = t.snapshot(next, next.date_naive());
    assert_eq!(s.today_total_tokens, 0);
    assert!(!s.sessions_capped);
}
#[test]
fn malformed_large_selected_scalars_do_not_materialize_json_trees() {
    let array = format!("[{}0]", "0,".repeat(200_000));
    let escaped = format!("\"{}\"", "\\u0061".repeat(100_000));
    let raw = format!(
        "{{\"type\":\"assistant\",\"timestamp\":{array},\"sessionId\":{array},\"message\":{{\"model\":{escaped},\"usage\":{{\"input_tokens\":{array},\"output_tokens\":7}}}}}}"
    );
    assert!(raw.len() < MAX_LINE_BYTES);
    let e = parse_line(Provider::Claude, raw.as_bytes(), "synthetic.jsonl").unwrap();
    assert_eq!(e.tokens, 7);
    assert_eq!(e.model, "");
    assert_eq!(e.session_key, "synthetic.jsonl");
    assert!(e.timestamp.is_none());
}
#[test]
fn extreme_chrono_bounds_do_not_panic() {
    let min = DateTime::<Utc>::MIN_UTC;
    let max = DateTime::<Utc>::MAX_UTC;
    let mut tracker = Tracker::new(min);
    let e = TokenEvent {
        provider: Provider::Claude,
        timestamp: Some(min),
        tokens: 600,
        model: String::new(),
        session_key: "minimum".into(),
        account_number: 0,
        source: None,
    };
    let s = tracker.ingest(&e, min, min.date_naive(), min.date_naive());
    assert_eq!(s.state, BurnState::Walk);
    assert_eq!(
        tracker.snapshot(min, min.date_naive()).today_total_tokens,
        600
    );
    let s = tracker.snapshot(max, max.date_naive());
    assert_eq!(s.today_total_tokens, 0);
    assert_eq!(s.rate_per_minute, 0.0);
    assert_eq!(tracker.snapshot(min, min.date_naive()).rate_per_minute, 0.0);
    let mut tracker = Tracker::new(max);
    let s = tracker.snapshot(max, max.date_naive());
    assert_eq!(s.state, BurnState::Idle);
    let high = TokenEvent {
        timestamp: Some(max),
        ..e
    };
    tracker.ingest(&high, max, max.date_naive(), max.date_naive());
    assert!(tracker
        .snapshot(min, min.date_naive())
        .rate_per_minute
        .is_finite());
}

#[test]
fn daily_session_set_retains_only_fixed_digests() {
    let now = at("2026-09-24T12:00:00Z");
    let mut tracker = Tracker::new(now);
    let name = "private-session-path/".repeat(90);
    let e = event("2026-09-24T12:00:00Z", 1, &name);
    tracker.ingest(&e, now, now.date_naive(), now.date_naive());
    tracker.ingest(&e, now, now.date_naive(), now.date_naive());
    assert_eq!(tracker.sessions.len(), 1);
    assert_eq!(std::mem::size_of_val(tracker.sessions.first().unwrap()), 32);
}
