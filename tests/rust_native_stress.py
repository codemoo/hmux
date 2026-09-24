#!/usr/bin/env python3
"""Native Rust pair churn/slow-view checks, with private synthetic state only."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import time

from rust_native_matrix import required_binary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cycles", type=int, default=100)
    parser.add_argument("--capacity", action="store_true",
                        help="check native eight-view rejection and recovery instead of churn")
    parser.add_argument("--codec", choices=("protobuf", "json", "both"), default="both")
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    if not 1 <= args.cycles <= 10000:
        parser.error("--cycles must be 1..10000")
    if args.capacity and args.cycles != 100:
        parser.error("--capacity cannot be combined with a custom --cycles value")
    rust = required_binary("HMUX_RUST_WEB_BIN")
    oracle = required_binary("HMUX_GATEWAY_ORACLE_BIN")
    os.umask(0o077)
    args.output_dir.mkdir(mode=0o700, parents=False, exist_ok=False)
    result = {
        "schema": 1, "status": "running",
        "scenario": "view-capacity" if args.capacity else "view-churn",
        "cycles_per_codec": 0 if args.capacity else args.cycles,
        "os": platform.system(), "machine": platform.machine(),
        "binaries": {path: hashlib.sha256(Path(path).read_bytes()).hexdigest()
                     for path in (rust, oracle)},
        "limits": ["synthetic tmux and provider fixtures; real native gateway/Home",
                   "RSS/PSS checkpoints exclude browser, tools, proxy and providers",
                   "not peak memory, CPU, latency, soak or physical device acceptance"],
        "runs": [],
    }
    summary = args.output_dir / "summary.json"

    def save():
        summary.write_text(json.dumps(result, indent=2) + "\n")

    save()
    result["status"] = "failed"
    try:
        for codec in (("protobuf", "json") if args.codec == "both" else (args.codec,)):
            timeout = 180 if args.capacity else 120 + args.cycles * 5
            env = {
                **os.environ, "HMUX_RUST_GATEWAY_PRODUCTION": "1",
                "HMUX_GATEWAY_IMPLEMENTATION": "rust",
                "HMUX_RUST_GATEWAY_BIN": rust, "HMUX_NATIVE_HOME_BIN": rust,
                "HMUX_NATIVE_HOME_IMPLEMENTATION": "rust",
                "HMUX_NATIVE_JSON_FALLBACK": "1" if codec == "json" else "",
                "HMUX_NATIVE_CHURN": "" if args.capacity else str(args.cycles),
                "HMUX_NATIVE_CAPACITY": "1" if args.capacity else "",
                "HMUX_NATIVE_PERF_COUNT": "", "HMUX_NATIVE_SOAK_SECONDS": "",
            }
            log_path = args.output_dir / (codec + ".log")
            run = {"codec": codec, "status": "failed", "resources": []}
            result["runs"].append(run)
            started = time.monotonic()
            label = "eight-view capacity" if args.capacity else f"{args.cycles} view cycles"
            print(f"START {codec}: {label}", flush=True)
            with log_path.open("x") as log:
                child = subprocess.Popen(
                    [oracle, "-test.run", "^TestRustFullGatewayWithGoHome$",
                     "-test.v", "-test.timeout", f"{timeout}s"],
                    env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
                try:
                    exit_code = child.wait(timeout=timeout + 15)
                except BaseException:
                    # Only the session created above; never signal a caller's or
                    # an existing Home's process group. Give Home time to join PTYs.
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
            capacity_passed = False
            oracle_passed = False
            with log_path.open() as log:
                for line in log:
                    if "native-resource " in line:
                        run["resources"].append(json.loads(line.split("native-resource ", 1)[1]))
                    if "native eight-view capacity rejection, surviving echoes, replacement and drain passed" in line:
                        capacity_passed = True
                    if line.startswith("--- PASS: TestRustFullGatewayWithGoHome"):
                        oracle_passed = True
            if exit_code:
                raise RuntimeError(f"{codec} failed; see {log_path}")
            if args.capacity:
                expected = {(stage, role) for stage in ("capacity-before", "capacity-at-limit",
                            "capacity-recovered", "capacity-after-drain") for role in ("gateway", "home")}
                actual = {(row["stage"], row["role"]) for row in run["resources"]}
                if not capacity_passed or not oracle_passed or actual != expected or len(run["resources"]) != len(expected):
                    raise RuntimeError(f"{codec} missing native capacity acceptance evidence; see {log_path}")
            run["status"] = "passed"
            save()
            print(f"PASS {codec} ({run['seconds']}s)", flush=True)
        result["status"] = "passed"
    finally:
        save()


if __name__ == "__main__":
    main()
