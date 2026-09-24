use hmux_protocol::wire::{CodecError, Message, MAX_DATA, MAX_MESSAGE, MAX_UPLOAD_CHUNK};
use serde::Deserialize;
use serde_json::value::RawValue;

#[derive(Deserialize)]
struct Fixture {
    name: String,
    input: String,
    go_accept: bool,
    rust_accept: bool,
    normalized: Option<String>,
    delta: Option<String>,
}

#[test]
fn go_wire_corpus() {
    let fixtures: Vec<Fixture> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/wire-v1/messages.json"
    ))
    .unwrap();
    let output = std::env::var_os("HMUX_RUST_WIRE_OUTPUT").map(std::path::PathBuf::from);
    if let Some(dir) = &output {
        std::fs::create_dir_all(dir).unwrap();
    }
    for fixture in fixtures {
        assert!(
            fixture.go_accept == fixture.rust_accept || fixture.delta.is_some(),
            "{}: undocumented delta",
            fixture.name
        );
        let decoded = Message::decode(fixture.input.as_bytes());
        assert_eq!(
            decoded.is_ok(),
            fixture.rust_accept,
            "{}: {:?}",
            fixture.name,
            decoded
        );
        if let Ok(message) = decoded {
            let encoded = message.encode().unwrap();
            assert_eq!(
                std::str::from_utf8(&encoded).unwrap(),
                fixture.normalized.unwrap(),
                "{}",
                fixture.name
            );
            if let Some(dir) = &output {
                std::fs::write(dir.join(format!("{}.json", fixture.name)), encoded).unwrap();
            }
        }
    }
}

#[test]
fn byte_and_field_bounds_match_go_transport_limits() {
    for (kind, limit) in [("data", MAX_DATA), ("upload-data", MAX_UPLOAD_CHUNK)] {
        let mut message = Message {
            kind: kind.to_owned(),
            data: vec![0; limit],
            ..Message::default()
        };
        let encoded = message.encode().unwrap();
        assert_eq!(Message::decode(&encoded).unwrap().data.len(), limit);
        message.data.push(0);
        assert_eq!(message.encode().unwrap_err(), CodecError::FieldLimit);
        // Bypass encode to verify an untrusted sender cannot bypass decode limits.
        assert_eq!(
            Message::decode(&serde_json::to_vec(&message).unwrap()).unwrap_err(),
            CodecError::FieldLimit
        );
    }
    assert_eq!(
        Message::decode(&vec![b' '; MAX_MESSAGE + 1]).unwrap_err(),
        CodecError::MessageLimit
    );
    for length in [16, 17] {
        let message = Message {
            capabilities: vec!["x".to_owned(); length],
            ..Message::default()
        };
        assert_eq!(message.validate_fields().is_ok(), length == 16);
    }
}

#[test]
fn invalid_inputs_never_authorize_or_echo_data_in_errors() {
    for input in [r#"{"password":"do-not-log"}"#, r#"{"data":"do-not-log"}"#] {
        assert_eq!(
            Message::decode(input.as_bytes()).unwrap_err().to_string(),
            "invalid web frame"
        );
    }
    // Geometry/identity belongs to the operation, not this framing codec.
    let message = Message::decode(br#"{"session":{"id":"arbitrary","created_at":0}}"#).unwrap();
    assert!(!message.session.is_valid());
}

#[test]
fn encoded_limit_counts_html_expansion_without_echoing_payload() {
    let mut message = Message {
        kind: "response".to_owned(),
        payload: Some(RawValue::from_string("\"\"".to_owned()).unwrap()),
        ..Message::default()
    };
    let baseline = message.encode().unwrap().len();
    let room = MAX_MESSAGE - baseline;
    let body = "&".repeat(room / 6) + &"a".repeat(room % 6);
    message.payload = Some(RawValue::from_string(format!("\"{body}\"")).unwrap());
    assert!(message.payload.as_ref().unwrap().get().len() < MAX_MESSAGE);
    let encoded = message.encode().unwrap();
    assert_eq!(encoded.len(), MAX_MESSAGE);
    assert!(encoded.windows(6).any(|bytes| bytes == br"\u0026"));

    message.payload = Some(RawValue::from_string(format!("\"{body}&\"")).unwrap());
    let error = message.encode().unwrap_err();
    assert_eq!(error, CodecError::MessageLimit);
    assert_eq!(error.to_string(), "web frame exceeds 4 MiB");
}
