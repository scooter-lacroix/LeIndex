## What

Brief description of the change.

## Why

What problem does this solve? Link to any related issues (e.g., `Fixes #123`).

## How

Summary of the approach taken. Highlight any non-obvious design decisions.

## Memory Impact

- [ ] No memory-impact: this PR does not change caps, caches, buffers, or baseline files
- [ ] Memory-impact attached: `cargo xtask memcheck` diff output or justification is included below

<details>
<summary>Memcheck diff (if applicable)</summary>

<!-- Paste `cargo xtask memcheck` output here when caps, caches, buffers, or baselines change. -->

</details>

<!-- If baseline files under docs/memory/baselines/ are modified, include one of:
     "Baseline update: <reason>", "Baseline change: <reason>",
     "Baseline justification: <reason>", or "Rebaseline: <reason>"
     so the baseline-metadata-check CI gate passes. -->

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
