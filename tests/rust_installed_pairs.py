#!/usr/bin/env python3
"""Exercise installed Go/Rust web/helper pairs across sequential replacement.

Four explicit executable paths are required. State, tools, and installations
live in a disposable Home. Each executable is published with its own rename.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


def executable(name):
    raw = os.environ.get(name, "")
    path = Path(raw)
    if not path.is_absolute() or not path.is_file() or not os.access(path, os.X_OK):
        raise SystemExit(f"{name} must name an absolute executable file")
    return path.resolve()


GO_WEB = executable("HMUX_GO_WEB_BIN")
RUST_WEB = executable("HMUX_RUST_WEB_BIN")
GO_AGENT = executable("HMUX_GO_AGENT_BIN")
RUST_AGENT = executable("HMUX_RUST_AGENT_BIN")
SOURCES = {
    "web": {"go": GO_WEB, "rust": RUST_WEB},
    "agent": {"go": GO_AGENT, "rust": RUST_AGENT},
}


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(65536), b""):
            value.update(block)
    return value.digest()


class InstalledPairs(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="hmux-e2e-installed-pair-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.bin = self.root / "native bin"
        self.bin.mkdir()
        self.config = self.root / ".config/hmux"
        self.state_path = self.root / ".local/state/hmux/workflows/state.json"
        self.workspace = self.root / "Workspace ; literal"
        tools = self.root / "tools"
        tools.mkdir()
        for name in ("tmux", "ps", "lsof"):
            tool = tools / name
            tool.write_text("#!/bin/sh\n" + (
                "echo 'no server running on hmux-e2e-isolated' >&2\nexit 1\n"
                if name == "tmux" else "exit 0\n"))
            tool.chmod(0o700)
        self.environment = {
            "HOME": str(self.root), "PATH": str(tools) + ":" + str(self.bin) + ":/usr/bin:/bin",
            "SHELL": "/bin/sh", "HMUX_TMUX_SESSION_ID": "$4",
            "HMUX_TMUX_SESSION_CREATED_AT": "1700000000",
        }

    def publish(self, role, implementation):
        """Model one completed rename, including the interval before its peer."""
        source = SOURCES[role][implementation]
        target = self.bin / ("hmux-web" if role == "web" else "hmux-agent")
        staged = self.bin / ("." + target.name + ".staged")
        shutil.copyfile(source, staged)
        staged.chmod(0o755)
        os.replace(staged, target)
        self.assertEqual(digest(target), digest(source))

    def run_installed(self, role, *args, data=b"", expected=0):
        target = self.bin / ("hmux-web" if role == "web" else "hmux-agent")
        result = subprocess.run([str(target), *args], input=data, cwd=self.root,
                                env=self.environment, capture_output=True,
                                timeout=25, check=False)
        self.assertEqual(result.returncode, expected, result.stderr.decode(errors="replace"))
        return result.stdout + result.stderr

    def config_snapshot(self):
        return {path.name: path.read_bytes() for path in self.config.iterdir() if path.is_file()}

    def workflow_state(self):
        raw = self.state_path.read_bytes()
        self.assertNotIn(b"fixture-private-prompt", raw)
        self.assertNotIn(b"raw-provider-session", raw)
        return json.loads(raw)

    def assert_pair_usable(self, web, agent, original_config, minimum_tasks):
        for role, implementation in (("web", web), ("agent", agent)):
            target = self.bin / ("hmux-web" if role == "web" else "hmux-agent")
            self.assertEqual(digest(target), digest(SOURCES[role][implementation]))
        # This web usage path starts the installed binary without opening a service.
        self.assertIn(b"usage: hmux-web", self.run_installed("web", expected=1))
        self.run_installed("agent", "setup-home")
        self.assertEqual(original_config, self.config_snapshot())
        workspace = self.run_installed("agent", "workspace", data=b"null\n")
        self.assertIsInstance(json.loads(workspace), dict)
        state = self.workflow_state()
        tasks = [node for workflow in state["workflows"].values()
                 for node in workflow["nodes"].values() if node["type"] == "task"]
        self.assertGreaterEqual(len(tasks), minimum_tasks)

    def test_sequential_upgrade_interrupted_pairs_and_current_state_rollback(self):
        self.publish("web", "go")
        self.publish("agent", "go")
        self.run_installed("agent", "setup-home", "--workspace-dir", str(self.workspace))
        original_config = self.config_snapshot()
        self.assertIn(str(self.workspace).encode(), original_config["inventory.toml"])
        event = {"session_id": "raw-provider-session", "turn_id": "raw-turn",
                 "hook_event_name": "UserPromptSubmit", "prompt": "fixture-private-prompt"}
        self.assertEqual(self.run_installed("agent", "workflow-hook",
                          data=json.dumps(event).encode()), b"{}\n")
        self.assert_pair_usable("go", "go", original_config, 0)

        # First upgrade rename: Rust web with existing Go helper.
        self.publish("web", "rust")
        self.assert_pair_usable("rust", "go", original_config, 0)
        self.run_installed("agent", "workflow-report", "--task-id", "fixture-upgrade",
                           "--status", "running")
        self.assert_pair_usable("rust", "go", original_config, 1)

        self.publish("agent", "rust")
        self.assert_pair_usable("rust", "rust", original_config, 1)
        self.run_installed("agent", "workflow-report", "--task-id", "fixture-upgrade",
                           "--status", "completed")
        self.assert_pair_usable("rust", "rust", original_config, 1)

        # First rollback rename: Go web with existing Rust helper.
        self.publish("web", "go")
        self.assert_pair_usable("go", "rust", original_config, 1)
        self.run_installed("agent", "workflow-report", "--task-id", "fixture-rollback",
                           "--status", "running")
        self.publish("agent", "go")
        self.assert_pair_usable("go", "go", original_config, 2)
        self.run_installed("agent", "workflow-report", "--task-id", "fixture-rollback",
                           "--status", "completed")
        state = self.workflow_state()
        tasks = [node for workflow in state["workflows"].values()
                 for node in workflow["nodes"].values() if node["type"] == "task"]
        self.assertEqual(len(tasks), 2)
        self.assertTrue(all(node["status"] == "completed" for node in tasks))
        self.assertEqual(original_config, self.config_snapshot())


if __name__ == "__main__":
    unittest.main()
