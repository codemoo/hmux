use super::*;
use serde_json::{json, Value};

fn now() -> DateTime<Utc> {
    parse_time("2026-07-03T13:01:00Z").unwrap()
}
fn equal_json(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal_json(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|w| equal_json(v, w)))
        }
        _ => a == b,
    }
}

#[test]
fn matches_go_synthetic_oracle_with_transport_privacy() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/usage-codex-v1/accounts.json"
    ))
    .unwrap();
    for (name, case) in fixtures.as_object().unwrap() {
        let got = parse_account_export(case["input"].as_str().unwrap().as_bytes(), now(), now());
        if !case["valid"].as_bool().unwrap() {
            assert!(got.is_err(), "{name}");
            continue;
        }
        let got = got.unwrap();
        let rows: Vec<Value> = got
            .accounts
            .iter()
            .map(|a| {
                json!({
                    "number": a.number, "email": a.email, "display_name": a.display_name,
                    "active": a.active, "status": a.status, "five_hour": a.five_hour,
                    "seven_day": a.seven_day, "tokens_per_hour": a.tokens_per_hour,
                    "total_tokens": a.total_tokens, "last_refresh_at": a.last_refresh_at,
                    "plan_type": a.plan_type,
                })
            })
            .collect();
        let actual = json!({"accounts": rows, "updated_at": got.updated_at});
        assert!(
            equal_json(&actual, &case["expected"]),
            "{name}: actual={actual} expected={}",
            case["expected"]
        );
    }
}

#[test]
fn privacy_malformed_and_freshness_bounds() {
    assert_eq!(
        parse_account_export(&vec![b'x'; 8 * 1024 * 1024 + 1], now(), now()),
        Err(Error::Limit)
    );
    assert_eq!(
        parse_account_export(b"[]", now(), now()),
        Err(Error::Invalid)
    );
    let raw = br#"{"schemaVersion":1,"accounts":[{"number":1,"alias":"\u202eBad","email":"secret@example.test","displayName":"Private"},{"number":2,"alias":"Public"}]}"#;
    let got = parse_account_export(raw, now(), now()).unwrap();
    assert_eq!(got.accounts[0].display_name, "");
    assert_eq!(got.accounts[1].display_name, "Public");
    assert!(got.accounts.iter().all(|a| a.email.is_empty()));
    assert!(!serde_json::to_string(&got.accounts)
        .unwrap()
        .contains("secret"));
    let raw = format!(
        "{{\"schemaVersion\":1,\"accounts\":[{}]}}",
        vec!["{\"number\":1}"; 129].join(",")
    );
    assert!(parse_account_export(raw.as_bytes(), now(), now()).is_err());
    let future = now() + Duration::hours(1);
    let got = parse_account_export(
        br#"{"schemaVersion":1,"accounts":[{"number":1}]}"#,
        now(),
        future,
    )
    .unwrap();
    assert_eq!(got.updated_at.as_deref(), Some("2026-07-03T13:01:00.000Z"));
    assert!(is_fresh(now() - Duration::minutes(30), now()));
    assert!(!is_fresh(
        now() - Duration::minutes(30) - Duration::seconds(1),
        now()
    ));
}
