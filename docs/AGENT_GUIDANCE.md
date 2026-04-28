# Agent Guidance Packs

LeIndex now ships a reusable guidance pack for AI coding agents that should prefer LeIndex MCP tools over raw file and shell navigation.

## Shared Skill Pack

Canonical skill location in this repo:

- `integrations/skills/leindex-toolkit/`

What it contains:

- `SKILL.md`: when to prefer LeIndex and the complete tool list.
- `references/tool-selection.md`: replacement map for `Read`, `Grep`, `Glob`, `rg`, `find`, `ls`, `cat`, and related workflows.
- `references/tool-schemas.md`: per-tool JSON schemas exported from the live CLI surface.

## Claude Code

Use both the shared skill and the hook.

1. Copy the skill directory to either:
- `~/.claude/skills/leindex-toolkit/`
- `.claude/skills/leindex-toolkit/`

2. Merge `integrations/claude-code/settings.example.json` into:
- `~/.claude/settings.json`
- or project-local `.claude/settings.json`

3. Make the hook executable:

```bash
chmod +x integrations/claude-code/hooks/use-leindex-instead.py
```

The hook blocks `Read`, `Grep`, `Glob`, and shell-based `rg`/`grep`/`find`/`ls`/`cat` style exploration and reminds Claude which LeIndex tool to use instead.

## Codex

Install the same shared skill pack into:

- `~/.codex/skills/leindex-toolkit/`

Codex already understands `SKILL.md`-based skills, so no extra translation layer is needed.

## Gemini CLI, Amp, OpenCode, Qwen, and iFlow

These agents already have MCP config examples in the public README surfaces. Reuse the shared skill content as your project or global instruction pack:

- Copy the guidance from `integrations/skills/leindex-toolkit/SKILL.md`
- Keep `references/tool-selection.md` nearby for tool-choice reminders
- Use `references/tool-schemas.md` when you need the exact LeIndex arguments

If an agent later adds first-class hook or skill packaging, this repo can grow a dedicated adapter beside the Claude Code one.
