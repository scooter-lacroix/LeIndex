## What

Brief description of the change.

## Why

What problem does this solve? Link to any related issues (e.g., `Fixes #123`).

## How

Summary of the approach taken. Highlight any non-obvious design decisions.

## Testing

- [ ] `cargo test --workspace` passes
- [ ] Tested manually with `leindex index` / `leindex search`
- [ ] MCP tools tested (if applicable)
- [ ] Dashboard builds (if frontend changes): `cd dashboard && bun run build`

## Checklist

- [ ] Code follows existing conventions and patterns
- [ ] No new warnings from `cargo clippy`
- [ ] Breaking changes are documented (if any)
- [ ] New MCP tools include JSON schema definitions
- [ ] Security-sensitive paths are excluded from discovery/scanning
