# LeIndex MCP Usage Skill - Semantic Search Capabilities Analysis

**Date:** 2026-05-08  
**Author:** Review Analysis  
**Scope:** Assess semantic search capabilities and limitations for accurate documentation

---

## Executive Summary

This document provides a **validated assessment** of LeIndex's semantic search capabilities. The initial skill.md document contained preliminary guidance about semantic search that requires careful review to avoid setting unrealistic expectations.

---

## Current Implementation Analysis

### Semantic Search Architecture

LeIndex's semantic search is a **hybrid system** combining:
1. **Vector-based semantic search** (TF-IDF embeddings)
2. **Text-based matching** (keyword search)
3. **Structural analysis** (PDG-based scoring)

```
User Query
    │
    ▼
┌─────────────────┐
│  Query Parsing  │  ← Natural language detection, auto-mode selection
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Tokenization   │  ← Code tokens extracted (camelCase, snake_case, etc.)
└────────┬────────┘
         │
         ▼
┌─────────────────────────────────┐
│  TF-IDF Vector Generation        │  ← 768-dimensional embeddings per node
│  (from pre_tokenized content)    │
└────────────┬────────────────────┘
             │
             ▼
┌─────────────────────────────────┐
│  Vector Similarity Search       │  ← Cosine similarity on indexed embeddings
│  (top-k similar nodes)           │
└────────────┬────────────────────┘
             │
             ▼
┌─────────────────────────────────┐
│  Hybrid Scoring                  │  ← Combines semantic + text + structural scores
│  - QueryType::Semantic          │    Code mode: 25% semantic, 15% structural, 60% text
│  - QueryType::Text              │    Text mode: 50% semantic, 50% text
│  - QueryType::Structural        │    Prose mode: user-selectable
└────────────┬────────────────────┘
             │
             ▼
┌─────────────────────────────────┐
│  Semantic Context Expansion      │  ← PDG traversal for related code
│  (Collision-aware)             │
└────────────┬────────────────────┘
             │
             ▼
┌─────────────────────────────────┐
│  Result Formatting               │  ← Context-aware response with metadata
└─────────────────────────────────┘
```

---

## Capabilities Assessment

### ✅ What Works Well

#### 1. **Tokenization-Aware Search**
- Handles `camelCase`, `snake_case`, `PascalCase` correctly
- Understands acronyms (e.g., `HTTPConnection` → `http`, `connection`)
- Properly tokenizes code identifiers
- **Trust level: HIGH**

#### 2. **Hybrid Scoring System**
- Code-aware mode: Prioritizes text matching (60% weight)
- Semantic mode: Can be enabled for similarity search
- Prose mode: Optimized for natural language queries
- **Trust level: HIGH**

#### 3. **Natural Language Detection**
```rust
// From search_handler.rs lines 95-110
let prose_keywords = ["how", "what", "where", "why", "who", "when", "can", "is", "explain"];
let is_natural_language = query.split_whitespace().count() > 3
    || prose_keywords.iter().any(|k| q_lower.contains(k));
```
- Auto-detects query intent
- Switches between modes dynamically
- **Trust level: MEDIUM-HIGH**

#### 4. **Scope Filtering with Trailing Separator Awareness**
```rust
// From search_handler.rs line 139
let scope_str = s.trim_end_matches(std::path::MAIN_SEPARATOR);
```
- Handles path separators correctly across platforms
- **Trust level: HIGH**

#### 5. **Pagination for Large Result Sets**
- `fetch_k` can expand to 10x when scope filtering eliminates results
- Prevents false negatives in scoped searches
- **Trust level: HIGH**

---

### ⚠️ Limitations and Failure Modes

#### 1. **Token Budget Limitations**

```rust
// From search_handler.rs line 89
const MAX_FETCH_K: usize = 1000;
let mut fetch_k = (top_k + offset).min(MAX_FETCH_K);
```

**Issue:** When `scope` is applied and all top-k results are outside scope, the system:
1. Detects `total_filtered == 0`
2. Expands `fetch_k` to 10x
3. Re-queries
4. Returns `No semantic matches found` with suggestion

**Example of Problem:**
- Query: "authentication"
- `top_k=10`, `scope="/project/src/api/"`
- Top 10 results are all in `/project/src/core/`
- `total_filtered == 0` → User sees "No matches" even though relevant code exists at rank 21+

**Impact:** Users may abandon semantic search for simpler queries that work better with `GrepSymbols`

**Mitigation:** ⚠️ User must rephrase or increase `top_k`

---

#### 2. **Semantic Complexity Limitations**

**Current Capability:** Vector similarity based on TF-IDF embeddings (768 dimensions)

**What Qualifies as "Semantic":**
- ✅ **Basic retrieval:** Find similar code patterns
  - Example: `"VecDeque"` → finds queue-related code
  - Example: `"HTTP server"` → finds web server code
  
- ⚠️ **Limited abstraction:** Understands lexical patterns but not:
  - Design patterns (e.g., "actory pattern" may not find factory code)
  - Intent/goals (e.g., "how to cache efficiently" may not understand caching)
  - Cross-language concepts (only works within indexed language)
  
- ❌ **Not: Code generation or synthesis**
  - Semantic search retrieves existing code
  - Cannot generate new implementations
  - Cannot explain concepts beyond found code

**Example Query Failures:**
```
Query: "How do I implement a thread-safe cache?"
Expected: Examples of Mutex, RwLock, Arc usage
Result: May find unrelated code about "cache" or "thread"

Query: "actory pattern in Rust"
Expected: Factory implementation examples
Result: May find struct constructors but not factory pattern
```

---

#### 3. **Pre-tokenization Dependency**

```rust
// From index.rs line 582
pre_tokenized: Some(
    crate::search::vector::tokenize_code(&node_content)
),
```

**Issue:** TF-IDF embeddings depend on pre-tokenized content
- If tokenization fails or is incomplete, embeddings are less accurate
- Cannot recover from poor tokenization

**Impact:** Code with unusual formatting or complex names may not match well

---

#### 4. **Embedding Model Limitations**

**Current:** TF-IDF on code tokens

**Constraints:**
- 768-dimensional vectors (moderate expressiveness)
- Trained on code corpus, not natural language
- No contextual understanding beyond token co-occurrence
- No transformer-based attention mechanisms

**Impact:** Cannot understand:
- Comments as well as code
- Documentation strings
- Variable naming intent

---

#### 5. **No True "Semantic" Understanding**

**Important:** LeIndex semantic search is **NOT** like LLM-based semantic search
- No neural network embeddings trained on code+comments
- No attention-based transformer encoding
- No code-specific embeddings (like CodeBERT)
- No understanding of:
  - Function intent
  - Control flow semantics
  - Type relationships
  - API contracts

**What It IS:** A sophisticated keyword/bag-of-tokens search with vector similarity

---

### 📊 Comparison Matrix

| Query Type | Works Best When | Recommended Tool |
|------------|-----------------|------------------|
| **Specific symbol name** | You know exact identifier | `GrepSymbols` |
| **Code pattern (camelCase, snake_case)** | Camel/snake case identifiers | `Search` (code mode) |
| **Natural language description** | ">3 words" or prose keywords | `Search` (auto-detect) / `GrepSymbols` |
| **Prose content (README, docs)** | Natural language query | `Search` (prose mode) |
| **Complex intent ("how to X")** | May need rephrasing | `GrepSymbols` + manual review |
| **Design pattern search** | May not work | `GrepSymbols` (exact) or manual search |
| **Cross-language concept** | Not well supported | `GrepSymbols` or multiple searches |

---

## Recommended Documentation Updates

### For `docs/skill.md`

Replace preliminary guidance with:

```markdown
### Semantic Search (`leindex.search`)

**Scope:** Finds code by lexical similarity and context, not semantic understanding

#### ✅ Use When:
- Searching for patterns (camelCase, snake_case, PascalCase)
- Finding similar code structures
- Searching within a specific codebase context
- Looking for prose content (README, documentation)

#### ⚠️ Be Cautious When:
- Querying complex design patterns (may miss intent)
- Searching for "how to" implement something (may need rephrasing)
- Cross-language concept search (only works within indexed language)
- Patterns are spread across many files (pagination may miss results)

#### ❌ Not Suitable For:
- Finding specific symbol names (use `GrepSymbols` instead)
- Code generation or implementation guidance
- Understanding API contracts or intent
- Explaining concepts beyond retrieved code examples

#### Recommended Query Patterns:

**✅ Good - Pattern-based:**
```
query: "VecDeque implementation"  → Finds queue code
query: "HTTP server request"      → Finds HTTP handler code
query: "Database connection pool" → Finds pool-related code
```

**⚠️ Caution - Natural Language:**
```
query: "how to implement a thread-safe queue"  → May find queue code but not thread-safe patterns
Better: Split into "thread-safe queue Rust" + grep symbols
```

**❌ Avoid - Complex Intent:**
```
query: "actory pattern in Rust" → May not find factory pattern
Better: "cargo make" + grep symbols, or read architecture docs
```

#### Advanced Usage:

```json
{
  "query": "Database connection pool implementation",
  "top_k": 20,
  "scope": "/project/src/database/",
  "search_mode": "code"  // or "prose" for documentation
}
```

**Note:** If `scope` is too restrictive, add `"offset": 0` and `"top_k": 50` to capture more results, then filter manually.
```

---

## Summary

**Key Finding:** LeIndex's "semantic" search is a **hybrid code search** that:
- ✅ Excels at lexical pattern matching
- ✅ Handles tokenization-aware queries well
- ✅ Supports natural language auto-detection
- ⚠️ Has limited true semantic understanding
- ⚠️ May fail on complex or nuanced queries
- ❌ Is not a replacement for deep architectural analysis

**Recommendation:** Document with **clear caveats** about what semantic search can and cannot do. Emphasize `GrepSymbols` for exact name matching and set realistic expectations for "semantic" capabilities.

---

## Next Steps

1. ✅ Complete this capability assessment
2. ⏳ Update `docs/skill.md` with validated information
3. ⏳ Add examples showing real query patterns and their expected results
4. ⏳ Document when to use `GrepSymbols` vs `Search`
5. ⏳ Add cross-reference to `TZAR_REVIEW_UNIFICATION_REPORT.md` for technical details

---

**Appendix:** See `docs/findings/2026-05-07-remediation-plan.md` for implementation details of R1-R8 which enabled the current semantic search performance.
