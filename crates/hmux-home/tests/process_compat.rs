use hmux_home::{
    binding::Status,
    process::{infer_agent_state, Snapshot},
};
fn status(status: Status) -> &'static str {
    match status {
        Status::Ready => "ready",
        Status::Ambiguous => "ambiguous",
        Status::Unavailable => "unavailable",
    }
}
#[test]
fn matches_real_go_process_selection_and_wrapper_graph() {
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/catalog-v1/go-process.json"
    ))
    .unwrap();
    for case in cases.as_array().unwrap() {
        let graph = Snapshot::parse(case["rows"].as_str().unwrap().as_bytes()).unwrap();
        let pane = case["pane"].as_i64().unwrap() as i32;
        let (pid, state) = graph.nearest_provider(pane);
        assert_eq!(
            pid,
            case["provider_pid"].as_i64().unwrap() as i32,
            "{}",
            case["name"]
        );
        assert_eq!(status(state), case["status"]);
        if let Some(node) = graph.candidate(pane) {
            assert_eq!(node.pid, case["candidate_pid"].as_i64().unwrap() as i32);
            assert_eq!(node.process, case["process"]);
            assert_eq!(infer_agent_state(node), case["state"]);
        } else {
            assert_eq!(case["candidate_pid"], 0);
        }
        if pid > 0 {
            let (wrappers, state) = graph.wrapper_chain(pid);
            assert_eq!(serde_json::to_value(wrappers).unwrap(), case["wrappers"]);
            assert_eq!(status(state), case["wrapper_status"]);
        }
    }
}
