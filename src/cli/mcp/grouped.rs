// Grouped MCP tool surface (v2.0.0).
//
// The server implements ~20 individual handlers, but advertising them all costs
// a model a schema each on every session and forces it to choose between
// near-synonyms. `tools/list` therefore advertises FIVE routers; the operation
// is selected with a discriminator argument and every other argument is
// forwarded unchanged to the per-feature handler:
//
//   leindex_explore  (mode)    search | symbol_lookup | find | structural | read_file
//                              | read_symbol | project_map | file_summary | context
//   leindex_analyze  (mode)    deep | impact | diagnostics | phase
//   leindex_edit     (action)  apply | preview | rename | write        (action REQUIRED)
//   leindex_manage   (action)  index | state | projects | evict | lease | cleanup
//   leindex_git      (action)  status | diff | log | show | blame | branch | remote
//                              | add | commit | checkout | stash | tag | merge
//                              | rebase | push | reset | force   (mutations preview-only)
//
// `leindex_edit` has no default on purpose: an ambiguous call must fail rather
// than guess at a mutation. The other routers default to their most common
// read-only branch so single-purpose calls stay one argument shorter.
//
// A grouped call is resolved to the underlying handler *before* dispatch, so
// output trimming/rendering (keyed by the underlying tool name) and every
// handler behave exactly as when called directly. The original per-tool names
// (`leindex_search`, `leindex_edit_apply`, ...) stay callable — configs,
// prompts and scripts written against them keep working — but are not
// advertised. Router schemas are derived from the handlers' own
// `argument_schema()` so the two can never drift apart.

use super::handlers::ToolHandler;
use super::protocol::JsonRpcError;
use serde_json::{Map, Value, json};

/// Set to `1` to make `tools/list` advertise the individual legacy tools as well.
pub const LEGACY_TOOLS_ENV: &str = "LEINDEX_MCP_LEGACY_TOOLS";

/// Set to `oneof` to make `tools/list` emit `oneOf`/`discriminator` router
/// schemas (the CLI `tools schema` output) instead of the flat union. Some LLM
/// APIs reject a top-level `oneOf` in a tool input schema, so flat is default.
pub const SCHEMA_STYLE_ENV: &str = "LEINDEX_MCP_SCHEMA";

/// Pointer appended to every router description.
pub const GUIDE_URI: &str = "leindex://tools/guide";

/// Response detail tiers accepted by every router via `tier`.
pub const TIERS: [&str; 3] = ["l0", "l1", "l2"];

/// One branch of a router and the handler it delegates to.
pub struct BranchSpec {
    /// Discriminator value.
    pub branch: &'static str,
    /// Canonical name of the underlying handler.
    pub tool: &'static str,
    /// Alternate spellings accepted for the discriminator value.
    pub aliases: &'static [&'static str],
    /// One-line summary used by the guide and the CLI table.
    pub summary: &'static str,
}

/// A tool advertised on the public MCP surface.
pub struct GroupSpec {
    /// Public tool name.
    pub name: &'static str,
    /// Human-readable title.
    pub title: &'static str,
    /// Description body (the guide pointer is appended).
    pub description: &'static str,
    /// Discriminator property (`mode` or `action`).
    pub discriminator: &'static str,
    /// Branch used when the caller omits the discriminator.
    pub default_branch: Option<&'static str>,
    /// Branches and their handlers.
    pub branches: &'static [BranchSpec],
}

const fn branch(
    branch: &'static str,
    tool: &'static str,
    aliases: &'static [&'static str],
    summary: &'static str,
) -> BranchSpec {
    BranchSpec {
        branch,
        tool,
        aliases,
        summary,
    }
}

/// The public tool surface.
pub static GROUPS: [GroupSpec; 5] = [
    GroupSpec {
        name: "leindex_explore",
        title: "LeIndex [Explore]",
        description: "Find and read code. mode: search (by meaning, default), find (exact text/regex/symbol names, anywhere on disk), structural (ast-grep AST pattern), symbol_lookup (callers/callees), read_file, read_symbol, project_map, file_summary, context.",
        discriminator: "mode",
        default_branch: Some("search"),
        branches: &[
            branch(
                "search",
                "leindex_search",
                &["semantic"],
                "Ranked semantic + structural search",
            ),
            branch(
                "symbol_lookup",
                "leindex_symbol_lookup",
                &["lookup", "symbol"],
                "Definition with callers, callees and dependencies",
            ),
            branch(
                "find",
                "leindex_find",
                &[
                    "grep",
                    "text",
                    "text_search",
                    "grep_symbols",
                    "symbols",
                    "rg",
                ],
                "Exact text/regex or symbol-name search: indexed for speed, live for correctness, any path on disk",
            ),
            branch(
                "structural",
                "leindex_structural",
                &["ast", "ast_grep", "astgrep", "pattern"],
                "AST-pattern search with ast-grep, e.g. `fn $NAME($$$ARGS) { $$$BODY }` (needs the structural-search feature and LEINDEX_FEATURE_STRUCTURAL_SEARCH=1)",
            ),
            branch(
                "read_file",
                "leindex_read_file",
                &[],
                "Read a file (line ranges) with a PDG symbol map",
            ),
            branch(
                "read_symbol",
                "leindex_read_symbol",
                &[],
                "Read one symbol's source with dependencies",
            ),
            branch(
                "project_map",
                "leindex_project_map",
                &["map"],
                "Annotated project tree with hotspots and module dependencies",
            ),
            branch(
                "file_summary",
                "leindex_file_summary",
                &["summary"],
                "Structured file overview",
            ),
            branch(
                "context",
                "leindex_context",
                &[],
                "Expand PDG context around a node or symbol",
            ),
        ],
    },
    GroupSpec {
        name: "leindex_analyze",
        title: "LeIndex [Analyze]",
        description: "Program-dependence-graph analysis. mode: deep (semantic search + graph expansion, default), impact (blast radius), diagnostics, phase (5-phase architecture report).",
        discriminator: "mode",
        default_branch: Some("deep"),
        branches: &[
            branch(
                "deep",
                "leindex_deep_analyze",
                &["deep_analyze"],
                "Semantic retrieval expanded through the PDG",
            ),
            branch(
                "impact",
                "leindex_impact_analysis",
                &["impact_analysis"],
                "Transitive impact of changing a symbol",
            ),
            branch(
                "diagnostics",
                "leindex_diagnostics",
                &[],
                "Index health, sizes, cache and memory statistics",
            ),
            branch(
                "phase",
                "leindex_phase_analysis",
                &["phase_analysis"],
                "5-phase architecture analysis",
            ),
        ],
    },
    GroupSpec {
        name: "leindex_edit",
        title: "LeIndex [Edit]",
        description: "Context-aware editing. action (required, no default): apply (atomic edit), preview (dry-run diff + impact), rename (cross-file symbol rename; preview by default), write (atomic file write).",
        discriminator: "action",
        default_branch: None,
        branches: &[
            branch(
                "apply",
                "leindex_edit_apply",
                &["edit_apply"],
                "Apply an edit atomically with impact analysis",
            ),
            branch(
                "preview",
                "leindex_edit_preview",
                &["edit_preview"],
                "Dry-run an edit; returns diff, impact and a preview_token",
            ),
            branch(
                "rename",
                "leindex_rename_symbol",
                &["rename_symbol"],
                "PDG-wide symbol rename (preview_only defaults to true)",
            ),
            branch(
                "write",
                "leindex_write",
                &[],
                "Atomic file create/overwrite",
            ),
        ],
    },
    GroupSpec {
        name: "leindex_manage",
        title: "LeIndex [Manage]",
        description: "Index lifecycle and state. action: index (build/refresh, pollable — default), state (usable? generation, phase, freshness), projects, evict, lease, cleanup (retention, dry-run).",
        discriminator: "action",
        default_branch: Some("index"),
        branches: &[
            branch(
                "index",
                "leindex_index",
                &[],
                "Build or refresh the index (pollable job)",
            ),
            branch(
                "state",
                "leindex_manage",
                &["status", "freshness"],
                "Is the index usable? generation, phase, freshness, counts",
            ),
            branch(
                "projects",
                "leindex_manage",
                &["resident", "list"],
                "Projects resident in memory, with the default",
            ),
            branch(
                "evict",
                "leindex_manage",
                &["release", "unload"],
                "Release a project from memory (data stays on disk); target required",
            ),
            branch(
                "lease",
                "leindex_manage",
                &["pin"],
                "Acquire a generation lease and report the pinned generation",
            ),
            branch(
                "cleanup",
                "leindex_manage",
                &["retention", "prune"],
                "Retention report for the generation store (dry_run by default)",
            ),
        ],
    },
    GroupSpec {
        name: "leindex_git",
        title: "LeIndex [Git]",
        description: "Live Git. action: status (default), diff, log, show, blame, branch, remote (read); add, commit, checkout, stash, tag, merge, rebase, push, reset, force (PREVIEW ONLY — each returns a dry-run plan and a preview_token).",
        discriminator: "action",
        default_branch: Some("status"),
        branches: &[
            branch(
                "status",
                "leindex_git_status",
                &[],
                "Working-tree status enriched with changed symbols",
            ),
            branch(
                "diff",
                "leindex_git_diff",
                &[],
                "PDG-enriched diff (working tree, staged, ref or range)",
            ),
            branch(
                "log",
                "leindex_git",
                &["history"],
                "Commit history: oid, date, author, subject",
            ),
            branch("show", "leindex_git", &[], "One commit: patch or stat"),
            branch(
                "blame",
                "leindex_git",
                &[],
                "Per-line last-touching commit for one file",
            ),
            branch(
                "branch",
                "leindex_git",
                &[],
                "Local and remote branches with tip commits",
            ),
            branch(
                "remote",
                "leindex_git",
                &[],
                "Configured remotes and their URLs",
            ),
            // Mutations: never fire from a default argument set. Each returns
            // a dry-run plan plus a preview_token; `confirm` is refused until
            // the apply side is wired.
            branch(
                "add",
                "leindex_git",
                &["stage"],
                "PREVIEW ONLY: stage the given paths",
            ),
            branch(
                "commit",
                "leindex_git",
                &[],
                "PREVIEW ONLY: commit the staged changes",
            ),
            branch(
                "checkout",
                "leindex_git",
                &["switch"],
                "PREVIEW ONLY: switch to a branch or commit",
            ),
            branch(
                "stash",
                "leindex_git",
                &[],
                "PREVIEW ONLY: stash working-tree changes",
            ),
            branch("tag", "leindex_git", &[], "PREVIEW ONLY: create a tag"),
            branch(
                "merge",
                "leindex_git",
                &[],
                "PREVIEW ONLY: merge a branch into the current one",
            ),
            branch(
                "rebase",
                "leindex_git",
                &[],
                "PREVIEW ONLY: rebase onto a target",
            ),
            branch(
                "push",
                "leindex_git",
                &[],
                "PREVIEW ONLY: push the current branch to its remote",
            ),
            branch(
                "reset",
                "leindex_git",
                &[],
                "PREVIEW ONLY: move the current branch to a commit",
            ),
            branch(
                "force",
                "leindex_git",
                &["force_push"],
                "PREVIEW ONLY: force-push (requires a target ref)",
            ),
        ],
    },
];

/// Router description including the guide pointer.
pub fn full_description(group: &GroupSpec) -> String {
    format!("{} See {GUIDE_URI} for branch args.", group.description)
}

/// Look up a public tool by name. Accepts `leindex_explore`, `explore`,
/// `leindex-explore`, `LeIndex [Explore]`, and dotted spellings.
pub fn group_by_name(name: &str) -> Option<&'static GroupSpec> {
    // `LeIndex [Explore]` (the display title) normalizes to `leindex_[explore]`.
    let normalized = super::output::normalize_tool_name(name).replace(['[', ']'], "");
    let short = normalized.strip_prefix("leindex_").unwrap_or(&normalized);
    GROUPS
        .iter()
        .find(|group| group.name.strip_prefix("leindex_") == Some(short))
}

/// Normalize a discriminator value: case-insensitive, `-`/space/dot tolerant,
/// and tolerant of the underlying tool's own name (`leindex_edit_apply`).
fn normalize_branch(raw: &str) -> String {
    let lowered = raw
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' ', '.'], "_");
    lowered
        .strip_prefix("leindex_")
        .unwrap_or(&lowered)
        .to_string()
}

fn matches_branch(spec: &BranchSpec, wanted: &str) -> bool {
    spec.branch == wanted
        || spec.tool.strip_prefix("leindex_") == Some(wanted)
        || spec.aliases.contains(&wanted)
}

fn find_branch<'a>(group: &'a GroupSpec, wanted: &str) -> Option<&'a BranchSpec> {
    group
        .branches
        .iter()
        .find(|spec| matches_branch(spec, wanted))
}

fn branch_list(group: &GroupSpec) -> String {
    group
        .branches
        .iter()
        .map(|spec| spec.branch)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Levenshtein distance, for "did you mean" suggestions.
pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            current.push(
                (previous[j] + cost)
                    .min(previous[j + 1] + 1)
                    .min(current[j] + 1),
            );
        }
        previous = current;
    }
    previous[b.len()]
}

/// Closest branch of `group` to `wanted`, if plausibly a typo.
pub fn branch_suggestion(group: &GroupSpec, wanted: &str) -> Option<&'static str> {
    group
        .branches
        .iter()
        .flat_map(|spec| std::iter::once(spec.branch).chain(spec.aliases.iter().copied()))
        .map(|candidate| (edit_distance(wanted, candidate), candidate))
        .filter(|(distance, candidate)| *distance <= 2.max(candidate.len() / 3))
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
        .and_then(|candidate| {
            group
                .branches
                .iter()
                .find(|spec| spec.branch == candidate || spec.aliases.contains(&candidate))
                .map(|spec| spec.branch)
        })
}

/// Error for a tool name that is neither a router nor a legacy tool, naming
/// the closest router/branch instead of a bare "method not found".
pub fn suggest_unknown_tool(name: &str) -> JsonRpcError {
    let normalized = normalize_branch(&super::output::normalize_tool_name(name));
    let mut best: Option<(usize, String)> = None;
    for group in &GROUPS {
        let router = group.name.strip_prefix("leindex_").unwrap_or(group.name);
        let d = edit_distance(&normalized, router);
        if d <= 3 && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
            best = Some((
                d,
                format!("{} ({}=<branch>)", group.name, group.discriminator),
            ));
        }
        for spec in group.branches {
            for candidate in std::iter::once(spec.branch)
                .chain(spec.aliases.iter().copied())
                .chain(std::iter::once(
                    spec.tool.strip_prefix("leindex_").unwrap_or(spec.tool),
                ))
            {
                let d = edit_distance(&normalized, candidate);
                if d <= 3.max(candidate.len() / 3) && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                    best = Some((
                        d,
                        format!(
                            "{} with {}=\"{}\"",
                            group.name, group.discriminator, spec.branch
                        ),
                    ));
                }
            }
        }
    }
    let tools = GROUPS
        .iter()
        .map(|group| group.name)
        .collect::<Vec<_>>()
        .join(", ");
    let hint = match best {
        Some((_, suggestion)) => format!("Did you mean {suggestion}? Available tools: {tools}."),
        None => format!("Available tools: {tools}. Read {GUIDE_URI} for branch arguments."),
    };
    JsonRpcError::invalid_params_with_suggestion(format!("Unknown tool '{name}'"), hint)
}

/// Resolve a call to `(underlying tool name, arguments)`.
///
/// Names that are not routers are returned unchanged, so direct calls to the
/// original tools keep working. For routers the discriminator is consumed and
/// the remaining arguments are forwarded verbatim.
pub fn resolve_call(name: &str, args: Value) -> Result<(String, Value), JsonRpcError> {
    let Some(group) = group_by_name(name) else {
        return Ok(redirect_retired_tool(name, args));
    };
    let mut args = args;
    let requested = take_discriminator(group, &mut args);
    let wanted = match (requested, group.default_branch) {
        (Some(wanted), _) if !wanted.is_empty() => wanted,
        (_, Some(default)) => default.to_string(),
        _ => {
            return Err(JsonRpcError::invalid_params_with_suggestion(
                format!(
                    "{} requires '{}' (an ambiguous call must not guess at a mutation)",
                    group.name, group.discriminator
                ),
                format!(
                    "Set {} to one of: {}",
                    group.discriminator,
                    branch_list(group)
                ),
            ));
        }
    };
    let spec = find_branch(group, &wanted).ok_or_else(|| {
        let hint = match branch_suggestion(group, &wanted) {
            Some(close) => format!("Did you mean '{close}'? "),
            None => String::new(),
        };
        JsonRpcError::invalid_params_with_suggestion(
            format!(
                "Unknown {} '{}' for {}",
                group.discriminator, wanted, group.name
            ),
            format!(
                "{hint}Set {} to one of: {}",
                group.discriminator,
                branch_list(group)
            ),
        )
    })?;
    apply_alias_defaults(spec, &wanted, &mut args);
    Ok((spec.tool.to_string(), args))
}

/// `grep`-style spellings mean "find symbols named X, else the text": the
/// `auto` target of `leindex_find`. Explicit `target` always wins.
///
/// `leindex_git` and `leindex_manage` are the two handlers that select an
/// operation from their own `action` argument, which is also the router's
/// discriminator. `take_discriminator` consumes that key before dispatch, so
/// the resolved branch is written back under it (canonical, so an alias such as
/// `status` arrives as `state`); without this the handler only ever sees its
/// default and every such branch silently runs it.
///
/// Keyed on the handler, not on the group: `manage`'s `index` branch targets
/// `leindex_index`, which reads no `action` and must not receive one, and
/// `leindex_git`'s `status`/`diff` branches likewise route to their own
/// handlers.
///
/// The phase report format does NOT belong here: `take_discriminator` consumes
/// `mode` before this runs, so a bare format value has already been handed down
/// as `format` together with the `phase` branch.
fn apply_alias_defaults(spec: &BranchSpec, wanted: &str, args: &mut Value) {
    if spec.tool == "leindex_find" && matches!(wanted, "grep" | "grep_symbols" | "symbols") {
        if let Some(object) = args.as_object_mut() {
            object.entry("target").or_insert_with(|| json!("auto"));
        }
    }
    if matches!(spec.tool, "leindex_git" | "leindex_manage") {
        if let Some(object) = args.as_object_mut() {
            object.insert("action".to_string(), json!(spec.branch));
        }
    }
}

/// `leindex_grep_symbols` and `leindex_text_search` were folded into
/// `leindex_find`; direct calls by those names keep working.
fn redirect_retired_tool(name: &str, mut args: Value) -> (String, Value) {
    match super::output::normalize_tool_name(name).as_str() {
        "leindex_text_search" | "text_search" => ("leindex_find".to_string(), args),
        "leindex_grep_symbols" | "grep_symbols" => {
            if let Some(object) = args.as_object_mut() {
                object.entry("target").or_insert_with(|| json!("auto"));
            }
            ("leindex_find".to_string(), args)
        }
        _ => (name.to_string(), args),
    }
}

/// Remove the branch selector from `args` and return the branch it names.
///
/// The router's own key wins. The other key is honoured only when the value
/// names one of this router's branches, because models often mix up
/// `mode`/`action`; the recovered value is re-inserted under the router's key,
/// so a handler that reads the discriminator (as `leindex_git` does) still
/// sees it.
fn take_discriminator(group: &GroupSpec, args: &mut Value) -> Option<String> {
    let object = args.as_object_mut()?;
    if let Some(value) = object.get(group.discriminator).and_then(Value::as_str) {
        let value = normalize_branch(value);
        object.remove(group.discriminator);
        // The value may belong to a branch's own argument of the same name
        // rather than to the router: `leindex_analyze`'s `mode` is also
        // `leindex_phase_analysis`'s report format. A bare
        // `{"mode": "ultra"}` names no branch, so it is a phase request —
        // resolve to that branch and hand the value down as `format`, the name
        // the phase schema documents for router calls.
        if find_branch(group, &value).is_none() {
            if let Some(spec) = phase_format_branch(group, &value) {
                object.insert("format".to_string(), json!(value));
                return Some(spec.branch.to_string());
            }
        }
        return Some(value);
    }
    let other = if group.discriminator == "mode" {
        "action"
    } else {
        "mode"
    };
    let candidate = object
        .get(other)
        .and_then(Value::as_str)
        .map(normalize_branch)?;
    find_branch(group, &candidate)?;
    object.remove(other);
    object.insert(group.discriminator.to_string(), json!(candidate));
    Some(candidate)
}

/// The branch implied by a value that is really a *branch argument* sharing the
/// router's discriminator name.
///
/// `leindex_phase_analysis` names its report format `mode`, but `mode` is
/// `leindex_analyze`'s branch selector, so the router consumes it. A value that
/// names no branch but is a valid format therefore means the phase branch, and
/// it must be handed down as `format` rather than silently changing the report
/// (or, worse, falling through to the default `deep`).
///
/// Returns `None` for every other router, so the caller falls back to its
/// normal branch matching.
// shh: the format enum is duplicated from `phase_analysis_schema`; if it gains
// a value, add it here too.
fn phase_format_branch(group: &GroupSpec, value: &str) -> Option<&'static BranchSpec> {
    if group.discriminator != "mode" || !matches!(value, "ultra" | "balanced" | "verbose") {
        return None;
    }
    group
        .branches
        .iter()
        .find(|spec| spec.tool == "leindex_phase_analysis")
}

fn find_handler<'a>(handlers: &'a [ToolHandler], tool: &str) -> Option<&'a ToolHandler> {
    handlers.iter().find(|handler| handler.name() == tool)
}

fn required_of(schema: &Value) -> Vec<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Shorten a property description to its first sentence (bounded), keeping the
/// schema payload small. Full text lives in the guide resource.
fn brief(description: &str) -> String {
    const MAX: usize = 80;
    let first = description
        .split(". ")
        .next()
        .unwrap_or(description)
        .trim_end_matches('.');
    if first.chars().count() <= MAX {
        return first.to_string();
    }
    let cut: String = first.chars().take(MAX - 1).collect();
    format!("{}…", cut.trim_end())
}

/// Shrink a property for the advertised listing: keep `type`/`enum`/`items`
/// and a one-sentence description; fold `default` into the description and
/// drop numeric bounds (the handler enforces them and reports a clear error).
/// Full definitions live in `leindex://tools/guide` and `tools schema`.
fn compact_property(definition: &Value) -> Value {
    let Some(source) = definition.as_object() else {
        return definition.clone();
    };
    let mut compact = Map::new();
    for key in ["type", "enum", "items"] {
        if let Some(value) = source.get(key) {
            compact.insert(key.to_string(), value.clone());
        }
    }
    let mut description = source
        .get("description")
        .and_then(Value::as_str)
        .map(brief)
        .unwrap_or_default();
    if let Some(default) = source.get("default") {
        if !description.to_ascii_lowercase().contains("default") && !default.is_null() {
            let text = match default {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            if text.len() <= 24 {
                description = if description.is_empty() {
                    format!("default {text}")
                } else {
                    format!("{description} (default {text})")
                };
            }
        }
    }
    if !description.is_empty() {
        compact.insert("description".to_string(), Value::String(description));
    }
    Value::Object(compact)
}

fn tier_property() -> Value {
    json!({
        "type": "string",
        "enum": TIERS,
        "description": "Detail: l0 card, l1 overview (default), l2 full"
    })
}

fn project_path_property() -> Value {
    json!({
        "type": "string",
        "description": "Project directory (auto-indexes on first use; omit for current)"
    })
}

/// Merge `incoming` property definitions into `merged`.
///
/// First definition wins. When another branch defines the same property
/// differently, value constraints (`enum`, bounds, defaults) are dropped and
/// only the shared `type` is kept: the handler still validates strictly, and a
/// schema stricter than one of its branches would make a client reject a valid
/// call.
fn merge_properties(
    merged: &mut Map<String, Value>,
    incoming: &Map<String, Value>,
    reserved: &[&str],
) {
    for (key, definition) in incoming {
        // The discriminator, project_path and tier are defined once by the router.
        if reserved.contains(&key.as_str()) {
            continue;
        }
        match merged.get(key) {
            None => {
                merged.insert(key.clone(), definition.clone());
            }
            Some(existing) if existing == definition => {}
            Some(existing) => {
                let mut loosened = Map::new();
                if existing.get("type") == definition.get("type") {
                    if let Some(kind) = existing.get("type") {
                        loosened.insert("type".to_string(), kind.clone());
                    }
                    if existing.get("items") == definition.get("items") {
                        if let Some(items) = existing.get("items") {
                            loosened.insert("items".to_string(), items.clone());
                        }
                    }
                }
                if let Some(description) = existing.get("description") {
                    loosened.insert("description".to_string(), description.clone());
                }
                merged.insert(key.clone(), Value::Object(loosened));
            }
        }
    }
}

/// `branch: arg*, arg, ...` so a model can see which arguments each branch
/// reads (`*` = required) without a schema per branch.
///
/// Branches that resolve to the SAME handler are folded into one
/// `branch,branch: args` line. A router whose branches are one handler's
/// `action` values (`leindex_manage`, `leindex_git`) would otherwise repeat an
/// identical argument list per branch, which is pure token cost: the list
/// cannot differ, because it comes from that one handler's schema.
fn branch_usage(group: &GroupSpec, handlers: &[ToolHandler]) -> String {
    // handler name -> (branches sharing it, its rendered argument list)
    let mut folded: Vec<(&str, Vec<&str>, String)> = Vec::new();
    for spec in group.branches {
        if let Some(entry) = folded.iter_mut().find(|(tool, _, _)| *tool == spec.tool) {
            entry.1.push(spec.branch);
            continue;
        }
        let Some(handler) = find_handler(handlers, spec.tool) else {
            continue;
        };
        let schema = handler.argument_schema();
        let required = required_of(&schema);
        let mut names: Vec<String> = schema
            .get("properties")
            .and_then(Value::as_object)
            .map(|properties| {
                properties
                    .keys()
                    .filter(|key| key.as_str() != "project_path")
                    .map(|key| {
                        if required.contains(key) {
                            format!("{key}*")
                        } else {
                            key.clone()
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort_by_key(|name| !name.ends_with('*'));
        folded.push((spec.tool, vec![spec.branch], names.join(", ")));
    }
    folded
        .into_iter()
        .map(|(_, branches, args)| format!("{}: {args}", branches.join(",")))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Flat router schema — one object whose properties are the union of every
/// branch's arguments. Works with clients/APIs that reject a top-level `oneOf`.
pub fn group_schema_flat(group: &GroupSpec, handlers: &[ToolHandler]) -> Value {
    let mut properties = Map::new();
    properties.insert(
        group.discriminator.to_string(),
        json!({
            "type": "string",
            "enum": group.branches.iter().map(|spec| spec.branch).collect::<Vec<_>>(),
            "description": format!(
                "Branch selector; other args are forwarded to it. {}Args per branch (* required): {}",
                match group.default_branch {
                    Some(default) => format!("Defaults to '{default}'. "),
                    None => "Required. ".to_string(),
                },
                branch_usage(group, handlers)
            ),
        }),
    );
    properties.insert("project_path".to_string(), project_path_property());
    properties.insert("tier".to_string(), tier_property());
    for spec in group.branches {
        let Some(handler) = find_handler(handlers, spec.tool) else {
            continue;
        };
        if let Some(incoming) = handler
            .argument_schema()
            .get("properties")
            .and_then(Value::as_object)
        {
            let incoming: Map<String, Value> = incoming
                .iter()
                .map(|(key, definition)| (key.clone(), compact_property(definition)))
                .collect();
            merge_properties(
                &mut properties,
                &incoming,
                &[group.discriminator, "project_path", "tier"],
            );
        }
    }
    let required: Vec<&str> = if group.default_branch.is_none() {
        vec![group.discriminator]
    } else {
        Vec::new()
    };
    json!({ "type": "object", "properties": properties, "required": required })
}

/// Discriminated-union router schema: `oneOf` over branches, each carrying
/// `{discriminator: {const: branch}}` plus that branch's own arguments,
/// defaults and bounds; `discriminator.propertyName` names the selector. The
/// top level also lists the branches as an `enum` for clients that read only
/// top-level properties.
pub fn group_schema_oneof(group: &GroupSpec, handlers: &[ToolHandler]) -> Value {
    let mut variants = Vec::new();
    for spec in group.branches {
        let Some(handler) = find_handler(handlers, spec.tool) else {
            continue;
        };
        let schema = handler.argument_schema();
        let is_default_branch = group
            .default_branch
            .is_some_and(|default| default == spec.branch);
        let mut discriminator_property =
            json!({ "const": spec.branch, "description": spec.summary });
        if is_default_branch {
            // Document the runtime default on the branch that owns it.
            discriminator_property["default"] = json!(spec.branch);
        }
        let mut properties = Map::new();
        properties.insert(group.discriminator.to_string(), discriminator_property);
        if let Some(own) = schema.get("properties").and_then(Value::as_object) {
            for (key, definition) in own {
                // The router owns the discriminator key. `leindex_phase_analysis`
                // declares its own `mode` (ultra|balanced|verbose), and copying it
                // here used to OVERWRITE the `{const: "phase"}` pin with that
                // enum — leaving the variant unable to match and the `oneOf`
                // union broken. Same reservation the flat schema's
                // `merge_properties` already makes.
                if key == group.discriminator {
                    continue;
                }
                properties.insert(key.clone(), definition.clone());
            }
        }
        properties.insert("project_path".to_string(), project_path_property());
        properties.insert("tier".to_string(), tier_property());
        let mut required = vec![group.discriminator.to_string()];
        required.extend(required_of(&schema));
        required.dedup();
        // A PRESENT discriminator pins the object to its own branch (every
        // non-default variant requires it, so `oneOf` still matches exactly
        // one); an OMITTED discriminator must also match exactly one variant
        // — the router's documented default branch — so only that variant
        // drops it from `required`. Runtime dispatch already accepts the
        // omission (it routes to `default_branch`); the schema now agrees
        // instead of rejecting calls the server accepts. Routers without a
        // default keep the discriminator required in every variant.
        if is_default_branch {
            let position = required
                .iter()
                .position(|key| key == group.discriminator)
                .expect("the discriminator was just pushed");
            required.remove(position);
        }
        variants.push(json!({
            "type": "object",
            "title": spec.branch,
            "properties": properties,
            "required": required,
        }));
    }
    json!({
        "type": "object",
        "properties": {
            group.discriminator: {
                "type": "string",
                "enum": group.branches.iter().map(|spec| spec.branch).collect::<Vec<_>>(),
                "description": "Select the granular handler branch; branch arguments are forwarded unchanged.",
            },
            "project_path": project_path_property(),
            "tier": tier_property(),
        },
        "required": if group.default_branch.is_none() { json!([group.discriminator]) } else { json!([]) },
        "oneOf": variants,
        "discriminator": { "propertyName": group.discriminator },
    })
}

/// The tools `tools/list` advertises.
pub fn public_tools_json(handlers: &[ToolHandler]) -> Vec<Value> {
    let oneof = std::env::var(SCHEMA_STYLE_ENV).is_ok_and(|v| v.eq_ignore_ascii_case("oneof"));
    let mut tools: Vec<Value> = GROUPS
        .iter()
        .map(|group| {
            json!({
                "name": group.name,
                "description": full_description(group),
                "inputSchema": if oneof {
                    group_schema_oneof(group, handlers)
                } else {
                    group_schema_flat(group, handlers)
                },
            })
        })
        .collect();
    if std::env::var(LEGACY_TOOLS_ENV).is_ok_and(|value| value == "1") {
        for handler in handlers {
            tools.push(json!({
                "name": handler.name(),
                "description": handler.description(),
                "inputSchema": handler.argument_schema(),
            }));
        }
    }
    tools
}

/// Markdown body of the `leindex://tools/guide` resource: every router, its
/// branches, and each branch's arguments (with defaults and bounds).
pub fn tools_guide_markdown(handlers: &[ToolHandler]) -> String {
    // The count and the selector list are DERIVED from GROUPS, not spelled out.
    // They were hardcoded, and adding `leindex_git` left this introduction
    // advertising "four tools" on a five-tool surface for a whole release. A
    // router can no longer be added without this line following it.
    let mut by_discriminator: Vec<(&str, Vec<&str>)> = Vec::new();
    for group in &GROUPS {
        let short = group.name.strip_prefix("leindex_").unwrap_or(group.name);
        match by_discriminator
            .iter_mut()
            .find(|(key, _)| *key == group.discriminator)
        {
            Some((_, names)) => names.push(short),
            None => by_discriminator.push((group.discriminator, vec![short])),
        }
    }
    let selectors = by_discriminator
        .iter()
        .map(|(key, names)| format!("`{key}` ({})", names.join(", ")))
        .collect::<Vec<_>>()
        .join(" or ");
    let mut out = format!(
        "# LeIndex tool guide\n\nLeIndex exposes {} tools. Pick the tool, then the branch with \
         {selectors}; every other argument is forwarded \
         unchanged to that branch. `project_path` is accepted everywhere (omit it to use the \
         server's project). `tier` (`l0` identity card, `l1` bounded overview — default, `l2` full \
         detail) is accepted everywhere.\n\nThe individual tool names (`leindex_search`, \
         `leindex_edit_apply`, ...) still work as direct calls.\n",
        GROUPS.len()
    );
    for group in &GROUPS {
        out.push_str(&format!(
            "\n## `{}` — `{}`{}\n\n{}\n",
            group.name,
            group.discriminator,
            match group.default_branch {
                Some(default) => format!(" (default `{default}`)"),
                None => " (required)".to_string(),
            },
            group.description
        ));
        for spec in group.branches {
            out.push_str(&format!(
                "\n### `{}` = `{}`\n\n{}\n\n",
                group.name, spec.branch, spec.summary
            ));
            let Some(handler) = find_handler(handlers, spec.tool) else {
                continue;
            };
            let schema = handler.argument_schema();
            let required = required_of(&schema);
            let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
                continue;
            };
            for (key, definition) in properties {
                if key == "project_path" {
                    continue;
                }
                let mut line = format!("- `{key}`");
                if required.contains(key) {
                    line.push_str(" (required)");
                }
                if let Some(kind) = definition.get("type").and_then(Value::as_str) {
                    line.push_str(&format!(" — {kind}"));
                }
                if let Some(default) = definition.get("default") {
                    line.push_str(&format!(", default {default}"));
                }
                if let Some(options) = definition.get("enum").and_then(Value::as_array) {
                    let options: Vec<String> = options
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect();
                    line.push_str(&format!(", one of {}", options.join("|")));
                }
                if let Some(description) = definition.get("description").and_then(Value::as_str) {
                    line.push_str(&format!(": {description}"));
                }
                out.push_str(&line);
                out.push('\n');
            }
        }
    }
    out
}

/// Compact table for `leindex tools list`.
pub fn cli_tools_table(verbose: bool) -> String {
    let mut out = String::new();
    for group in &GROUPS {
        out.push_str(&format!(
            "{:<17} {}={}\n",
            group.name,
            group.discriminator,
            group
                .branches
                .iter()
                .map(|spec| spec.branch)
                .collect::<Vec<_>>()
                .join("|")
        ));
        if verbose {
            out.push_str(&format!("    {}\n", full_description(group)));
            for spec in group.branches {
                out.push_str(&format!("      {:<13} {}\n", spec.branch, spec.summary));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::handlers::all_tool_handlers;

    #[test]
    fn test_every_branch_targets_a_registered_handler() {
        let handlers = all_tool_handlers();
        for group in &GROUPS {
            for spec in group.branches {
                assert!(
                    find_handler(&handlers, spec.tool).is_some(),
                    "{}::{} targets unknown handler {}",
                    group.name,
                    spec.branch,
                    spec.tool
                );
            }
        }
    }

    #[test]
    fn test_every_handler_is_reachable_through_a_router() {
        for handler in all_tool_handlers() {
            // `phase_analysis` is a compatibility alias of leindex_phase_analysis.
            if handler.name() == "phase_analysis" {
                continue;
            }
            assert!(
                GROUPS
                    .iter()
                    .flat_map(|group| group.branches)
                    .any(|spec| spec.tool == handler.name()),
                "{} is not reachable through any router",
                handler.name()
            );
        }
    }

    #[test]
    fn test_public_surface_is_the_five_routers() {
        let tools = public_tools_json(&all_tool_handlers());
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(
            names,
            [
                "leindex_explore",
                "leindex_analyze",
                "leindex_edit",
                "leindex_manage",
                "leindex_git"
            ]
        );
    }

    #[test]
    fn test_descriptions_stay_short_and_point_at_the_guide() {
        for group in &GROUPS {
            let description = full_description(group);
            assert!(
                description.len() <= 300,
                "{} is {} chars",
                group.name,
                description.len()
            );
            assert!(description.ends_with("for branch args."), "{description}");
            assert!(description.contains(GUIDE_URI));
        }
    }

    #[test]
    fn test_edit_requires_its_action() {
        let error = resolve_call("leindex_edit", json!({"file_path": "a"})).unwrap_err();
        assert!(error.message_with_hint().contains("action"), "{error}");
        let (tool, args) =
            resolve_call("leindex_edit", json!({"action": "apply", "file_path": "a"})).unwrap();
        assert_eq!(tool, "leindex_edit_apply");
        assert_eq!(args, json!({"file_path": "a"}));
    }

    #[test]
    fn test_read_only_routers_have_defaults() {
        let (tool, _) = resolve_call("leindex_explore", json!({"query": "x"})).unwrap();
        assert_eq!(tool, "leindex_search");
        let (tool, _) = resolve_call("leindex_analyze", json!({"query": "x"})).unwrap();
        assert_eq!(tool, "leindex_deep_analyze");
        let (tool, _) = resolve_call("leindex_manage", json!({"project_path": "/p"})).unwrap();
        assert_eq!(tool, "leindex_index");
    }

    #[test]
    fn test_branch_selects_handler_and_is_consumed() {
        let (tool, args) = resolve_call(
            "leindex_explore",
            json!({"mode": "grep", "pattern": "todo"}),
        )
        .unwrap();
        assert_eq!(tool, "leindex_find");
        assert_eq!(args, json!({"pattern": "todo", "target": "auto"}));
        // `git_diff` is no longer an `analyze` branch; it lives on `leindex_git`.
        // Naming it on `analyze` must fail loudly rather than resolve to a
        // near-synonym.
        let error = resolve_call("leindex_analyze", json!({"mode": "git_diff"})).unwrap_err();
        assert!(
            error.message_with_hint().contains("git_diff"),
            "{}",
            error.message_with_hint()
        );
        let (tool, _) = resolve_call("leindex_git", json!({"action": "diff"})).unwrap();
        assert_eq!(tool, "leindex_git_diff");
    }

    #[test]
    fn test_manage_branch_reaches_its_handler() {
        // `ManageHandler` reads `action` to pick its operation, and the router
        // consumes that same key as the discriminator. A branch routed to it
        // must therefore have the canonical name handed back, or the handler
        // falls through to its `index` default and silently builds an index
        // where the caller asked for state. A branch routed to a handler that
        // reads no `action` (`index` -> `leindex_index`) must NOT receive one.
        let group = group_by_name("leindex_manage").expect("manage router");
        for spec in group.branches {
            let (tool, args) =
                resolve_call(group.name, json!({ group.discriminator: spec.branch })).unwrap();
            assert_eq!(tool, spec.tool, "{}::{}", group.name, spec.branch);
            let handed_back = args.get(group.discriminator).and_then(Value::as_str);
            if spec.tool == "leindex_manage" {
                assert_eq!(
                    handed_back,
                    Some(spec.branch),
                    "{}::{} lost its action on the way to the handler",
                    group.name,
                    spec.branch
                );
            } else {
                assert_eq!(
                    handed_back, None,
                    "{}::{} targets {}, which reads no action",
                    group.name, spec.branch, spec.tool
                );
            }
        }
        // Omitted `action` still defaults to `index`, and that branch keeps
        // targeting `leindex_index` so output trimming stays keyed by the real
        // tool (`output::trim` matches on `leindex_index`).
        let (tool, _) = resolve_call("leindex_manage", json!({"project_path": "/p"})).unwrap();
        assert_eq!(tool, "leindex_index");
        // An alias resolves to the canonical branch, not the spelling.
        let (tool, args) = resolve_call("leindex_manage", json!({ "action": "status" })).unwrap();
        assert_eq!(tool, "leindex_manage");
        assert_eq!(args.get("action").and_then(Value::as_str), Some("state"));
    }

    #[test]
    fn test_router_and_branch_spellings_are_tolerated() {
        for name in [
            "leindex_explore",
            "explore",
            "leindex-explore",
            "LeIndex [Explore]",
        ] {
            assert!(group_by_name(name).is_some(), "{name}");
        }
        // Every historical spelling of a text/symbol search lands on `find`.
        for spelling in [
            "find",
            "grep",
            "text",
            "text_search",
            "text-search",
            "leindex_text_search",
            "grep_symbols",
            "leindex_grep_symbols",
            "symbols",
            "TEXT",
        ] {
            let (tool, _) =
                resolve_call("explore", json!({"mode": spelling, "query": "q"})).unwrap();
            assert_eq!(tool, "leindex_find", "spelling {spelling}");
        }
    }

    #[test]
    fn test_mode_and_action_are_interchangeable_when_unambiguous() {
        let (tool, _) = resolve_call("leindex_edit", json!({"mode": "preview"})).unwrap();
        assert_eq!(tool, "leindex_edit_preview");
        let (tool, _) = resolve_call("leindex_explore", json!({"action": "read_file"})).unwrap();
        assert_eq!(tool, "leindex_read_file");
    }

    #[test]
    fn test_phase_keeps_its_own_format_through_the_analyze_router() {
        // `mode` is `leindex_analyze`'s branch selector, so the phase branch
        // cannot receive its own format knob under that name. Before the fix
        // `{"mode": "ultra"}` was consumed as a discriminator, matched no
        // branch, and silently fell through to the DEFAULT branch (`deep`) —
        // a wrong tool AND a lost format. It must be handed down as `format`.
        let (tool, args) = resolve_call(
            "leindex_analyze",
            json!({"mode": "phase", "format": "ultra", "phase": 3}),
        )
        .unwrap();
        assert_eq!(tool, "leindex_phase_analysis");
        assert_eq!(args, json!({"format": "ultra", "phase": 3}));

        // A bare format value must NOT be read as a branch and must NOT drop
        // through to `deep`.
        let (tool, args) = resolve_call("leindex_analyze", json!({"mode": "ultra"})).unwrap();
        assert_eq!(tool, "leindex_phase_analysis");
        assert_eq!(args, json!({"format": "ultra"}));
    }

    #[test]
    fn test_unknown_branch_suggests_the_closest_one() {
        let error = resolve_call("leindex_analyze", json!({"mode": "impcat"})).unwrap_err();
        let text = error.message_with_hint();
        assert!(
            text.contains("impcat") && text.contains("Did you mean 'impact'"),
            "{text}"
        );
    }

    #[test]
    fn test_unknown_tool_suggests_router_and_branch() {
        // `git_diff` used to live on `leindex_analyze`; it is now a branch of
        // the dedicated `leindex_git` router, so the hint must name THAT.
        let text = suggest_unknown_tool("leindex_git_dif").message_with_hint();
        assert!(
            text.contains("leindex_git") && text.contains("diff"),
            "{text}"
        );
        let text = suggest_unknown_tool("totally_unrelated").message_with_hint();
        assert!(text.contains("Available tools"), "{text}");
    }

    #[test]
    fn test_retired_search_tools_redirect_to_find() {
        let (tool, args) = resolve_call("leindex_text_search", json!({"query": "x"})).unwrap();
        assert_eq!(
            (tool.as_str(), &args),
            ("leindex_find", &json!({"query": "x"}))
        );
        let (tool, args) = resolve_call("leindex.grep-symbols", json!({"pattern": "x"})).unwrap();
        assert_eq!(tool, "leindex_find");
        assert_eq!(args["target"], "auto");
        // grep-style aliases search symbol names first; text/find do not.
        let (_, args) = resolve_call("explore", json!({"mode": "grep", "pattern": "x"})).unwrap();
        assert_eq!(args["target"], "auto");
        let (_, args) = resolve_call("explore", json!({"mode": "text", "pattern": "x"})).unwrap();
        assert!(args.get("target").is_none());
        let (_, args) = resolve_call(
            "explore",
            json!({"mode": "grep", "pattern": "x", "target": "text"}),
        )
        .unwrap();
        assert_eq!(args["target"], "text", "an explicit target wins");
    }

    #[test]
    fn test_legacy_and_dotted_names_pass_through() {
        for name in [
            "leindex_edit_apply",
            "leindex.edit-apply",
            "phase_analysis",
            "leindex_search",
        ] {
            let (tool, args) = resolve_call(name, json!({"a": 1})).unwrap();
            assert_eq!(tool, name);
            assert_eq!(args, json!({"a": 1}));
        }
    }

    #[test]
    fn test_flat_schema_exposes_discriminator_and_union_of_arguments() {
        let handlers = all_tool_handlers();
        let group = group_by_name("leindex_explore").unwrap();
        let schema = group_schema_flat(group, &handlers);
        assert!(
            schema.get("oneOf").is_none(),
            "flat schema must not use a top-level oneOf"
        );
        let properties = schema["properties"].as_object().unwrap();
        assert_eq!(
            properties["mode"]["enum"].as_array().unwrap().len(),
            group.branches.len()
        );
        for key in [
            "query",
            "project_path",
            "top_k",
            "regex",
            "paths",
            "tier",
            "file_path",
        ] {
            assert!(properties.contains_key(key), "missing {key}");
        }
        assert_eq!(schema["required"], json!([]));
        let edit = group_schema_flat(group_by_name("leindex_edit").unwrap(), &handlers);
        assert_eq!(edit["required"], json!(["action"]));
    }

    #[test]
    fn test_oneof_schema_carries_const_discriminators_per_branch() {
        let handlers = all_tool_handlers();
        let group = group_by_name("leindex_analyze").unwrap();
        let schema = group_schema_oneof(group, &handlers);
        assert_eq!(schema["discriminator"]["propertyName"], "mode");
        let variants = schema["oneOf"].as_array().unwrap();
        assert_eq!(variants.len(), group.branches.len());
        for (variant, spec) in variants.iter().zip(group.branches) {
            assert_eq!(variant["properties"]["mode"]["const"], spec.branch);
            assert!(variant["properties"].get("project_path").is_some());
        }
    }

    /// An OMITTED discriminator must validate against exactly one `oneOf`
    /// variant — the router's documented default branch — because runtime
    /// dispatch routes the omission there. Every non-default variant keeps
    /// the discriminator required (a present value still pins the branch),
    /// and routers without a default require it everywhere (round-11 Codex
    /// P2: `leindex_explore({"query":"x"})` used to fail schema validation
    /// while the server accepted it).
    #[test]
    fn test_oneof_schema_omission_matches_only_the_default_branch() {
        let handlers = all_tool_handlers();

        // A router WITH a default: only the default branch drops the
        // discriminator from `required`.
        let group = group_by_name("leindex_explore").unwrap();
        let default_branch = group.default_branch.expect("explore documents a default");
        let explore_schema = group_schema_oneof(group, &handlers);
        let variants = explore_schema["oneOf"].as_array().unwrap().clone();
        for (variant, spec) in variants.iter().zip(group.branches) {
            let required = variant["required"].as_array().unwrap();
            if spec.branch == default_branch {
                assert_eq!(
                    variant["properties"]["mode"]["default"],
                    json!(default_branch),
                    "the default variant documents the default"
                );
                assert!(
                    !required.contains(&json!("mode")),
                    "the default variant must accept an omitted discriminator"
                );
            } else {
                assert!(
                    required.contains(&json!("mode")),
                    "non-default variants still pin their branch"
                );
            }
        }

        // A router WITHOUT a default: every variant requires the
        // discriminator.
        let group = group_by_name("leindex_edit").unwrap();
        assert!(group.default_branch.is_none());
        let edit_schema = group_schema_oneof(group, &handlers);
        let variants = edit_schema["oneOf"].as_array().unwrap();
        for variant in variants {
            assert!(
                variant["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("action")),
                "no default means no variant accepts an omitted discriminator"
            );
        }
    }

    #[test]
    fn test_branch_usage_folds_branches_that_share_a_handler() {
        let handlers = all_tool_handlers();
        // `leindex_manage` is five `action` values over one handler: the
        // argument list cannot differ per branch, so it is emitted once with
        // the branches joined. This is what keeps the router listing from
        // repeating an identical list five times.
        let manage = group_by_name("leindex_manage").expect("manage router");
        let usage = branch_usage(manage, &handlers);
        assert!(
            usage.contains("state,projects,evict,lease,cleanup:"),
            "expected the shared branches folded into one entry: {usage}"
        );
        assert_eq!(
            usage.matches("target").count(),
            1,
            "a shared handler's args must appear once, not once per branch: {usage}"
        );
        // `index` targets a different handler, so it keeps its own entry.
        assert!(usage.contains("index:"), "{usage}");
        // Branches that target distinct handlers are NOT folded.
        let explore = group_by_name("leindex_explore").expect("explore router");
        let usage = branch_usage(explore, &handlers);
        assert!(
            usage.contains("search:") && usage.contains("context:"),
            "{usage}"
        );
        assert!(
            !usage.contains("search,"),
            "distinct handlers stay separate: {usage}"
        );
    }

    #[test]
    fn test_guide_documents_every_branch() {
        let guide = tools_guide_markdown(&all_tool_handlers());
        for group in &GROUPS {
            assert!(guide.contains(group.name));
            for spec in group.branches {
                assert!(
                    guide.contains(&format!("`{}` = `{}`", group.name, spec.branch)),
                    "guide misses {}::{}",
                    group.name,
                    spec.branch
                );
            }
        }
    }

    /// Payload-size regression gate for `tools/list` (progressive disclosure:
    /// argument detail lives behind `leindex://tools/guide`).
    #[test]
    fn test_benchmark_tools_list_sizes() {
        let handlers = all_tool_handlers();
        let grouped = serde_json::to_string(&public_tools_json(&handlers))
            .unwrap()
            .len();
        let legacy: usize = handlers
            .iter()
            .map(|handler| {
                json!({
                    "name": handler.name(),
                    "description": handler.description(),
                    "inputSchema": handler.argument_schema(),
                })
                .to_string()
                .len()
            })
            .sum();
        assert!(
            grouped * 10 < legacy * 6,
            "router listing ({grouped} bytes) must stay under 60% of the flat listing ({legacy} bytes)"
        );
        // 13_000 was the ceiling before `leindex_explore` gained its
        // `structural` branch; that branch contributes a new `lang` argument
        // plus one more entry in the per-branch arg map that `branch_usage`
        // builds, which put the listing at 13_251 bytes (flat listing 24_546,
        // ratio 0.54 — the 60% assertion above is the guard that actually
        // bounds growth; this absolute ceiling is the backstop). Raised rather
        // than dropped, per owner decision. If this number moves again, say
        // which branch moved it.
        // 14_000 -> 16_500: the FIFTH router `leindex_git` was added. It
        // advertises 17 operations (7 reads + 10 preview-only mutations) and
        // one `action` entry per operation in the arg map. The ratio guard
        // STILL passes (the router surface remains far smaller than the flat
        // per-tool listing), so growth is bounded; the absolute ceiling moves
        // with it.
        // 16_500 -> 17_500: `leindex_manage` went from ONE branch (`index`) to
        // six, which on its own pushed the listing to 17_258 bytes because
        // `branch_usage` emitted the shared handler's identical argument list
        // once per branch. Folding branches that share a handler brought it
        // back to 15_742 (flat 30_472, ratio 0.52), so the ceiling goes UP
        // only to match the branch count, not to hide a regression: the next
        // branch that lands here must not push past this without the same
        // accounting.
        assert!(grouped <= 17_500, "tools/list grew to {grouped} bytes");
    }
}
