#!/usr/bin/env python3
"""Block low-level navigation tools and point Claude Code toward LeIndex."""

from __future__ import annotations

import json
import re
import sys


DIRECT_TOOL_ADVICE = {
    "Read": "Use `leindex_file_summary` for orientation, `leindex_read_symbol` for a specific implementation, or `leindex_read_file` for exact contents with PDG annotations.",
    "Grep": "Use `leindex_grep_symbols` for symbol lookup or `leindex_text_search` for exact text and regex matches.",
    "Glob": "Use `leindex_project_map` for directory and file exploration instead of raw globbing.",
}

SHELL_PATTERNS = [
    (
        re.compile(r"(^|[|(;&\s])(?:rg|grep|git\s+grep|ag|ack)(?:$|[\s|;&])"),
        "Use `leindex_text_search` for literal or regex search, or `leindex_grep_symbols` for symbol-aware lookup.",
    ),
    (
        re.compile(r"(^|[|(;&\s])(?:find|fd|ls|tree)(?:$|[\s|;&])"),
        "Use `leindex_project_map` for project structure instead of shell directory scans.",
    ),
    (
        re.compile(r"(^|[|(;&\s])(?:cat|head|tail|less|more)(?:$|[\s|;&])|sed\s+-n|awk\s+"),
        "Use `leindex_read_file`, `leindex_read_symbol`, or `leindex_file_summary` instead of raw file reads when exploring code.",
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
