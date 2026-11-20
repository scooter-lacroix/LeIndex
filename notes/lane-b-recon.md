# Lane B recon (verified in worktree @ base 7e88fabc)

## Text surface (memory claim verified)
- MCP transport (`server.rs` ~L1226): L1 (default tier) = `trim_llm_payload` -> `render_tool_output_plain` -> per-tool renderer in `output/render/mod.rs` TOOL_RENDERERS table. L2 = raw untrimmed handler JSON. L0 = identity card.
- `preview_token` IS in the preview JSON and trim_edit keeps it, but the L1 text render (`render_edit_preview`) NEVER prints it => memory "text surface carries no preview_token" is TRUE for the text surface. Fix: first line of `render_edit_preview`.
- `render_split_diff`/`render_unified_diff` in `output/diff.rs`; `render_one_diff` (dispatch for all 5 diff payload shapes incl. rename's `diffs[]` list) calls `render_split_diff` => THE side-by-side path. `DiffFormatter` has zero callers outside diff.rs.
- Preview handler `diff_text` already = `render_unified_diff` (full lines, git-style); but the L1 render uses `render_one_diff` (split). Both must become the same compact unified renderer.

## Token flow
- `EditCacheEntry { file_path, preview_token, original_text, modified_text, changes, timestamp }` in `edit_cache.rs`; cache keyed by FILE PATH (hot LRU 10k + cold JSON under `<storage>/edit_cache/<blake3(path)>.json`). Token lookup today = get(path) + token compare, so apply-by-token REQUIRES file_path (`apply_request_args` -> `extract_string(args,"file_path")` errors when absent).
- Staleness today: `atomic_write_with_expected_async(path, modified, original)` — expected-content guard (edit/engine.rs L258). Token-mode apply reads original/modified from the CACHE (no fresh read), so the guard does the staleness check implicitly at write time. No explicit hash. Fix: store per-file blake3 in entry, re-validate BEFORE write with a clear error.

## changes[] today
- `parse_edit_changes` (helpers.rs L574): items are `{type: replace_text|rename_symbol}` against ONE file (file_path arg). `replace_text` without start_byte/end_byte locates old_text by exact find -> ws-normalized fallback (`find_normalised_whitespace`). No multi-file support anywhere.
- `EditChange::ReplaceText {start, end, new_text}` production constructors: ONLY helpers.rs parse_replace_change (3 sites). Adding an optional `file_path` field is safe (all matches use `..`); test constructors in edit_cache.rs + edit/mod.rs need the field added.

## Risk today
- `compute_risk_level(affected_nodes, affected_files)` (preview handler): breadth-only heuristic; renders `Risk level: low` always + `Change count: N` + `Breaking: ...` lines in render_edit_preview. No caller/pub/tests signal. PDG nodes (`graph/pdg.rs Node`) have NO visibility field; `validation/drift.rs::is_public_api` is an admitted stub ("consider all functions and classes as potential API"). Real pub detection must read the def line from source (I'll regex `\bpub\s` on the line starting the node's byte_range).

## Apply echo today
- `render_edit_apply` prints status + File + Message + Affected symbols/files counts + Breaking count + `Surrounding region:` block via `render_edit_region`; handler emits `edit_region` (L first_diff-5..+10 of modified).
- Tier is transport-only (`tier.rs take_from` removes the arg before handlers run) => handlers cannot see tier. Plan: handler keeps emitting `edit_region`; render + trim DROP it => region available only via tier=l2 raw JSON.

## Syntax validation
- `validation/syntax.rs` SyntaxValidator: parse_content + find_error_nodes but returns ONE error per change (early return) and maps name->LanguageId via a private 15-language match. Reuse `LanguageId::from_extension` + `from_cache` (parse/grammar.rs; GLOBAL_GRAMMAR_CACHE) directly in my module; doc flavors (Markdown/Rst/Adoc/PlainText) get a json stand-in grammar => skip validation for those extensions (report `syntax: skipped (no grammar)`).
- `ResolvedEditChange.infer_language()` exists (validation) — but I parse the WRITTEN FILE (not a change struct): extension -> LanguageId -> Parser -> walk error/missing nodes.

## Ownership per SPEC
- B owns render/mod.rs: render_edit_preview, render_edit_region, render_edit_apply, edit_apply_status, render_rename_symbol, render_write*. diff.rs = edit-preview domain (not claimed by A/C/D/E/F/G). grouped.rs: add branch rows + one-line guide entries only.
- Integration wires validate:"check" to lane F check_scoped; I ship ONE error site for it.

## Gates
- env: source /mnt/WD-SSD/Prod/leindex-roadmap/env.sh 2 (target dir leindex-rm-target-2, jobs=4, no debuginfo, never --release).
- fmt: cargo fmt --all --check; clippy: cargo clippy --workspace --all-targets -- -D warnings; tests scoped (cargo test --lib <modules> + touched files). NEVER full workspace suite. Stop if env.sh reports <12G free.
