use hmux_home::{binding::Provider, transcript::parse_tail};
use tokio_util::sync::CancellationToken;

#[test]
fn public_messages_and_ids_match_the_real_go_parsers() {
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/conversation-v1/go-oracle.json"
    ))
    .unwrap();
    let mut failures = Vec::new();
    for case in fixtures.as_array().unwrap() {
        let data = format!("{}\n", case["line"].as_str().unwrap());
        let provider = if case["provider"] == "claude" {
            Provider::Claude
        } else {
            Provider::Codex
        };
        let parsed = parse_tail(
            data.as_bytes(),
            0,
            [0; 16],
            provider,
            &CancellationToken::new(),
        )
        .unwrap();
        if serde_json::to_value(parsed.messages).unwrap() != case["messages"]
            || parsed.truncated != case["truncated"].as_bool().unwrap()
        {
            failures.push(case["name"].as_str().unwrap());
        }
    }
    assert!(
        failures.is_empty(),
        "Go transcript parity failures: {failures:?}"
    );
}
