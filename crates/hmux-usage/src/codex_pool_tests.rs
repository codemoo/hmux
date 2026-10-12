use super::*;
use serde_json::{json, Value};
fn now() -> DateTime<Utc> {
    parse_time("2026-10-12T00:00:00Z").unwrap()
}
fn row(id: &str, plan: &str, pct: f64) -> Value {
    json!({"accountId":id,"planType":plan,"status":"active","usageRefreshedAt":"2026-10-11T23:59:00Z",
        "primary":{"remainingPercent":pct,"windowMinutes":300,"resetAt":"2026-10-12T04:00:00Z"},
        "secondary":{"remainingPercent":pct,"windowMinutes":10080,"resetAt":"2026-10-18T00:00:00Z"}})
}
fn parse(rows: Vec<Value>) -> Snapshot {
    parse_fleet(
        &serde_json::to_vec(&json!({"accounts":rows})).unwrap(),
        1,
        now(),
    )
    .unwrap()
}
#[test]
fn mixed_plans_and_exhausted_account_use_capacity_not_mean() {
    let mut exhausted = row("plus", "plus", 0.0);
    exhausted["status"] = json!("quota_exceeded");
    let s = parse(vec![
        row("pro", "pro", 80.0),
        row("team", "team", 20.0),
        exhausted,
    ]);
    let expected_remaining = (50400.0 * 0.8 + 7560.0 * 0.2) / (50400.0 + 7560.0 + 7560.0);
    assert!((s.weekly.used_pct - (1.0 - expected_remaining)).abs() < 1e-12);
    assert!(s.accounts[2].active);
    assert_eq!(s.status.quota_weighting, "plan_estimate");
    assert_eq!(
        s.status.quota_observed_at.as_deref(),
        Some("2026-10-11T23:59:00.000Z")
    );
    let encoded = crate::transport::encode(&s).unwrap();
    assert_eq!(crate::transport::decode(&encoded).unwrap(), s);
}
#[test]
fn reported_capacity_and_remaining_credits_override_plan_and_percent() {
    let mut a = row("a", "pro", 99.0);
    a["capacityCreditsSecondary"] = json!(100);
    a["remainingCreditsSecondary"] = json!(25);
    let mut b = row("b", "plus", 99.0);
    b["capacityCreditsSecondary"] = json!(900);
    b["remainingCreditsSecondary"] = json!(450);
    a["primary"] = json!({});
    b["primary"] = json!({});
    let s = parse(vec![a, b]);
    assert!((s.weekly.used_pct - 0.525).abs() < 1e-12);
    assert_eq!(s.accounts[0].seven_day.as_ref().unwrap().used_pct, 0.75);
    assert_eq!(s.status.quota_weighting, "reported_capacity");
    assert!(!s.rolling_5h_observed);
}
#[test]
fn missing_hidden_stale_wrong_window_or_unknown_plan_never_inflate_pool() {
    for field in [
        "missing",
        "stale",
        "expired",
        "unknown",
        "wrong_window",
        "invalid_capacity",
        "invalid_pct",
        "unknown_status",
    ] {
        let mut b = row("b", "plus", 50.0);
        match field {
            "missing" => b["secondary"] = json!({}),
            "stale" => b["usageRefreshedAt"] = json!("2026-10-11T22:00:00Z"),
            "expired" => b["secondary"]["resetAt"] = json!("2026-10-11T23:59:00Z"),
            "unknown" => b["planType"] = json!("future-pro-tier"),
            "wrong_window" => b["secondary"]["windowMinutes"] = json!(43200),
            "invalid_capacity" => b["capacityCreditsSecondary"] = json!(-1),
            "invalid_pct" => b["secondary"]["remainingPercent"] = json!(101),
            _ => b["status"] = json!("unknown"),
        }
        let s = parse(vec![row("a", "pro", 80.0), b]);
        assert!(!s.weekly_observed, "{field}");
    }
    assert!(!parse(vec![]).weekly_observed);
    assert!(parse(vec![]).status.quota_weighting.is_empty());
    let mut b = row("b", "plus", 0.0);
    b["status"] = json!("paused");
    assert!((parse(vec![row("a", "pro", 80.0), b]).weekly.used_pct - 0.2).abs() < 1e-12);
}
#[test]
fn aliases_privacy_prolite_and_absent_primary() {
    let mut a = row("private-id", "chatgpt_prolite", 50.0);
    a["displayName"] = json!("Private@Example.test");
    a["email"] = json!("private@example.test");
    a["primary"] = json!({});
    let mut b = row("b", "team", 100.0);
    b["displayName"] = json!("Team alias");
    let s = parse(vec![a, b]);
    assert_eq!(s.accounts[0].plan_type, "prolite");
    assert_eq!(
        s.accounts[0].quota_observed_at.as_deref(),
        Some("2026-10-11T23:59:00.000Z")
    );
    assert_eq!(s.accounts[0].display_name, "Account 1");
    assert_eq!(s.accounts[1].display_name, "Team alias");
    assert!(!s.rolling_5h_observed);
    assert!((s.weekly.used_pct - (37800.0 * 0.5 / (37800.0 + 7560.0))).abs() < 1e-12);
    let text = String::from_utf8(crate::transport::encode(&s).unwrap()).unwrap();
    assert!(!text.contains("private-id"));
    assert!(!text.contains("example.test"));
}
#[test]
fn duplicates_and_bounds_rejected_and_earliest_reset_is_preserved() {
    assert!(parse_fleet(
        &serde_json::to_vec(&json!({"accounts":[row("a","plus",10.0),row("a","plus",10.0)]}))
            .unwrap(),
        1,
        now()
    )
    .is_err());
    assert!(parse_fleet(&serde_json::to_vec(&json!({"accounts":(0..129).map(|i|row(&i.to_string(),"plus",10.0)).collect::<Vec<_>>()})).unwrap(),1,now()).is_err());
    assert_eq!(
        parse_fleet(&vec![b'x'; MAX_SNAPSHOT_BYTES + 1], 1, now()),
        Err(Error::Limit)
    );
    let mut b = row("b", "plus", 50.0);
    b["secondary"]["resetAt"] = json!("2026-10-13T00:00:00Z");
    assert_eq!(
        parse(vec![row("a", "pro", 50.0), b])
            .weekly
            .resets_at
            .as_deref(),
        Some("2026-10-13T00:00:00.000Z")
    );
}
