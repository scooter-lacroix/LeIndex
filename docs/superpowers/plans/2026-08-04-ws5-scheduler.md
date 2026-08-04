# WS5: Fair Bounded Scheduler + Global Admission Controller

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development.

**Goal:** Replace unstepped `spawn_blocking` indexing + error-at-cap `MemoryCapGuard` with a fair, bounded scheduler where every heavy operation yields after a quantum, and a global admission controller that **defers** (not errors) when memory would exceed budget.

**Architecture:** A `Scheduler` owns per-(client,project) deficit-round-robin queues keyed by work class (§5.1). Heavy work implements `BoundedJob::step(budget)` yielding `Step::Yield(progress)|Complete`. An `AdmissionController` reads RSS + mmap resident bytes + provider reserve before each step; on near-cap it defers admission, evicts idle project caches, and reduces concurrency — never converts valid work into errors (spec §5.3).

**Spec refs:** §5 (Scheduling Model), §8.1 (threads), §11.3 (memory pressure).
**Depends on:** SP2 (daemon hosts the scheduler), SP3a (`GenerationLease` lets reads proceed during deferred writes).
**Tech Stack:** Rust, tokio, existing `MemoryCapGuard::current_rss_mb()`.

**Existing infra (reuse, don't reinvent):**
- `MemoryCapGuard::current_rss_mb()` (`src/cli/memory_cap.rs:15`) — RSS read primitive for admission.
- `index_slots` (`src/cli/registry.rs:242`) — same-project index consolidation; generalize into the scheduler's coalescing.
- `incremental_refresh_guard` (`src/cli/registry.rs:264`) — duplicate-refresh prevention.
- `evict_idle_engines` (registry_evict.rs) — pressure-response cache eviction.
- `ProjectWriteLock` (`src/cli/leindex/mod.rs:98`) — cross-process write serialization stays.

**What does NOT exist:** `WorkBudget`/`Step`/`BoundedJob`, DRR queues, work classes, a global admission controller that defers.

---

## File Structure

| File | Responsibility |
|---|---|
| `src/scheduler/mod.rs` | `Scheduler` owning DRR queues + admission |
| `src/scheduler/budget.rs` | `WorkBudget`, `Step<T>`, `BoundedJob` trait |
| `src/scheduler/queue.rs` | DRR queue per (client, project, class) with aging |
| `src/scheduler/admission.rs` | `AdmissionController`: RSS + mmap + provider reserve → admit/defer/reduce |
| `src/scheduler/classes.rs` | Work-class enum + weights (§5.1) |
| `src/cli/leindex/indexing/mod.rs` | Convert `index_project_inner` into a `BoundedJob` (stepped) |
| `src/cli/registry.rs` | Replace `index_slots` consolidation with scheduler admission |

---

## Task 1: WorkBudget + Step + BoundedJob trait

**Files:** `src/scheduler/budget.rs`, `src/scheduler/mod.rs` (add to lib)

- [ ] **Step 1: Write failing test** — a toy `BoundedJob` that counts items; `step(budget)` processes ≤ `max_items`, returns `Yield(progress)` or `Complete`.

```rust
pub struct WorkBudget {
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_items: usize,
    pub max_cpu_time: std::time::Duration,
}
pub enum Step<Progress> { Yield(Progress), Complete }
pub trait BoundedJob: Send {
    type Progress: Send;
    fn step(&mut self, budget: WorkBudget) -> anyhow::Result<Step<Self::Progress>>;
    fn estimated_next_bytes(&self) -> usize;
}
```

- [ ] **Step 2-4:** TDD.
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(scheduler): WorkBudget + Step + BoundedJob trait"
```

---

## Task 2: Work classes + weights

**Files:** `src/scheduler/classes.rs`

- [ ] **Step 1: Write failing test** — `WorkClass::InteractiveRead` gets higher weight than `IndexChunk`; aging boosts starved classes.
- [ ] **Step 2-4:** TDD per spec §5.1 (InteractiveRead, InteractiveCompute, Mutation, IndexChunk, Maintenance).
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(scheduler): work classes + DRR weights + aging"
```

---

## Task 3: DRR queue + coalescing

**Files:** `src/scheduler/queue.rs`

- [ ] **Step 1: Write failing test** — two projects' index jobs interleave fairly; same-project duplicate index requests coalesce by target source generation (§5.2); a starved project catches up via aging.
- [ ] **Step 2-4:** TDD. Generalize the `index_slots` consolidation into queue-level coalescing.
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(scheduler): DRR queue with coalescing + aging"
```

---

## Task 4: AdmissionController (defer, not error)

**Files:** `src/scheduler/admission.rs`

**Critical spec invariant (§5.3):** "A memory cap should prevent overlapping peaks, not convert valid work into errors." This is the explicit contrast with the current `MemoryCapGuard` which bails.

- [ ] **Step 1: Write failing test** — admission decisions: `Admit | Defer | ReduceConcurrency(shape)`; under simulated RSS near cap → `Defer`; under pressure → also evict idle caches (call into registry `evict_idle_engines`).

```rust
pub enum Admission { Admit, Dever, Reduce { batch_shape: BatchShape } }
pub struct AdmissionController { rss_reader: fn()->Result<u64>, mmap_resident: AtomicU64, provider_reserve: u64, cap: u64 }
impl AdmissionController {
    pub fn decide(&self, next_estimate: usize) -> Admission { todo!() }
}
```

- [ ] **Step 2-4:** TDD. Reuse `current_rss_mb()`; mmap resident read via `/proc/<pid>/smaps` (Linux) — reuse the WS1 memcheck sampler helper if available, else a minimal reader.
- [ ] **Step 5: Verify it NEVER errors** — property test: any RSS input yields Admit/Defer/Reduce, never an error.
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(scheduler): admission controller (defer/reduce, never error)"
```

---

## Task 5: Convert indexing into a BoundedJob

**Files:** `src/cli/leindex/indexing/mod.rs`

- [ ] **Step 1: Write failing test** — `IndexJob` implements `BoundedJob` with `step()` advancing one phase-chunk (scan N files / parse N files / merge PDG batch / TF-IDF pass / neural batch); yields between chunks.
- [ ] **Step 2-4:** TDD. Refactor `index_project_inner`'s phase loops into stepped chunks. Keep checkpoint/resume semantics (existing `ScanCheckpoint` etc.) — a yield point aligns with a checkpoint boundary.
- [ ] **Step 5: Verify resume** — kill mid-step; restart resumes at the last checkpoint (spec §11.2).
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(indexing): convert pipeline into stepped BoundedJob"
```

---

## Task 6: Scheduler run loop + admission wiring

**Files:** `src/scheduler/mod.rs`

- [ ] **Step 1: Write failing test** — multi-project contention: project A indexes in chunks, project B's reads interleave and are not starved; under simulated pressure the scheduler defers A's next chunk rather than erroring.
- [ ] **Step 2-4:** TDD. The scheduler owns ONE admission controller; before dequeuing an `IndexChunk`/`Maintenance` it calls `decide()`.
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(scheduler): run loop with admission-gated dequeue"
```

---

## Task 7: Replace MemoryCapGuard error path; wire into daemon

**Files:** `src/cli/memory_cap.rs`, `src/cli/leindex/indexing/mod.rs`, daemon entry (SP2)

- [ ] **Step 1:** Keep `MemoryCapGuard::current_rss_mb()` + `apply_hard_limit()` as primitives; **remove the bail-at-cap error path** from the indexing hot loop (replaced by admission deferral). `--max-memory` CLI flag now sets the admission cap, not a hard error.
- [ ] **Step 2: Write failing test** — indexing under cap completes; indexing over cap defers (and eventually completes after eviction) rather than erroring.
- [ ] **Step 3: Wire scheduler** as the daemon's single admission point (spec §5.3).
- [ ] **Step 4: Commit**

```bash
git commit -m "feat(scheduler): admission replaces error-at-cap; wired into daemon"
```

---

## Task 8: Validation + fairness measurement

- [ ] **Step 1: Validation suite** (`cargo fmt && clippy && test --workspace`).
- [ ] **Step 2: Fairness bench** — WS1 memcheck `contention_3c_2p` phase: assert no project's p95 read latency regresses vs baseline under concurrent indexing.
- [ ] **Step 3: No-error-under-pressure** — cgroup memory-pressure test (spec §13 scenario 18): indexing defers and completes, never errors.
- [ ] **Step 4: Record** `docs/baselines/2026-08-04-ws5-scheduler.json`.
- [ ] **Step 5: Commit**

```bash
git commit -m "docs(ws5): record fairness + no-error-under-pressure evidence"
```

---

## Handoff Summary (fill after execution)

```text
Workstream: WS5
Revision: 1.0
Invariant status: §5.3 — admission defers, never errors; no work shed silently (anti-cheat §2.1 #10)
Files changed: src/scheduler/*, src/cli/leindex/indexing/mod.rs, src/cli/registry.rs, src/cli/memory_cap.rs
Tests run/results: [fill]
Benchmark artifacts: docs/baselines/2026-08-04-ws5-scheduler.json
Before/after: indexing under cap errored → now defers+completes; p95 read under contention [fill]
TBD resolutions: none (all decided inline)
Unverified assumptions: [fill — e.g., mmap resident read path on non-Linux]
Known risks: stepped refactor of index_project_inner is large — gate behind feature flag
Rollback: feature flag `bounded-scheduler` OFF = legacy spawn_blocking + error-at-cap
Next: SP4 (WS6-9) consumes BoundedJob for per-stage streaming
```

## Spec-coverage check (§5)

| §5 requirement | Task |
|---|---|
| §5.1 work classes | 2 |
| §5.2 DRR + weights + aging | 3 |
| §5.2 coalesce same-project dup | 3 |
| §5.2 yield after quantum | 1, 5 |
| §5.2 cancellation at yield | 5 (checkpoint boundaries) |
| §5.2 no unbounded tokio task | 6 |
| §5.3 read memory before heavy step | 4 |
| §5.3 admit/defer/reduce | 4 |
| §5.3 never reduce semantic scope | 4 (test) |
| §5.3 cap prevents peaks, not errors | 4, 7 |
| §8.1 explicit small thread bounds | inherits SP1 Tokio workers |

## Forbidden shortcuts (anti-cheat §2.1)

- Do NOT shed work to meet memory budget (§2.1 #10) — defer, don't drop nodes/files.
- Do NOT serialize work so severely that p95/p99 regresses (§2.1 #9).
- Do NOT make valid repos fail under pressure (§2.1 #10) — defer + evict + complete.
