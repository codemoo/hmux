use hmux_model::{
    decode_catalog_json, decode_conversation_json, decode_inventory_json, decode_session_json,
    Catalog, Conversation, ConversationMessage, HostMetrics, Inventory, Profile, Session,
    SessionIdentity, Workflow, WorkflowNode, WorkflowSummary,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

#[test]
fn go_model_json_round_trips_with_exact_omission_and_null_shape() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/config-v1/go-model-oracle.json"
    ))
    .unwrap();
    let catalog: Catalog = serde_json::from_value(oracle["catalog"].clone()).unwrap();
    assert_eq!(serde_json::to_value(&catalog).unwrap(), oracle["catalog"]);
    assert_eq!(catalog.sessions.as_ref().unwrap()[0].pane_pid, 0);
    let conversation: Conversation =
        serde_json::from_value(oracle["conversation"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&conversation).unwrap(),
        oracle["conversation"]
    );
}

#[test]
fn nil_and_empty_optional_session_slices_have_same_go_output() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/config-v1/go-model-oracle.json"
    ))
    .unwrap();
    assert_eq!(oracle["session_nil_slices"], oracle["session_empty_slices"]);
    let mut session: Session =
        serde_json::from_value(oracle["session_nil_slices"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&session).unwrap(),
        oracle["session_nil_slices"]
    );
    session.tags = Some(Vec::new());
    session.workflows = Some(Vec::<Workflow>::new());
    assert_eq!(
        serde_json::to_value(&session).unwrap(),
        oracle["session_empty_slices"]
    );
}

#[test]
fn arrays_do_not_decode_as_public_model_structs() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/config-v1/go-model-oracle.json"
    ))
    .unwrap();
    let cases = &oracle["array_decode_rejections"];
    for name in [
        "catalog",
        "catalog_nested_session",
        "catalog_nested_workflow",
        "catalog_nested_node",
    ] {
        assert!(
            decode_catalog_json(cases[name].as_str().unwrap().as_bytes()).is_err(),
            "accepted {name}"
        );
        assert!(
            serde_json::from_str::<Catalog>(cases[name].as_str().unwrap()).is_err(),
            "direct Catalog accepted {name}"
        );
    }
    for name in ["conversation", "conversation_nested_message"] {
        assert!(
            decode_conversation_json(cases[name].as_str().unwrap().as_bytes()).is_err(),
            "accepted {name}"
        );
        assert!(
            serde_json::from_str::<Conversation>(cases[name].as_str().unwrap()).is_err(),
            "direct Conversation accepted {name}"
        );
    }
    for name in ["inventory", "inventory_nested_profile"] {
        assert!(
            decode_inventory_json(cases[name].as_str().unwrap().as_bytes()).is_err(),
            "accepted {name}"
        );
        assert!(
            serde_json::from_str::<Inventory>(cases[name].as_str().unwrap()).is_err(),
            "direct Inventory accepted {name}"
        );
    }
    for name in [
        "session",
        "session_nested_identity",
        "session_nested_summary",
    ] {
        assert!(
            decode_session_json(cases[name].as_str().unwrap().as_bytes()).is_err(),
            "accepted {name}"
        );
        assert!(
            serde_json::from_str::<Session>(cases[name].as_str().unwrap()).is_err(),
            "direct Session accepted {name}"
        );
    }
    fn rejects_array<T: DeserializeOwned>() {
        assert!(serde_json::from_str::<T>("[]").is_err());
    }
    rejects_array::<Profile>();
    rejects_array::<SessionIdentity>();
    rejects_array::<HostMetrics>();
    rejects_array::<WorkflowSummary>();
    rejects_array::<Workflow>();
    rejects_array::<WorkflowNode>();
    rejects_array::<ConversationMessage>();
    assert_eq!(
        serde_json::from_str::<Catalog>("null").unwrap(),
        Catalog::default()
    );
}

#[test]
fn inventory_null_tags_and_validation_are_preserved() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/config-v1/go-oracle.json"
    ))
    .unwrap();
    let inventory: Inventory = serde_json::from_value(oracle["inventory"].clone()).unwrap();
    inventory.validate().unwrap();
    assert_eq!(
        serde_json::to_value(inventory).unwrap(),
        oracle["inventory"]
    );
    assert_eq!(oracle["inventory"]["profiles"][1]["tags"], Value::Null);
}

#[test]
fn metrics_invalid_values_fail_open_and_null_remains_absent() {
    let base = json!({"protocol_version":1,"generated_at":"2026-09-08T12:00:00Z","sessions":[]});
    for metrics in [
        json!("wrong"),
        json!({"observed_at":"bad","cpu_percent":20}),
        json!({"observed_at":"2026-09-08T12:00:00Z","cpu_percent":101}),
        json!({"observed_at":"2026-09-08T12:00:00Z","cpu_percent":20,"private":"bad"}),
    ] {
        let mut value = base.clone();
        value["host_metrics"] = metrics;
        let result: Catalog = serde_json::from_value(value).unwrap();
        assert!(result.host_metrics.unwrap().validate().is_err());
    }
    let mut value = base;
    value["host_metrics"] = Value::Null;
    let result: Catalog = serde_json::from_value(value).unwrap();
    assert!(result.host_metrics.is_none());
}

#[test]
fn positional_metrics_fail_open_like_go() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/config-v1/go-model-oracle.json"
    ))
    .unwrap();
    let catalog: Catalog =
        serde_json::from_str(oracle["metrics_array_input"].as_str().unwrap()).unwrap();
    assert!(catalog.host_metrics.as_ref().unwrap().validate().is_err());
    assert_eq!(
        serde_json::to_value(catalog).unwrap(),
        oracle["metrics_array_result"]
    );
}
