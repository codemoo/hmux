#!/usr/bin/env python3
"""Notice coverage and unsafe/missing input failures, using synthetic sources."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("rust_notices", Path(__file__).resolve().parents[1] / "scripts/rust_notices.py")
notices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notices)


class NoticeCoverage(unittest.TestCase):
    def test_transitive_normal_and_build_dependencies_but_not_dev(self):
        names = ["hmux-web", "hmux-agent", "runtime", "transitive", "build-helper", "test-only"]
        packages = [{"id": name, "name": name, "version": "1.0.0", "license": "MIT",
                     "source": None if name.startswith("hmux-") else notices.REGISTRY} for name in names]
        deps = {"hmux-web": [("runtime", None), ("test-only", "dev")],
                "hmux-agent": [("build-helper", "build")], "runtime": [("transitive", None)]}
        metadata = {"packages": packages, "workspace_members": ["hmux-web", "hmux-agent"], "resolve": {"nodes": [
            {"id": name, "deps": [{"pkg": dep, "dep_kinds": [{"kind": kind}]} for dep, kind in deps.get(name, [])]}
            for name in names]}}
        self.assertEqual({p["name"] for p in notices.release_packages(metadata)}, {"runtime", "transitive", "build-helper"})
        packages[2]["source"] = "git+https://example.invalid/source"
        with self.assertRaisesRegex(ValueError, "unreviewed dependency"):
            notices.release_packages(metadata)

    def test_missing_notices_fail_even_with_a_license_declaration(self):
        with tempfile.TemporaryDirectory(prefix="hmux-e2e-notices-") as root:
            package = {"manifest_path": str(Path(root) / "Cargo.toml"), "name": "example", "license": "MIT"}
            with self.assertRaisesRegex(ValueError, "no upstream license"):
                notices.component_notices(package)
            (Path(root) / "LICENSE").symlink_to(Path(root) / "absent")
            with self.assertRaisesRegex(ValueError, "symlink"):
                notices.component_notices(package)

    def test_nested_and_source_copyrights_are_retained(self):
        with tempfile.TemporaryDirectory(prefix="hmux-e2e-notices-") as root:
            base = Path(root)
            (base / "LICENSE").write_text("ISC umbrella notice\n")
            (base / "src").mkdir()
            (base / "src/LICENSE-MIT").write_text("Nested upstream license\n")
            header = "/* Copyright Example C Author.\n * Permission to use this software. */"
            (base / "src/native.c").write_text(header + "\nint example;\n")
            assembly = "# Copyright Example Assembly Author.\n# Permission to use this software.\n"
            (base / "src/native.S").write_text(assembly + ".text\n")
            arm = "@ Copyright Example ARM Author.\n@ Licensed under Apache-2.0.\n"
            (base / "src/arm.S").write_text(arm + ".text\n")
            package = {"manifest_path": str(base / "Cargo.toml"), "name": "ring", "license": "ISC"}
            result = notices.component_notices(package)
            self.assertEqual(result["src/LICENSE-MIT"], b"Nested upstream license\n")
            self.assertIn(header.encode(), result["SOURCE-COPYRIGHTS.txt"])
            self.assertIn(assembly.encode(), result["SOURCE-COPYRIGHTS.txt"])
            self.assertIn(arm.encode(), result["SOURCE-COPYRIGHTS.txt"])


if __name__ == "__main__":
    unittest.main()
