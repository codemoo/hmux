use super::*;
use serde_json::json;

fn snapshot() -> Snapshot {
    Snapshot::degraded(
        Provider::Codex,
        1,
        parse_time("2026-09-24T00:00:00Z").unwrap(),
        "ok",
    )
}
#[test]
fn go_transport_oracle() {
    #[derive(Deserialize)]
    struct Case {
        name: String,
        raw: String,
        valid: bool,
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/usage-transport-v1/go-oracle.json"
    ))
    .unwrap();
    assert!(cases.len() >= 35);
    for c in cases {
        assert_eq!(decode(c.raw.as_bytes()).is_ok(), c.valid, "{}", c.name);
    }
}
#[test]
fn round_trip_is_allowlisted_and_missing_windows_stay_unobserved() {
    let s = snapshot();
    let raw = encode(&s).unwrap();
    assert_eq!(decode(&raw).unwrap(), s);
    assert!(!s.rolling_5h_observed);
    assert!(!String::from_utf8(raw).unwrap().contains("producer"));
}
#[test]
fn oversize_duplicate_trailing_and_nested_sources_rejected() {
    assert_eq!(
        decode(&vec![b' '; MAX_SNAPSHOT_BYTES + 1]),
        Err(Error::Limit)
    );
    let raw = String::from_utf8(encode(&snapshot()).unwrap()).unwrap();
    assert!(decode(format!("{raw} null").as_bytes()).is_err());
    assert!(decode(
        raw.replacen("\"schema\":1", "\"schema\":1,\"schema\":1", 1)
            .as_bytes()
    )
    .is_err());
    let mut child = snapshot();
    child.sources.insert("cli".into(), snapshot());
    let mut root = snapshot();
    root.sources.insert("cli".into(), child);
    assert!(encode(&root).is_err());
    assert!(decode(&serde_json::to_vec(&root).unwrap()).is_err());
    let value =
        json!({"schema":1,"provider":"codex","sources":{"cli":{},"codex-lb":{},"third":{}}});
    assert!(decode(&serde_json::to_vec(&value).unwrap()).is_err());
}
#[test]
fn serializer_rejects_nonfinite_and_unsafe_labels() {
    let mut s = snapshot();
    s.rolling_5h.used_pct = f64::NAN;
    assert!(encode(&s).is_err());
    s.rolling_5h.used_pct = 0.0;
    s.accounts.push(Account {
        number: 1,
        email: "synthetic@example.invalid".into(),
        ..Default::default()
    });
    assert!(encode(&s).is_err());
    s.provider = Provider::Claude;
    assert!(encode(&s).is_ok());
    s.accounts[0].display_name = "x\u{2066}".into();
    assert!(encode(&s).is_err());
}
