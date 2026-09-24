#!/usr/bin/env python3
"""Five native Go/Rust gateway/Home pairs, with synthetic TLS and private state."""
import os
from pathlib import Path
import subprocess
import sys


def required_binary(name):
    path = Path(os.environ[name])
    if not path.is_absolute() or not path.is_file() or not os.access(path, os.X_OK):
        raise SystemExit(f"{name} must name an absolute executable file")
    return str(path)


def main():
    rust = required_binary("HMUX_RUST_WEB_BIN")
    go = required_binary("HMUX_GO_WEB_BIN")
    oracle = required_binary("HMUX_GATEWAY_ORACLE_BIN")
    for gateway, home, fallback in (
        ("go", "go", False), ("rust", "go", False),
        ("go", "rust", False), ("rust", "rust", False),
        ("rust", "rust", True),
    ):
        go_core = home == "go" and sys.platform == "darwin"
        home_label = "go-core (isolated CA)" if go_core else home
        print(f"Pair: gateway={gateway}, Home={home_label}, force JSON={fallback}", flush=True)
        env = {
            **os.environ,
            "HMUX_RUST_GATEWAY_PRODUCTION": "1",
            "HMUX_GATEWAY_IMPLEMENTATION": gateway,
            "HMUX_RUST_GATEWAY_BIN": rust,
            "HMUX_GO_GATEWAY_BIN": go,
            "HMUX_NATIVE_HOME_BIN": "" if go_core else (go if home == "go" else rust),
            "HMUX_NATIVE_HOME_IMPLEMENTATION": home,
            "HMUX_NATIVE_JSON_FALLBACK": "1" if fallback else "",
        }
        subprocess.run([oracle, "-test.run", "^TestRustFullGatewayWithGoHome$",
                        "-test.v", "-test.timeout", "2m"],
                       env=env, check=True, timeout=150)
    print("Current-state authentication rollback: Rust -> Go -> Rust", flush=True)
    subprocess.run([oracle, "-test.run", "^TestRustNativeCurrentStateRollback$",
                    "-test.v", "-test.timeout", "2m"],
                   env={**os.environ, "HMUX_RUST_WEB_BIN": rust, "HMUX_GO_WEB_BIN": go},
                   check=True, timeout=150)


if __name__ == "__main__":
    main()
