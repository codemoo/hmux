"""Frozen source provenance for optional external migration/benchmark oracles.

Normal Rust builds/tests never import this module. Old source stays in Git history,
not the active tree. Explicit external binaries must match their recorded hashes.
"""
import hashlib
from pathlib import Path
import subprocess

BASELINE = "c061f28fe7ea8e865578ac1189240447d0ebaa6f"
ROOT = Path(__file__).resolve().parents[1]


def baseline_digest(name):
    content = subprocess.check_output(
        ["git", "show", f"{BASELINE}:{name}"], cwd=ROOT, stderr=subprocess.PIPE)
    return hashlib.sha256(content).hexdigest()
