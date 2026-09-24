use hmux_protocol::{
    actions::ResponseContext,
    legacy,
    protobuf::{self as pb, types as p, Direction as D},
    wire,
};
use serde_json::json;

#[test]
fn actions_round_trip_with_explicit_response_context() {
    let id = "0123456789abcdef0123456789abcdef";
    let identity = json!({"id":"$1","created_at":9007199254740993i64});
    let requests = [
        ("profiles", json!(null)),
        ("create", json!({})),
        ("alias", json!({"alias":null})),
        ("hidden", json!({"hidden":false})),
        ("conversation", json!(null)),
        ("workspace", json!({"change":null})),
        ("providers", json!({})),
        ("provider-key", json!({})),
        ("provider-job-start", json!({})),
        ("provider-job", json!({})),
        ("provider-job-input", json!({})),
        ("provider-job-cancel", json!({})),
    ];
    for (operation, payload) in requests {
        let value = json!({"type":"request","id":id,"operation":operation,"session":identity,"payload":payload});
        let original = wire::Message::decode(value.to_string().as_bytes()).unwrap();
        let proto = legacy::from_json(original, D::ToHome).unwrap();
        let decoded = pb::decode(pb::encode(&proto, D::ToHome).unwrap(), D::ToHome).unwrap();
        let restored = legacy::to_json(decoded, D::ToHome).unwrap();
        assert_eq!(restored.operation, operation);
    }
    let result = json!({"type":"response","id":id,"payload":{"id":"$1","created_at":9007199254740993i64,"reused":false}});
    let msg = wire::Message::decode(result.to_string().as_bytes()).unwrap();
    assert!(legacy::from_json(msg.clone(), D::ToGateway).is_err());
    let envelope = legacy::from_json_with_context(
        msg,
        D::ToGateway,
        Some(ResponseContext::Operation(p::Operation::Create)),
    )
    .unwrap();
    let raw = pb::encode(&envelope, D::ToGateway).unwrap();
    let restored = legacy::to_json(pb::decode(raw, D::ToGateway).unwrap(), D::ToGateway).unwrap();
    assert_eq!(
        restored.payload.unwrap().get(),
        r#"{"id":"$1","created_at":9007199254740993,"reused":false}"#
    );
    let error = wire::Message::decode(
        format!(r#"{{"type":"response","id":"{id}","error":"busy"}}"#).as_bytes(),
    )
    .unwrap();
    assert!(legacy::from_json(error, D::ToGateway).is_ok());
}

#[test]
fn usage_tags_and_direction_cannot_cross_provider_slots() {
    for raw in [
        r#"{"provider":"codex"}"#,
        r#"["codex"]"#,
        r#"{"provider":"codex","provider":"claude"}"#,
    ] {
        let message =
            wire::Message::decode(format!(r#"{{"type":"usage","payload":{raw}}}"#).as_bytes())
                .unwrap();
        assert!(legacy::from_json(message, D::ToGateway).is_err());
    }
    let mut snapshot = hmux_protocol::snapshots::usage_to_proto(
        hmux_usage::transport::decode(br#"{"schema":1,"provider":"codex","generated_at_utc":"2026-09-24T00:00:00Z","status":{"state":"ok"}}"#).unwrap()
    ).unwrap();
    let mut child = snapshot.clone();
    child.provider = p::Provider::Claude as i32;
    snapshot.sources.push(p::UsageSource {
        name: "cli".into(),
        snapshot: Some(child),
    });
    let message = p::Envelope {
        version: pb::VERSION,
        body: Some(p::envelope::Body::Usage(Box::new(snapshot))),
    };
    assert!(pb::encode(&message, D::ToGateway).is_err());
    let input = wire::Message::decode(br#"{"type":"input","id":"view","data":"eA=="}"#).unwrap();
    assert!(matches!(
        legacy::from_json(input, D::ToGateway),
        Err(pb::Error::Direction)
    ));
    let unknown = wire::Message::decode(br#"{"type":"future-type"}"#).unwrap();
    assert!(matches!(
        legacy::from_json(unknown, D::ToGateway),
        Err(pb::Error::Unsupported)
    ));
}
