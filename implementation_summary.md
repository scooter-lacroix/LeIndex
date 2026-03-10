# PDG Implementation Summary
## For Agent Use — Read Before Touching Any File

This document accompanies three rewritten source files:
- `pdg_rewrite.rs` → replaces `src/graph/pdg.rs`
- `extraction_rewrite.rs` → replaces `src/graph/extraction.rs`
- `pdg_utils_rewrite.rs` → replaces `src/graph/pdg_utils.rs`

It provides a full rationale for every structural decision, a map of what changed and why,
and operating principles the agent must follow during implementation.

---

## 1. Core Structural Changes

### 1.1 New EdgeType: `Containment`

**Problem:** The original code used `Call` edges for Class→Method containment.
This meant:
- Any traversal filtering for `Call` edges accidentally included structural containment
- Impact analysis returned methods when querying class-level nodes, via the wrong semantic
- `get_forward_impact` on a Class node would traverse into all its methods' callees,
  which is not impact — it is just structure

**Fix:** `EdgeType::Containment` is a new variant. It is a structural edge only.
It must never appear in `allowed_edge_types` for semantic traversal configs.

**Rule for the agent:** Every place `add_call_graph_edges` was called with
containment intent (Phase 1b in the original `extract_pdg_from_signatures`) must
be replaced with `add_containment_edges`. Check for comments mentioning
"containment edge" or "Class → Method" near call edge additions.

**Serialization:** `edge_type_code` in `pdg_utils` now maps Containment to `5`.
This breaks backward compatibility with existing serialized PDGs that encoded
containment as Call (code `1`). Migration: on deserialize, detect edges where
`edge_type = Call` and `from.node_type = Class` and `to.node_type = Method`
and retype them as Containment. A migration helper is not included in the scaffold
but the agent should add one to `SerializablePDG::to_pdg` if backward compat
is required.

---

### 1.2 TraversalConfig replaces all `_bounded` / `_filtered` variants

**Problem:** The original had `get_forward_impact` (unbounded, returns up to the
full graph), `get_forward_impact_bounded` (depth + node cap), and no filtered
variants. The unbounded version was dangerous and there was no way to filter
by edge type at traversal time.

**Fix:** All traversal goes through `TraversalConfig`. No method accepts
just a depth parameter.

**TraversalConfig fields:**
```
max_depth:           Option<usize>    — None = unlimited (avoid in production)
max_nodes:           Option<usize>    — hard ceiling; STRONGLY RECOMMENDED always set
allowed_edge_types:  Option<Vec<..>>  — None = traverse all types
excluded_node_types: Option<Vec<..>>  — collect but skip these node types
min_complexity:      Option<u32>      — skip low-complexity nodes from results
min_edge_confidence: f32              — skip inferred edges below threshold
```

**Named constructors for common patterns:**
- `TraversalConfig::for_llm_context()` → tight (depth 3, 50 nodes, Call+DataDep only)
- `TraversalConfig::for_semantic_analysis()` → moderate (depth 5, 150 nodes)
- `TraversalConfig::for_impact_analysis()` → broad (no depth limit, 500 nodes cap)
- `TraversalConfig::for_import_graph()` → Import edges only

**Critical:** Existing callers of `get_forward_impact(node_id)` must be updated
to `forward_impact(node_id, &config)`. The unbounded DFS method is gone. This
is intentional — the agent should not add it back. If a caller genuinely needs
unbounded traversal, they should use `TraversalConfig::for_impact_analysis()` and
document why.

**Backward compatibility:** The method names changed:
```
get_forward_impact         → forward_impact(start, config)
get_backward_impact        → backward_impact(start, config)
get_forward_impact_bounded → forward_impact(start, config) with max_depth set
get_backward_impact_bounded→ backward_impact(start, config) with max_depth set
(new)                      → bidirectional_impact(start, config)
```

---

### 1.3 Embeddings externalized to EmbeddingStore

**Problem:** `Node.embedding: Option<Vec<f32>>` stored up to ~6KB per node inline.
At 50k nodes with 1536-dim embeddings: ~300MB in the graph struct. This bloats
serialization size and heap usage for any code path that doesn't need embeddings.

**Fix:** `EmbeddingStore` is a separate `HashMap<String, Vec<f32>>` keyed by `node.id`.
It is NOT part of `ProgramDependenceGraph`. The caller (embedding module) manages it.

**Impact on existing code:** Any code doing `node.embedding = Some(vec)` must
be updated to `embedding_store.insert(&node.id, vec)`. Any code reading
`node.embedding` must read from the store instead: `embedding_store.get(&node.id)`.

**Serialization:** `EmbeddingStore` is not serialized with the PDG. If persistence
is needed, serialize it separately. This keeps PDG binary files small.

**Note:** The `Node` struct no longer has an `embedding` field. If existing parsers
set `embedding`, that field assignment will fail to compile — this is intentional
and should be treated as a compile-time migration guide.

---

### 1.4 `find_by_name_in_file` O(n) elimination

**Problem:** Steps 2 and 3 of the original `find_by_name_in_file` did full graph
scans (`for nid in self.graph.node_indices()`). For a 100k-node graph called
frequently during context construction, this is the dominant CPU cost.

**Fix:** `name_lower_index: HashMap<String, Vec<NodeId>>` maps lowercase names
to node IDs. Step 2 is now an index lookup. Step 3 (substring match) is scoped
to the file when a `file_hint` is provided, reducing the scan space dramatically.

**The agent must ensure** `name_lower_index` is maintained in all mutation paths:
- `add_node`: insert into both `name_index` and `name_lower_index`
- `remove_node`: remove from both
- `deserialize` (`to_pdg`): rebuild both indices (handled in backward-compat block)

---

### 1.5 `add_edge` return type changed

**Problem:** Original returned `Option<EdgeId>` but always returned `Some`. This
was misleading — callers couldn't distinguish "edge added" from "edge not added".

**Fix:** Returns `EdgeId` directly. Debug assert validates both endpoints exist.
If you need fallible add, add a `try_add_edge` variant that checks `contains_node`.

---

## 2. Type Dependency Extraction

**Original problem:** `extract_type_dependencies` generated an O(n²) clique for
every type shared by N functions. 10 functions sharing `String` → 45 edges,
all semantically meaningless.

**New approach — 3 directional signals:**

### Signal A: Return → Parameter (confidence 0.85)
```
producer.return_type == consumer.param.type_annotation
Edge: producer → consumer
```
This is actual data flow. A function that produces `User` and another that
consumes `User` are likely connected.

### Signal B: Shared return type + explicit call (confidence 0.65)
```
a.return_type == b.return_type AND a calls b (or b calls a)
Edge: caller → callee
```
Captures transform chains: functions that process and pass along the same type.

### Signal C: Shared param type + explicit call (confidence 0.45)
```
a and b share a param type AND a calls b
Edge: a → b
```
Lowest confidence. Shared param type is weak; the call relationship is the signal.

### Excluded types
The constant `EXCLUDED_TYPES` lists types too common to serve as signals:
`str`, `String`, `int`, `bool`, `void`, `Vec`, `List`, `Option`, `Result`,
`HashMap`, generic single-letter types (`T`, `U`, `K`, `V`), etc.

**The agent must extend this list** if domain-specific ubiquitous types appear
that generate false positives (e.g., a project-wide `Context` type passed
to every function should be added to this list).

**Critical:** The `extract_type_dependencies` function is gone. Callers of it
(there shouldn't be any outside `extraction.rs`) must be updated to use
`extract_data_flow_edges` which returns `Vec<(NodeId, NodeId, String, f32)>`.
The 4-tuple adds `confidence: f32`. `add_data_flow_edges` now accepts this tuple.

**The original `add_data_flow_edges` signature:**
```rust
fn add_data_flow_edges(&mut self, flows: Vec<(NodeId, NodeId, String)>)
```
**New signature:**
```rust
fn add_data_flow_edges(&mut self, flows: Vec<(NodeId, NodeId, String, f32)>)
```
Update all callers.

---

## 3. Inheritance Detection

**Original problem:** Checked if two classes shared method names. Common method
names (`new`, `init`, `get`, `set`, `update`) caused massive false positives.
Only split on `::` so Python/JS methods with `.` separators were missed entirely.

**New approach — 4-signal evidence model with confidence scoring:**

### Signal 1: Super/parent call (confidence 0.90) — PRIMARY SIGNAL
```
Dog::speak.calls contains "super.speak" or "Animal.speak" or "parent.speak"
→ Dog inherits Animal, confidence 0.90
```
**How it works:** Scans the `calls` field of each method for:
- Calls starting with `super.` / `super::` / `parent.`
- Calls containing the other class's name + the same method name

**Language coverage:** Every OOP language encodes super calls in AST:
- Python: `super().speak()` → calls contains `super.speak`
- Java/C#: `super.speak()` → same
- Rust: `Base::speak(self)` → calls contains `Base.speak`
- Ruby: `super` → calls contains `super`
- PHP: `parent::speak()` → calls contains `parent.speak`
- JS/TS: `super.speak()` → same
Parsers must populate `SignatureInfo.calls` with these patterns for this signal
to fire. If a parser doesn't extract super calls, only signals 2-4 will work.

**This is why this signal is highest confidence:** it cannot fire on coincidental
method name sharing. It requires an actual reference to the parent.

### Signal 2: Method override count (confidence scales with count)
```
shared non-trivial method names:
  0–1: 0.0 (noise threshold)
  2:   0.45
  3:   0.60
  4+:  0.75
```
`is_common_method` filters `new`, `init`, `toString`, `equals`, `clone`, etc.
The constant `COMMON_METHOD_NAMES` should be extended with project-specific
common method names.

**The agent must NOT** lower the threshold below 2 shared methods. With 1 shared
method the false positive rate is too high.

### Signal 3: Naming conventions (confidence 0.50)
```
AbstractAnimal, BaseService, IRepository, UserMixin, ClickListener
→ likely abstract base/interface
```
Patterns checked:
- Prefixes: `Abstract`, `Base`, `I` (must be followed by uppercase)
- Suffixes: `Base`, `Mixin`, `Interface`, `Protocol`, `Trait`, `ABC`, `Abstract`

Applied only when at least 1 shared method exists (prevents firing on completely
unrelated classes that happen to have these names).

### Signal 4: Qualified name nesting (confidence 0.70)
```
cls_b = "pkg.Animal.Dog" and cls_a = "pkg.Animal"
→ Dog is nested within Animal (inheritance or inner class)
```
Checks if one class name is a prefix segment of the other.

### Confidence combination
The maximum confidence across all applicable signals is used. Minimum threshold
to produce an edge: `MIN_INHERITANCE_CONFIDENCE = 0.45`.

**Direction determination (child → parent):**
1. If super_call signal fired: the class making super calls is the child
2. If naming convention: abstract-named class is the parent
3. If nesting: more-nested (longer) qualified name is the child
4. Fallback: shorter class name = parent (more abstract/general)

### Edge targets
Inheritance edges connect the class nodes (if they exist) or the representative
first method of each class (if class nodes haven't been inferred yet).
After `infer_class_nodes_and_containment` runs, class nodes exist and will be used.

**Recommendation:** Run Phase 1b before Phase 3 to ensure class nodes exist.
The current phase ordering in `extract_pdg_from_signatures` does this correctly.

---

## 4. Containment Edge Restructuring

### Phase 1b logic
`infer_class_nodes_and_containment` replaces the inline class inference block.
It:
1. Groups method NodeIds by their class prefix (normalized with `.` separators)
2. Creates a Class node if one doesn't already exist
3. Returns containment edges as `Vec<(class_nid, method_nid)>`
4. Calls `pdg.add_containment_edges(containment)` — NOT `add_call_graph_edges`

**The agent must verify** that no other location in the codebase calls
`add_call_graph_edges` with containment intent. Search for patterns like:
```
// containment
pdg.add_call_graph_edges(...)
```
or any edge addition immediately following Class node creation.

---

## 5. Import Parsing — Multi-line Robustness

**Original problem:** Line-by-line parsing missed:
- Python: `from x import (\n    a,\n    b\n)`
- Rust: `use x::{\n    A,\n    B,\n};`
- TypeScript: `import {\n    A,\n    B\n} from 'react';`
- Go: `import (\n    "pkg1"\n    "pkg2"\n)`
- All 8 other supported languages: not handled at all

**New approach per language:**

| Language    | Strategy                           | Key edge case                          |
|-------------|------------------------------------|----------------------------------------|
| Python      | Regex with `(?s)` DOTALL for `()`  | `from x import (a,\nb)` multi-line     |
| JavaScript  | Regex DOTALL for `{}`              | `import type { ... }` TS type imports  |
| TypeScript  | Same as JS                         | `export { } from 'x'` re-exports       |
| Rust        | `collapse_multiline` + brace expand| Nested: `use x::{a::{B}, c}`           |
| Go          | Regex DOTALL for `()`              | `import (\n "a"\n "b"\n)`              |
| Java        | Regex with multiline flag          | `import static x.y.Z;`                 |
| C#          | Regex with multiline flag          | `using static X.Y.Z;`                  |
| Ruby        | Regex for require/require_relative  | `require_relative '../lib/foo'`        |
| PHP         | Regex for `use` + `require/include` | `use X\Y\Z as Alias;`                 |
| Lua         | Regex for `require()`              | `require('x.y.z')` → path conversion   |
| Scala       | `collapse_multiline` + brace expand| `import x.y.{A => B, C}`              |
| C/C++       | Regex for `#include`               | Both `<>` and `""` forms               |

**`collapse_multiline(source, prefix, terminator)`:**
Collects multi-line statements starting with `prefix` and ending at `terminator`.
Used for Rust (`use ` / `;`) and Scala (`import ` / `\n`).

**`expand_rust_use(stmt, out)`:**
Handles nested brace expansion: `std::{collections::HashMap, sync::Arc}` →
`["std.collections.HashMap", "std.sync.Arc"]`.
Handles `self`: `use foo::{self, Bar}` → `["foo", "foo.Bar"]`.

**Block comment stripping:**
`strip_block_comments` removes `/* ... */` before parsing for C-family languages.
Prevents false import detection inside commented-out code blocks.
Python/Ruby don't need this (their block "comments" are string literals, not
parseable as imports).

**The agent must NOT** replace the regex-based approach with another line-by-line
parser. The line-by-line approach is a known-bad pattern for this use case.
If a language-specific parser is available (tree-sitter), prefer it, but the
regex approach is sufficient for import extraction where full AST isn't needed.

---

## 6. PDG Utils Changes

### 6.1 `ParsingResult` needs `source_bytes: Option<Vec<u8>>`

**The agent must add this field to `ParsingResult`:**
```rust
pub struct ParsingResult {
    pub file_path: PathBuf,
    pub language: Option<String>,
    pub signatures: Vec<SignatureInfo>,
    pub source_bytes: Option<Vec<u8>>,  // ADD THIS
    pub error: Option<String>,
    pub parse_time_ms: u64,
}
```
The parallel parser that populates `ParsingResult` must store source bytes here
instead of discarding them after parsing. This eliminates the disk re-read in
`merged_pdg_from_results`.

If adding this field to `ParsingResult` is not feasible (e.g., memory constraints
for large codebases), fall back to reading from disk but wrap the read in a
`Result` and handle the error explicitly rather than using `unwrap_or_default`.

### 6.2 Single-pass edge key collection in merge loop

**Original:** `collect_edge_keys(target)` was called once per file merge, rebuilding
the full edge key set on every iteration.

**New:** `existing_edges: HashSet` is built once before the loop and passed as
a mutable reference to `merge_pdgs_with_keys`. Each call only adds new keys,
never rebuilds from scratch. This is O(E_total) total instead of O(E * F)
where F is the number of files.

### 6.3 RelinkConfig

Scoring constants are no longer magic numbers. `RelinkConfig` documents each one.
Default values are preserved from the original but `max_candidates` is raised
from 1 to 3. Use `RelinkConfig::strict()` to get the original single-match behavior.

### 6.4 Orphan cleanup uses pre-computed degree

Original `cleanup_orphan_external_modules` recomputed the full degree map of
all nodes. New version only checks the external nodes that lost edges during
relinking — the set is tracked in `to_remove`. This is O(removed_edges) not O(E).

---

## 7. Test Coverage Gaps (Agent Should Address)

The scaffold tests cover the primary happy paths. The agent should add tests for:

1. **Inheritance direction with abstract base naming:**
   `AbstractAnimal::speak` + `Dog::speak` (2 methods) → Dog is child, AbstractAnimal is parent

2. **Import parsing for each of the 12 languages:**
   At minimum: Python multi-line, Rust nested braces, TypeScript re-export, Go import block

3. **TraversalConfig: confidence filtering:**
   Graph with a 0.3-confidence DataDependency edge should not be traversed when
   `min_edge_confidence = 0.5`

4. **Merge idempotency:**
   Merging the same PDG twice should not produce duplicate edges

5. **Signal A data flow direction:**
   Verify edge goes producer → consumer, not the other direction

6. **Containment not traversed by semantic configs:**
   `forward_impact` with `for_semantic_analysis()` should not traverse through
   Class→Method containment edges

7. **EmbeddingStore independence:**
   Serializing and deserializing a PDG should not affect the EmbeddingStore
   (it's external)

---

## 8. Breaking API Changes Summary (for callers outside these 3 files)

| Old API                                    | New API                                          |
|--------------------------------------------|--------------------------------------------------|
| `pdg.get_forward_impact(nid)`              | `pdg.forward_impact(nid, &config)`               |
| `pdg.get_backward_impact(nid)`             | `pdg.backward_impact(nid, &config)`              |
| `pdg.get_forward_impact_bounded(nid, d)`   | `pdg.forward_impact(nid, &config)` with depth    |
| `pdg.get_backward_impact_bounded(nid, d)`  | `pdg.backward_impact(nid, &config)` with depth   |
| `pdg.add_edge(..) -> Option<EdgeId>`       | `pdg.add_edge(..) -> EdgeId`                     |
| `node.embedding = Some(vec)`               | `embedding_store.insert(&node.id, vec)`          |
| `add_data_flow_edges(Vec<(N,N,String)>)`   | `add_data_flow_edges(Vec<(N,N,String,f32)>)`     |
| `add_inheritance_edges(Vec<(N,N)>)`        | `add_inheritance_edges(Vec<(N,N,f32)>)`          |
| `add_call_graph_edges(..)` (containment)   | `add_containment_edges(..)`                      |
| `merged_pdg_from_results(results)`         | `merged_pdg_from_results(results, config)`       |
| `ParsingResult` (no source_bytes)          | `ParsingResult` (with `source_bytes: Option<..>`) |

---

## 9. Implementation Order

The agent should implement in this order to minimize cascading compile errors:

1. **`pdg.rs`** — Core types must be defined first. Everything depends on them.
   - Add `EdgeType::Containment`
   - Add `TraversalConfig`
   - Add `EmbeddingStore`
   - Add `name_lower_index` to struct and all mutation paths
   - Fix `add_edge` return type
   - Replace traversal methods
   - Update serialization to handle `name_lower_index` and `Containment`

2. **`extraction.rs`** — Depends on updated `pdg.rs` types.
   - Update `add_data_flow_edges` call signature (add confidence f32)
   - Update `add_inheritance_edges` call signature (add confidence f32)
   - Replace containment call with `add_containment_edges`
   - Replace `extract_type_dependencies` with `extract_data_flow_edges`
   - Replace `extract_inheritance_edges` with new evidence model
   - Replace import parsing with multi-line regex approach

3. **`parse/parallel.rs`** — Add `source_bytes` to `ParsingResult`.
   (Outside the 3 files but required for `pdg_utils.rs` to compile)

4. **`pdg_utils.rs`** — Depends on all of the above.
   - Update `merged_pdg_from_results` signature (add `RelinkConfig`)
   - Use `source_bytes` from `ParsingResult`
   - Use single-pass edge key collection
   - Add `RelinkConfig`
   - Update `edge_type_code` for `Containment`

5. **All callers** — Update call sites per the breaking API table above.
   Search for `get_forward_impact`, `get_backward_impact`, `add_call_graph_edges`,
   `add_data_flow_edges`, `add_inheritance_edges`, `node.embedding`.

---

## 10. Operating Principles for the Agent

1. **Do not add back unbounded traversal.** If a caller asks for unlimited traversal,
   they must use `TraversalConfig::for_impact_analysis()` and document the intent.
   The `max_nodes: Some(500)` cap in that config is still a safety net.

2. **Do not lower `MIN_INHERITANCE_CONFIDENCE` below 0.45.**
   The threshold is already permissive. Lower values will flood the graph with
   false inheritance edges that poison impact analysis.

3. **Do not add types to `EXCLUDED_TYPES` without evidence.**
   Only exclude types that demonstrably produce false positive data flow edges
   in practice. The list is already comprehensive for primitives.

4. **Preserve edge confidence in all merge operations.**
   `merge_pdgs` copies edges from source to target. The `confidence` field in
   `EdgeMetadata` must be preserved. Do not zero it out or strip it.

5. **The containment edge is never a semantic edge.**
   Never include `EdgeType::Containment` in the `allowed_edge_types` of a
   `TraversalConfig` used for impact analysis or LLM context construction.
   It is a structural/display edge only.

6. **Regex compilation is not free.**
   The import parsing functions compile regexes on every call. In production,
   these should be compiled once as lazy statics (`once_cell::sync::Lazy` or
   `std::sync::OnceLock`). The scaffold uses inline `Regex::new` for clarity;
   the agent should add lazy static compilation before production deployment.

7. **The `regex` crate must be added to Cargo.toml.**
   Current `extraction.rs` uses `regex::Regex`. Verify it is in dependencies.
   If the project already uses `regex`, no change needed. If not, add:
   `regex = "1"` to `[dependencies]` in `Cargo.toml`.
