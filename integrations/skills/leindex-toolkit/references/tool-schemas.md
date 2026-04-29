# LeIndex Tool Schemas

Generated from the live CLI surface with `leindex tools schema <tool>`.

## leindex_deep_analyze

Deep analysis: semantic search + PDG traversal for definition, callers, callees, data flow, and impact radius. Use for broad codebase understanding queries.

```json
{
  "properties": {
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
      "type": "string"
    },
    "query": {
      "description": "Analysis query (e.g., 'How does authentication work?', 'Where is user data stored?')",
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
    "query"
  ],
  "type": "object"
}

```

## leindex_diagnostics

Get diagnostic information about the indexed project, including memory usage, index statistics, and system health.

```json
{
  "properties": {
    "project_path": {
      "description": "Project directory (omit to use current project)",
      "type": "string"
    }
  },
  "required": [],
  "type": "object"
}

```

## leindex_index

Index a project. Auto-indexes on first use; returns cached stats on repeat calls. Use force_reindex=true only to rebuild after external file changes. All other tools also accept project_path and auto-index, so explicit indexing is optional.

```json
{
  "properties": {
    "force_reindex": {
      "default": false,
      "description": "If true, re-index even if already indexed (default: false). Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
      "type": "boolean"
    },
    "project_path": {
      "description": "Absolute path to the project directory to index",
      "type": "string"
    }
  },
  "required": [
    "project_path"
  ],
  "type": "object"
}

```

## leindex_context

Expand context around a code node via PDG: callers, callees, data dependencies, and sibling nodes. Supersedes Read for understanding how a function fits into its module without reading the entire file. Accepts project_path to auto-switch between projects.

```json
{
  "properties": {
    "node_id": {
      "description": "Node ID to expand context around (short name like 'my_func' or full ID like 'file.py:Class.method')",
      "type": "string"
    },
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
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
    "node_id"
  ],
  "type": "object"
}

```

## leindex_search

Semantic code search. Finds symbols by meaning, not just name. Returns ranked results with composite scores (semantic + text + structural). Accepts project_path to auto-switch/auto-index projects.

```json
{
  "properties": {
    "offset": {
      "default": 0,
      "description": "Skip the first N results for pagination (default: 0)",
      "minimum": 0,
      "type": "integer"
    },
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
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
      "description": "Scoring mode: 'code' (default) emphasizes semantic/structural similarity, 'prose' boosts text-match weight for natural-language queries (e.g. roadmap, README content), 'auto' detects based on query shape.",
      "enum": [
        "code",
        "prose",
        "auto"
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
    "query"
  ],
  "type": "object"
}

```

## leindex_phase_analysis

Run additive 5-phase analysis with freshness-aware incremental execution. Defaults to all 5 phases when `phase` is omitted.

```json
{
  "properties": {
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
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
      "type": "string"
    },
    "top_n": {
      "default": 10,
      "type": "integer"
    }
  },
  "required": [],
  "type": "object"
}

```

## phase_analysis

Alias for leindex_phase_analysis

```json
{
  "properties": {
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
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
      "type": "string"
    },
    "top_n": {
      "default": 10,
      "type": "integer"
    }
  },
  "required": [],
  "type": "object"
}

```

## leindex_file_summary

File overview: symbol inventory, complexity scores, cross-file dependencies, and module role. Use for understanding structure without reading raw content. For exact file contents use leindex_read_file; for a specific implementation use leindex_read_symbol.

```json
{
  "properties": {
    "file_path": {
      "description": "Absolute path to the file to analyze",
      "type": "string"
    },
    "focus_symbol": {
      "description": "Focus analysis on a specific symbol name (optional)",
      "type": "string"
    },
    "include_source": {
      "default": false,
      "description": "Include source snippets for key symbols (default: false). Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
      "type": "boolean"
    },
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
      "type": "string"
    },
    "token_budget": {
      "default": 1000,
      "description": "Max tokens for response (default: 1000)",
      "type": "integer"
    }
  },
  "required": [
    "file_path"
  ],
  "type": "object"
}

```

## leindex_symbol_lookup

Symbol relationship lookup: callers, callees, data dependencies, and impact radius. Use for understanding how a symbol connects to the rest of the codebase. For the exact source implementation use leindex_read_symbol.

```json
{
  "properties": {
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
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
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
    "token_budget": {
      "default": 1500,
      "description": "Max tokens for response (default: 1500)",
      "type": "integer"
    }
  },
  "required": [],
  "type": "object"
}

```

## leindex_project_map

Project structure map — use instead of Glob/ls for directory listing. Shows files with symbol counts, complexity hotspots, and inter-module dependency arrows. Supports scoping to subdirectories, sorting, and pagination.

```json
{
  "properties": {
    "depth": {
      "default": 3,
      "description": "Tree depth (default: 3, max: 10)",
      "maximum": 10,
      "minimum": 1,
      "type": "integer"
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
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
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
    "token_budget": {
      "default": 2000,
      "description": "Max tokens for response (default: 2000)",
      "type": "integer"
    }
  },
  "required": [],
  "type": "object"
}

```

## leindex_grep_symbols

Search for symbols across the codebase with structural awareness. Supports substring and regex patterns. Results include symbol type (function/class) and its role in the dependency graph.

```json
{
  "properties": {
    "include_context_lines": {
      "default": 0,
      "description": "Source context lines around each match (default: 0, max: 10)",
      "maximum": 10,
      "minimum": 0,
      "type": "integer"
    },
    "max_results": {
      "default": 20,
      "description": "Maximum results (default: 20, max: 200)",
      "maximum": 200,
      "minimum": 1,
      "type": "integer"
    },
    "offset": {
      "default": 0,
      "description": "Skip the first N results for pagination (default: 0)",
      "minimum": 0,
      "type": "integer"
    },
    "pattern": {
      "description": "Symbol name or substring to search for",
      "type": "string"
    },
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
      "type": "string"
    },
    "scope": {
      "description": "Limit results to a file or directory path (optional)",
      "type": "string"
    },
    "token_budget": {
      "default": 1500,
      "description": "Max tokens for response (default: 1500)",
      "type": "integer"
    },
    "type_filter": {
      "default": "all",
      "description": "Filter by symbol type (default: all)",
      "enum": [
        "function",
        "class",
        "method",
        "variable",
        "module",
        "all"
      ],
      "type": "string"
    }
  },
  "required": [
    "pattern"
  ],
  "type": "object"
}

```

## leindex_read_symbol

PRIMARY symbol reader — returns exact source code with line numbers, doc comments, and compact caller/callee locations (file:line). Use instead of Read for specific functions, methods, classes, or types. Set include_dependencies=true for full signatures.

```json
{
  "properties": {
    "file_path": {
      "description": "Disambiguate when symbol exists in multiple files (optional)",
      "type": "string"
    },
    "include_dependencies": {
      "default": false,
      "description": "Include dependency signatures (default: false). Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
      "type": "boolean"
    },
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
      "type": "string"
    },
    "symbol": {
      "description": "Symbol name to read source for",
      "type": "string"
    },
    "token_budget": {
      "default": 8000,
      "description": "Max tokens for response (default: 8000)",
      "type": "integer"
    }
  },
  "required": [
    "symbol"
  ],
  "type": "object"
}

```

## leindex_edit_preview

Preview a code edit: unified diff, affected symbols/files, breaking changes, and risk level — all before touching the filesystem. No equivalent in standard tools. Run before leindex_edit_apply to understand the blast radius of your change.

```json
{
  "properties": {
    "changes": {
      "description": "Advanced mode: list of changes to preview. Each has 'type' (replace_text/rename_symbol) and type-specific fields.",
      "items": {
        "type": "object"
      },
      "type": "array"
    },
    "file_path": {
      "description": "Absolute path to the file to edit",
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
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
      "type": "string"
    }
  },
  "required": [
    "file_path"
  ],
  "type": "object"
}

```

## leindex_edit_apply

PRIMARY file editor — use instead of edit_file. Simple mode: provide file_path + old_text + new_text for exact replacement. Advanced mode: use changes[] array for multiple or byte-offset edits. Supports dry_run=true for preview.

```json
{
  "properties": {
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
      "description": "Absolute path to the file to edit",
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
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
      "type": "string"
    }
  },
  "required": [
    "file_path"
  ],
  "type": "object"
}

```

## leindex_rename_symbol

Rename a symbol across all files using PDG to find all reference sites. Generates a unified multi-file diff (preview_only=true by default for safety). Replaces manual Grep + multi-file Edit with a single atomic operation.

```json
{
  "properties": {
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
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
      "type": "string"
    },
    "scope": {
      "description": "Limit rename to a file or directory path (optional)",
      "type": "string"
    }
  },
  "required": [
    "old_name",
    "new_name"
  ],
  "type": "object"
}

```

## leindex_impact_analysis

Analyze the transitive impact of changing a symbol: shows all symbols and files affected at each dependency depth level, with a risk assessment. Use before refactoring to understand the blast radius of your change. No equivalent in standard tools.

```json
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
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
      "type": "string"
    },
    "symbol": {
      "description": "Symbol to analyze impact for",
      "type": "string"
    }
  },
  "required": [
    "symbol"
  ],
  "type": "object"
}

```

## leindex_text_search

PRIMARY text search — use instead of Grep/rg. Returns exact matching lines with file:line and the owning symbol name+type for each match. One call replaces Grep + Read to understand match context. Supports regex, globs, scope, and context_lines.

```json
{
  "properties": {
    "case_sensitive": {
      "default": false,
      "description": "Case-sensitive search (default: false)",
      "type": "boolean"
    },
    "context_lines": {
      "default": 2,
      "description": "Lines of context above/below each match (default: 2)",
      "maximum": 10,
      "minimum": 0,
      "type": "integer"
    },
    "exclude_globs": {
      "description": "Exclude files matching these globs, e.g. [\"*_test.rs\"]",
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "include_globs": {
      "description": "Only search files matching these globs, e.g. [\"*.rs\", \"*.ts\"]",
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "is_regex": {
      "default": false,
      "description": "Treat query as regex (default: false = literal match)",
      "type": "boolean"
    },
    "max_results": {
      "default": 100,
      "description": "Maximum results to return (default: 100)",
      "maximum": 1000,
      "minimum": 1,
      "type": "integer"
    },
    "offset": {
      "default": 0,
      "description": "Skip the first N results for pagination (default: 0)",
      "minimum": 0,
      "type": "integer"
    },
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
      "type": "string"
    },
    "query": {
      "description": "Text pattern to search for (literal or regex)",
      "type": "string"
    },
    "scope": {
      "description": "Restrict search to a directory path",
      "type": "string"
    }
  },
  "required": [
    "query"
  ],
  "type": "object"
}

```

## leindex_read_file

PRIMARY file reader — returns exact file contents with line numbers PLUS context showing symbols, imports, and dependents. One call replaces Read + Grep for imports. Works for any text file including configs and docs.

```json
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
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
      "type": "string"
    },
    "start_line": {
      "default": 1,
      "description": "Start line, 1-indexed (default: 1)",
      "minimum": 1,
      "type": "integer"
    }
  },
  "required": [
    "file_path"
  ],
  "type": "object"
}

```

## leindex_git_status

Show git working tree status enriched with PDG structural analysis. Maps changed files to affected symbols, their callers, and transitive forward impact. Turns a raw diff into a structural change summary with blast radius.

```json
{
  "properties": {
    "diff_context_lines": {
      "default": 3,
      "description": "Context lines for diff output (default: 3)",
      "maximum": 20,
      "minimum": 0,
      "type": "integer"
    },
    "include_diff": {
      "default": false,
      "description": "Include unified diff content for modified files (default: false)",
      "type": "boolean"
    },
    "project_path": {
      "description": "Project directory (auto-indexes on first use; omit to use current project)",
      "type": "string"
    }
  },
  "required": [],
  "type": "object"
}

```

