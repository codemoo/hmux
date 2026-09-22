#!/usr/bin/env python3
"""Install Home binaries and configure the base directory for new sessions."""
import argparse
from datetime import datetime, timezone
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile

BINARIES = ("hmux-web", "hmux-agent")


def read_regular(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(source.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or info.st_mode & 0o022 or not 0 < info.st_size <= 256 * 1024 * 1024):
            raise ValueError("binary must be a nonempty owner-controlled regular file")
        return source.read(), info


def fingerprint(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_size,
            info.st_mtime_ns, info.st_ctime_ns)


def current(path):
    try:
        return fingerprint(path.lstat())
    except FileNotFoundError:
        return None


def check_directory(path, create=False, mode=0o755):
    path = Path(os.path.abspath(path))
    for directory in (path, *path.parents):
        try:
            info = directory.lstat()
        except FileNotFoundError:
            if create:
                continue
            raise
        sticky_root = info.st_uid == 0 and info.st_mode & stat.S_ISVTX
        if (not stat.S_ISDIR(info.st_mode) or info.st_uid not in (0, os.getuid())
                or (info.st_mode & 0o022 and not sticky_root)):
            raise ValueError("directory path must be trusted and must not traverse symlinks")
    if create:
        path.mkdir(mode=mode, parents=True, exist_ok=True)
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
        raise ValueError("source and installation directories must be owner-controlled")
    return path


def install(source_dir, bin_dir):
    source_dir = check_directory(source_dir)
    bin_dir = check_directory(bin_dir, create=True)
    planned = []
    for name in BINARIES:
        data, source_info = read_regular(source_dir / name)
        if not source_info.st_mode & 0o111:
            raise ValueError("source binary must be executable")
        target = bin_dir / name
        old_data, old_info = (None, None)
        if current(target) is not None:
            old_data, old_info = read_regular(target)
        planned.append((target, data, old_data, old_info))
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    for target, data, old_data, old_info in planned:
        expected = fingerprint(old_info) if old_info else None
        if current(target) != expected:
            raise ValueError("installed binary changed; retry after reviewing it")
        if old_data is not None:
            backup = target.with_name(target.name + ".backup-" + stamp)
            fd = os.open(backup, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, "wb") as output:
                output.write(old_data)
                output.flush()
                os.fsync(output.fileno())
        fd, staged = tempfile.mkstemp(prefix="." + target.name + "-", dir=bin_dir)
        try:
            with os.fdopen(fd, "wb") as output:
                output.write(data)
                os.fchmod(output.fileno(), 0o755)
                output.flush()
                os.fsync(output.fileno())
            if current(target) != expected:
                raise ValueError("installed binary changed before activation")
            os.replace(staged, target)
            directory_fd = os.open(bin_dir, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
        finally:
            if os.path.exists(staged):
                os.unlink(staged)
        print("Installed " + target.name)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, default=Path("dist/web-darwin-arm64"))
    parser.add_argument("--bin-dir", type=Path, default=Path.home() / ".local/bin")
    parser.add_argument("--config-dir", type=Path, default=Path.home() / ".config/hmux")
    parser.add_argument("--workspace-dir", help="session base; new installs default to ~/.hmux")
    parser.add_argument("--binaries-only", action="store_true", help="leave configuration untouched")
    args = parser.parse_args()
    try:
        workspace = args.workspace_dir
        if not args.binaries_only:
            config_dir = check_directory(args.config_dir.expanduser(), create=True, mode=0o700)
            existing = any(os.path.lexists(config_dir / name)
                           for name in ("home.toml", "client.toml", "inventory.toml"))
            if workspace is None and not existing and sys.stdin.isatty():
                workspace = input("New-session base directory [~/.hmux]: ").strip() or "~/.hmux"
        install(args.source_dir, args.bin_dir)
        if not args.binaries_only:
            command = [str(check_directory(args.bin_dir) / "hmux-agent"),
                       "setup-home", "--config-dir", str(config_dir)]
            if workspace is not None:
                command.extend(["--workspace-dir", workspace])
            subprocess.run(command, check=True)
            print("Home configured; existing paths are preserved unless --workspace-dir is supplied.")
    except (OSError, ValueError, subprocess.CalledProcessError, EOFError) as error:
        parser.exit(1, "Home installation refused: " + str(error) + "\n")
