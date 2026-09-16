#!/usr/bin/env python3
"""Extract selected HMuxStore methods verbatim into a minimal smoke shell."""

from __future__ import annotations

import pathlib
import re
import sys


METHODS = [
    "acceptCatalogProjection",
    "setAlias",
    "setHidden",
    "matches",
    "requireWorkspaceBinding",
    "publishAliasProjection",
    "publishHiddenProjection",
    "resortSessionRows",
    "beginAliasConfirmation",
    "beginHiddenConfirmation",
    "isNewConfirmation",
    "setQuickSwitcherInteraction",
    "setInteraction",
    "scheduleDeferredApplyAfterEvent",
    "applyOrDefer",
    "apply",
    "retryRecoveredTabs",
    "sidebarGroupingKey",
]


def extract_method(source: str, name: str) -> str:
    match = re.search(
        rf"(?m)^[ \t]*(?:private[ \t]+)?func[ \t]+{re.escape(name)}[ \t]*\(", source
    )
    if match is None:
        raise SystemExit(f"missing production method: {name}")
    start = match.start()
    previous_line_start = source.rfind("\n", 0, max(0, start - 1)) + 1
    previous = source[previous_line_start:start].strip()
    if previous.startswith("@"):
        start = previous_line_start
    opening = source.find("{", match.end())
    if opening < 0:
        raise SystemExit(f"missing opening brace: {name}")
    depth = 0
    for index in range(opening, len(source)):
        char = source[index]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return source[start : index + 1]
    raise SystemExit(f"unterminated production method: {name}")


def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit("usage: extract_store_methods.py STORE SHELL OUTPUT")
    store_path, shell_path, output_path = map(pathlib.Path, sys.argv[1:])
    source = store_path.read_text()
    methods = "\n\n".join(extract_method(source, name) for name in METHODS)
    shell = shell_path.read_text()
    marker = "// __EXACT_PRODUCTION_METHODS__"
    if shell.count(marker) != 1:
        raise SystemExit("invalid smoke shell marker")
    output_path.write_text(shell.replace(marker, methods))


if __name__ == "__main__":
    main()
