#!/usr/bin/env python3
"""Bundle notices for the native release dependency closure, without network I/O."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

ROOTS = {"hmux-web", "hmux-agent"}
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
NOTICE = re.compile(r"^(?:licen[sc]e|copying|copyright|notice)(?:$|[-_.])", re.I)
NAME = re.compile(r"^[A-Za-z0-9_.+-]+$")
COPYRIGHT_BLOCK = re.compile(r"/\*.*?\*/|(?:(?:[ \t]*//|[ \t]*[\#@])[^\n]*(?:\n|$))+", re.S)
SOURCE_SUFFIXES = {".rs", ".c", ".h", ".S", ".s", ".asm", ".pl", ".py"}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def release_packages(metadata):
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {p["id"]: p for p in metadata["resolve"]["nodes"]}
    roots = [p for p in metadata["packages"] if p["name"] in ROOTS and p["source"] is None]
    if {p["name"] for p in roots} != ROOTS:
        raise ValueError("missing native release root")
    pending, seen = [p["id"] for p in roots], set()
    while pending:
        key = pending.pop()
        if key in seen:
            continue
        seen.add(key)
        for dep in nodes[key]["deps"]:
            if any(kind["kind"] != "dev" for kind in dep["dep_kinds"]):
                pending.append(dep["pkg"])
    third_party = []
    for key in seen:
        p = packages[key]
        if p["source"] is None:
            if key not in metadata["workspace_members"]:
                raise ValueError("external path dependency needs explicit notice handling")
            continue
        if p["source"] != REGISTRY or not p["license"]:
            raise ValueError(f"unreviewed dependency source/license: {p['name']}")
        if not NAME.fullmatch(p["name"]) or not NAME.fullmatch(p["version"]):
            raise ValueError("unsafe component name")
        third_party.append(p)
    return sorted(third_party, key=lambda p: (p["name"], p["version"]))


def source_bytes(base, path, limit=4 << 20):
    relative = path.relative_to(base)
    if path.is_symlink() or any(p.is_symlink() for p in path.parents if p != base and base in p.parents):
        raise ValueError(f"symlink in notice source: {relative}")
    if not path.is_file() or path.stat().st_size > limit:
        raise ValueError(f"missing or excessive notice source: {relative}")
    path.resolve().relative_to(base.resolve())
    return path.read_bytes()


def component_notices(package):
    base = Path(package["manifest_path"]).parent
    notices = {}
    headers = []
    count = 0
    for directory, dirs, files in os.walk(base, followlinks=False):
        parent = Path(directory)
        if any((parent / name).is_symlink() for name in dirs):
            raise ValueError("symlink directory in dependency source")
        for name in sorted(files):
            path = parent / name
            count += 1
            if count > 20000:
                raise ValueError("excessive dependency source entries")
            if NOTICE.match(name):
                notices[path.relative_to(base).as_posix()] = source_bytes(base, path)
            # ring's umbrella LICENSE explicitly delegates some ISC copyrights
            # to source headers. Preserve those original blocks too, with paths.
            if package["name"] == "ring" and path.suffix in SOURCE_SUFFIXES:
                raw = source_bytes(base, path).decode("utf-8")
                for match in COPYRIGHT_BLOCK.finditer(raw):
                    if "copyright" in match[0].lower():
                        headers.append((path.relative_to(base).as_posix(), match[0]))
    explicit = package.get("license_file")
    if explicit:
        path = Path(explicit)
        if not path.is_absolute():
            path = base / path
        notices[path.relative_to(base).as_posix()] = source_bytes(base, path)
    if not notices:
        raise ValueError(f"no upstream license/notice files for {package['name']}")
    if package["name"] == "ring":
        if not headers:
            raise ValueError("ring source copyright notices unavailable")
        notices["SOURCE-COPYRIGHTS.txt"] = ("Original source copyright/license comment blocks; retained without edits.\n\n" +
            "\n\n".join(f"Source: {path}\n{text}" for path, text in sorted(headers))).encode()
    if sum(map(len, notices.values())) > 8 << 20:
        raise ValueError("component notice bytes exceed bound")
    return notices


def write_set(output, prefix, notices):
    records = []
    for name, data in sorted(notices.items()):
        path = output / prefix / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        records.append({"path": path.relative_to(output).as_posix(), "sha256": sha(data), "bytes": len(data)})
    return records


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    rustc = subprocess.check_output(["rustc", "-vV"], text=True)
    host = next(line.removeprefix("host: ") for line in rustc.splitlines() if line.startswith("host: "))
    collected = {}
    # Host build tools/proc macros can have host-specific dependencies during a
    # cross-build. Retain both closures conservatively instead of omitting them.
    for target in sorted({host, args.target}):
        metadata = json.loads(subprocess.check_output([
            "cargo", "metadata", "--locked", "--offline", "--format-version", "1", "--filter-platform", target], cwd=root))
        collected.update((p["id"], p) for p in release_packages(metadata))
    packages = sorted(collected.values(), key=lambda p: (p["name"], p["version"]))
    args.output.mkdir(parents=False, exist_ok=False)
    report = {"schema": 1, "target": args.target, "host": host, "cargo_lock_sha256": sha((root / "Cargo.lock").read_bytes()),
              "roots": sorted(ROOTS), "scope": "union of host/target normal/build dependency closures; excludes dev-only dependencies",
              "components": []}
    for package in packages:
        report["components"].append({key: package.get(key) for key in ("name", "version", "license", "repository", "authors")})
        report["components"][-1]["files"] = write_set(args.output, package["name"] + "-" + package["version"], component_notices(package))
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    doc = sysroot / "share/doc/rust"
    standard = {"COPYRIGHT-library.html": source_bytes(doc, doc / "COPYRIGHT-library.html")}
    for path in sorted((doc / "licenses").glob("*.txt")):
        standard[path.relative_to(doc).as_posix()] = source_bytes(doc, path)
    if len(standard) < 2:
        raise ValueError("Rust standard library license texts unavailable")
    report["standard_library"] = {"rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
                                  "files": write_set(args.output, "rust-standard-library", standard)}
    (args.output / "INDEX.json").write_text(json.dumps(report, indent=2) + "\n")
    (args.output / "README.txt").write_text(
        "Native Rust notices\n\nINDEX.json identifies normal/build dependencies for hmux-web/hmux-agent,\n"
        "their declared license expressions and SHA256 of retained upstream notice files.\n"
        "Host and target closures include build dependencies conservatively; this is not a binary-size attribution.\n"
        "All upstream alternatives are retained. License policy is checked separately by cargo deny.\n"
        "The Rust standard library copyright index and referenced license texts are also retained.\n"
        "Other HMux ports, web/font assets and their notices remain in the bundle's main notice index.\n")
    print(f"Native notices: {len(packages)} crates and Rust standard library ({args.target})")


if __name__ == "__main__":
    main()
