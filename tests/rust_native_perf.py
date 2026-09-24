#!/usr/bin/env python3
"""Opt-in paired native gateway/Home socket and process measurements."""

import argparse
from legacy_baseline import BASELINE, baseline_digest
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import signal
import subprocess
import time


def executable(parser, value):
    path = Path(value)
    if not path.is_absolute() or not path.is_file() or not os.access(path, os.X_OK):
        parser.error(f"expected an absolute executable file: {value}")
    return path


def version(command, cwd=None):
    try:
        return subprocess.check_output(command, text=True, stderr=subprocess.STDOUT,
                                       timeout=10, cwd=cwd).strip()
    except (OSError, subprocess.SubprocessError):
        return None


def digest(path):
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1 << 20), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def linux_limits():
    limits = {"affinity_cpus": len(os.sched_getaffinity(0))}
    for label, path in (("cgroup_memory_max", Path("/sys/fs/cgroup/memory.max")),
                        ("cgroup_cpu_max", Path("/sys/fs/cgroup/cpu.max"))):
        try:
            limits[label] = path.read_text().strip()
        except OSError:
            limits[label] = None
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                limits["host_mem_total_kib"] = int(line.split()[1])
                break
    except OSError:
        pass
    return limits


def bounded(parser, name, value, low, high):
    if not low <= value <= high:
        parser.error(f"{name} must be {low}..{high}")


def timeout_seconds(count, idle_seconds, requested):
    # A slow host has room for setup and approximately 2 ms per input, while
    # the subprocess and its process group always retain a finite deadline.
    if requested is not None:
        return requested
    return min(1800, max(180, 120 + idle_seconds + math.ceil(count / 500)))


def parse_log(path, samples_path, count, idle_seconds):
    result = None
    resources = []
    idle = None
    passed = False
    with path.open() as source:
        for line in source:
            if "native-perf-result " in line:
                result = json.loads(line.split("native-perf-result ", 1)[1])
            if "native-perf-resource " in line:
                resources.append(json.loads(line.split("native-perf-resource ", 1)[1]))
            if "native-perf-idle " in line:
                idle = json.loads(line.split("native-perf-idle ", 1)[1])
            if line.startswith("--- PASS: TestRustFullGatewayWithGoHome"):
                passed = True
    if not passed or result is None or idle is None:
        raise RuntimeError(f"missing oracle or performance checkpoint: {path}")
    if result.get("count") != count or result.get("errors") != 0 or not result.get("echoes_per_second", 0) > 0:
        raise RuntimeError(f"echo count/error/throughput mismatch: {path}")
    if idle.get("samples") != idle_seconds or idle.get("seconds", 0) < idle_seconds:
        raise RuntimeError(f"connected idle checkpoint incomplete: {path}")
    for percentile in ("p50", "p95", "p99"):
        if result.get("socket_receipt_rtt_ms", {}).get(percentile, 0) <= 0:
            raise RuntimeError(f"missing {percentile}: {path}")
    if result.get("socket_receipt_samples_file") != samples_path.name:
        raise RuntimeError(f"missing RTT sample artifact reference: {path}")
    # The Go oracle writes this once. Do not copy a potentially large array into
    # either the verbose test log or summary.json.
    if not samples_path.is_file() or samples_path.stat().st_size > 64 * count + 2:
        raise RuntimeError(f"missing or oversized raw RTT sample artifact: {samples_path}")
    samples = json.loads(samples_path.read_text())
    if not isinstance(samples, list) or len(samples) != count or any(
            not isinstance(value, (int, float)) or not math.isfinite(value) or value <= 0
            for value in samples):
        raise RuntimeError(f"invalid raw RTT samples: {samples_path}")
    del samples
    idle_stride = max(1, (idle_seconds + 4) // 5)
    echo_stride = max(1, (count + 9) // 10)
    stages = {"warmup-start", "warmup-end", "connected-idle-start", "echo-start",
              f"connected-idle-{idle_seconds}", f"echo-{count}"}
    stages.update(f"connected-idle-{number}" for number in range(idle_stride, idle_seconds + 1, idle_stride))
    stages.update(f"echo-{number}" for number in range(echo_stride, count + 1, echo_stride))
    for stage in stages:
        if {row.get("role") for row in resources if row.get("stage") == stage} != {"gateway", "home"}:
            raise RuntimeError(f"missing process sample at {stage}: {path}")
    for row in resources:
        if row.get("rss_bytes") is None or row.get("pss_bytes") is None:
            raise RuntimeError(f"missing Linux RSS/PSS: {path}")
        if any(row.get(field) is None for field in ("fds", "threads", "cpu_ticks", "cpu_seconds")):
            raise RuntimeError(f"missing Linux process counter: {path}")
    if len(resources) != 2 * len(stages):
        raise RuntimeError(f"unexpected process sample count: {path}")
    resolution = idle.get("cpu_resolution_seconds")
    if not isinstance(resolution, (int, float)) or not 0 < resolution <= 1:
        raise RuntimeError(f"invalid CPU tick resolution: {path}")
    idle["cpu_ticks_by_role"] = {}
    idle["cpu_seconds_by_role"] = {}
    for role in ("gateway", "home"):
        start = next(row for row in resources if row["stage"] == "connected-idle-start" and row["role"] == role)
        end = next(row for row in resources if row["stage"] == f"connected-idle-{idle_seconds}" and row["role"] == role)
        ticks = end["cpu_ticks"] - start["cpu_ticks"]
        if ticks < 0 or start["pid"] != end["pid"]:
            raise RuntimeError(f"invalid idle CPU counter: {path}")
        idle["cpu_ticks_by_role"][role] = ticks
        idle["cpu_seconds_by_role"][role] = ticks * resolution
    return result, idle, resources, {"file": samples_path.name, "sha256": digest(samples_path), "count": count}


def run_child(command, env, log_path, timeout):
    with log_path.open("x") as log:
        child = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT,
                                 start_new_session=True)
        try:
            exit_code = child.wait(timeout=timeout + 15)
            if exit_code:
                try:
                    os.killpg(child.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
            return exit_code
        except BaseException:
            # The process group belongs to this invocation and contains only
            # the oracle plus its isolated fixture children.
            try:
                os.killpg(child.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                child.wait(timeout=20)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                child.wait()
            raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--go-bin", required=True)
    parser.add_argument("--rust-bin", required=True)
    parser.add_argument("--oracle-bin", required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--pairs", type=int, default=3)
    parser.add_argument("--count", type=int, default=250000)
    parser.add_argument("--input-bytes", type=int, default=64)
    parser.add_argument("--warmup-count", type=int, default=20)
    parser.add_argument("--idle-seconds", type=int, default=60)
    parser.add_argument("--timeout-seconds", type=int, help="per-run override (default scales with workload)")
    parser.add_argument("--rust-protobuf", action="store_true")
    parser.add_argument("--go-tuning-only", action="store_true", help="compare default/tuned Go only; requires explicit tuning")
    parser.add_argument("--gogc", help="explicit Go-child GOGC override")
    parser.add_argument("--gomemlimit", help="explicit Go-child GOMEMLIMIT override")
    args = parser.parse_args()
    if args.go_tuning_only and (args.rust_protobuf or (args.gogc is None and args.gomemlimit is None)):
        parser.error("--go-tuning-only requires tuning and cannot include --rust-protobuf")
    for name, value, low, high in (("--pairs", args.pairs, 1, 10),
                                  ("--count", args.count, 100, 500000),
                                  ("--input-bytes", args.input_bytes, 64, 4096),
                                  ("--warmup-count", args.warmup_count, 1, 100),
                                  ("--idle-seconds", args.idle_seconds, 1, 120)):
        bounded(parser, name, value, low, high)
    if args.timeout_seconds is not None:
        bounded(parser, "--timeout-seconds", args.timeout_seconds, 30, 1800)
        if args.timeout_seconds <= args.idle_seconds + 30:
            parser.error("--timeout-seconds must allow connected idle plus 30 seconds")
    variants = 2 if args.go_tuning_only else 2 + int(args.gogc is not None or args.gomemlimit is not None) + int(args.rust_protobuf)
    if args.pairs * variants * args.count > 10_000_000:
        parser.error("paired runs exceed the 10,000,000-input artifact/workload bound")
    timeout = timeout_seconds(args.count, args.idle_seconds, args.timeout_seconds)
    if platform.system() != "Linux":
        parser.error("native Go/Go comparison requires Linux; macOS Go Home uses a distinct in-process CA fixture")
    go = executable(parser, args.go_bin)
    rust = executable(parser, args.rust_bin)
    oracle = executable(parser, args.oracle_bin)
    if not args.output_dir.is_absolute():
        parser.error("--output-dir must be absolute")
    os.umask(0o077)
    args.output_dir.mkdir(mode=0o700, parents=False, exist_ok=False)
    source_root = Path(__file__).resolve().parents[1]
    workload = {"input_count": args.count, "input_bytes_including_newline": args.input_bytes,
                "warmup_count": args.warmup_count, "connected_idle_seconds": args.idle_seconds,
                "sequential_persistent_view": True, "seed": "perf-%04d",
                "precondition": "full functional setup and slow-view burst run before measurement"}
    report = {"schema": 2, "status": "running", "metric": "socket receipt RTT; excludes xterm rendering",
              "os": platform.platform(), "kernel": platform.release(), "machine": platform.machine(),
              "cpu_count": os.cpu_count(), "machine_limits": linux_limits(),
              "toolchain": {"go": "retained external artifact; see binary hash",
              "rustc": version(["rustc", "--version"])},
              "revision": version(["git", "rev-parse", "HEAD"], source_root),
              "baseline_ref": BASELINE,
              "source_files": {name: (baseline_digest(name) if name.startswith("internal/") else digest(source_root / name)) for name in (
                  "internal/webgateway/rust_full_gateway_test.go",
                  "internal/webgateway/rust_native_stress_test.go",
                  "internal/webgateway/rust_native_soak_test.go",
                  "tests/rust_native_perf.py")},
              "dependency_locks": {name: (baseline_digest(name) if name == "go.sum" else digest(source_root / name)) for name in ("go.sum", "Cargo.lock")
                                   if name == "go.sum" or (source_root / name).is_file()},
              "binaries": {label: {"path": str(path), "sha256": digest(path)}
                           for label, path in (("go", go), ("rust", rust), ("oracle", oracle))},
              "workload": workload, "pairs": args.pairs, "rust_protobuf": args.rust_protobuf,
              "planned_total_inputs": args.pairs * variants * args.count,
              "go_tuning_only": args.go_tuning_only,
              "timeout_seconds_per_run": timeout,
              "go_tuning": {"GOGC": args.gogc, "GOMEMLIMIT": args.gomemlimit},
              "limits": ["synthetic tools/TLS; real native gateway and Home",
                         "per-PID gateway/Home samples exclude browser, driver, proxy and tmux",
                         "RSS/PSS checkpoints are not peaks; no xterm-render timing or budget gate",
                         "throughput elapsed time includes synchronous resource sampling overhead",
                         "oracle and synthetic tools clear Go tuning variables; only native Go candidates are tuned",
                         "CPU counters use whole Linux clock ticks; idle deltas below one tick are unresolved"],
              "runs": []}
    summary = args.output_dir / "summary.json"

    def save():
        temporary = summary.with_suffix(".tmp")
        temporary.write_text(json.dumps(report, indent=2) + "\n")
        temporary.replace(summary)

    def interrupted(number, frame):
        del frame
        raise InterruptedError(f"runner received signal {number}")

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)

    save()
    try:
        for pair in range(1, args.pairs + 1):
            configurations = [("go-default", "go", "json-v1"), ("rust-json", "rust", "json-v1")]
            if args.go_tuning_only:
                configurations = [("go-default", "go", "json-v1")]
            if args.gogc is not None or args.gomemlimit is not None:
                configurations.insert(1, ("go-tuned", "go", "json-v1"))
            if pair % 2 == 0:
                configurations.reverse()
            if args.rust_protobuf:
                configurations.append(("rust-protobuf", "rust", "protobuf-v2"))
            for variant, implementation, codec in configurations:
                name = f"pair-{pair:02d}-{variant}"
                run = {"name": name, "pair": pair, "implementation": implementation,
                       "variant": variant, "codec": codec, "status": "running", "log": name + ".log",
                       "go_tuning": {"GOGC": args.gogc if variant == "go-tuned" else None,
                                     "GOMEMLIMIT": args.gomemlimit if variant == "go-tuned" else None}}
                report["runs"].append(run)
                save()
                env = os.environ.copy()
                for key in ("GOGC", "GOMEMLIMIT", "GOMAXPROCS", "GODEBUG",
                            "HMUX_PERF_GO_GOGC", "HMUX_PERF_GO_GOMEMLIMIT"):
                    env.pop(key, None)
                env.update({"HMUX_RUST_GATEWAY_PRODUCTION": "1",
                            "HMUX_GATEWAY_IMPLEMENTATION": implementation,
                            "HMUX_RUST_GATEWAY_BIN": str(rust), "HMUX_GO_GATEWAY_BIN": str(go),
                            "HMUX_NATIVE_HOME_BIN": str(go if implementation == "go" else rust),
                            "HMUX_NATIVE_HOME_IMPLEMENTATION": implementation,
                            "HMUX_NATIVE_JSON_FALLBACK": "1" if codec == "json-v1" and implementation == "rust" else "",
                            "HMUX_NATIVE_PERF_COUNT": str(args.count),
                            "HMUX_NATIVE_PERF_BYTES": str(args.input_bytes),
                            "HMUX_NATIVE_PERF_WARMUP": str(args.warmup_count),
                            "HMUX_NATIVE_PERF_IDLE_SECONDS": str(args.idle_seconds),
                            "HMUX_NATIVE_PERF_SAMPLES_PATH": str(args.output_dir / (name + ".samples.json")),
                            "HMUX_NATIVE_SOAK_SECONDS": "",
                            "HMUX_NATIVE_CHURN": ""})
                if variant == "go-tuned":
                    if args.gogc is not None:
                        env["HMUX_PERF_GO_GOGC"] = args.gogc
                    if args.gomemlimit is not None:
                        env["HMUX_PERF_GO_GOMEMLIMIT"] = args.gomemlimit
                started = time.monotonic()
                print(f"START {name}", flush=True)
                log_path = args.output_dir / run["log"]
                exit_code = run_child([str(oracle), "-test.run", "^TestRustFullGatewayWithGoHome$",
                                       "-test.v", "-test.timeout", f"{timeout}s"],
                                      env, log_path, timeout)
                run["seconds"] = round(time.monotonic() - started, 3)
                run["exit_code"] = exit_code
                if exit_code:
                    raise RuntimeError(f"{name} exited {exit_code}; see {log_path}")
                run["echo"], run["idle"], run["resources"], run["samples"] = parse_log(
                    log_path, args.output_dir / (name + ".samples.json"), args.count, args.idle_seconds)
                run["status"] = "passed"
                save()
                print(f"PASS {name} ({run['seconds']}s)", flush=True)
        report["status"] = "measured"
    except BaseException as error:
        report["status"] = "failed"
        report["error"] = str(error)
        if report["runs"] and report["runs"][-1]["status"] == "running":
            report["runs"][-1]["status"] = "failed"
        raise
    finally:
        save()


if __name__ == "__main__":
    main()
