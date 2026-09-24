use bytes::Bytes;
use hmux_protocol::{
    actions::{self, ResponseContext},
    legacy,
    protobuf::{self as pb, types as p, Direction, Error},
    wire,
};
use prost::Message;

#[test]
fn request_shapes_and_workspace_revision_are_exact() {
    use p::Operation as O;
    assert!(actions::request_from_json(O::Create, br#"{"profile":null,"name":null}"#).is_ok());
    assert!(actions::request_from_json(O::Create, br#"{"profile":"shell","extra":1}"#).is_err());
    assert!(actions::request_from_json(O::Create, br#"{"profile":"a","profile":"b"}"#).is_err());
    assert!(actions::request_from_json(O::Hidden, br#"{}"#).is_err());
    assert!(actions::request_from_json(O::Hidden, br#"{"hidden":false}"#).is_ok());
    assert!(actions::request_from_json(O::Providers, b"null").is_ok());
    assert!(actions::request_from_json(O::Providers, b"{}").is_ok());
    assert!(actions::request_from_json(O::Providers, br#"{"x":1}"#).is_err());
    let raw=br#"{"change":{"operation_id":"1234567890abcdef","revision":18446744073709551615,"base":[],"tabs":[]}}"#;
    let payload = actions::request_from_json(O::Workspace, raw).unwrap();
    let request = p::Request {
        id: "id".into(),
        operation: O::Workspace as i32,
        session: None,
        payload: Some(payload),
    };
    let restored: serde_json::Value =
        serde_json::from_slice(&actions::request_payload(&request).unwrap()).unwrap();
    assert_eq!(restored["change"]["revision"].as_u64(), Some(u64::MAX));
    assert!(actions::request_from_json(O::Workspace,br#"{"change":{"operation_id":"1234567890abcdef","revision":18446744073709551616,"base":[],"tabs":[]}}"#).is_err());
}
#[test]
fn response_context_and_strict_json_prevent_shape_inference() {
    let create = actions::response_from_json(
        br#"{"id":"$1","created_at":42,"reused":false}"#,
        ResponseContext::Operation(p::Operation::Create),
    )
    .unwrap()
    .unwrap();
    let response = p::Response {
        id: "id".into(),
        error: String::new(),
        result: Some(create),
    };
    assert!(actions::response_matches(
        &response,
        ResponseContext::Operation(p::Operation::Create)
    ));
    assert!(!actions::response_matches(
        &response,
        ResponseContext::Operation(p::Operation::Profiles)
    ));
    assert!(actions::response_from_json(
        br#"{"ok":true,"ok":true}"#,
        ResponseContext::Operation(p::Operation::Alias)
    )
    .is_err());
    assert!(
        actions::response_from_json(br#"{"ok":true,"x":1}"#, ResponseContext::TerminalOpen)
            .is_err()
    );
    assert!(actions::response_from_json(
        br#"{"0":{"id":"shell","label":"Shell"}}"#,
        ResponseContext::Operation(p::Operation::Profiles)
    )
    .is_err());
    let msg =
        wire::Message::decode(br#"{"type":"response","id":"id","payload":{"ok":true}}"#).unwrap();
    assert!(legacy::from_json(msg, Direction::ToGateway).is_err());
    let err =
        wire::Message::decode(br#"{"type":"response","id":"id","error":"busy","payload":null}"#)
            .unwrap();
    assert!(legacy::from_json(err, Direction::ToGateway).is_err());
}
#[test]
fn dense_profile_json_is_rejected_before_vector_expansion() {
    let mut raw = Vec::with_capacity(900_002);
    raw.push(b'[');
    for i in 0..300_000 {
        if i > 0 {
            raw.push(b',')
        }
        raw.extend_from_slice(b"{}");
    }
    raw.push(b']');
    assert!(matches!(
        actions::response_from_json(&raw, ResponseContext::Operation(p::Operation::Profiles)),
        Err(Error::Size)
    ));
}
#[test]
fn reserved_json_tag_and_duplicate_oneof_are_rejected_predecode() {
    let req = p::Request {
        id: "id".into(),
        operation: p::Operation::Profiles as i32,
        session: None,
        payload: Some(p::request::Payload::Empty(p::Empty {})),
    };
    let mut raw = p::Envelope {
        version: pb::VERSION,
        body: Some(p::envelope::Body::Request(Box::new(req))),
    }
    .encode_to_vec();
    // request is length-delimited; build a raw envelope with an obsolete field 4.
    let mut bad_request = vec![0x0a, 2, b'i', b'd', 0x10, 1, 0x52, 0];
    bad_request.extend_from_slice(&[0x22, 0]);
    let mut frame = vec![0x08, 2, 0x5a, bad_request.len() as u8];
    frame.extend_from_slice(&bad_request);
    assert!(pb::decode(Bytes::from(frame), Direction::ToHome).is_err());
    // Append a second payload occurrence to a valid request submessage.
    raw.extend_from_slice(&[0x5a, 8, 0x0a, 2, b'i', b'd', 0x10, 1, 0x52, 0]);
    assert!(pb::decode(Bytes::from(raw), Direction::ToHome).is_err());
}

#[test]
fn provider_in_band_error_and_stage_file_limits_are_distinct() {
    use p::response::Result as R;
    let result = actions::response_from_json(
        br#"{"providers":null,"job":{"state":"failed","log":["reason"]},"error":"login failed"}"#,
        ResponseContext::Operation(p::Operation::ProviderJob),
    )
    .unwrap()
    .unwrap();
    let R::Providers(provider) = result else {
        panic!("providers")
    };
    assert_eq!(provider.error.as_deref(), Some("login failed"));
    let response = p::Response {
        id: "id".into(),
        error: String::new(),
        result: Some(R::Providers(provider)),
    };
    assert!(actions::response_matches(
        &response,
        ResponseContext::Operation(p::Operation::ProviderJob)
    ));
    assert!(actions::response_payload(&response).is_ok());
    let stage=br#"{"protocol_version":1,"request_id":"0123456789abcdef0123456789abcdef","stage_id":"stage","session":{"id":"$1","created_at":42},"expires_at_unix":100,"files":[{"index":0,"path":"/tmp/file","size":3,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}"#;
    let result = actions::response_from_json(stage, ResponseContext::Upload)
        .unwrap()
        .unwrap();
    let response = p::Response {
        id: "id".into(),
        error: String::new(),
        result: Some(result),
    };
    assert!(actions::response_matches(
        &response,
        ResponseContext::Upload
    ));
    assert!(actions::response_payload(&response).is_ok());
    if let Some(R::Staged(mut s)) = response.result {
        s.files[0].path = "x".repeat(4097);
        let invalid = p::Response {
            id: "id".into(),
            error: String::new(),
            result: Some(R::Staged(s)),
        };
        assert!(actions::validate_response(&invalid).is_err());
    } else {
        panic!("staged")
    }
}

#[test]
fn special_response_envelopes_enforce_their_roles() {
    let ok = p::Response {
        id: "id".into(),
        error: String::new(),
        result: Some(p::response::Result::Ok(p::Empty {})),
    };
    let frame = |body| p::Envelope {
        version: pb::VERSION,
        body: Some(body),
    };
    assert!(pb::encode(
        &frame(p::envelope::Body::Response(ok.clone())),
        Direction::ToGateway
    )
    .is_ok());
    assert!(pb::encode(
        &frame(p::envelope::Body::TerminalExit(ok.clone())),
        Direction::ToGateway
    )
    .is_err());
    assert!(pb::encode(
        &frame(p::envelope::Body::RefreshResult(ok.clone())),
        Direction::ToGateway
    )
    .is_err());
    assert!(pb::encode(
        &frame(p::envelope::Body::UploadComplete(ok.clone())),
        Direction::ToGateway
    )
    .is_err());
    assert!(pb::encode(
        &frame(p::envelope::Body::UploadError(ok)),
        Direction::ToGateway
    )
    .is_err());
    let empty = p::Response {
        id: "id".into(),
        error: String::new(),
        result: None,
    };
    assert!(pb::encode(
        &frame(p::envelope::Body::TerminalExit(empty.clone())),
        Direction::ToGateway
    )
    .is_ok());
    assert!(pb::encode(
        &frame(p::envelope::Body::RefreshResult(empty.clone())),
        Direction::ToGateway
    )
    .is_ok());
    assert!(pb::encode(
        &frame(p::envelope::Body::UploadComplete(empty)),
        Direction::ToGateway
    )
    .is_err());
}

#[test]
fn every_action_payload_round_trips_on_the_binary_wire() {
    use p::Operation as O;
    let cases: [(O,&[u8]);12]=[
        (O::Profiles,b""),(O::Create,br#"{"profile":null,"name":"demo"}"#),
        (O::Alias,br#"{"alias":"Renamed"}"#),(O::Hidden,br#"{"hidden":false}"#),
        (O::Conversation,b""),(O::Workspace,br#"{"change":{"operation_id":"1234567890abcdef","revision":18446744073709551615,"base":[],"tabs":[]}}"#),
        (O::Providers,b"{}"),(O::ProviderKey,br#"{"provider":"codex","key":""}"#),
        (O::ProviderJobStart,br#"{"provider":"codex","action":"login"}"#),
        (O::ProviderJob,br#"{"provider":"codex"}"#),
        (O::ProviderJobInput,br#"{"provider":"codex","text":"otp"}"#),
        (O::ProviderJobCancel,br#"{"provider":"codex"}"#),
    ];
    for (op, raw) in cases {
        let session = if matches!(op, O::Alias | O::Hidden | O::Conversation) {
            Some(p::Session {
                id: "$1".into(),
                created_at: 42,
            })
        } else {
            None
        };
        let request = p::Request {
            id: "id".into(),
            operation: op as i32,
            session,
            payload: Some(actions::request_from_json(op, raw).unwrap()),
        };
        let envelope = p::Envelope {
            version: pb::VERSION,
            body: Some(p::envelope::Body::Request(Box::new(request))),
        };
        let decoded = pb::decode(
            pb::encode(&envelope, Direction::ToHome).unwrap(),
            Direction::ToHome,
        )
        .unwrap();
        assert!(decoded == envelope);
        let Some(p::envelope::Body::Request(request)) = decoded.body else {
            panic!("request")
        };
        let json = actions::request_payload(&request).unwrap();
        if op == O::Workspace {
            let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
            assert_eq!(value["change"]["revision"].as_u64(), Some(u64::MAX));
        }
    }
}

#[test]
fn every_reply_result_round_trips_on_the_binary_wire() {
    use p::Operation as O;
    let stage=br#"{"protocol_version":1,"request_id":"0123456789abcdef0123456789abcdef","stage_id":"stage","session":{"id":"$1","created_at":42},"expires_at_unix":100,"files":[{"index":0,"path":"/tmp/file","size":3,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}"#;
    let cases: [(ResponseContext, &[u8]); 8] = [
        (
            ResponseContext::Operation(O::Profiles),
            br#"[{"id":"shell","label":"Shell"}]"#,
        ),
        (
            ResponseContext::Operation(O::Create),
            br#"{"id":"$1","created_at":42,"reused":false}"#,
        ),
        (ResponseContext::Operation(O::Alias), br#"{"ok":true}"#),
        (
            ResponseContext::Operation(O::Conversation),
            br#"{"session_id":"$1","created_at":42,"status":"ready"}"#,
        ),
        (
            ResponseContext::Operation(O::Conversation),
            br#"{"session_id":"$1","created_at":42,"status":"ready","messages":[]}"#,
        ),
        (
            ResponseContext::Operation(O::Workspace),
            br#"{"version":1,"initialized":false,"revision":18446744073709551615,"tabs":[]}"#,
        ),
        (
            ResponseContext::Operation(O::ProviderJob),
            br#"{"job":{"state":"failed","log":["reason"]},"error":"login failed"}"#,
        ),
        (ResponseContext::Upload, stage),
    ];
    for (context, raw) in cases {
        let result = actions::response_from_json(raw, context).unwrap();
        let response = p::Response {
            id: "id".into(),
            error: String::new(),
            result,
        };
        assert!(actions::response_matches(&response, context));
        let envelope = p::Envelope {
            version: pb::VERSION,
            body: Some(p::envelope::Body::Response(response.clone())),
        };
        let decoded = pb::decode(
            pb::encode(&envelope, Direction::ToGateway).unwrap(),
            Direction::ToGateway,
        )
        .unwrap();
        assert!(decoded == envelope);
        let Some(p::envelope::Body::Response(response)) = decoded.body else {
            panic!("response")
        };
        let json = actions::response_payload(&response).unwrap();
        if context == ResponseContext::Operation(O::Workspace) {
            let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
            assert_eq!(v["revision"].as_u64(), Some(u64::MAX));
        }
        if context == ResponseContext::Operation(O::Conversation) {
            let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
            assert_eq!(
                v.get("messages").is_some_and(serde_json::Value::is_array),
                raw.windows(b"\"messages\"".len())
                    .any(|w| w == b"\"messages\"")
            );
        }
    }
}
