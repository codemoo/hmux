#!/usr/bin/env python3
"""Native Home activity backfill, polling and terminal echo acceptance."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import signal
import subprocess
import time

from rust_native_matrix import required_binary


PHASES = ("backfill", "quiet", "append", "inode-replacement", "post-replacement-quiet")
SOURCES = (
    "internal/webgateway/rust_full_gateway_test.go",
    "internal/webgateway/rust_native_activity_test.go",
    "internal/webgateway/rust_native_stress_test.go",
    "tests/rust_native_activity.py",
)


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main():
    def interrupted(signum, _frame):
        raise KeyboardInterrupt(f"signal {signum}")

    for signum in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, interrupted)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codec", choices=("protobuf", "json", "both"), default="both")
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    rust = required_binary("HMUX_RUST_WEB_BIN")
    oracle = required_binary("HMUX_GATEWAY_ORACLE_BIN")
    repo = Path(__file__).resolve().parent.parent
    os.umask(0o077)
    args.output_dir.mkdir(mode=0o700, parents=False, exist_ok=False)
    result = {
        "schema": 1, "status": "failed", "scenario": "native-activity-echo",
        "os": platform.system(), "machine": platform.machine(),
        "binaries": {path: sha(path) for path in (rust, oracle)},
        "sources": {name: sha(repo / name) for name in SOURCES},
        "limits": [
            "synthetic JSONL and tmux fixtures; native gateway and Home executables",
            "socket receipt RTT excludes browser rendering",
            "resource checkpoints are whole native gateway/Home, not peaks or a Go comparison",
            "no physical device, external provider or production service acceptance",
        ],
        "runs": [],
    }
    summary = args.output_dir / "summary.json"

    def save():
        summary.write_text(json.dumps(result, indent=2) + "\n")

    save()
    try:
        for codec in (("protobuf", "json") if args.codec == "both" else (args.codec,)):
            env = {
                **os.environ,
                "HMUX_RUST_GATEWAY_PRODUCTION": "1",
                "HMUX_GATEWAY_IMPLEMENTATION": "rust",
                "HMUX_RUST_GATEWAY_BIN": rust,
                "HMUX_NATIVE_HOME_BIN": rust,
                "HMUX_NATIVE_HOME_IMPLEMENTATION": "rust",
                "HMUX_NATIVE_JSON_FALLBACK": "1" if codec == "json" else "",
                "HMUX_NATIVE_ACTIVITY": "1",
                "HMUX_NATIVE_CAPACITY": "", "HMUX_NATIVE_CHURN": "",
                "HMUX_NATIVE_PERF_COUNT": "", "HMUX_NATIVE_SOAK_SECONDS": "",
            }
            log_path = args.output_dir / (codec + ".log")
            run = {"codec": codec, "status": "failed", "resources": [], "phases": []}
            result["runs"].append(run)
            save()
            started = time.monotonic()
            print(f"START {codec}: 512 files/provider, 32 lines/file", flush=True)
            with log_path.open("x") as log:
                child = subprocess.Popen(
                    [oracle, "-test.run", "^TestRustFullGatewayWithGoHome$",
                     "-test.v", "-test.timeout", "150s"],
                    env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
                try:
                    exit_code = child.wait(timeout=165)
                except BaseException:
                    # Signal only the fresh fixture process group. The oracle
                    # owns its native children and disposable synthetic PTYs.
                    try:
                        os.killpg(child.pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                    try:
                        child.wait(timeout=20)
                    except subprocess.TimeoutExpired:
                        os.killpg(child.pid, signal.SIGKILL)
                        child.wait()
                    raise
            run["seconds"] = round(time.monotonic() - started, 3)
            run["exit_code"] = exit_code
            oracle_passed = False
            skipped = False
            results = []
            with log_path.open() as log:
                for line in log:
                    if "native-perf-resource " in line:
                        row = json.loads(line.split("native-perf-resource ", 1)[1])
                        if row.get("stage", "").startswith("activity-"):
                            run["resources"].append(row)
                    if "native-activity-phase " in line:
                        run["phases"].append(json.loads(line.split("native-activity-phase ", 1)[1]))
                    if "native-activity-result " in line:
                        results.append(json.loads(line.split("native-activity-result ", 1)[1]))
                    if line.startswith("--- PASS: TestRustFullGatewayWithGoHome"):
                        oracle_passed = True
                    if line.startswith("--- SKIP: TestRustFullGatewayWithGoHome"):
                        skipped = True
            run["result"] = results[0] if len(results) == 1 else None
            expected_resources = {(f"activity-{phase}", role)
                                  for phase in PHASES for role in ("gateway", "home")}
            actual_resources = {(row["stage"], row["role"]) for row in run["resources"]}
            expected_tokens = (16384, 16384, 17408, 17411, 17411)
            valid_phases = (len(run["phases"]) == len(PHASES) and
                            tuple(row.get("phase") for row in run["phases"]) == PHASES and
                            tuple(row.get("tokens_per_provider") for row in run["phases"]) == expected_tokens and
                            all(row.get("sessions_per_provider") == 512 for row in run["phases"]))
            measured = run["result"] or {}
            if platform.system() == "Linux":
                for row in run["resources"]:
                    if any(row.get(key) is None for key in ("pss_bytes", "cpu_ticks", "cpu_seconds", "fds", "threads")):
                        raise RuntimeError(f"{codec} missing Linux process resource fields")
            samples = measured.get("socket_receipt_rtt_samples_ms", [])
            valid_samples = (0 < len(samples) <= 1024 and len(samples) == measured.get("echoes") and
                             all(isinstance(v, (float, int)) and math.isfinite(v) and v > 0 for v in samples))
            if valid_samples:
                ordered = sorted(samples)
                valid_samples = all(measured.get("socket_receipt_rtt_ms", {}).get(p) ==
                                    ordered[(len(ordered) * percentile + 99) // 100 - 1]
                                    for p, percentile in (("p50", 50), ("p95", 95), ("p99", 99)))
            valid_result = (valid_samples and measured.get("files_per_provider") == 512 and
                            measured.get("lines_per_file") == 32 and
                            measured.get("echoes", 0) > 0 and measured.get("errors") == 0 and
                            all(measured.get("socket_receipt_rtt_ms", {}).get(p, 0) > 0
                                for p in ("p50", "p95", "p99")))
            if (exit_code or skipped or not oracle_passed or not valid_phases or not valid_result or
                    actual_resources != expected_resources or len(run["resources"]) != len(expected_resources)):
                raise RuntimeError(f"{codec} missing current native activity acceptance evidence; see {log_path}")
            run["status"] = "passed"
            save()
            print(f"PASS {codec} ({run['seconds']}s)", flush=True)
        result["status"] = "passed"
    finally:
        save()


if __name__ == "__main__":
    main()
