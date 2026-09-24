use bytes::Bytes;
use hmux_protocol::{
    protobuf::{self as pb, types as p, Direction as D, Error},
    wire,
};
use prost::Message;

fn envelope(body: p::envelope::Body) -> p::Envelope {
    p::Envelope {
        version: pb::VERSION,
        body: Some(body),
    }
}
fn output(size: usize) -> p::Envelope {
    envelope(p::envelope::Body::TerminalOutput(p::Data {
        id: "synthetic-view".into(),
        data: Bytes::from(vec![b'x'; size]),
    }))
}

#[test]
fn binary_output_reduces_wire_bytes_without_base64() {
    for size in [1, 64, 16384] {
        let message = output(size);
        let bytes = pb::encode(&message, D::ToGateway).unwrap();
        let decoded = pb::decode(bytes.clone(), D::ToGateway).unwrap();
        assert!(decoded == message);
        let legacy = wire::Message {
            kind: "data".into(),
            id: "synthetic-view".into(),
            data: vec![b'x'; size],
            ..Default::default()
        }
        .encode()
        .unwrap();
        assert!(bytes.len() < legacy.len());
        if size == 16384 {
            eprintln!(
                "synthetic 16-KiB output: protobuf={} JSON-v1={} bytes",
                bytes.len(),
                legacy.len()
            );
        }
        assert!(matches!(
            pb::decode(bytes, D::ToHome),
            Err(Error::Direction)
        ));
    }
}

#[test]
fn terminal_output_fits_one_ack_credit_but_input_may_be_larger() {
    assert!(pb::encode(&output(16384), D::ToGateway).is_ok());
    for size in [16385, 32768] {
        assert!(pb::encode(&output(size), D::ToGateway).is_err());
        assert!(pb::decode(output(size).encode_to_vec().into(), D::ToGateway).is_err());
    }
    let input = envelope(p::envelope::Body::TerminalInput(p::Data {
        id: "view".into(),
        data: Bytes::from(vec![b'x'; 32768]),
    }));
    assert!(pb::encode(&input, D::ToHome).is_ok());
}
#[test]
fn handshake_requires_explicit_selection() {
    assert_eq!(
        pb::negotiate(Some("hmux-home.pb.v2")),
        Err(Error::Unsupported)
    );
    assert_eq!(pb::negotiate(None), Ok(pb::Negotiated::JsonV1));
    assert_eq!(
        pb::negotiate(Some(pb::SUBPROTOCOL)),
        Ok(pb::Negotiated::ProtobufV2)
    );
    assert_eq!(
        pb::negotiate(Some("hmux-home.pb.v99")),
        Err(Error::Unsupported)
    );
}
#[test]
fn bounds_and_ambiguous_frames_fail_before_dispatch() {
    let good = pb::encode(&output(10), D::ToGateway).unwrap();
    for length in 0..good.len() {
        assert!(pb::decode(good.slice(..length), D::ToGateway).is_err());
    }
    let mut repeated = good.to_vec();
    repeated.extend_from_slice(&good);
    assert!(matches!(
        pb::decode(Bytes::from(repeated), D::ToGateway),
        Err(Error::Fields)
    ));
    let mut unknown = good.to_vec();
    unknown.extend_from_slice(&[0xf8, 0x03, 0x00]);
    assert!(matches!(
        pb::decode(Bytes::from(unknown), D::ToGateway),
        Err(Error::Unsupported)
    ));
    let hello = envelope(p::envelope::Body::Hello(p::Hello {
        capabilities: vec!["x".into(); 17],
    }));
    assert!(pb::decode(Bytes::from(hello.encode_to_vec()), D::ToGateway).is_err());
    assert!(matches!(
        pb::decode(Bytes::from(vec![0; wire::MAX_MESSAGE + 1]), D::ToGateway),
        Err(Error::Size)
    ));
    assert!(pb::decode(
        Bytes::from(output(wire::MAX_DATA + 1).encode_to_vec()),
        D::ToGateway
    )
    .is_err());
    let missing = p::Envelope {
        version: 2,
        body: None,
    };
    assert!(pb::decode(Bytes::from(missing.encode_to_vec()), D::ToGateway).is_err());
}
#[test]
fn unknown_operation_and_bad_geometry_rejected() {
    for operation in [0, 999] {
        let request = envelope(p::envelope::Body::Request(Box::new(p::Request {
            id: "request".into(),
            operation,
            session: None,
            payload: Some(p::request::Payload::Empty(p::Empty {})),
        })));
        assert!(pb::decode(Bytes::from(request.encode_to_vec()), D::ToHome).is_err());
    }
    let open = envelope(p::envelope::Body::TerminalOpen(p::TerminalOpen {
        id: "view".into(),
        session: Some(p::Session {
            id: "$1".into(),
            created_at: 42,
        }),
        cols: 1,
        rows: 24,
        capabilities: vec![],
    }));
    assert!(pb::encode(&open, D::ToHome).is_err());
}
#[test]
fn uploads_keep_exact_identity_and_quotas() {
    let id = "0123456789abcdef0123456789abcdef";
    let make = |count: u32, size: i64| {
        envelope(p::envelope::Body::UploadStart(p::UploadStart {
            id: id.into(),
            header: Some(p::UploadHeader {
                protocol_version: 1,
                request_id: id.into(),
                session: Some(p::Session {
                    id: "$1".into(),
                    created_at: 42,
                }),
                file_count: count,
                total_bytes: size,
                files: vec![p::FileHeader {
                    index: 0,
                    size,
                    extension: "txt".into(),
                }],
            }),
        }))
    };
    let good = make(1, 32 << 20);
    let raw = pb::encode(&good, D::ToHome).unwrap();
    assert!(pb::decode(raw, D::ToHome).is_ok());
    for (count, size) in [(0, 1), (2, 1), (1, 0), (1, 33 << 20)] {
        assert!(pb::encode(&make(count, size), D::ToHome).is_err());
    }
}

#[test]
fn oversized_wire_integers_cannot_wrap_into_valid_values() {
    use prost::encoding::{encode_key, encode_varint, WireType};
    fn integer(tag: u32, value: u64) -> Vec<u8> {
        let mut raw = Vec::new();
        encode_key(tag, WireType::Varint, &mut raw);
        encode_varint(value, &mut raw);
        raw
    }
    fn nested(tag: u32, value: &[u8]) -> Vec<u8> {
        let mut raw = Vec::new();
        encode_key(tag, WireType::LengthDelimited, &mut raw);
        encode_varint(value.len() as u64, &mut raw);
        raw.extend_from_slice(value);
        raw
    }
    let hello = nested(10, &[]);
    for version in [(1u64 << 32) + 2, u64::MAX] {
        let raw = [integer(1, version), hello.clone()].concat();
        assert!(matches!(
            pb::decode(raw.into(), D::ToGateway),
            Err(Error::Fields)
        ));
    }
    for (tag, good) in [(2, 80), (3, 24)] {
        let mut resize = nested(1, b"view");
        resize.extend(integer(2, if tag == 2 { (1u64 << 32) + good } else { 80 }));
        resize.extend(integer(3, if tag == 3 { (1u64 << 32) + good } else { 24 }));
        let raw = [integer(1, 2), nested(16, &resize)].concat();
        assert!(matches!(
            pb::decode(raw.into(), D::ToHome),
            Err(Error::Fields)
        ));
    }
    let request = [nested(1, b"request"), integer(2, (1u64 << 32) + 1)].concat();
    let raw = [integer(1, 2), nested(11, &request)].concat();
    assert!(matches!(
        pb::decode(raw.into(), D::ToHome),
        Err(Error::Fields)
    ));
    let usage = integer(4, (1u64 << 32) + 1);
    let raw = [integer(1, 2), nested(36, &usage)].concat();
    assert!(matches!(
        pb::decode(raw.into(), D::ToGateway),
        Err(Error::Fields)
    ));

    // int64 identities must retain their full range; they are not uint32 fields.
    let open = envelope(p::envelope::Body::TerminalOpen(p::TerminalOpen {
        id: "view".into(),
        session: Some(p::Session {
            id: "$1".into(),
            created_at: i64::MAX,
        }),
        cols: 80,
        rows: 24,
        capabilities: vec![],
    }));
    let raw = pb::encode(&open, D::ToHome).unwrap();
    assert!(pb::decode(raw, D::ToHome).unwrap() == open);
}

fn field_bytes(tag: u32, value: &[u8]) -> Vec<u8> {
    use prost::encoding::{encode_key, encode_varint, WireType};
    let mut raw = Vec::new();
    encode_key(tag, WireType::LengthDelimited, &mut raw);
    encode_varint(value.len() as u64, &mut raw);
    raw.extend_from_slice(value);
    raw
}
fn field_integer(tag: u32, value: u64) -> Vec<u8> {
    use prost::encoding::{encode_key, encode_varint, WireType};
    let mut raw = Vec::new();
    encode_key(tag, WireType::Varint, &mut raw);
    encode_varint(value, &mut raw);
    raw
}
fn snapshot_frame(tag: u32, body: &[u8]) -> Bytes {
    [field_integer(1, pb::VERSION.into()), field_bytes(tag, body)]
        .concat()
        .into()
}
#[test]
fn retired_json_snapshot_tags_are_never_reinterpreted() {
    for tag in [23, 24] {
        assert_eq!(
            pb::decode(snapshot_frame(tag, &[]), D::ToGateway).err(),
            Some(Error::Unsupported)
        );
    }
}
#[test]
fn typed_catalog_allows_more_than_255_sessions_and_keeps_envelopes_small() {
    let catalog = hmux_model::Catalog {
        protocol_version: 1,
        sessions: Some(
            (0..300)
                .map(|i| hmux_model::Session {
                    id: format!("${i}"),
                    created_at: 42,
                    window_names: Some(vec![]),
                    ..Default::default()
                })
                .collect(),
        ),
        ..Default::default()
    };
    let proto = hmux_protocol::snapshots::catalog_to_proto(catalog.clone()).unwrap();
    let message = envelope(p::envelope::Body::Catalog(Box::new(proto)));
    let raw = pb::encode(&message, D::ToGateway).unwrap();
    let decoded = pb::decode(raw, D::ToGateway).unwrap();
    let Some(p::envelope::Body::Catalog(decoded)) = decoded.body else {
        panic!("catalog")
    };
    assert_eq!(
        hmux_protocol::snapshots::catalog_from_proto(*decoded).unwrap(),
        catalog
    );
    // Typed controls must not enlarge every terminal frame/queue item.
    assert!(size_of::<p::Envelope>() <= 160);
}
#[test]
fn snapshot_preflight_bounds_tree_expansion_and_nested_ambiguity() {
    let identity = [field_bytes(1, b"$1"), field_integer(2, 42)].concat();
    let good_session = field_bytes(1, &identity);
    // Each session is small on the wire; its repeated empty strings still own
    // Vec slots. Reject their aggregate before prost materializes the tree.
    let windows = field_bytes(1, b"").repeat(40);
    let session = [good_session.clone(), field_bytes(9, &windows)].concat();
    let sessions = field_bytes(1, &session).repeat(10_000);
    let catalog = field_bytes(3, &sessions);
    assert!(catalog.len() < wire::MAX_MESSAGE);
    assert_eq!(
        pb::decode(snapshot_frame(35, &catalog), D::ToGateway).err(),
        Some(Error::Size)
    );
    // Duplicate singular values, unknown nested fields and non-boolean varints.
    for invalid in [
        [good_session.clone(), good_session.clone()].concat(),
        [good_session.clone(), field_integer(26, 1)].concat(),
        [good_session, field_integer(4, 2)].concat(),
    ] {
        let catalog = field_bytes(3, &field_bytes(1, &invalid));
        assert!(pb::decode(snapshot_frame(35, &catalog), D::ToGateway).is_err());
    }
    // Usage source bundles allow exactly one child level.
    let child = field_bytes(17, &field_bytes(1, b"cli"));
    let source = [field_bytes(1, b"cli"), field_bytes(2, &child)].concat();
    assert_eq!(
        pb::decode(snapshot_frame(36, &field_bytes(17, &source)), D::ToGateway).err(),
        Some(Error::Unsupported)
    );
}
#[test]
fn snapshot_numbers_reject_wrong_wire_types_and_non_finite_usage() {
    let usage = field_integer(6, 1); // burn_rate is double, not varint
    assert_eq!(
        pb::decode(snapshot_frame(36, &usage), D::ToGateway).err(),
        Some(Error::Malformed)
    );
    let usage = hmux_usage::transport::decode(br#"{"schema":1,"provider":"codex","generated_at_utc":"2026-09-24T00:00:00Z","status":{"state":"ok"}}"#).unwrap();
    let mut proto = hmux_protocol::snapshots::usage_to_proto(usage).unwrap();
    for value in [f64::NAN, f64::INFINITY, -1.0] {
        proto.burn_rate_per_min = value;
        let message = envelope(p::envelope::Body::Usage(Box::new(proto.clone())));
        assert!(pb::encode(&message, D::ToGateway).is_err());
        assert!(pb::decode(message.encode_to_vec().into(), D::ToGateway).is_err());
    }
}
