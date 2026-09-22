#!/usr/bin/env python3
"""Exercise Home installation only in disposable directories, never the user's bin."""
import importlib.util
from pathlib import Path
import stat
import tempfile
import unittest

MODULE = Path(__file__).resolve().parents[1] / "deploy/web/install-home.py"
spec = importlib.util.spec_from_file_location("home_install", MODULE)
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class HomeInstallTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="hmux-e2e-install-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = self.root / "source"
        self.source.mkdir()
        self.target = self.root / "bin"
        for name in installer.BINARIES:
            path = self.source / name
            path.write_bytes(b"new-binary")
            path.chmod(0o755)

    def test_install_backs_up_existing_and_preserves_unrelated_files(self):
        self.target.mkdir()
        existing = self.target / "hmux-web"
        existing.write_bytes(b"old-binary")
        existing.chmod(0o755)
        unrelated = self.target / "unrelated"
        unrelated.write_bytes(b"keep")
        installer.install(self.source, self.target)
        for name in installer.BINARIES:
            path = self.target / name
            self.assertEqual(path.read_bytes(), b"new-binary")
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o755)
        backups = list(self.target.glob("hmux-web.backup-*"))
        self.assertEqual(len(backups), 1)
        self.assertEqual(backups[0].read_bytes(), b"old-binary")
        self.assertEqual(stat.S_IMODE(backups[0].stat().st_mode), 0o600)
        self.assertEqual(unrelated.read_bytes(), b"keep")

    def test_symlinked_or_writable_source_directory_is_rejected(self):
        linked = self.root / "source-link"
        linked.symlink_to(self.source, target_is_directory=True)
        with self.assertRaises(ValueError):
            installer.install(linked, self.target)
        self.assertFalse(self.target.exists())
        self.source.chmod(0o777)
        with self.assertRaises(ValueError):
            installer.install(self.source, self.target)
        self.assertFalse(self.target.exists())

    def test_symlinked_second_target_rejects_entire_preflight(self):
        self.target.mkdir()
        first = self.target / "hmux-web"
        first.write_bytes(b"old-binary")
        victim = self.root / "victim"
        victim.write_bytes(b"keep")
        (self.target / "hmux-agent").symlink_to(victim)
        with self.assertRaises(OSError):
            installer.install(self.source, self.target)
        self.assertEqual(first.read_bytes(), b"old-binary")
        self.assertEqual(victim.read_bytes(), b"keep")
        self.assertFalse(list(self.target.glob("*.backup-*")))

    def test_symlinked_directory_or_source_and_writable_target_are_rejected(self):
        real = self.root / "real"
        real.mkdir()
        self.target.symlink_to(real, target_is_directory=True)
        with self.assertRaises(ValueError):
            installer.install(self.source, self.target)
        self.target.unlink()
        self.target.mkdir()
        self.target.chmod(0o777)
        with self.assertRaises(ValueError):
            installer.install(self.source, self.target)
        self.target.chmod(0o755)
        source = self.source / "hmux-agent"
        source.unlink()
        source.symlink_to(self.source / "hmux-web")
        with self.assertRaises(OSError):
            installer.install(self.source, self.target)
        self.assertFalse(list(self.target.iterdir()))


if __name__ == "__main__":
    unittest.main()
