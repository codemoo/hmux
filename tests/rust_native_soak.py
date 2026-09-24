#!/usr/bin/env python3
"""Bounded native Rust elapsed-time checks using private synthetic state only."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import subprocess
import sys
import time


# Retain a live process-group leader until the runner finishes cleanup. Waiting
# on the oracle itself would reap its PID before its descendants are gone.
GUARD = r'''
import json, signal, subprocess, sys, time
from pathlib import Path
signal.signal(signal.SIGTERM, lambda *_: None)
signal.signal(signal.SIGINT, lambda *_: None)
result = Path(sys.argv[1])
child = subprocess.Popen(sys.argv[2:], stdin=subprocess.DEVNULL)
code = child.wait()
temporary = result.with_suffix('.tmp')
temporary.write_text(json.dumps({'exit_code': code, 'oracle_pid': child.pid}))
temporary.replace(result)
while True:
    signal.pause()
'''


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1 << 20), b""):
            value.update(chunk)
    return value.hexdigest()


def executable(parser, value):
    path = Path(value)
    if not path.is_absolute() or not path.is_file() or not os.access(path, os.X_OK):
        parser.error(f"expected an absolute executable: {value}")
    return path


def stop_child(child, grace=20):
    # The guard remains the live leader, so its PGID cannot be reused here.
    if child.poll() is not None:
        raise RuntimeError("owned process-group guard exited unexpectedly; inspect fixture children")
    try:
        os.killpg(child.pid, signal.SIGTERM)
    except ProcessLookupError:
        raise RuntimeError("owned process group disappeared before cleanup") from None
    time.sleep(grace)
    os.killpg(child.pid, signal.SIGKILL)
    child.wait(timeout=5)


def verify_fixture_exit(output_dir):
    # PTY clients may own a separate session. Their executable is our frozen
    # oracle; a path match is an ownership check, never a reason to signal a PID.
    paths = [str(output_dir / name) for name in ("oracle", "hmux-web")]
    for attempt in range(10):
        listing = subprocess.check_output(["/bin/ps", "-axww", "-o", "pid=,stat=,command="],
                                          text=True, timeout=5)
        remaining = []
        for line in listing.splitlines():
            fields = line.split(None, 2)
            if len(fields) != 3 or fields[1].startswith("Z"):
                continue
            if any(fields[2] == path or fields[2].startswith(path + " ") for path in paths):
                remaining.append(int(fields[0]))
        if not remaining:
            return
        time.sleep(0.1)
    raise RuntimeError(f"owned fixture processes survived cleanup: {remaining}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-bin", required=True)
    parser.add_argument("--oracle-bin", required=True)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--seconds", type=int, default=86400)
    parser.add_argument("--codec", choices=("protobuf", "json"), default="protobuf")
    args = parser.parse_args()
    if not 10 <= args.seconds <= 259200:
        parser.error("--seconds must be 10..259200 (maximum 72h)")
    rust = executable(parser, args.rust_bin)
    oracle = executable(parser, args.oracle_bin)
    if not args.output_dir.is_absolute():
        parser.error("--output-dir must be absolute and new")
    if platform.system() not in ("Darwin", "Linux"):
        parser.error("native host checks require macOS or Linux")
    os.umask(0o077)
    args.output_dir.mkdir(mode=0o700, parents=False, exist_ok=False)
    source_root = Path(__file__).resolve().parents[1]
    report = {
        "schema": 1, "status": "preparing", "runner_pid": os.getpid(),
        "started_at_unix": time.time(), "requested_seconds": args.seconds, "codec": args.codec,
        "os": platform.platform(), "machine": platform.machine(),
        "binaries": {}, "resources": [], "progress": None,
        "source_hashes": {name: digest(source_root / name) for name in (
            "tests/rust_native_soak.py", "internal/webgateway/rust_full_gateway_test.go",
            "internal/webgateway/rust_native_soak_test.go",
            "internal/webgateway/rust_native_stress_test.go")},
        "limits": ["synthetic tmux/providers and private loopback TLS; native Rust gateway/Home",
                   "one persistent view; one echo and authenticated catalog check per second",
                   "transient view once per minute; reconnect/revocation/shutdown after soak",
                   "short smoke runs use a transient view every five seconds",
                   "bounded diagnostic and resource samples; not peak memory or browser/device acceptance",
                   "frozen copied binaries; changing a release artifact invalidates final RC acceptance"],
    }
    summary = args.output_dir / "summary.json"

    def save():
        temporary = summary.with_suffix(".tmp")
        temporary.write_text(json.dumps(report, indent=2) + "\n")
        temporary.replace(summary)

    save()
    child = None

    def interrupted(number, frame):
        del frame
        raise InterruptedError(f"runner received signal {number}")

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        # Freeze artifacts rather than holding paths that a later build overwrites.
        for name, path in (("hmux-web", rust), ("oracle", oracle)):
            expected = digest(path)
            target = args.output_dir / name
            shutil.copyfile(path, target)
            target.chmod(0o700)
            if digest(target) != expected or digest(path) != expected:
                raise RuntimeError(f"binary changed while copying {name}")
            report["binaries"][name] = {"source": str(path), "sha256": expected}
        env = os.environ.copy()
        for name in tuple(env):
            if name.startswith("HMUX_NATIVE_") or name in ("GOGC", "GOMEMLIMIT"):
                env.pop(name)
        env.update({
            "HMUX_RUST_GATEWAY_PRODUCTION": "1", "HMUX_GATEWAY_IMPLEMENTATION": "rust",
            "HMUX_RUST_GATEWAY_BIN": str(args.output_dir / "hmux-web"),
            "HMUX_NATIVE_HOME_BIN": str(args.output_dir / "hmux-web"),
            "HMUX_NATIVE_HOME_IMPLEMENTATION": "rust",
            "HMUX_NATIVE_JSON_FALLBACK": "1" if args.codec == "json" else "",
            "HMUX_NATIVE_SOAK_SECONDS": str(args.seconds),
        })
        log_path = args.output_dir / "oracle.log"
        timeout = args.seconds + 180
        result = None
        passed = False
        started = time.monotonic()
        with log_path.open("x") as log, log_path.open() as reader:
            result_path = args.output_dir / "oracle-exit.json"
            child = subprocess.Popen([
                sys.executable, "-c", GUARD, str(result_path),
                str(args.output_dir / "oracle"), "-test.run", "^TestRustFullGatewayWithGoHome$",
                "-test.v", "-test.timeout", f"{timeout}s"], env=env,
                stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            report.update(status="running", process_group_guard_pid=child.pid)
            save()
            print(f"START {args.codec} {args.seconds}s; results: {summary}", flush=True)
            while True:
                code = None
                if result_path.exists():
                    exit_record = json.loads(result_path.read_text())
                    code = exit_record["exit_code"]
                    report["oracle_pid"] = exit_record["oracle_pid"]
                if child.poll() is not None:
                    raise RuntimeError("process-group guard exited unexpectedly")
                if log_path.stat().st_size > 8 << 20:
                    raise RuntimeError("oracle diagnostic log exceeded 8 MiB")
                for line in reader:
                    if "native-perf-resource " in line:
                        report["resources"].append(json.loads(line.split("native-perf-resource ", 1)[1]))
                        if len(report["resources"]) > 600:
                            raise RuntimeError("resource checkpoint count exceeded limit")
                    elif "native-soak-progress " in line:
                        report["progress"] = json.loads(line.split("native-soak-progress ", 1)[1])
                    elif "native-soak-result " in line:
                        result = json.loads(line.split("native-soak-result ", 1)[1])
                    elif line.startswith("--- PASS: TestRustFullGatewayWithGoHome"):
                        passed = True
                report["runner_elapsed_seconds"] = time.monotonic() - started
                report["updated_at_unix"] = time.time()
                save()
                if code is not None:
                    break
                if time.monotonic() - started > timeout + 15:
                    raise TimeoutError("oracle exceeded elapsed-time deadline")
                time.sleep(min(10, args.seconds))
            report["exit_code"] = code
            if code != 0 or not passed or result is None:
                raise RuntimeError("oracle did not complete all lifecycle assertions; see oracle.log")
            if (result.get("requested_seconds") != args.seconds or
                    result.get("elapsed_seconds", 0) < args.seconds or result.get("errors") != 0 or
                    result.get("echoes", 0) < max(1, args.seconds // 2) or
                    result.get("transient_views", 0) < max(1, args.seconds // (65 if args.seconds >= 60 else 10)) or
                    not 0 <= result.get("max_check_gap_seconds", 999) <= 5 or
                    not 0 <= result.get("max_transient_gap_seconds", 999) <= (65 if args.seconds >= 60 else 10)):
                raise RuntimeError("soak evidence is incomplete")
            final_roles = {row["role"] for row in report["resources"] if row["stage"] == "soak-end"}
            if final_roles != {"gateway", "home"}:
                raise RuntimeError("missing final native resource samples")
            stop_child(child, grace=1)
            child = None
            verify_fixture_exit(args.output_dir)
            report.update(status="passed", result=result)
            print(f"PASS {args.codec} {args.seconds}s including final reconnect/revocation/shutdown", flush=True)
    except BaseException as error:
        report.update(status="interrupted" if isinstance(error, (InterruptedError, KeyboardInterrupt)) else "failed",
                      error=str(error))
        if child is not None:
            # Do not let a second interrupt prevent bounded owned-child cleanup.
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            signal.signal(signal.SIGINT, signal.SIG_IGN)
            stop_child(child)
            verify_fixture_exit(args.output_dir)
        raise
    finally:
        report["updated_at_unix"] = time.time()
        save()


if __name__ == "__main__":
    main()
