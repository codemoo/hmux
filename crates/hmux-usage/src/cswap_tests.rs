use super::*;
use serde_json::{json, Value};

fn at(s: &str) -> DateTime<Utc> {
    model::parse_time(s).unwrap()
}

fn normalized_numbers(value: Value) -> Value {
    match value {
        Value::Number(n) => Value::String(format!("number:{}", n.as_f64().unwrap())),
        Value::Array(items) => Value::Array(items.into_iter().map(normalized_numbers).collect()),
        Value::Object(items) => Value::Object(
            items
                .into_iter()
                .map(|(k, v)| (k, normalized_numbers(v)))
                .collect(),
        ),
        other => other,
    }
}

#[test]
fn matches_go_command_oracle() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/usage-cswap-v1/cases.json"
    ))
    .unwrap();
    let oracle: Vec<Value> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/usage-cswap-v1/go-oracle.json"
    ))
    .unwrap();
    assert_eq!(cases.len(), oracle.len());
    for (case, expected) in cases.iter().zip(oracle) {
        let input = serde_json::to_vec(&case["input"]).unwrap();
        let parsed = parse_command(&input, at(case["now"].as_str().unwrap()));
        let mut actual = json!({"name":case["name"], "valid":parsed.is_ok()});
        if let Ok(parsed) = parsed {
            if !parsed.accounts.is_empty() {
                actual["accounts"] = serde_json::to_value(parsed.accounts).unwrap();
            }
            if let Some(updated) = parsed.updated_at {
                actual["updated_at"] = json!(updated);
            }
        }
        assert_eq!(
            normalized_numbers(actual),
            normalized_numbers(expected),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn account_and_byte_limits_are_enforced_before_unbounded_allocation() {
    let now = at("2026-05-05T12:00:00Z");
    let row = r#"{"number":1}"#;
    let rows = vec![row; MAX_ACCOUNTS + 1].join(",");
    let oversized_rows = format!(r#"{{"schemaVersion":1,"accounts":[{rows}]}}"#);
    assert_eq!(
        parse_command(oversized_rows.as_bytes(), now),
        Err(Error::Invalid)
    );
    assert_eq!(
        parse_command(&vec![b' '; MAX_COMMAND_BYTES + 1], now),
        Err(Error::Limit)
    );
    assert_eq!(
        parse_command(br#"{"schemaVersion":1,"accounts":null}"#, now),
        Err(Error::Invalid)
    );
    assert_eq!(parse_command(b"{", now), Err(Error::Invalid));
}

#[test]
fn cache_expires_from_read_time_and_original_measurement() {
    let now = at("2026-05-05T12:00:00Z");
    let source = br#"{"schemaVersion":1,"accounts":[{"number":1,"usageStatus":"ok","usage":{"fiveHour":{"pct":50}},"usageFetchedAt":"2026-05-05T11:50:00Z"}]}"#;
    let parsed = parse_command(source, now).unwrap();
    let mut cache = LastGood::default();
    cache.replace(parsed, now);
    assert!(
        cache.current(now + Duration::minutes(19)).unwrap().accounts[0]
            .five_hour
            .is_some()
    );
    let stale = cache.current(now + Duration::minutes(20)).unwrap();
    assert!(stale.accounts[0].five_hour.is_none());
    assert_eq!(stale.accounts[0].status, "unavailable");
    assert_eq!(
        stale.accounts[0].last_refresh_at.as_deref(),
        Some("2026-05-05T11:50:00.000Z")
    );
    assert_eq!(
        stale.updated_at.as_deref(),
        Some("2026-05-05T11:50:00.000Z")
    );
    assert!(cache.current(now + Duration::minutes(30)).is_some());
    assert!(cache
        .current(now + Duration::minutes(30) + Duration::milliseconds(1))
        .is_none());
}
