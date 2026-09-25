use hmux_home::metrics_parsers::{
    cpu_darwin, cpu_linux, disk_bytes, gpu_darwin, memory_darwin, memory_linux,
};

#[test]
fn preserves_shared_go_metrics_and_rejects_retired_top_output() {
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/hostmetrics-v1/go-oracle.json"
    ))
    .unwrap();
    let cases = cases.as_array().unwrap();
    assert_eq!(cases.len(), 30);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let first = case["first"].as_str().unwrap_or("").as_bytes();
        let second = case["second"].as_str().unwrap_or("").as_bytes();
        let want = &case["want"];
        let want_percent = want.get("percent").and_then(serde_json::Value::as_f64);
        let want_bytes = want
            .get("used")
            .and_then(serde_json::Value::as_u64)
            .zip(want.get("total").and_then(serde_json::Value::as_u64));
        match case["kind"].as_str().unwrap() {
            // The native collector now uses CPU-only iostat instead of top.
            // Preserve the historical oracle, but do not require that retired
            // command format to be accepted by the current iostat decoder.
            // Current interval/invalid iostat cases live in metrics_parsers.
            "cpu_darwin" => assert_eq!(cpu_darwin(first), None, "{name}"),
            "memory_darwin" => assert_eq!(memory_darwin(first, second), want_bytes, "{name}"),
            "cpu_linux" => assert_eq!(cpu_linux(first, second), want_percent, "{name}"),
            "memory_linux" => assert_eq!(memory_linux(first), want_bytes, "{name}"),
            "disk_bytes" => {
                let blocks = case["blocks"].as_u64().unwrap_or(0);
                let free = case["free"].as_u64().unwrap_or(0);
                let size = case["size"].as_u64().unwrap_or(0);
                assert_eq!(disk_bytes(blocks, free, size), want_bytes, "{name}");
            }
            "gpu_darwin" => assert_eq!(gpu_darwin(first), want_percent, "{name}"),
            kind => panic!("unknown host metrics oracle kind: {kind}"),
        }
    }
}
