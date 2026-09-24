use super::*;

fn run(data: &str, provider: Provider) -> Parsed {
    parse_tail(
        data.as_bytes(),
        0,
        [0; 16],
        provider,
        &CancellationToken::new(),
    )
    .unwrap()
}
fn codex(role: &str, kind: &str, text: &str) -> String {
    format!("{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":{role:?},\"content\":[{{\"type\":{kind:?},\"text\":{}}}]}}}}\n", serde_json::to_string(text).unwrap())
}
#[test]
fn codex_public_filter_and_identity() {
    let mut data = codex("user", "input_text", "Please review the change.");
    data.push_str(&codex(
        "user",
        "input_text",
        "<environment_context>secret</environment_context>",
    ));
    data.push_str("{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"channel\":\"analysis\",\"content\":[{\"type\":\"output_text\",\"text\":\"secret\"}]}}\n");
    data.push_str("{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"recipient\":\"functions.exec\",\"content\":[{\"type\":\"output_text\",\"text\":\"tool\"}]}}\n");
    data.push_str(&codex(
        "assistant",
        "output_text",
        "The review is complete.",
    ));
    let parsed = run(&data, Provider::Codex);
    assert_eq!(
        parsed
            .messages
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        ["Please review the change.", "The review is complete."]
    );
    let first = &parsed.messages[0];
    assert_eq!(first.id, "993e7c9dab517f7d4f7bb53d4c6abe1b"); // Go SHA-256 identity/offset/line oracle
    assert_ne!(
        first.id,
        parse_tail(
            data.as_bytes(),
            1,
            [0; 16],
            Provider::Codex,
            &CancellationToken::new()
        )
        .unwrap()
        .messages
        .first()
        .unwrap()
        .id
    );
}
#[test]
fn claude_public_text_and_go_escaped_identity() {
    let data = concat!(
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<A>&\\u2028\\u2029 한글\"}}\n",
        "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"thinking\",\"thinking\":\"secret\"},{\"type\":\"tool_use\",\"input\":{\"secret\":\"private\"}},{\"type\":\"text\",\"text\":\"| A | B |\\n|---|---|\"}]}}\n",
        "{\"type\":\"assistant\",\"isSidechain\":true,\"message\":{\"role\":\"assistant\",\"content\":\"secret\"}}\n",
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<system-reminder>secret</system-reminder>\"}}\n"
    );
    let parsed = run(data, Provider::Claude);
    assert_eq!(parsed.messages.len(), 2);
    assert_eq!(parsed.messages[0].text, "<A>&\u{2028}\u{2029} 한글");
    assert_eq!(parsed.messages[0].id, "420f4750040d309a6aab8a0c63556040"); // Go canonical JSON oracle
    assert_eq!(parsed.messages[1].text, "| A | B |\n|---|---|");
}
#[test]
fn incomplete_partial_oversized_and_count() {
    let mut data = "partial\n".to_owned();
    data.push_str(&"x".repeat(LINE_LIMIT + 1));
    data.push('\n');
    for i in 0..205 {
        data.push_str(&codex("user", "input_text", &format!("message {i}")));
    }
    data.push_str("{\"type\":\"response_item\"");
    let parsed = parse_tail(
        data.as_bytes(),
        50,
        [0; 16],
        Provider::Codex,
        &CancellationToken::new(),
    )
    .unwrap();
    assert!(parsed.truncated);
    assert_eq!(parsed.messages.len(), 200);
    assert_eq!(parsed.messages[0].text, "message 5");
    assert_eq!(parsed.messages[199].text, "message 204");
}
#[test]
fn compaction_confirmation_and_original_user_specification() {
    let summary = "## Active goal and scope\n\nContinue the unfinished work.";
    let envelope = format!("{CONTINUATION_PREFIX} More continuation instructions.\n{CONTINUATION_SUMMARY}, use the information in this summary to assist with your own analysis:\n\n{summary}");
    let mut data = codex("user", "input_text", summary);
    data.push_str(&codex("assistant", "output_text", summary));
    data.push_str(&format!("{{\"type\":\"compacted\",\"payload\":{{\"message\":{}}},\"replacement_history\":[{{\"private\":\"ignored\"}}]}}\n", serde_json::to_string(&envelope).unwrap()));
    let parsed = run(&data, Provider::Codex);
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages[0].role, "user");
    let without_complete_compaction = run(&data[..data.len() - 1], Provider::Codex);
    assert_eq!(without_complete_compaction.messages.len(), 2);
    assert!(without_complete_compaction.truncated);
}
#[test]
fn null_invalid_and_limits() {
    let invalid = concat!(
        "{\"type\":null,\"payload\":null}\n",
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":null}}\n",
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":1}]}}\n",
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[null,{\"type\":\"input_text\",\"text\":\"visible\"}]}}\n",
        "{\"type\":\"response_item\""
    );
    let parsed = run(invalid, Provider::Codex);
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages[0].text, "visible");
    assert!(parsed.truncated);
    let oversized = codex("assistant", "output_text", &"x".repeat(MESSAGE_LIMIT + 1));
    let parsed = run(&oversized, Provider::Codex);
    assert!(parsed.messages.is_empty());
    assert!(parsed.truncated);
    assert_eq!(
        parse_tail(
            &vec![b'x'; TAIL_LIMIT + 1],
            0,
            [0; 16],
            Provider::Codex,
            &CancellationToken::new()
        )
        .unwrap_err(),
        Error::InvalidTail
    );
    let stop = CancellationToken::new();
    stop.cancel();
    assert_eq!(
        parse_tail(b"{}\n", 0, [0; 16], Provider::Codex, &stop).unwrap_err(),
        Error::Cancelled
    );
}

#[test]
fn limits_total_text_and_large_compaction_record() {
    let mut data = String::new();
    for i in 0..4 {
        data.push_str(&codex(
            "user",
            "input_text",
            &format!("{i}{}", "a".repeat(180_000)),
        ));
    }
    let parsed = run(&data, Provider::Codex);
    assert!(parsed.truncated);
    assert_eq!(parsed.messages.len(), 2);
    assert!(parsed.messages.iter().map(|m| m.text.len()).sum::<usize>() <= TEXT_LIMIT);
    assert!(parsed.messages[0].text.starts_with('2'));

    let summary = "A normal public summary.";
    let envelope = format!("{CONTINUATION_PREFIX} {CONTINUATION_SUMMARY}: {summary}");
    let mut compacted = codex("assistant", "output_text", summary);
    compacted.push_str(&format!(
        "{{\"type\":\"compacted\",\"payload\":{{\"message\":{}}},\"replacement_history\":\"{}\"}}\n",
        serde_json::to_string(&envelope).unwrap(),
        "x".repeat(LINE_LIMIT)
    ));
    let parsed = run(&compacted, Provider::Codex);
    assert!(parsed.messages.is_empty());
    assert!(parsed.truncated); // Compaction line exceeds the public line limit.
}

#[test]
fn handoff_shape_does_not_hide_user_specification() {
    let handoff = "## Task and constraints\n\nWorkspace: `/example/project`\nContinue work.";
    let mut data = codex("user", "input_text", handoff);
    data.push_str(&codex("assistant", "output_text", handoff));
    data.push_str(&codex(
        "assistant",
        "output_text",
        "Please review this format:\n",
    ));
    let parsed = run(&data, Provider::Codex);
    assert_eq!(parsed.messages.len(), 2);
    assert_eq!(parsed.messages[0].role, "user");
    assert_eq!(parsed.messages[1].role, "assistant");
}

#[test]
fn claude_discards_extra_part_metadata_when_canonicalizing() {
    let line = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"public\",\"recipient\":\"tool\",\"channel\":\"analysis\"}]}}\n";
    let parsed = run(line, Provider::Claude);
    assert_eq!(parsed.messages.len(), 1);
    assert_eq!(parsed.messages[0].text, "public");
}

#[test]
fn invalid_utf8_is_replaced_in_text_but_raw_codex_bytes_define_id() {
    let mut raw = b"{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"a".to_vec();
    raw.push(0xff);
    raw.extend_from_slice(b"b\"}]}}\n");
    let parsed = parse_tail(&raw, 0, [0; 16], Provider::Codex, &CancellationToken::new()).unwrap();
    assert_eq!(parsed.messages[0].text, "a\u{fffd}b");
    let mut hash = Sha256::new();
    hash.update([0; 24]);
    hash.update(&raw[..raw.len() - 1]);
    let expected = hash.finalize()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    assert_eq!(parsed.messages[0].id, expected);
}

#[test]
fn undecodable_complete_compaction_never_exposes_its_possible_handoff() {
    let summary = "A private synthetic recovery summary.";
    let envelope = format!("{CONTINUATION_PREFIX} {CONTINUATION_SUMMARY}: {summary}");
    let mut data = codex("assistant", "output_text", summary);
    data.push_str(&format!(
        "{{\"type\":\"compacted\",\"payload\":{{\"message\":{}}},\"replacement_history\":{}0{}}}\n",
        serde_json::to_string(&envelope).unwrap(),
        "[".repeat(512),
        "]".repeat(512)
    ));
    match parse_tail(
        data.as_bytes(),
        0,
        [0; 16],
        Provider::Codex,
        &CancellationToken::new(),
    ) {
        Ok(parsed) => assert!(parsed.messages.is_empty()),
        Err(error) => assert_eq!(error, Error::InvalidTail),
    }
    let malformed =
        codex("assistant", "output_text", summary) + "{\"type\":\"compacted\", invalid}\n";
    assert_eq!(
        parse_tail(
            malformed.as_bytes(),
            0,
            [0; 16],
            Provider::Codex,
            &CancellationToken::new()
        )
        .unwrap_err(),
        Error::InvalidTail
    );
}
