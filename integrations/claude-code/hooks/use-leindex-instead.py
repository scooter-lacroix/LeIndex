#!/usr/bin/env python3
"""Block low-level navigation tools and point Claude Code toward LeIndex."""

from __future__ import annotations

import json
import re
import sys


DIRECT_TOOL_ADVICE = {
    "Read": "Use `leindex_explore` with mode=file_summary for orientation, mode=read_symbol for a specific implementation, or mode=read_file for exact contents with PDG annotations.",
    "Grep": "Use `leindex_explore` with mode=find for exact text, regex and symbol matches (add target=symbols for definitions; paths=[...] for directories outside the project).",
    "Glob": "Use `leindex_explore` with mode=project_map for directory and file exploration instead of raw globbing.",
}

SHELL_PATTERNS = [
    (
        re.compile(r"(^|[|(;&\s])(?:rg|grep|git\s+grep|ag|ack)(?:$|[\s|;&])"),
        "Use `leindex_explore` with mode=find for literal or regex search (target=symbols for definitions, paths=[...] outside the project).",
    ),
    (
        re.compile(r"(^|[|(;&\s])(?:find|fd|ls|tree)(?:$|[\s|;&])"),
        "Use `leindex_explore` with mode=project_map for project structure instead of shell directory scans.",
    ),
    (
        re.compile(r"(^|[|(;&\s])(?:cat|head|tail|less|more)(?:$|[\s|;&])|sed\s+-n|awk\s+"),
        "Use `leindex_explore` with mode=read_file, read_symbol or file_summary instead of raw file reads when exploring code.",
    ),
]


def block(message: str) -> int:
    print(
        "LeIndex guidance hook blocked this action.\n"
        f"{message}\n"
        "If LeIndex cannot answer the need, explain that limitation before falling back.",
        file=sys.stderr,
    )
    return 2


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except json.JSONDecodeError:
        return 0

    tool_name = payload.get("tool_name") or payload.get("toolName") or ""
    tool_input = payload.get("tool_input") or payload.get("toolInput") or {}

    if tool_name in DIRECT_TOOL_ADVICE:
        return block(DIRECT_TOOL_ADVICE[tool_name])

    if tool_name != "Bash":
        return 0

    command = (tool_input.get("command") or "").strip()
    if not command or "leindex" in command:
        return 0

    lower_command = command.lower()
    for pattern, advice in SHELL_PATTERNS:
        if pattern.search(lower_command):
            return block(advice)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
