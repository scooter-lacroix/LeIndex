import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { Type } from "@sinclair/typebox";

export default function (pi: ExtensionAPI) {
  // Register leindex_index
  pi.registerTool({
    name: "leindex_index",
    label: "LeIndex: Index Project",
    description: "Index a project for code search and analysis. Parses source files, builds the Program Dependence Graph, and creates the semantic search index.",
    parameters: Type.Object({
      project_path: Type.String({ description: "Absolute path to the project directory to index" }),
      force_reindex: Type.Optional(Type.Boolean({ description: "If true, re-index even if already indexed (default: false)" })),
    }),
    async execute(toolCallId, params, onUpdate, ctx, signal) {
      const args = ["index", params.project_path];
      if (params.force_reindex) {
        args.push("--force");
      }

      onUpdate?.({ content: [{ type: "text", text: "Indexing project..." }] });
      const result = await pi.exec("leindex", args, { signal });

      return {
        content: [{ type: "text", text: result.stdout || result.stderr }],
        isError: result.code !== 0,
        details: { exitCode: result.code },
      };
    },
  });

  // Register leindex_search
  pi.registerTool({
    name: "leindex_search",
    label: "LeIndex: Semantic Search",
    description: "Search indexed code using semantic search. Returns the most relevant code snippets matching your query.",
    parameters: Type.Object({
      query: Type.String({ description: "Search query (e.g., 'authentication', 'database connection')" }),
      top_k: Type.Optional(Type.Number({ description: "Maximum number of results to return (default: 10)", minimum: 1, maximum: 100 })),
    }),
    async execute(toolCallId, params, onUpdate, ctx, signal) {
      const args = ["search", params.query];
      if (params.top_k) {
        args.push("--top-k", params.top_k.toString());
      }

      const result = await pi.exec("leindex", args, { signal });

      return {
        content: [{ type: "text", text: result.stdout || result.stderr }],
        isError: result.code !== 0,
        details: { exitCode: result.code },
      };
    },
  });

  // Register leindex_analyze
  pi.registerTool({
    name: "leindex_analyze",
    label: "LeIndex: Deep Analysis",
    description: "Perform deep code analysis with context expansion. Uses semantic search combined with PDG traversal.",
    parameters: Type.Object({
      query: Type.String({ description: "Analysis query" }),
      token_budget: Type.Optional(Type.Number({ description: "Maximum tokens for context expansion (default: 2000)" })),
    }),
    async execute(toolCallId, params, onUpdate, ctx, signal) {
      const args = ["analyze", params.query];
      if (params.token_budget) {
        args.push("--tokens", params.token_budget.toString());
      }

      onUpdate?.({ content: [{ type: "text", text: "Performing deep analysis..." }] });
      const result = await pi.exec("leindex", args, { signal });

      return {
        content: [{ type: "text", text: result.stdout || result.stderr }],
        isError: result.code !== 0,
        details: { exitCode: result.code },
      };
    },
  });

  // Register leindex_diagnostics
  pi.registerTool({
    name: "leindex_diagnostics",
    label: "LeIndex: Diagnostics",
    description: "Get diagnostic information about the indexed project.",
    parameters: Type.Object({}),
    async execute(toolCallId, params, onUpdate, ctx, signal) {
      const result = await pi.exec("leindex", ["diagnostics"], { signal });

      return {
        content: [{ type: "text", text: result.stdout || result.stderr }],
        isError: result.code !== 0,
        details: { exitCode: result.code },
      };
    },
  });

  // Register leindex_context
  pi.registerTool({
    name: "leindex_context",
    label: "LeIndex: Context Expansion",
    description: "Expand context around a specific code node using Program Dependence Graph traversal.",
    parameters: Type.Object({
      node_id: Type.String({ description: "Full node identifier (e.g., file_path:qualified_name)" }),
      token_budget: Type.Optional(Type.Number({ description: "Maximum tokens for context (default: 2000)" })),
    }),
    async execute(toolCallId, params, onUpdate, ctx, signal) {
      const args = ["context", params.node_id];
      if (params.token_budget) {
        args.push("--tokens", params.token_budget.toString());
      }

      const result = await pi.exec("leindex", args, { signal });

      return {
        content: [{ type: "text", text: result.stdout || result.stderr }],
        isError: result.code !== 0,
        details: { exitCode: result.code },
      };
    },
  });

  // Register /leindex command
  pi.registerCommand("leindex", {
    description: "LeIndex operations",
    async handler(args, ctx) {
      if (!args || args === "help") {
        ctx.ui.notify("Available LeIndex commands:\n/leindex index [path]\n/leindex search <query>\n/leindex analyze <query>\n/leindex diag", "info");
        return;
      }

      const [cmd, ...rest] = args.split(" ");
      const val = rest.join(" ");

      switch (cmd) {
        case "index":
          ctx.ui.notify(`Indexing ${val || ctx.cwd}...`, "info");
          await pi.exec("leindex", ["index", val || ctx.cwd]);
          ctx.ui.notify("Indexing complete!", "success");
          break;
        case "search":
          const results = await pi.exec("leindex", ["search", val]);
          ctx.ui.notify(results.stdout, "info");
          break;
        case "diag":
          const diag = await pi.exec("leindex", ["diagnostics"]);
          ctx.ui.notify(diag.stdout, "info");
          break;
        default:
          ctx.ui.notify(`Unknown leindex command: ${cmd}`, "error");
      }
    },
  });
}
