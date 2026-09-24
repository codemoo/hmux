use super::*;
use serde_json::{json, Value};

fn id(n: usize) -> SessionIdentity {
    SessionIdentity {
        id: format!("${n}"),
        created_at: n as i64 + 100,
    }
}
fn session(identity: SessionIdentity, from: Option<SessionIdentity>) -> SessionLineage {
    SessionLineage {
        id: identity.id,
        created_at: identity.created_at,
        restored_from: from,
    }
}

#[test]
fn go_oracle_matches_merge_and_state_transition_sequence() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/workspace-v1/go-oracle.json"
    ))
    .unwrap();
    for row in oracle["merges"].as_array().unwrap() {
        let current: Snapshot = serde_json::from_value(row["current"].clone()).unwrap();
        let change: Change = serde_json::from_value(row["change"].clone()).unwrap();
        let got = merge(&current, &change);
        if row["result"].is_null() {
            assert!(got.is_err(), "{}", row["name"])
        } else {
            assert_eq!(
                serde_json::to_value(got.unwrap()).unwrap(),
                row["result"],
                "{}",
                row["name"]
            )
        }
    }
    let mut current = Snapshot::empty();
    for row in oracle["syncs"].as_array().unwrap() {
        let change: Option<Change> = serde_json::from_value(row["change"].clone()).unwrap();
        let sessions: Option<Vec<SessionLineage>> =
            serde_json::from_value(row["sessions"].clone()).unwrap();
        let old_revision = current.revision;
        let update = reconcile(
            current,
            change.as_ref(),
            sessions.as_deref().unwrap_or_default(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(update.reply()).unwrap(),
            row["result"],
            "{}",
            row["name"]
        );
        assert_eq!(update.changed, update.state.revision != old_revision);
        assert!(update.state.conflict.is_empty());
        current = update.state;
    }
}

#[test]
fn identity_validation_tab_budget_and_local_selection() {
    let valid = Change {
        operation_id: "operation-validation".into(),
        tabs: vec![id(1)],
        ..Change::default()
    };
    assert!(valid.valid());
    for bad in [
        Change {
            operation_id: "short".into(),
            ..valid.clone()
        },
        Change {
            operation_id: "operation-with/slash".into(),
            ..valid.clone()
        },
        Change {
            tabs: vec![
                id(1),
                SessionIdentity {
                    id: "$1".into(),
                    created_at: 999,
                },
            ],
            ..valid.clone()
        },
        Change {
            selected: Some(id(2)),
            ..valid.clone()
        },
        Change {
            tabs: (0..33).map(id).collect(),
            ..valid.clone()
        },
    ] {
        assert!(!bad.valid());
    }
    let full = Snapshot {
        tabs: (0..32).map(id).collect(),
        ..Snapshot::empty()
    };
    let new = Change {
        tabs: vec![id(32)],
        ..valid.clone()
    };
    assert_eq!(merge(&full, &new), Err(Error::TooManyTabs));
    let legacy = Snapshot {
        selected: Some(id(1)),
        tabs: vec![id(1)],
        ..Snapshot::empty()
    };
    let update = reconcile(legacy, None, &[]).unwrap();
    assert!(update.changed);
    assert_eq!(update.state.revision, 1);
    assert!(update.state.selected.is_none());
    assert_eq!(update.state.tabs, vec![id(1)]);
}

#[test]
fn restore_requires_unique_lineage_and_live_identity_wins() {
    let original = Snapshot {
        tabs: vec![id(1)],
        ..Snapshot::empty()
    };
    let ambiguous = vec![session(id(8), Some(id(1))), session(id(9), Some(id(1)))];
    assert_eq!(
        reconcile(original.clone(), None, &ambiguous)
            .unwrap()
            .state
            .tabs,
        vec![id(1)]
    );
    let mut live = ambiguous[..1].to_vec();
    live.push(session(id(1), None));
    assert_eq!(
        reconcile(original.clone(), None, &live).unwrap().state.tabs,
        vec![id(1)]
    );
    let restored = reconcile(original, None, &ambiguous[..1]).unwrap();
    assert_eq!(restored.state.tabs, vec![id(8)]);
    assert!(restored.changed);
    let dedup = Snapshot {
        tabs: vec![id(1), id(8)],
        ..Snapshot::empty()
    };
    assert_eq!(
        reconcile(dedup, None, &ambiguous[..1]).unwrap().state.tabs,
        vec![id(8)]
    );
}

#[test]
fn decode_and_revision_overflow_fail_closed() {
    for raw in [
        "null",
        "[]",
        "{}",
        r#"{"version":1,"tabs":[["$1",101]]}"#,
        r#"{"version":1,"revision":-1}"#,
        r#"{"version":1,"unknown":true}"#,
        r#"{"version":1}{}"#,
    ] {
        assert_eq!(
            Snapshot::decode(raw.as_bytes()),
            Err(Error::Invalid),
            "{raw}"
        );
    }
    let nulls = Snapshot::decode(
        br#"{"version":1,"tabs":null,"applied":null,"initialized":null,"revision":null}"#,
    )
    .unwrap();
    assert_eq!(nulls, Snapshot::empty());
    assert!(Snapshot::decode(&vec![b' '; MAX_BYTES + 1]).is_err());
    assert!(serde_json::from_value::<Change>(json!(["operation-00000001", 0, [], []])).is_err());
    let current = Snapshot {
        revision: u64::MAX,
        ..Snapshot::empty()
    };
    let change = Change {
        operation_id: "operation-overflow".into(),
        revision: u64::MAX,
        ..Change::default()
    };
    assert!(matches!(
        reconcile(current, Some(&change), &[]),
        Err(Error::RevisionExhausted)
    ));
}

#[test]
fn catalog_projection_keeps_only_exact_identity_and_lineage() {
    let raw=serde_json::to_vec(&json!({"unknown_top_level":[1,2],"sessions":[{"id":"$9","created_at":999,"restored_from":{"id":"$1","created_at":101},"title":"x".repeat(100_000),"processes":[{"ignored":true}]},null]})).unwrap();
    let got = decode_catalog(&raw).unwrap();
    assert_eq!(
        got,
        vec![
            SessionLineage {
                id: "$9".into(),
                created_at: 999,
                restored_from: Some(id(1))
            },
            SessionLineage::default()
        ]
    );
    for bad in [
        br#"{"sessions":[["$1",101]]}"#.as_slice(),
        b"[]",
        br#"{"sessions":{}}"#,
        br#"{"sessions":[]}{}"#,
    ] {
        assert!(decode_catalog(bad).is_err());
    }
    assert!(decode_catalog(b"null").unwrap().is_empty());
    assert!(decode_catalog(&vec![b' '; (4 << 20) + 1]).is_err());
}
