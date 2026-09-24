use super::*;
#[test]
fn matches_actual_go_oauth_oracle() {
    #[derive(Deserialize)]
    struct Case {
        name: String,
        provider: Provider,
        raw: String,
        want: Option<serde_json::Value>,
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/usage-oauth-v1/go-oracle.json"
    ))
    .unwrap();
    assert!(cases.len() >= 30);
    let now = parse_time("2026-09-24T00:00:00Z").unwrap();
    for c in cases {
        let result = normalize(c.provider, c.raw.as_bytes(), 9, now);
        assert_eq!(result.is_ok(), c.want.is_some(), "{}: {result:?}", c.name);
        if let (Ok(actual), Some(want)) = (result, c.want) {
            assert_eq!(
                actual,
                transport::decode(&serde_json::to_vec(&want).unwrap()).unwrap(),
                "{}",
                c.name
            );
        }
    }
}
#[test]
fn bounded_and_no_private_fields() {
    let now = parse_time("2026-09-24T00:00:00Z").unwrap();
    assert_eq!(
        normalize(Provider::Codex, &vec![b' '; MAX_RESPONSE_BYTES + 1], 1, now),
        Err(Error::Limit)
    );
    let raw=br#"{"secondary":{"usedPercent":25},"accountEmail":"synthetic-private@example.invalid","unknown":{"access_token":"synthetic-secret"}}"#;
    let s = normalize(Provider::Codex, raw, 1, now).unwrap();
    assert!(!s.rolling_5h_observed);
    assert!(s.weekly_observed);
    let encoded = String::from_utf8(transport::encode(&s).unwrap()).unwrap();
    assert!(!encoded.contains("synthetic"));
}
