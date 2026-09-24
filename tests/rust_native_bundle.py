#!/usr/bin/env python3
"""Exercise a built native bundle in disposable Homes, without any user service."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

BUNDLE = Path(os.environ["HMUX_RUST_BUNDLE"]).resolve()


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(65536), b""):
            result.update(chunk)
    return result.hexdigest()


class NativeBundle(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="hmux-e2e-bundle space-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.bin = self.root / "native bin"
        self.config = self.root / ".config/hmux"
        tools = self.root / "tools"
        tools.mkdir()
        for name in ("tmux", "ps", "lsof"):
            path = tools / name
            path.write_text("#!/bin/sh\n" + (
                "echo 'no server running on hmux-e2e-isolated' >&2\nexit 1\n"
                if name == "tmux" else "exit 0\n"))
            path.chmod(0o700)
        self.environment = {
            "HOME": str(self.root), "PATH": str(tools) + ":/usr/bin:/bin",
            "SHELL": "/bin/sh",
        }

    def run_command(self, *args, data=b""):
        result = subprocess.run(args, input=data, cwd=self.root, env=self.environment,
                                capture_output=True, timeout=25, check=False)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
        return result.stdout

    def install(self, *extra, wrapper=False):
        prefix = ([sys.executable, str(BUNDLE / "install-home.py")] if wrapper
                  else [str(BUNDLE / "hmux-web"), "install-home"])
        return self.run_command(*prefix, "--source-dir", str(BUNDLE),
                                "--bin-dir", str(self.bin),
                                "--config-dir", str(self.config), *extra)

    def snapshot(self):
        return {path.name: path.read_bytes() for path in self.config.iterdir() if path.is_file()}

    def test_manifest_covers_every_bundled_file(self):
        listed = set()
        for line in (BUNDLE / "SHA256SUMS").read_text().splitlines():
            expected, name = line.split("  ", 1)
            relative = Path(name)
            self.assertFalse(relative.is_absolute())
            self.assertNotIn("..", relative.parts)
            self.assertNotIn(relative, listed)
            listed.add(relative)
            self.assertEqual(digest(BUNDLE / relative), expected, name)
        actual = {p.relative_to(BUNDLE) for p in BUNDLE.rglob("*")
                  if p.is_file() and p.name != "SHA256SUMS"}
        self.assertEqual(listed, actual)
        self.assertTrue({Path(p) for p in (
            "hmux-web", "hmux-agent", "install-home.py", "web/index.html",
            "THIRD_PARTY_NOTICES.md", "RELEASE")} <= listed)
        self.assertIn("runtime=rust", (BUNDLE / "RELEASE").read_text())

    def test_installed_commands_upgrade_and_compatibility_wrapper(self):
        workspace = self.root / "Work base ; literal"
        self.install("--workspace-dir", str(workspace))
        initial = self.snapshot()
        self.assertIn(str(workspace).encode(), initial["inventory.toml"])
        version = self.run_command(str(self.bin / "hmux-agent"), "version")
        self.assertTrue(version.startswith(b"hmux-agent "))
        self.run_command(str(self.bin / "hmux-agent"), "setup-home")
        self.assertEqual(initial, self.snapshot())
        state = json.loads(self.run_command(str(self.bin / "hmux-agent"),
                                            "workspace", data=b"null\n"))
        self.assertIsInstance(state, dict)
        for wrapper in (False, True, False):
            self.install(wrapper=wrapper)
            self.assertEqual(initial, self.snapshot())
            for name in ("hmux-web", "hmux-agent"):
                self.assertEqual(digest(self.bin / name), digest(BUNDLE / name))
                self.assertEqual((self.bin / name).stat().st_mode & 0o022, 0)
                self.assertLessEqual(len(list(self.bin.glob(f".{name}.backup-*"))), 1)
        self.assertFalse((self.bin / ".hmux-install.journal").exists())
        self.assertFalse((self.root / "Library/LaunchAgents").exists())
        self.assertFalse((self.config.parent / "systemd/user").exists())

    def test_native_dependency_and_standard_library_notices(self):
        root = BUNDLE / "licenses/rust"
        index = json.loads((root / "INDEX.json").read_text())
        self.assertEqual(set(index["roots"]), {"hmux-web", "hmux-agent"})
        self.assertTrue({"ring", "rustls", "tokio", "prost", "serde"} <=
                        {p["name"] for p in index["components"]})
        self.assertIn("target=" + index["target"], (BUNDLE / "RELEASE").read_text())
        files = [f for p in index["components"] for f in p["files"]]
        files.extend(index["standard_library"]["files"])
        seen = set()
        for record in files:
            path = Path(record["path"])
            self.assertFalse(path.is_absolute())
            self.assertNotIn("..", path.parts)
            self.assertNotIn(path, seen)
            seen.add(path)
            self.assertEqual(digest(root / path), record["sha256"])
            self.assertEqual((root / path).stat().st_size, record["bytes"])
        actual = {p.relative_to(root) for p in root.rglob("*") if p.is_file()}
        self.assertEqual(actual, seen | {Path("INDEX.json"), Path("README.txt")})

    def test_failed_setup_and_service_preflight_keep_installed_binaries(self):
        self.install("--binaries-only")
        old = {}
        for name in ("hmux-web", "hmux-agent"):
            path = self.bin / name
            path.write_bytes(("#!/bin/sh\n# old " + name + "\n").encode())
            path.chmod(0o700)
            old[name] = path.read_bytes()
        prefix = [str(BUNDLE / "hmux-web"), "install-home", "--source-dir", str(BUNDLE),
                  "--bin-dir", str(self.bin), "--config-dir", str(self.config)]
        for extra in (("--workspace-dir", "relative-workspace"),
                      ("--enable-service", "--url", "bad", "--token-file", str(self.root / "missing-token"))):
            result = subprocess.run(prefix + list(extra), cwd=self.root, env=self.environment,
                                    capture_output=True, timeout=25, check=False)
            self.assertNotEqual(result.returncode, 0)
            for name, content in old.items():
                self.assertEqual((self.bin / name).read_bytes(), content)

    def test_binaries_only_leaves_configuration_absent(self):
        self.install("--binaries-only")
        self.assertFalse(self.config.exists())
        for name in ("hmux-web", "hmux-agent"):
            self.assertEqual(digest(self.bin / name), digest(BUNDLE / name))


if __name__ == "__main__":
    unittest.main()
