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
fn matches_go_synthetic_oracle() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/usage-codex-v1/lb.json"
    ))
    .unwrap();
    for (name, case) in fixtures.as_object().unwrap() {
        let got = parse_usage(case["input"].as_str().unwrap().as_bytes(), 7, now());
        if !case["valid"].as_bool().unwrap() {
            assert!(got.is_err(), "{name}");
            continue;
        }
        let snap = got.unwrap();
        let actual = json!({
            "schema": snap.schema, "seq": snap.seq, "generated_at_utc": snap.generated_at_utc,
            "provider": snap.provider, "burn_state": snap.burn_state,
            "rolling_5h": snap.rolling_5h, "weekly": snap.weekly,
            "rolling_5h_observed": snap.rolling_5h_observed, "weekly_observed": snap.weekly_observed,
            "status": { "state": snap.status.state, "data_source": snap.status.data_source,
                "quota_source": snap.status.quota_source, "stale": snap.status.stale }
        });
        assert!(
            equal_json(&actual, &case["expected"]),
            "{name}: actual={actual} expected={}",
            case["expected"]
        );
    }
}

#[test]
fn rejects_malformed_and_bounded_limits() {
    assert_eq!(
        parse_usage(&vec![b'x'; (1 << 20) + 1], 1, now()),
        Err(Error::Limit)
    );
    assert_eq!(parse_usage(b"[{}]", 1, now()), Err(Error::Invalid));
    assert_eq!(
        parse_usage(b"{\"upstream_limits\":null}", 1, now()),
        Err(Error::Invalid)
    );
    assert!(parse_usage(
        b"{\"upstream_limits\":null,\"account_pool_usage\":{\"primary\":80}}",
        1,
        now()
    )
    .is_ok());
    let row = r#"{"source":"aggregate","limit_type":"credits","limit_window":"5h","max_value":1,"current_value":0,"remaining_value":1}"#;
    let raw = format!("{{\"upstream_limits\":[{}]}}", vec![row; 129].join(","));
    assert!(parse_usage(raw.as_bytes(), 1, now()).is_err());
    let raw = format!(
        "{{\"account_pool_usage\":{{\"primary\":50}},\"ignored\":\"{}\"}}",
        "secret".repeat(10)
    );
    let snap = parse_usage(raw.as_bytes(), 1, now()).unwrap();
    assert!(!serde_json::to_string(&snap).unwrap().contains("secret"));
}
