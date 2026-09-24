#!/usr/bin/env python3
"""Compatibility launcher for a Rust Home bundle; installation lives in hmux-web."""
import argparse
import os
from pathlib import Path
import stat
import sys


def main():
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--source-dir", type=Path, default=Path(__file__).resolve().parent)
    options, _ = parser.parse_known_args()
    source = Path(os.path.abspath(options.source_dir.expanduser()))
    for directory in (source, *source.parents):
        info = directory.lstat()
        sticky_root = info.st_uid == 0 and info.st_mode & stat.S_ISVTX
        if (not stat.S_ISDIR(info.st_mode) or info.st_uid not in (0, os.getuid())
                or (info.st_mode & 0o022 and not sticky_root)):
            raise ValueError("bundle directory must be owner-controlled without symlink components")
    binary = source / "hmux-web"
    info = binary.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
            or info.st_mode & 0o022 or not info.st_mode & 0o111):
        raise ValueError("bundle hmux-web must be an owner-controlled executable")
    os.execv(binary, [str(binary), "install-home", "--source-dir", str(source), *sys.argv[1:]])


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        sys.exit("Home installation refused: " + str(error))
