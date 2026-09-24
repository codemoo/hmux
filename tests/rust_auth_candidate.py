#!/usr/bin/env python3
"""Isolated Rust auth-only binary smoke: startup, restart, revoke and SIGTERM.

Run after make rust-gateway-candidate. Never reads existing HMux state.
"""

import base64
import hashlib
import http.client
import json
import os
from pathlib import Path
import secrets
import select
import subprocess
import sys
import tempfile


def write_private(path, value):
    with open(path, "x", encoding="utf-8") as output:
        os.chmod(path, 0o600)
        output.write(value)


def main():
    binary = Path(sys.argv[1] if len(sys.argv) == 2 else "target/release/examples/auth_gateway").resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="hmux-e2e-rust-auth-") as temporary:
        root = Path(temporary).resolve()
        password = "synthetic-http-candidate-password"
        salt = bytes(range(32))
        write_private(root / "credentials.json", json.dumps({
            "username": "synthetic", "salt": base64.b64encode(salt).decode(),
            "hash": base64.b64encode(hashlib.pbkdf2_hmac("sha256", password.encode(), salt, 600_000)).decode(),
            "totp_secret": base64.b32encode(bytes(range(20))).decode(),
            "last_step": 0, "totp_disabled": True,
        }))
        write_private(root / "connector.token", secrets.token_urlsafe(32))
        arguments = [str(binary), "--experimental-auth-only", "--origin", "https://hmux.example",
                     "--credentials", str(root / "credentials.json"), "--token-file", str(root / "connector.token"),
                     "--listen", "127.0.0.1:0"]
        process = None

        def start():
            nonlocal process
            process = subprocess.Popen(arguments, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
            if not select.select([process.stderr], [], [], 10)[0]:
                raise AssertionError("candidate readiness timed out")
            line = process.stderr.readline()
            prefix = "Experimental auth-only listener ready at 127.0.0.1:"
            if not line.startswith(prefix):
                raise AssertionError("candidate did not start")
            return int(line[len(prefix):].split(";", 1)[0])

        def stop():
            nonlocal process
            process.terminate()
            assert process.wait(timeout=10) == 0, "SIGTERM did not shut down successfully"
            process.stderr.close()
            process = None

        def request(port, method, path, cookie=None, csrf=None, body=None):
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
            headers = {"Host": "hmux.example", "Origin": "https://hmux.example"}
            if cookie:
                headers["Cookie"] = cookie
            if csrf:
                headers["X-CSRF-Token"] = csrf
            if body is not None:
                headers["Content-Type"] = "application/json"
                body = json.dumps(body)
            try:
                connection.request(method, path, body=body, headers=headers)
                response = connection.getresponse()
                status, cookie_header = response.status, response.getheader("Set-Cookie")
                assert response.getheader("Cache-Control") == "no-store"
                raw = response.read()
                return status, cookie_header, raw
            finally:
                connection.close()

        try:
            port = start()
            assert request(port, "GET", "/api/session")[0] == 401
            status, cookie_header, _ = request(port, "POST", "/api/login", body={"username": "synthetic", "password": password})
            assert status == 200 and cookie_header
            cookie = cookie_header.split(";", 1)[0]
            assert "HttpOnly; Secure; SameSite=Strict" in cookie_header
            assert request(port, "GET", "/api/state", cookie=cookie)[0] == 503
            stop()
            port = start()
            status, _, raw = request(port, "GET", "/api/session", cookie=cookie)
            assert status == 200, "login did not survive process restart"
            csrf = json.loads(raw)["csrf"]
            assert request(port, "POST", "/api/logout", cookie=cookie)[0] == 403
            assert request(port, "POST", "/api/logout", cookie=cookie, csrf=csrf)[0] == 200
            stop()
            port = start()
            assert request(port, "GET", "/api/session", cookie=cookie)[0] == 401, "revoked login revived after restart"
            stop()
        finally:
            if process is not None:
                process.kill()
                process.wait(timeout=10)
                process.stderr.close()
    print("Rust auth candidate: startup, 3-process restart/revocation and SIGTERM checks passed")


if __name__ == "__main__":
    main()
