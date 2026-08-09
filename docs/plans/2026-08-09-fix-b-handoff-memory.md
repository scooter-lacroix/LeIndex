# Fix B Implementation Handoff and Memory

## Current repository state

- Repository: `/mnt/WD-SSD/code_index_update/LeIndexer-release-1.8.4`
- Branch: `v2.0.0`
- Existing commits through `16b2b835` are clean and validated.
- New report: `docs/plans/2026-08-09-fix-b-deep-investigation.md`
- New task list: `docs/plans/2026-08-09-outstanding-tasks.md`

## Verified facts

`src/embed/runtime.rs` still tokenizes the entire request in `run_onnx_embed` using one `tokenizer.encode_batch` call, then chunks already-built encodings for inference. The earlier pad/trim sub-batch correctness fix is present. The MIGraphX compile timeout/CPU fallback is present. The full Fix B per-sub-batch tokenization/pipeline goal is not present.

## Safe implementation order

1. Delegate a read-only design review of B1/B2/B4 to one subagent.
2. Implement B1/B2 sequentially in the primary worktree because they touch the same runtime call graph.
3. Delegate independent test-design/review work in parallel only after the runtime shape is stable.
4. Implement or adjust tests in the primary worktree, then run ONNX-gated checks.
5. Update all tracking docs only after code and tests prove the claims.
6. Run full repository validation.

## Delegation safety

- Do not spawn multiple agents that edit the same files.
- Prefer read-only subagents for architecture and test review.
- If an agent is given an implementation task, restrict it to a disjoint file set or use a separate worktree/branch if supported.
- Record every delegated task and result in the execution tracking document.
- If the session crashes, resume from this file and the outstanding task list; do not infer state from memory.

## Key correctness constraints

- Preserve fixed-batch MIGraphX/ROCm pad/trim semantics.
- Preserve collapsed-batch retry semantics without recursive fixed-batch padding.
- Preserve output ordering and `count * dimension` invariants.
- Keep tokenizer and ORT session use thread-safe.
- Keep cancellation checks between bounded sub-batches.
- Do not claim true pipelining unless a bounded producer/consumer implementation and tests exist.
