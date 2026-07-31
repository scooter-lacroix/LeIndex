---
name: Bug Report
about: Report a bug or unexpected behavior in LeIndex
title: "[Bug] "
labels: ["type:bug", "state:needs-triage"]
assignees: ''
---

## Describe the Bug

A clear description of what the bug is.

## To Reproduce

Steps to reproduce the behavior:

1. Run `leindex ...`
2. With this input: ...
3. See error

## Expected Behavior

What you expected to happen.

## Actual Behavior

What actually happened. Include error messages or unexpected output.

## Environment

- **OS:** (e.g., Ubuntu 24.04, macOS 15, Windows 11)
- **LeIndex version:** (`leindex --version`)
- **Rust version:** (`rustc --version`, if building from source)
- **Install method:** (curl installer / cargo install / from source)

## Priority

- [ ] P0-critical (blocks release, data loss, security)
- [ ] P1-high (should fix before next release)
- [ ] P2-medium (should fix in current milestone)
- [x] P3-low (backlog)

## Affected Area

- [ ] area:parse (tree-sitter parsing)
- [ ] area:search (search engine)
- [ ] area:graph (dependency graph)
- [ ] area:storage (SQLite/libSQL)
- [ ] area:cli (command-line interface)
- [ ] area:mcp (MCP server)
- [ ] area:edit (code editing)
- [ ] area:onnx (neural embeddings)
- [ ] area:packaging (distribution)
- [ ] area:infra (CI/CD)

## Logs / Output

```
Paste any relevant log output or error messages here.
```

## Additional Context

- Are you using LeIndex as a CLI tool or via MCP?
- If MCP, which client? (Claude Code, Cursor, other)
- How large is the codebase being indexed? (approximate file count / languages)
