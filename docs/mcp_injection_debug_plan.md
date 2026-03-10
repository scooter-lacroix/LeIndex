# LeIndex — Complete Bug Report & Fix Specification
> Analysed: 2026-02-27
> Claude Code version: 2.1.62 · LeIndex version: 0.1.0
> Source analysed: `/mnt/WD-SSD/code_index_update/LeIndexer/`
> Two independent bugs documented with exact file + line citations.

---

## Bug 1 — MCP Stdio Tool Injection Failure

### 1.1 Observed Symptoms

From `~/.claude/debug/latest`:
```
22:08:58.378Z  MCP server "leindex": Starting connection (timeout 30000ms)
22:08:58.668Z  [ERROR] Server stderr: Starting LeIndex MCP stdio server...
22:08:58.668Z  Successfully connected to stdio server in 291ms
22:08:58.668Z  Connection established with capabilities:
               {"hasTools":true,"hasPrompts":false,"hasResources":false,
                "hasResourceSubscribe":false,"serverVersion":{"name":"leindex","version":"0.1.0"}}
22:08:58.669Z  STDIO connection dropped after 0s uptime
22:08:58.669Z  Connection error: JSON Parse error: Unexpected EOF
```

All four events occur within **1 millisecond** — the connection is established and killed in the
same event-loop tick, before any tool call registers.

Last working session (`debug/0c4a2c06`, 2026-02-17):
```
02:20:40Z  Calling MCP tool: leindex_index
02:20:40Z  STDIO connection dropped after 138s uptime   <- drop AFTER tool call
02:20:40Z  Tool 'leindex_index' completed successfully in 156ms
```

Since Feb 18: every session shows `0s uptime` drop during init — before any tool call.

Confirmation that leindex process is NOT crashing:
```bash
ps aux | grep "leindex mcp"
# scooter  2155208  0.0  0.1 ... leindex mcp  (STILL RUNNING)
```
Claude Code closed its own pipe handles and declared the connection dead. Leindex is waiting
on stdin indefinitely.

---

### 1.2 Confirmed Root Cause — Double-Newline in Response Writer

**File**: `crates/lepasserelle/src/cli.rs`
**Lines**: 676 and 724 (identical pattern in two branches)

```rust
// Line 676 — error response path:
} else if writeln!(stdout, "{}\n", response).is_err() {
    break;
}

// Line 724 — normal response path (fires during init):
} else if writeln!(stdout, "{}\n", response_json).is_err() {
    eprintln!("[ERROR] Failed to write to stdout");
    break;
}
```

`writeln!` already appends `\n` to its output. The extra `\n` inside the format string `"{}\n"`
produces a **double newline** after every JSON response:

```
{"jsonrpc":"2.0","id":1,"result":{...initialize response...}}\n
\n                          <- second empty line
```

Claude Code's JSON-RPC reader processes messages line-by-line. After consuming line 1 (the
valid JSON), it reads line 2 (the empty line), attempts `JSON.parse("")`, and receives:

```
JSON Parse error: Unexpected EOF
```

Claude Code interprets this as a broken connection, closes its pipe handles, and logs
`STDIO connection dropped after 0s uptime`. The leindex subprocess is unaware — it is still
alive waiting for more stdin.

**Note on `hasResourceSubscribe: false`**: This field does NOT appear anywhere in the
LeIndexer source code. The `initialize` handler (cli.rs line 856-868) only sends
`"capabilities": {"tools": {}}`. The `hasResourceSubscribe: false` visible in the
Claude Code debug log is Claude Code's own derived capability annotation — it logs which
capability fields are absent from the server response. It is not a field that leindex sends,
and it is not a factor in the failure.

**Why the double-newline does not fire in Content-Length mode**: The code switches to
Content-Length framing (lines 712-723) only when `use_content_length` becomes `true`, which
happens only if Claude Code sends the first request with a `Content-Length:` header.
Claude Code's stdio MCP client sends plain newline-delimited JSON without headers, so
`use_content_length` stays `false` and the broken `else if writeln!(stdout, "{}\n", ...)`
branch fires every time.

**Why the manual pipe test succeeds**: When testing with:
```bash
printf '...init...\n...tools/list...\n' | leindex mcp
```
All messages are piped together without waiting for responses. leindex processes them
sequentially and the double newlines appear in the output, but a human observer ignores blank
lines between JSON objects. Claude Code's streaming reader does not — it tries to parse each
line.

---

### 1.3 The Fix

**Change two lines in `crates/lepasserelle/src/cli.rs`**:

```diff
- } else if writeln!(stdout, "{}\n", response).is_err() {        // line 676
+ } else if writeln!(stdout, "{}", response).is_err() {

- } else if writeln!(stdout, "{}\n", response_json).is_err() {   // line 724
+ } else if writeln!(stdout, "{}", response_json).is_err() {
```

`writeln!` already writes the terminating newline. Removing the `\n` from the format string
restores single-newline-terminated JSON messages, which is what Claude Code's line reader
expects.

**Rebuild and reinstall after patching**:
```bash
cd /mnt/WD-SSD/code_index_update/LeIndexer
cargo build --release -p lepasserelle
cp target/release/leindex $(which leindex)
```

---

### 1.4 Verification Steps

After patching:
1. Start a new Claude Code session
2. Check `/mcp` panel — leindex should show as `connected`
3. Ask Claude Code "what leindex tools do you have?" — should list all 7 tools
4. Confirm `~/.claude/debug/latest` no longer shows
   `STDIO connection dropped after 0s uptime`

Optional wire-level spy:
```bash
cat > /tmp/leindex-spy << 'EOF'
#!/bin/bash
tee /tmp/mcp-spy-stdin-$$.log | leindex mcp 2>/tmp/mcp-spy-stderr-$$.log \
  | tee /tmp/mcp-spy-stdout-$$.log
EOF
chmod +x /tmp/leindex-spy
```
Point `~/.claude.json` command at `/tmp/leindex-spy`, start a session, then:
```bash
xxd /tmp/mcp-spy-stdout-*.log | head -60   # look for \n\n (0a 0a) after JSON
```

---

## Bug 2 — Semantic Score Permanently Zero (`semantic=0.0`)

### 2.1 Observed Symptoms

Every search and deep-analyze result shows zero semantic contribution regardless of query:
```
Example score breakdown:
  overall:    0.461
  semantic:   0.000   <- ZERO across all results, all queries
  structural: 0.121
  text_match: 1.000
```

This is **not project-specific**. It is reproducible for any indexed project because the
cause is inside the LeIndex embedding pipeline itself.

---

### 2.2 Confirmed Root Cause — Placeholder Hash Function Used as Embedding

**File**: `crates/lepasserelle/src/leindex.rs`
**Lines**: 1065–1107

```rust
// Entry point for query embedding (line 1065-1070):
pub fn generate_query_embedding(&self, query: &str) -> Vec<f32> {
    // Only the query text is used; file_path and content are not applicable here.
    self.generate_deterministic_embedding(query, "", "")
}

// The actual embedding generator (lines 1077-1107):
fn generate_deterministic_embedding(
    &self,
    symbol_name: &str,
    _file_path: &str,   // <- IGNORED (underscore prefix)
    _content: &str,     // <- IGNORED (underscore prefix)
) -> Vec<f32> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Source comment: "In a real system, this would be an LLM embedding of the content."
    // This is the placeholder — it only hashes the symbol name.
    let mut base_hasher = DefaultHasher::new();
    symbol_name.to_lowercase().hash(&mut base_hasher);
    let base_hash = base_hasher.finish();

    let mut embedding = Vec::with_capacity(768);
    for i in 0..768 {
        let mut hasher = DefaultHasher::new();
        base_hash.hash(&mut hasher);
        i.hash(&mut hasher);
        let hash_val = hasher.finish();
        // Maps 64-bit hash to f32 in [-1.0, 1.0]
        let val = (hash_val as f64 / u64::MAX as f64) * 2.0 - 1.0;
        embedding.push(val as f32);
    }
    embedding
}
```

**What this actually produces**: 768 pseudorandom floats derived from hashing only the
symbol name. File content and source code are never used. The `_file_path` and `_content`
parameters are explicitly suppressed with underscore prefixes.

**Why cosine similarity is ~0.0 for all real queries**: Two pseudorandom 768-dimensional
vectors are statistically orthogonal. The expected cosine similarity between
`hash("fallback")` and `hash("create_and_emit_fallback_event")` is 0.0 with standard
deviation ~1/sqrt(768) = 0.036. The vector search (`lerecherche/src/vector.rs:126-146`)
computes cosine similarity and returns the top-K nodes with the highest hash similarity —
which are effectively random nodes with no semantic relationship to the query.

**Why the reported score is exactly `0.000`**: The `semantic_score` in search results
(`lerecherche/src/search.rs:585-589`) is populated from the vector search HashMap:

```rust
let semantic_score = if query.semantic {
    *vector_results.get(&node.node_id).unwrap_or(&0.0)
} else {
    0.0
};
```

The vector search only returns `top_k=10` results. For any node NOT in those 10 (which is
all text-match hits, since text matches and random-hash-nearest-neighbours are disjoint
sets), `semantic_score = 0.0` exactly.

The 10 random vector hits do have small non-zero semantic scores (0.03-0.08) but they score
so low on `text_match` that they don't pass the overall threshold
(`threshold: Some(0.1)`) or are outranked and not displayed.

**Scoring formula** (`lerecherche/src/ranking.rs:54-82`, `HybridScorer::score()`):
```rust
let overall = semantic * 0.5       // semantic weight
            + structural * 0.1     // structural weight
            + text_match * 0.4;    // text weight
```
With `semantic=0.0`, maximum achievable `overall` = `0.0 + structural*0.1 + 1.0*0.4 = 0.4`.
Observed values around `0.461` include a small structural bonus from node complexity.

**There is no CodeRankEmbed integration**: The 768-dimension constant and the "CodeRankEmbed"
string in `legraphe/src/embedding.rs` are pre-built infrastructure that was never connected.
The `NodeEmbedding` struct exists, but `pdg::Node.embedding` is `None` for all nodes
produced by the parser. The `index_nodes` function (leindex.rs:1155) correctly falls back:

```rust
let embedding = node.embedding.clone().unwrap_or_else(|| {
    self.generate_deterministic_embedding(&node.name, &node.file_path, &node_content)
});
```

Since `node.embedding` is always `None`, the placeholder hash function is always used.

---

### 2.3 The Fix — Integration Points

The placeholder must be replaced with a real embedding function. Three viable approaches in
order of implementation effort:

#### Option A — ONNX Local Model (Recommended: no API dependency)

Load a compact code-oriented embedding model (e.g. `microsoft/codebert-base` or
`jinaai/jina-embeddings-v2-small-en`) via the `ort` crate (ONNX Runtime for Rust):

```toml
# lepasserelle/Cargo.toml additions:
[dependencies]
ort = "2"           # ONNX Runtime bindings
tokenizers = "0.19" # HuggingFace tokenizers
```

**Integration point**: Replace the `DefaultHasher` loop body at `leindex.rs:1083-1106`
with an ONNX inference call that encodes `content` (not just `symbol_name`). The model
produces a real 768-dim vector from source code text.

At query time, encode the query string through the same model via
`generate_query_embedding()` at `leindex.rs:1065`.

#### Option B — TF-IDF Bag-of-Words Vector (No external deps, pure Rust)

Build an IDF table across all node content during `index_nodes()`. At query time, compute a
TF-IDF vector and compare with cosine similarity.

**Integration point**: `leindex.rs:index_nodes()` (line 1110) — build IDF table during
indexing, then compute TF-IDF vectors per node as the embedding. `generate_query_embedding`
at line 1065 computes a TF-IDF vector for the query string against the same IDF table.

Result: Not as good as a trained model but far better than a hash. `semantic` score becomes
non-zero for topically related terms. No external dependencies.

#### Option C — API-Based Embedding (Voyage, OpenAI, etc.)

Add an optional `[embedding]` config section (already has infrastructure in `config.rs`):
```toml
[embedding]
provider = "voyage"    # or "openai"
api_key  = "pa-..."
model    = "voyage-code-2"
```

At index time, call the embedding API for each node's content. Cache embeddings keyed by
content hash to avoid re-embedding unchanged nodes on incremental re-index.

**Note**: An API provider is NOT required — this is a quality choice, not an architecture
necessity. Option A (ONNX local) provides the same semantic quality without API latency or
cost and should be the default.

---

### 2.4 Verification After Fix

After implementing real embeddings and re-indexing:
```bash
leindex index /path/to/project --force
leindex search "authentication credential refresh" -p /path/to/project
```

Expected: `semantic:` fields should now be non-zero for semantically related results:
```
[1] credentials.rs:with_refresh (score: 0.812)
    semantic:   0.743   <- now non-zero
    structural: 0.089
    text_match: 0.612
```

For exploratory queries with no exact symbol-name matches (e.g. "how does the fallback
system decide to retry"), results should now include semantically relevant nodes even with
zero `text_match`.

---

## 3. Immediate CLI Workaround (Active Now)

While both bugs are being fixed, the leindex CLI directly via Bash is the functional
fallback:

```bash
# Search
leindex search "query" -p /mnt/WD-SSD/Prod/Radis_Rust

# 5-phase analysis
leindex phase --all -p /mnt/WD-SSD/Prod/Radis_Rust

# Deep analysis
leindex analyze "query" -p /mnt/WD-SSD/Prod/Radis_Rust

# Diagnostics
leindex diagnostics -p /mnt/WD-SSD/Prod/Radis_Rust
```

---

## 4. Fix Priority & Effort Summary

| Bug | Severity | Fix Location | Effort |
|-----|----------|-------------|--------|
| Bug 1: Double-newline (MCP injection) | **CRITICAL** — tools completely unavailable via MCP | `cli.rs:676,724` — remove `\n` from `"{}\n"` format strings | **~5 minutes** — 2-char change × 2 lines |
| Bug 2: Hash-based embedding (semantic=0.0) | **HIGH** — exploratory queries degrade to pure text matching | `leindex.rs:1077-1107` — replace `DefaultHasher` loop with real embedding | **High** — requires ONNX or API integration |

**Bug 1 should be fixed first** — it takes 5 minutes and immediately restores MCP tool
injection. Bug 2 is a deeper architectural feature gap requiring model selection,
integration work, and benchmarking.

---

## 5. File Reference Map

| File | Lines | Relevance |
|------|-------|-----------|
| `crates/lepasserelle/src/cli.rs` | 676, 724 | **Bug 1 fix locations** — `writeln!(stdout, "{}\n", ...)` double-newline |
| `crates/lepasserelle/src/cli.rs` | 554-735 | `cmd_mcp_stdio_impl()` — full stdio MCP server loop |
| `crates/lepasserelle/src/cli.rs` | 833-889 | `handle_mcp_request()` — JSON-RPC method dispatch |
| `crates/lepasserelle/src/cli.rs` | 856-868 | `initialize` handler — `"capabilities": {"tools": {}}` only, no `hasResourceSubscribe` |
| `crates/lepasserelle/src/leindex.rs` | 1065-1070 | `generate_query_embedding()` — delegates to placeholder |
| `crates/lepasserelle/src/leindex.rs` | 1077-1107 | **Bug 2 fix location** — `generate_deterministic_embedding()` DefaultHasher placeholder |
| `crates/lepasserelle/src/leindex.rs` | 1110-1182 | `index_nodes()` — builds vector index (calls Bug 2 placeholder) |
| `crates/lepasserelle/src/leindex.rs` | 547-571 | `search()` — constructs `SearchQuery` with `query_embedding: Some(...)` |
| `crates/lerecherche/src/search.rs` | 483-634 | `SearchEngine::search()` — hybrid search, semantic score lookup |
| `crates/lerecherche/src/search.rs` | 387-439 | `SearchEngine::index_nodes()` — stores node embeddings in vector index |
| `crates/lerecherche/src/vector.rs` | 126-146 | `VectorIndex::search()` — cosine similarity (correct, not the bug) |
| `crates/lerecherche/src/ranking.rs` | 54-82 | `HybridScorer::score()` — semantic×0.5 + structural×0.1 + text×0.4 |
| `crates/legraphe/src/embedding.rs` | — | `NodeEmbedding` struct — 768-dim "CodeRankEmbed" label, never populated |
