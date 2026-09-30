# LeIndex Tool Schemas

Generated from the live CLI surface with `leindex tools schema <tool>`. Each tool is a router: the `mode` / `action` property selects one of the `oneOf` branches.

## leindex_explore

Find and read code. mode: search (by meaning, default), find (exact text/regex/symbol names, anywhere on disk), symbol_lookup (callers/callees), read_file, read_symbol, project_map, file_summary, context. See leindex://tools/guide for branch args.

```json
{
  "discriminator": {
    "propertyName": "mode"
  },
  "oneOf": [
    {
      "properties": {
        "allow_partial": {
          "default": true,
          "description": "Return core TF-IDF results when optional enrichment exceeds the budget",
          "type": "boolean"
        },
        "max_latency_ms": {
          "default": 500,
          "description": "Optional enrichment budget; never cancels the search (default: 500)",
          "maximum": 60000,
          "minimum": 0,
          "type": "integer"
        },
        "mode": {
          "const": "search",
          "description": "Ranked semantic + structural search"
        },
        "offset": {
          "default": 0,
          "description": "Skip the first N results for pagination (default: 0)",
          "minimum": 0,
          "type": "integer"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "query": {
          "description": "Search query (e.g., 'authentication', 'database connection')",
          "type": "string"
        },
        "scope": {
          "description": "Optional path to limit results (absolute or relative to project root)",
          "type": "string"
        },
        "search_mode": {
          "default": "code",
          "description": "Scoring mode: 'code' (default) emphasizes semantic/structural similarity, 'prose' boosts text-match weight for natural-language queries (e.g. roadmap, README content), 'auto' detects based on query shape, 'exact' prioritizes exact symbol name matches (higher text/structural weights), 'semantic' prioritizes conceptual relevance (higher TF-IDF semantic weights).",
          "enum": [
            "code",
            "prose",
            "auto",
            "exact",
            "semantic"
          ],
          "type": "string"
        },
        "task_context": {
          "description": "Optional bounded review/task context used for this retrieval only",
          "maxLength": 2000,
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        },
        "top_k": {
          "default": 10,
          "description": "Maximum number of results to return (default: 10)",
          "maximum": 100,
          "minimum": 1,
          "type": "integer"
        }
      },
      "required": [
        "mode",
        "query"
      ],
      "title": "search",
      "type": "object"
    },
    {
      "properties": {
        "allow_partial": {
          "default": true,
          "type": "boolean"
        },
        "depth": {
          "default": 2,
          "description": "Call graph traversal depth (default: 2, max: 5)",
          "maximum": 5,
          "minimum": 1,
          "type": "integer"
        },
        "include_callees": {
          "default": true,
          "description": "Include callees (default: true). Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
          "type": "boolean"
        },
        "include_callers": {
          "default": true,
          "description": "Include callers (default: true). Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
          "type": "boolean"
        },
        "include_source": {
          "default": false,
          "description": "Include source code of definition (default: false). Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
          "type": "boolean"
        },
        "max_latency_ms": {
          "default": 250,
          "description": "Optional caller/callee enrichment budget (default: 250)",
          "maximum": 60000,
          "minimum": 0,
          "type": "integer"
        },
        "mode": {
          "const": "symbol_lookup",
          "description": "Definition with callers, callees and dependencies"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "scope": {
          "description": "Optional path to limit lookup (absolute or relative to project root)",
          "type": "string"
        },
        "symbol": {
          "description": "Symbol name to look up (single symbol)",
          "type": "string"
        },
        "symbols": {
          "description": "Batch mode: look up multiple symbols in one call (max 20)",
          "items": {
            "type": "string"
          },
          "type": "array"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        },
        "token_budget": {
          "default": 1500,
          "description": "Max tokens for response (default: 1500)",
          "type": "integer"
        }
      },
      "required": [
        "mode"
      ],
      "title": "symbol_lookup",
      "type": "object"
    },
    {
      "properties": {
        "case": {
          "default": "smart",
          "description": "smart: ignore case unless pattern has uppercase",
          "enum": [
            "smart",
            "sensitive",
            "insensitive"
          ],
          "type": "string"
        },
        "context_lines": {
          "default": 0,
          "description": "Context lines per match",
          "maximum": 10,
          "minimum": 0,
          "type": "integer"
        },
        "exclude_globs": {
          "description": "Skip these, e.g. [\"vendor/\"]",
          "items": {
            "type": "string"
          },
          "type": "array"
        },
        "include_globs": {
          "description": "Only these, e.g. [\"*.rs\"]",
          "items": {
            "type": "string"
          },
          "type": "array"
        },
        "kind": {
          "description": "target=symbols: function, class, struct, ...",
          "type": "string"
        },
        "limit": {
          "default": 50,
          "description": "Hits per page; 0 = all",
          "minimum": 0,
          "type": "integer"
        },
        "max_line_chars": {
          "default": 200,
          "description": "Longest line shown",
          "maximum": 2000,
          "minimum": 20,
          "type": "integer"
        },
        "mode": {
          "const": "find",
          "description": "Exact text/regex or symbol-name search: indexed for speed, live for correctness, any path on disk"
        },
        "offset": {
          "default": 0,
          "description": "Hits to skip (use next_offset)",
          "minimum": 0,
          "type": "integer"
        },
        "output": {
          "default": "matches",
          "description": "matches (default), files, count, or symbols (enclosing symbols by hits)",
          "enum": [
            "matches",
            "files",
            "count",
            "symbols"
          ],
          "type": "string"
        },
        "paths": {
          "description": "Extra files/dirs anywhere on disk (no index needed)",
          "items": {
            "type": "string"
          },
          "type": "array"
        },
        "pattern": {
          "description": "Text to find (regex when regex=true)",
          "type": "string"
        },
        "per_file_cap": {
          "default": 20,
          "description": "Shown per file (all counted); 0 = no cap",
          "minimum": 0,
          "type": "integer"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "regex": {
          "default": false,
          "description": "Treat pattern as a regular expression",
          "type": "boolean"
        },
        "scope": {
          "description": "Restrict to a project-relative directory or file",
          "type": "string"
        },
        "target": {
          "default": "text",
          "description": "text: matching lines. symbols: definitions by name. auto: symbols, else text",
          "enum": [
            "text",
            "symbols",
            "auto"
          ],
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        },
        "timeout_ms": {
          "default": 20000,
          "description": "Time budget; partial results + has_more. 0 = none",
          "minimum": 0,
          "type": "integer"
        },
        "word": {
          "default": false,
          "description": "Whole-word matches only",
          "type": "boolean"
        }
      },
      "required": [
        "mode",
        "pattern"
      ],
      "title": "find",
      "type": "object"
    },
    {
      "properties": {
        "end_line": {
          "description": "End line, 1-indexed inclusive (default: end of file)",
          "minimum": 1,
          "type": "integer"
        },
        "file_path": {
          "description": "Absolute path to file to read",
          "type": "string"
        },
        "include_symbol_map": {
          "default": false,
          "description": "Include PDG symbol annotations (default: false). Set true when structural context is useful.",
          "type": "boolean"
        },
        "max_lines": {
          "default": 500,
          "description": "Maximum lines to return (default: 500, safety cap)",
          "maximum": 2000,
          "minimum": 1,
          "type": "integer"
        },
        "mode": {
          "const": "read_file",
          "description": "Read a file (line ranges) with a PDG symbol map"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "start_line": {
          "default": 1,
          "description": "Start line, 1-indexed (default: 1)",
          "minimum": 1,
          "type": "integer"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        }
      },
      "required": [
        "mode",
        "file_path"
      ],
      "title": "read_file",
      "type": "object"
    },
    {
      "properties": {
        "allow_partial": {
          "default": true,
          "type": "boolean"
        },
        "file_path": {
          "description": "Optional file disambiguator",
          "type": "string"
        },
        "include_dependencies": {
          "default": false,
          "type": "boolean"
        },
        "max_latency_ms": {
          "default": 250,
          "maximum": 60000,
          "minimum": 0,
          "type": "integer"
        },
        "mode": {
          "const": "read_symbol",
          "description": "Read one symbol's source with dependencies"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "symbol": {
          "description": "Symbol name to read source for",
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        },
        "token_budget": {
          "default": 8000,
          "type": "integer"
        }
      },
      "required": [
        "mode",
        "symbol"
      ],
      "title": "read_symbol",
      "type": "object"
    },
    {
      "properties": {
        "depth": {
          "default": 3,
          "description": "Tree depth (default: 3, max: 10)",
          "maximum": 10,
          "minimum": 1,
          "type": "integer"
        },
        "focus": {
          "description": "Semantic focus area \u2014 ranks files by relevance to this topic (e.g., 'authentication', 'database layer', 'payment flow')",
          "type": "string"
        },
        "group_by": {
          "default": "tree",
          "description": "Grouping mode: tree (default, flat file list) or community (Leiden communities \u2014 module boundaries the codebase itself may not have named)",
          "enum": [
            "tree",
            "community"
          ],
          "type": "string"
        },
        "include_symbols": {
          "default": false,
          "description": "Include top symbols per file (default: false). Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
          "type": "boolean"
        },
        "limit": {
          "description": "Maximum number of files to return (default: unlimited, subject to token_budget)",
          "minimum": 1,
          "type": "integer"
        },
        "mode": {
          "const": "project_map",
          "description": "Annotated project tree with hotspots and module dependencies"
        },
        "offset": {
          "default": 0,
          "description": "Skip the first N files for pagination (default: 0)",
          "minimum": 0,
          "type": "integer"
        },
        "path": {
          "description": "Subdirectory to scope to (default: project root)",
          "type": "string"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "sort_by": {
          "default": "complexity",
          "description": "Sort order (default: complexity)",
          "enum": [
            "complexity",
            "name",
            "dependencies",
            "size"
          ],
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        },
        "token_budget": {
          "default": 2000,
          "description": "Max tokens for response (default: 2000)",
          "type": "integer"
        }
      },
      "required": [
        "mode"
      ],
      "title": "project_map",
      "type": "object"
    },
    {
      "properties": {
        "allow_partial": {
          "default": true,
          "type": "boolean"
        },
        "file_path": {
          "description": "File to analyze",
          "type": "string"
        },
        "focus_symbol": {
          "type": "string"
        },
        "include_source": {
          "default": false,
          "type": "boolean"
        },
        "max_latency_ms": {
          "default": 250,
          "maximum": 60000,
          "minimum": 0,
          "type": "integer"
        },
        "mode": {
          "const": "file_summary",
          "description": "Structured file overview"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        },
        "token_budget": {
          "default": 1000,
          "type": "integer"
        }
      },
      "required": [
        "mode",
        "file_path"
      ],
      "title": "file_summary",
      "type": "object"
    },
    {
      "properties": {
        "allow_partial": {
          "default": true,
          "description": "Allow bounded PDG context when the enrichment budget is reached",
          "type": "boolean"
        },
        "max_latency_ms": {
          "default": 1500,
          "description": "Optional enrichment budget; elapsed work returns partial PDG context (default: 1500)",
          "maximum": 60000,
          "minimum": 0,
          "type": "integer"
        },
        "mode": {
          "const": "context",
          "description": "Expand PDG context around a node or symbol"
        },
        "node_id": {
          "description": "Node ID to expand context around (short name like 'my_func' or full ID like 'file.py:Class.method')",
          "type": "string"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        },
        "token_budget": {
          "default": 2000,
          "description": "Maximum tokens for context (default: 2000)",
          "maximum": 100000,
          "minimum": 100,
          "type": "integer"
        }
      },
      "required": [
        "mode",
        "node_id"
      ],
      "title": "context",
      "type": "object"
    }
  ],
  "properties": {
    "mode": {
      "description": "Select the granular handler branch; branch arguments are forwarded unchanged.",
      "enum": [
        "search",
        "symbol_lookup",
        "find",
        "read_file",
        "read_symbol",
        "project_map",
        "file_summary",
        "context"
      ],
      "type": "string"
    },
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit for current)",
      "type": "string"
    },
    "tier": {
      "description": "Detail: l0 card, l1 overview (default), l2 full",
      "enum": [
        "l0",
        "l1",
        "l2"
      ],
      "type": "string"
    }
  },
  "required": [],
  "type": "object"
}
```

## leindex_analyze

Program-dependence-graph analysis and repo state. mode: deep (semantic search + graph expansion, default), impact (blast radius), diagnostics, git_status, git_diff. See leindex://tools/guide for branch args.

```json
{
  "discriminator": {
    "propertyName": "mode"
  },
  "oneOf": [
    {
      "properties": {
        "allow_partial": {
          "default": true,
          "description": "Return partial PDG context when the budget is reached",
          "type": "boolean"
        },
        "max_latency_ms": {
          "default": 1500,
          "description": "PDG context-expansion budget; configured neural startup/inference is not cancelled (default: 1500)",
          "maximum": 60000,
          "minimum": 0,
          "type": "integer"
        },
        "mode": {
          "const": "deep",
          "description": "Semantic retrieval expanded through the PDG"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "query": {
          "description": "Analysis query (e.g., 'How does authentication work?', 'Where is user data stored?')",
          "type": "string"
        },
        "task_context": {
          "description": "Optional bounded review/task context used only for this query",
          "maxLength": 2000,
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        },
        "token_budget": {
          "default": 2000,
          "description": "Maximum tokens for context expansion (default: 2000)",
          "maximum": 100000,
          "minimum": 100,
          "type": "integer"
        }
      },
      "required": [
        "mode",
        "query"
      ],
      "title": "deep",
      "type": "object"
    },
    {
      "properties": {
        "change_type": {
          "default": "modify",
          "description": "Type of change to analyze (default: modify)",
          "enum": [
            "modify",
            "remove",
            "rename",
            "change_signature"
          ],
          "type": "string"
        },
        "depth": {
          "default": 3,
          "description": "Traversal depth (default: 3, max: 5)",
          "maximum": 5,
          "minimum": 1,
          "type": "integer"
        },
        "mode": {
          "const": "impact",
          "description": "Transitive impact of changing a symbol"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "symbol": {
          "description": "Symbol to analyze impact for",
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        }
      },
      "required": [
        "mode",
        "symbol"
      ],
      "title": "impact",
      "type": "object"
    },
    {
      "properties": {
        "mode": {
          "const": "diagnostics",
          "description": "Index health, sizes, cache and memory statistics"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        }
      },
      "required": [
        "mode"
      ],
      "title": "diagnostics",
      "type": "object"
    },
    {
      "properties": {
        "allow_partial": {
          "default": true,
          "type": "boolean"
        },
        "enrich_pdg": {
          "default": true,
          "description": "Accepted for compatibility; resident PDG enrichment remains enabled.",
          "type": "boolean"
        },
        "max_latency_ms": {
          "default": 150,
          "type": "integer"
        },
        "mode": {
          "const": "git_status",
          "description": "Live git status enriched with changed symbols"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "scope": {
          "description": "Optional project-relative scope for advisory stage candidates",
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        }
      },
      "required": [
        "mode"
      ],
      "title": "git_status",
      "type": "object"
    },
    {
      "properties": {
        "allow_partial": {
          "default": true,
          "type": "boolean"
        },
        "enrich_pdg": {
          "default": true,
          "description": "Map changed hunks to PDG symbols and compute impact",
          "type": "boolean"
        },
        "include_patch": {
          "default": true,
          "description": "Include the (bounded) unified patch",
          "type": "boolean"
        },
        "max_latency_ms": {
          "default": 250,
          "description": "PDG enrichment budget",
          "maximum": 60000,
          "minimum": 0,
          "type": "integer"
        },
        "max_patch_chars": {
          "default": 20000,
          "description": "Patch size bound",
          "maximum": 200000,
          "minimum": 0,
          "type": "integer"
        },
        "mode": {
          "const": "git_diff",
          "description": "PDG-enriched diff (working tree, staged, ref or range)"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "range": {
          "description": "Revision range, e.g. main..feature or HEAD~3..HEAD",
          "type": "string"
        },
        "ref": {
          "description": "Diff one commit against its parent (e.g. HEAD, HEAD~1, a1b2c3d)",
          "type": "string"
        },
        "scope": {
          "description": "Only report files under this project-relative path",
          "type": "string"
        },
        "staged": {
          "default": false,
          "description": "Diff the index against HEAD instead of the working tree",
          "type": "boolean"
        },
        "stat_only": {
          "default": false,
          "description": "Per-file statistics only; no patch, no hunk-level symbol mapping",
          "type": "boolean"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        }
      },
      "required": [
        "mode"
      ],
      "title": "git_diff",
      "type": "object"
    }
  ],
  "properties": {
    "mode": {
      "description": "Select the granular handler branch; branch arguments are forwarded unchanged.",
      "enum": [
        "deep",
        "impact",
        "diagnostics",
        "git_status",
        "git_diff"
      ],
      "type": "string"
    },
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit for current)",
      "type": "string"
    },
    "tier": {
      "description": "Detail: l0 card, l1 overview (default), l2 full",
      "enum": [
        "l0",
        "l1",
        "l2"
      ],
      "type": "string"
    }
  },
  "required": [],
  "type": "object"
}
```

## leindex_edit

Context-aware editing. action (required, no default): apply (atomic edit), preview (dry-run diff + impact), rename (cross-file symbol rename; preview by default), write (atomic file write). See leindex://tools/guide for branch args.

```json
{
  "discriminator": {
    "propertyName": "action"
  },
  "oneOf": [
    {
      "properties": {
        "action": {
          "const": "apply",
          "description": "Apply an edit atomically with impact analysis"
        },
        "changes": {
          "description": "Advanced mode: list of changes to apply. Each has type (replace_text/rename_symbol) and type-specific fields.",
          "items": {
            "type": "object"
          },
          "type": "array"
        },
        "dry_run": {
          "default": false,
          "description": "If true, return preview without modifying files (default: false). Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
          "type": "boolean"
        },
        "file_path": {
          "description": "Absolute or project-relative path. Relative paths resolve against the project root.",
          "type": "string"
        },
        "new_str": {
          "description": "Alias for new_text (compatibility with edit_file)",
          "type": "string"
        },
        "new_text": {
          "description": "Simple mode: replacement text",
          "type": "string"
        },
        "old_str": {
          "description": "Alias for old_text (compatibility with edit_file)",
          "type": "string"
        },
        "old_text": {
          "description": "Simple mode: text to find and replace (exact match)",
          "type": "string"
        },
        "preview_token": {
          "description": "The token returned by a previous LeIndex [Edit Preview] (tool: leindex.edit-preview) call. Required if using cached preview.",
          "type": "string"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        }
      },
      "required": [
        "action",
        "file_path"
      ],
      "title": "apply",
      "type": "object"
    },
    {
      "properties": {
        "action": {
          "const": "preview",
          "description": "Dry-run an edit; returns diff, impact and a preview_token"
        },
        "changes": {
          "description": "Advanced mode: list of changes to preview. Each has 'type' (replace_text/rename_symbol) and type-specific fields.",
          "items": {
            "type": "object"
          },
          "type": "array"
        },
        "file_path": {
          "description": "Absolute or project-relative path to the file to edit. Relative paths resolve against the project root.",
          "type": "string"
        },
        "new_text": {
          "description": "Simple mode: replacement text",
          "type": "string"
        },
        "old_text": {
          "description": "Simple mode: text to find and replace (exact match)",
          "type": "string"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        }
      },
      "required": [
        "action",
        "file_path"
      ],
      "title": "preview",
      "type": "object"
    },
    {
      "properties": {
        "action": {
          "const": "rename",
          "description": "PDG-wide symbol rename (preview_only defaults to true)"
        },
        "new_name": {
          "description": "New symbol name",
          "type": "string"
        },
        "old_name": {
          "description": "Current symbol name",
          "type": "string"
        },
        "preview_only": {
          "default": true,
          "description": "If true, return diff without applying changes (default: true). Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
          "type": "boolean"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "scope": {
          "description": "Limit rename to a file or directory path (optional)",
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        }
      },
      "required": [
        "action",
        "old_name",
        "new_name"
      ],
      "title": "rename",
      "type": "object"
    },
    {
      "properties": {
        "action": {
          "const": "write",
          "description": "Atomic file create/overwrite"
        },
        "content": {
          "description": "Full content to write to the file",
          "type": "string"
        },
        "file_path": {
          "description": "Absolute or project-relative path. Relative paths resolve against the project root.",
          "type": "string"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        }
      },
      "required": [
        "action",
        "file_path",
        "content"
      ],
      "title": "write",
      "type": "object"
    }
  ],
  "properties": {
    "action": {
      "description": "Select the granular handler branch; branch arguments are forwarded unchanged.",
      "enum": [
        "apply",
        "preview",
        "rename",
        "write"
      ],
      "type": "string"
    },
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit for current)",
      "type": "string"
    },
    "tier": {
      "description": "Detail: l0 card, l1 overview (default), l2 full",
      "enum": [
        "l0",
        "l1",
        "l2"
      ],
      "type": "string"
    }
  },
  "required": [
    "action"
  ],
  "type": "object"
}
```

## leindex_manage

Index lifecycle and architecture reports. action: index (build/refresh; returns a pollable job, default), phase (5-phase architecture analysis). See leindex://tools/guide for branch args.

```json
{
  "discriminator": {
    "propertyName": "action"
  },
  "oneOf": [
    {
      "properties": {
        "action": {
          "const": "index",
          "description": "Build or refresh the index (pollable job)"
        },
        "force_reindex": {
          "default": false,
          "description": "If true, re-index even if already indexed (default: false). Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
          "type": "boolean"
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        },
        "wait": {
          "default": false,
          "description": "Wait for completion instead of returning a pollable job snapshot (default: false)",
          "type": "boolean"
        }
      },
      "required": [
        "action",
        "project_path"
      ],
      "title": "index",
      "type": "object"
    },
    {
      "properties": {
        "action": {
          "const": "phase",
          "description": "5-phase architecture analysis"
        },
        "docs_mode": {
          "default": "off",
          "description": "Controls which documentation files to include: 'off' (default, code only), 'markdown' (*.md files like README, CHANGELOG), 'text' (*.txt, *.rst), 'all' (all doc formats). Use 'markdown' or 'all' to analyze project documentation alongside code.",
          "enum": [
            "off",
            "markdown",
            "text",
            "all"
          ],
          "type": "string"
        },
        "include_docs": {
          "default": false,
          "description": "IMPORTANT: Enable to include prose/documentation files (README, docs/, *.md) in the analysis. Without this, only source code files are analyzed. Set to true when you need architectural docs, changelogs, or project documentation. Also accepts strings: 'true'/'false'.",
          "type": "boolean"
        },
        "max_chars": {
          "default": 12000,
          "type": "integer"
        },
        "max_files": {
          "default": 2000,
          "type": "integer"
        },
        "max_focus_files": {
          "default": 20,
          "type": "integer"
        },
        "mode": {
          "default": "balanced",
          "enum": [
            "ultra",
            "balanced",
            "verbose"
          ],
          "type": "string"
        },
        "path": {
          "description": "File or directory to analyze (defaults to project root)",
          "type": "string"
        },
        "phase": {
          "default": "all",
          "oneOf": [
            {
              "maximum": 5,
              "minimum": 1,
              "type": "integer"
            },
            {
              "enum": [
                "all",
                "1",
                "2",
                "3",
                "4",
                "5"
              ],
              "type": "string"
            }
          ]
        },
        "project_path": {
          "description": "Project directory (auto-indexes on first use; omit for current)",
          "type": "string"
        },
        "tier": {
          "description": "Detail: l0 card, l1 overview (default), l2 full",
          "enum": [
            "l0",
            "l1",
            "l2"
          ],
          "type": "string"
        },
        "top_n": {
          "default": 10,
          "type": "integer"
        }
      },
      "required": [
        "action"
      ],
      "title": "phase",
      "type": "object"
    }
  ],
  "properties": {
    "action": {
      "description": "Select the granular handler branch; branch arguments are forwarded unchanged.",
      "enum": [
        "index",
        "phase"
      ],
      "type": "string"
    },
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit for current)",
      "type": "string"
    },
    "tier": {
      "description": "Detail: l0 card, l1 overview (default), l2 full",
      "enum": [
        "l0",
        "l1",
        "l2"
      ],
      "type": "string"
    }
  },
  "required": [],
  "type": "object"
}
```
