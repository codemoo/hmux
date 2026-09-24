//! Synthetic codec-only wall-time samples. Not a gateway RSS/CPU/latency benchmark.
use bytes::Bytes;
use hmux_protocol::{
    legacy,
    protobuf::{self as pb, types as p, Direction},
    wire,
};
use serde_json::json;
use std::{hint::black_box, time::Instant};

#[derive(Clone, Copy)]
enum Codec {
    Json,
    Protobuf,
}
impl Codec {
    fn name(self) -> &'static str {
        match self {
            Self::Json => "json-v1-with-adapter",
            Self::Protobuf => "protobuf-v2",
        }
    }
    fn encode(self, envelope: &p::Envelope, direction: Direction) -> Bytes {
        match self {
            Self::Json => legacy::to_json(envelope.clone(), direction)
                .unwrap()
                .encode()
                .unwrap()
                .into(),
            Self::Protobuf => pb::encode(envelope, direction).unwrap(),
        }
    }
    fn decode(self, raw: &Bytes, direction: Direction) -> p::Envelope {
        match self {
            Self::Json => {
                legacy::from_json(wire::Message::decode(raw).unwrap(), direction).unwrap()
            }
            Self::Protobuf => pb::decode(raw.clone(), direction).unwrap(),
        }
    }
}
struct Workload {
    operation: &'static str,
    direction: Direction,
    payload_bytes: usize,
    body: p::envelope::Body,
}
fn workloads() -> Vec<Workload> {
    use p::envelope::Body;
    let data = |size| p::Data {
        id: "synthetic-view".into(),
        data: (0..size).map(|i| (i * 73 + 19) as u8).collect(),
    };
    let mut values: Vec<_> = [1, 64, 16_384]
        .into_iter()
        .map(|size| Workload {
            operation: "terminal-output",
            direction: Direction::ToGateway,
            payload_bytes: size,
            body: Body::TerminalOutput(data(size)),
        })
        .collect();
    values.extend([
        Workload {
            operation: "terminal-input",
            direction: Direction::ToHome,
            payload_bytes: wire::MAX_DATA,
            body: Body::TerminalInput(data(wire::MAX_DATA)),
        },
        Workload {
            operation: "upload-data",
            direction: Direction::ToHome,
            payload_bytes: wire::MAX_UPLOAD_CHUNK,
            body: Body::UploadData(data(wire::MAX_UPLOAD_CHUNK)),
        },
        Workload {
            operation: "output-ack",
            direction: Direction::ToHome,
            payload_bytes: 0,
            body: Body::OutputAck(p::Ack {
                id: "synthetic-view".into(),
                received: 16_384,
            }),
        },
        Workload {
            operation: "profiles-request",
            direction: Direction::ToHome,
            payload_bytes: 0,
            body: Body::Request(Box::new(p::Request {
                id: "synthetic-request".into(),
                operation: p::Operation::Profiles as i32,
                session: None,
                payload: Some(p::request::Payload::Empty(p::Empty {})),
            })),
        },
    ]);
    // Typed snapshot transport with the same semantic catalog at the v1 boundary.
    let catalog = serde_json::to_vec(&json!({
        "protocol_version": 1, "generated_at": "2026-01-01T00:00:00Z",
        "sessions": (0..100).map(|i| json!({
            "id": format!("${i}"), "name": format!("synthetic-{i}"),
            "created_at": 1_767_225_600_i64 + i, "kind": "shell",
            "current_path": "/synthetic/work", "width": 120, "height": 40,
        })).collect::<Vec<_>>(),
    }))
    .unwrap();
    values.push(Workload {
        operation: "catalog-typed-100",
        direction: Direction::ToGateway,
        payload_bytes: catalog.len(),
        body: Body::Catalog(Box::new(
            hmux_protocol::snapshots::catalog_from_json(&catalog).unwrap(),
        )),
    });
    values
}
fn elapsed(iterations: usize, mut operation: impl FnMut()) -> u128 {
    let start = Instant::now();
    for _ in 0..iterations {
        operation();
    }
    start.elapsed().as_nanos()
}
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let iterations = match args.as_slice() {
        [] => 10_000,
        [count] => count.parse::<usize>().unwrap_or(0),
        _ => 0,
    };
    if !(100..=1_000_000).contains(&iterations) {
        eprintln!("usage: codec_bench [iterations: 100..1000000]");
        std::process::exit(2);
    }
    let mut samples = Vec::new();
    for workload in workloads() {
        let direction = workload.direction;
        let envelope = p::Envelope {
            version: pb::VERSION,
            body: Some(workload.body),
        };
        // Alternate codec order over seven rounds; keep only one output alive
        // at a time. Inputs, WS transport, allocator profiling and IO are outside
        // this boundary. Decode includes each codec's validation and v1 adapter.
        for round in 0..7 {
            let codecs = if round % 2 == 0 {
                [Codec::Json, Codec::Protobuf]
            } else {
                [Codec::Protobuf, Codec::Json]
            };
            for codec in codecs {
                let raw = codec.encode(&envelope, direction);
                assert!(codec.decode(&raw, direction) == envelope);
                for _ in 0..100 {
                    black_box(codec.encode(black_box(&envelope), direction));
                    black_box(codec.decode(black_box(&raw), direction));
                }
                let encode_ns = elapsed(iterations, || {
                    black_box(codec.encode(black_box(&envelope), direction));
                });
                let decode_ns = elapsed(iterations, || {
                    black_box(codec.decode(black_box(&raw), direction));
                });
                samples.push(json!({
                    "round":round, "codec":codec.name(),
                    "operation":workload.operation,
                    "direction":match direction { Direction::ToHome => "to-home", Direction::ToGateway => "to-gateway" },
                    "payload_bytes":workload.payload_bytes,
                    "encoded_bytes":raw.len(), "iterations":iterations,
                    "encode_elapsed_ns":encode_ns, "decode_elapsed_ns":decode_ns,
                }));
            }
        }
    }
    println!(
        "{}",
        json!({
            "schema":2, "scenario":"synthetic-home-codec-only",
            "os":std::env::consts::OS, "arch":std::env::consts::ARCH,
            "debug_assertions":cfg!(debug_assertions), "samples":samples,
            "limitations":"wall time includes scheduler noise; no Go implementation, IO, queue, RSS/PSS, process CPU, browser render, or allocation measurement",
        })
    );
}
