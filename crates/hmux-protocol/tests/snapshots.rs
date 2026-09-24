use hmux_model::{Catalog, Session};
use hmux_protocol::{protobuf::types as p, snapshots::*};
use hmux_usage::{self as usage, Account, AccountWindow, Provider};
use prost::Message;

#[test]
fn catalog_golden_roundtrip_preserves_lists_metrics_and_workflows() {
    let raw = include_str!("../../../tests/fixtures/catalog-v1/go-oracle.json");
    let cases: serde_json::Value = serde_json::from_str(raw).unwrap();
    for case in cases
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["valid"] == true)
    {
        let catalog: Catalog = serde_json::from_value(case["catalog"].clone()).unwrap();
        let expected = catalog.clone();
        let proto = catalog_to_proto(catalog).unwrap();
        let decoded = p::CatalogSnapshot::decode(proto.encode_to_vec().as_slice()).unwrap();
        assert_eq!(
            catalog_from_proto(decoded).unwrap(),
            expected,
            "{}",
            case["name"]
        );
    }
    let mut rich: Catalog = serde_json::from_str(
        r#"{
      "protocol_version":1,"generated_at":"2026-09-24T00:00:00Z",
      "host_metrics":{"observed_at":"2026-09-24T00:00:00Z","cpu_percent":12.5,
        "gpu_percent":0,"memory_used_bytes":9007199254740993,"memory_total_bytes":9007199254740994,
        "disk_used_bytes":5,"disk_total_bytes":10},
      "sessions":[{"id":"$7","name":"agent","created_at":1700000000,
        "activity_at":1700000200,"attached_clients":1,"window_count":1,
        "window_names":[],"active_window":"main","current_path":"/tmp",
        "current_command":"codex","tags":["ready"],"workflow":{
          "running":1,"waiting_approval":0,"waiting_input":0,"completed":0,
          "failed":0,"interrupted":0,"stale":0,"updated_at":1700000200},
        "workflows":[{"id":"wf-1","source":"codex","status":"running",
          "started_at":1700000000,"updated_at":1700000200,
          "nodes":[{"id":"n-1","type":"agent","provider":"codex",
            "status":"running","started_at":1700000000,"updated_at":1700000200}]}]}]}
    "#,
    )
    .unwrap();
    let expected = rich.clone();
    let wire = catalog_to_proto(rich.clone()).unwrap();
    assert_eq!(catalog_from_proto(wire).unwrap(), expected);
    rich.sessions.as_mut().unwrap()[0].tags = None;
    let none = catalog_to_proto(rich).unwrap();
    assert!(none.sessions.unwrap().items[0].tags.is_none());
}

#[test]
fn catalog_required_and_bounds() {
    let base = p::CatalogSnapshot {
        sessions: Some(p::CatalogSessions {
            items: vec![p::CatalogSession::default()],
        }),
        ..Default::default()
    };
    assert_eq!(catalog_from_proto(base), Err(Error::Invalid));
    let c = Catalog {
        sessions: Some(vec![
            Session {
                id: "$1".into(),
                created_at: 1,
                ..Default::default()
            };
            MAX_CATALOG_SESSIONS + 1
        ]),
        ..Default::default()
    };
    assert!(matches!(catalog_to_proto(c), Err(Error::Limit)));
    let c = Catalog {
        generated_at: "x".repeat(129),
        ..Default::default()
    };
    assert!(matches!(catalog_to_proto(c), Err(Error::Limit)));
}

fn source_fixture() -> usage::Snapshot {
    let raw = include_str!("../../../tests/fixtures/usage-transport-v1/sources-go-oracle.json");
    let entries: serde_json::Value = serde_json::from_str(raw).unwrap();
    let bytes = serde_json::to_vec(&entries.as_array().unwrap()[0]).unwrap();
    usage::transport::decode(&bytes).unwrap()
}
#[test]
fn usage_golden_roundtrip_preserves_sources_accounts_and_presence() {
    let mut s = source_fixture();
    s.accounts.push(Account {
        number: 1,
        email: "a@example.test".into(),
        display_name: "A".into(),
        active: true,
        status: "ok".into(),
        five_hour: Some(AccountWindow {
            used_pct: 0.5,
            resets_at: None,
        }),
        seven_day: None,
        tokens_per_hour: Some(0.0),
        total_tokens: Some(0),
        last_refresh_at: Some("2026-09-24T00:00:00Z".into()),
        plan_type: "plus".into(),
    });
    s.accounts_updated_at = Some("2026-09-24T00:00:00Z".into());
    let expected = s.clone();
    let wire = usage_to_proto(s).unwrap();
    let decoded = p::UsageSnapshot::decode(wire.encode_to_vec().as_slice()).unwrap();
    assert_eq!(usage_from_proto(decoded).unwrap(), expected);
}
#[test]
fn usage_rejects_required_duplicate_recursive_provider_and_limits() {
    let good = usage_to_proto(source_fixture()).unwrap();
    let mut bad = good.clone();
    bad.rolling_5h = None;
    assert_eq!(usage_from_proto(bad), Err(Error::Invalid));
    let mut bad = good.clone();
    bad.sources = vec![bad.sources[0].clone(), bad.sources[0].clone()];
    assert_eq!(usage_from_proto(bad), Err(Error::Invalid));
    let mut bad = good.clone();
    bad.sources[0]
        .snapshot
        .as_mut()
        .unwrap()
        .sources
        .push(good.sources[0].clone());
    assert_eq!(usage_from_proto(bad), Err(Error::Invalid));
    let mut bad = good.clone();
    bad.sources[0].snapshot.as_mut().unwrap().provider = p::Provider::Codex as i32;
    assert_eq!(usage_from_proto(bad), Err(Error::Invalid));
    let mut bad = good.clone();
    bad.sources[0].name = "x".repeat(17);
    assert_eq!(usage_from_proto(bad), Err(Error::Limit));
    let mut bad = good;
    bad.accounts = vec![p::UsageAccount::default(); 129];
    assert_eq!(usage_from_proto(bad), Err(Error::Limit));
    let mut s = source_fixture();
    s.provider = Provider::Codex;
    assert!(matches!(usage_to_proto(s), Err(Error::Invalid)));
}

#[test]
fn catalog_json_defaults_presence_and_metrics_fail_open() {
    for raw in ["null", "{}", r#"{"generated_at":null,"sessions":null}"#] {
        let p = catalog_from_json(raw.as_bytes()).unwrap();
        assert!(p.sessions.is_none());
        assert_eq!(catalog_from_proto(p).unwrap(), Catalog::default());
    }
    let empty = catalog_from_json(br#"{"sessions":[]}"#).unwrap();
    assert_eq!(empty.sessions.as_ref().unwrap().items.len(), 0);
    assert!(catalog_from_proto(empty)
        .unwrap()
        .sessions
        .unwrap()
        .is_empty());
    let bad_metrics =
        catalog_from_json(br#"{"host_metrics":{"observed_at":"bad","cpu_percent":101}}"#).unwrap();
    assert!(bad_metrics.host_metrics.is_some());
    assert!(catalog_from_proto(bad_metrics)
        .unwrap()
        .host_metrics
        .unwrap()
        .validate()
        .is_err());
    let positional = catalog_from_json(br#"{"host_metrics":[1,2,3]}"#).unwrap();
    assert!(positional.host_metrics.is_some());
    assert!(matches!(
        catalog_from_json(br#"{"generated_at":"yesterday"}"#),
        Err(Error::Invalid)
    ));
}

#[test]
fn catalog_store_boundaries_and_json_tree_budget() {
    let workflows = (0..MAX_WORKFLOWS)
        .map(|n| {
            format!(r#"{{"id":"w{n}","status":"running","nodes":[{{"id":"n","type":"agent"}}]}}"#)
        })
        .collect::<Vec<_>>()
        .join(",");
    let raw = format!(r#"{{"sessions":[{{"id":"$1","created_at":1,"workflows":[{workflows}]}}]}}"#);
    let p = catalog_from_json(raw.as_bytes()).unwrap();
    assert_eq!(
        p.sessions.as_ref().unwrap().items[0]
            .workflows
            .as_ref()
            .unwrap()
            .items
            .len(),
        1024
    );
    let too_many =
        format!(r#"{{"sessions":[{{"id":"$1","created_at":1,"workflows":[{workflows},{{}}]}}]}}"#);
    assert!(matches!(
        catalog_from_json(too_many.as_bytes()),
        Err(Error::Limit) | Err(Error::Invalid)
    ));
    let windows = format!(
        r#"{{"sessions":[{{"id":"$1","created_at":1,"window_names":[{}]}}]}}"#,
        vec!["\"\""; 100_000].join(",")
    );
    assert!(catalog_from_json(windows.as_bytes()).is_ok());
    let oversized_list = format!(
        r#"{{"sessions":[{{"id":"$1","created_at":1,"window_names":[{}]}}]}}"#,
        vec!["\"\""; 100_001].join(",")
    );
    assert!(matches!(
        catalog_from_json(oversized_list.as_bytes()),
        Err(Error::Limit)
    ));
    // 300,000 small JSON numbers fit the frame, but their decoded Value tree would exceed 16 MiB.
    let deep_array = format!(r#"{{"host_metrics":[{}]}}"#, vec!["0"; 300_000].join(","));
    assert!(deep_array.len() < MAX_CATALOG_BYTES);
    assert!(matches!(
        catalog_from_json(deep_array.as_bytes()),
        Err(Error::Limit)
    ));
    let nested = format!(
        r#"{{"host_metrics":[{}0{}] }}"#,
        "[".repeat(129),
        "]".repeat(129)
    );
    assert!(catalog_from_json(nested.as_bytes()).is_err());
}

#[test]
fn borrowed_validators_match_conversion_semantics() {
    let mut catalog = catalog_from_json(
        br#"{"sessions":[{"id":"$1","created_at":1,"window_names":[],"tags":[] }]}"#,
    )
    .unwrap();
    assert_eq!(validate_catalog(&catalog), Ok(()));
    let session = &mut catalog.sessions.as_mut().unwrap().items[0];
    assert_eq!(session.window_names.as_ref().unwrap().items.len(), 0);
    session.tags.as_mut().unwrap().items = vec!["x".into(); 65];
    assert_eq!(validate_catalog(&catalog), Err(Error::Limit));
    let mut usage = usage_to_proto(source_fixture()).unwrap();
    assert_eq!(validate_usage(&usage), Ok(()));
    usage.rolling_5h.as_mut().unwrap().used_pct = 1.5;
    assert_eq!(validate_usage(&usage), Err(Error::Invalid));
}

#[test]
fn catalog_null_session_and_restored_zero_identity_survive() {
    let wire =
        catalog_from_json(br#"{"sessions":[null,{"id":"$1","created_at":1,"restored_from":{}}]}"#)
            .unwrap();
    let sessions = catalog_from_proto(wire).unwrap().sessions.unwrap();
    assert_eq!(sessions[0], Session::default());
    assert_eq!(sessions[1].restored_from.as_ref().unwrap().id, "");
    assert_eq!(sessions[1].restored_from.as_ref().unwrap().created_at, 0);
}

#[test]
fn workflow_node_boundary_is_128() {
    let nodes = (0..128)
        .map(|i| format!(r#"{{"id":"n{i}","type":"agent"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let raw = format!(
        r#"{{"sessions":[{{"id":"$1","created_at":1,"workflows":[{{"id":"w","nodes":[{nodes}]}}]}}]}}"#
    );
    assert!(catalog_from_json(raw.as_bytes()).is_ok());
    let over = format!(
        r#"{{"sessions":[{{"id":"$1","created_at":1,"workflows":[{{"id":"w","nodes":[{nodes},{{}}]}}]}}]}}"#
    );
    assert!(matches!(
        catalog_from_json(over.as_bytes()),
        Err(Error::Limit)
    ));
}
