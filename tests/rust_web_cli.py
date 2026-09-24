#!/usr/bin/env python3
"""Isolated production CLI checks; no real credentials, services or tmux state."""
import base64
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time
import unittest

BINARY = os.environ.get("HMUX_TEST_RUST_WEB")


@unittest.skipUnless(BINARY, "set HMUX_TEST_RUST_WEB to the compiled native binary")
class NativeWebCLI(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="hmux-e2e-web-cli-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.cred = self.root / "credentials.json"
        self.token = self.root / "connector.token"
        self.env = {**os.environ, "HOME": str(self.root)}

    def command(self, *args):
        return subprocess.run([BINARY, *args], env=self.env, input=b"", capture_output=True, timeout=10)

    def init_args(self):
        return ["init", "--credentials", str(self.cred), "--token-file", str(self.token)]

    def test_validation_before_runtime_access(self):
        for args, expected in [([], b"usage:"), (["unknown"], b"unknown command"),
                               (["serve", "unexpected"], b"unexpected arguments"),
                               (["serve", "--listen", "0.0.0.0:8088"], b"loopback"),
                               (["init"], b"--credentials and --token-file required")]:
            result = self.command(*args)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(expected, result.stderr)

    def test_init_refuses_pipe_existing_and_symlink(self):
        result = self.command(*self.init_args())
        self.assertIn(b"interactive terminal", result.stderr)
        self.assertFalse(self.cred.exists())
        self.cred.write_bytes(b"synthetic-existing")
        result = self.command(*self.init_args())
        self.assertIn(b"refusing to overwrite", result.stderr)
        self.assertEqual(self.cred.read_bytes(), b"synthetic-existing")
        self.assertFalse(self.token.exists())
        self.cred.unlink()
        self.cred.symlink_to(self.root / "missing")
        self.assertIn(b"refusing to overwrite", self.command(*self.init_args()).stderr)

    def start_terminal(self):
        master, slave = os.openpty()
        self.addCleanup(os.close, master)
        self.addCleanup(os.close, slave)
        process = subprocess.Popen([BINARY, *self.init_args()], stdin=slave, stdout=slave, stderr=slave, env=self.env)
        def cleanup():
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)
        self.addCleanup(cleanup)
        return process, master, slave

    def read_until(self, master, marker):
        data = bytearray()
        deadline = time.monotonic() + 10
        while marker not in data:
            if time.monotonic() >= deadline:
                self.fail("synthetic CLI prompt timed out")
            if select.select([master], [], [], 0.1)[0]:
                data.extend(os.read(master, 4096))
                self.assertLess(len(data), 16384)
        return bytes(data)

    def test_enrollment_totp_and_private_go_compatible_files(self):
        process, master, slave = self.start_terminal()
        self.read_until(master, b"Username: ")
        os.write(master, b"synthetic\n")
        self.read_until(master, b"Password (at least 8 bytes): ")
        self.assertFalse(termios.tcgetattr(slave)[3] & termios.ECHO)
        password = b"example-password-123"
        os.write(master, password + b"\n")
        output = self.read_until(master, b"Confirm password: ")
        self.assertNotIn(password, output)
        os.write(master, password + b"\n")
        output = self.read_until(master, b"Current 6-digit code: ")
        self.assertNotIn(password, output)
        secret = re.search(rb"secret=([A-Z2-7]{32})", output).group(1)
        digest = hmac.new(base64.b32decode(secret), struct.pack(">Q", int(time.time()) // 30), hashlib.sha1).digest()
        offset = digest[-1] & 15
        code = (int.from_bytes(digest[offset:offset + 4], "big") & 0x7fffffff) % 1000000
        os.write(master, f"{code:06d}\n".encode())
        self.assertEqual(process.wait(timeout=10), 0)
        self.assertTrue(termios.tcgetattr(slave)[3] & termios.ECHO)
        cred = json.loads(self.cred.read_bytes())
        self.assertEqual(cred["username"], "synthetic")
        self.assertEqual(base64.b64decode(cred["hash"]), hashlib.pbkdf2_hmac("sha256", password, base64.b64decode(cred["salt"]), 600000))
        self.assertEqual(cred["totp_secret"].encode(), secret)
        self.assertGreater(cred["last_step"], 0)
        self.assertEqual(len(base64.urlsafe_b64decode(self.token.read_text().strip() + "=")), 32)
        for path in [self.cred, self.token]:
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(path.stat().st_nlink, 1)

    def test_sigterm_during_password_restores_echo_without_files(self):
        process, master, slave = self.start_terminal()
        self.read_until(master, b"Username: ")
        os.write(master, b"synthetic\n")
        self.read_until(master, b"Password (at least 8 bytes): ")
        self.assertFalse(termios.tcgetattr(slave)[3] & termios.ECHO)
        process.send_signal(signal.SIGTERM)
        self.assertNotEqual(process.wait(timeout=5), 0)
        self.assertTrue(termios.tcgetattr(slave)[3] & termios.ECHO)
        self.assertFalse(self.cred.exists())
        self.assertFalse(self.token.exists())


if __name__ == "__main__":
    unittest.main()
