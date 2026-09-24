#!/usr/bin/env python3
"""Actual Go/Rust helper handoffs with synthetic HOME, tools, and state only."""
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

GO = Path(os.environ["HMUX_GO_AGENT_BIN"]).resolve()
RUST = Path(os.environ["HMUX_RUST_AGENT_BIN"]).resolve()


class HelperCompatibility(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="hmux-e2e-helper-matrix-")
        self.root = Path(self.temporary.name).resolve()
        tools = self.root / "tools"
        tools.mkdir()
        for name in ("tmux", "ps", "lsof"):
            tool = tools / name
            tool.write_text("#!/bin/sh\n" + (
                "echo 'no server running on /tmp/hmux-e2e-no-server' >&2\nexit 1\n"
                if name == "tmux" else "exit 0\n"))
            tool.chmod(0o700)
        self.environment = {
            "HOME": str(self.root), "PATH": str(tools) + ":/usr/bin:/bin",
            "SHELL": "/bin/sh", "HMUX_TMUX_SESSION_ID": "$4",
            "HMUX_TMUX_SESSION_CREATED_AT": "1700000000",
        }

    def tearDown(self):
        self.temporary.cleanup()

    def run_helper(self, binary, *args, data=b""):
        result = subprocess.run([binary, *args], input=data, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, env=self.environment,
                                cwd=self.root, timeout=20, check=False)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
        return result.stdout

    def state(self):
        path = self.root / ".local/state/hmux/workflows/state.json"
        raw = path.read_bytes()
        self.assertNotIn(b"fixture-private-prompt", raw)
        self.assertNotIn(b"raw-provider-session", raw)
        return json.loads(raw)

    def test_setup_and_workspace_handoff_both_directions(self):
        for first, second in ((GO, RUST), (RUST, GO)):
            with self.subTest(first=first.name, second=second.name):
                self.run_helper(first, "setup-home")
                config = self.root / ".config/hmux"
                before = {p.name: p.read_bytes() for p in config.iterdir() if p.is_file()}
                self.run_helper(second, "setup-home")
                self.assertEqual(before, {p.name: p.read_bytes() for p in config.iterdir() if p.is_file()})
                left = json.loads(self.run_helper(first, "workspace", data=b"null\n"))
                right = json.loads(self.run_helper(second, "workspace", data=b"null\n"))
                self.assertEqual(left, right)

    def test_hook_and_reports_preserve_current_state_across_helpers(self):
        self.run_helper(GO, "setup-home")
        event = {"session_id": "raw-provider-session", "turn_id": "raw-turn",
                 "hook_event_name": "UserPromptSubmit", "prompt": "fixture-private-prompt"}
        for writer in (GO, RUST):
            self.assertEqual(self.run_helper(writer, "workflow-hook", data=json.dumps(event).encode()), b"{}\n")
            self.assertTrue(self.state()["workflows"])
        jobs = [(GO if i % 2 == 0 else RUST, "workflow-report", "--task-id", "fixture-" + str(i),
                 "--status", "running") for i in range(4)]
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            list(pool.map(lambda job: self.run_helper(*job), jobs))
        before = self.state()
        for i in range(4):
            self.run_helper(RUST if i % 2 == 0 else GO, "workflow-report", "--task-id",
                            "fixture-" + str(i), "--status", "completed")
        after = self.state()
        self.assertEqual(set(before["workflows"]), set(after["workflows"]))
        tasks = [node for workflow in after["workflows"].values()
                 for node in workflow["nodes"].values() if node["type"] == "task"]
        self.assertEqual(len(tasks), 4)
        self.assertTrue(all(node["status"] == "completed" for node in tasks))
        self.assertNotIn("fixture-0", json.dumps(after))


if __name__ == "__main__":
    unittest.main()
