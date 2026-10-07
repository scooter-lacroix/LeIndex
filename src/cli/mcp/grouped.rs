// Grouped MCP tool surface (v2.0.0).
//
// The server implements ~20 individual handlers, but advertising them all costs
// a model a schema each on every session and forces it to choose between
// near-synonyms. `tools/list` therefore advertises FOUR routers; the operation
// is selected with a discriminator argument and every other argument is
// forwarded unchanged to the per-feature handler:
//
//   leindex_explore  (mode)    search | symbol_lookup | find | read_file | read_symbol
//                              | project_map | file_summary | context
//   leindex_analyze  (mode)    deep | impact | diagnostics | git_status | git_diff
//   leindex_edit     (action)  apply | preview | rename | write        (action REQUIRED)
//   leindex_manage   (action)  index | phase
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
pub static GROUPS: [GroupSpec; 4] = [
    GroupSpec {
        name: "leindex_explore",
        title: "LeIndex [Explore]",
        description: "Find and read code. mode: search (by meaning, default), find (exact text/regex/symbol names, anywhere on disk), symbol_lookup (callers/callees), read_file, read_symbol, project_map, file_summary, context.",
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
        description: "Program-dependence-graph analysis and repo state. mode: deep (semantic search + graph expansion, default), impact (blast radius), diagnostics, git_status, git_diff.",
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
                "git_status",
                "leindex_git_status",
                &[],
                "Live git status enriched with changed symbols",
            ),
            branch(
                "git_diff",
                "leindex_git_diff",
                &["diff"],
                "PDG-enriched diff (working tree, staged, ref or range)",
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
        description: "Index lifecycle and architecture reports. action: index (build/refresh; returns a pollable job, default), phase (5-phase architecture analysis).",
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
                "phase",
                "leindex_phase_analysis",
                &["phase_analysis"],
                "5-phase architecture analysis",
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
fn edit_distance(a: &str, b: &str) -> usize {
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
fn apply_alias_defaults(spec: &BranchSpec, wanted: &str, args: &mut Value) {
    if spec.tool == "leindex_find" && matches!(wanted, "grep" | "grep_symbols" | "symbols") {
        if let Some(object) = args.as_object_mut() {
            object.entry("target").or_insert_with(|| json!("auto"));
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

/// Remove and return the discriminator from `args`.
///
/// The router's own key wins. The other key is honoured only when the value
/// names one of this router's branches, because models often mix up
/// `mode`/`action` — but never for a branch argument that legitimately reuses
/// the key (`leindex_manage` `phase` takes its own `mode: ultra|balanced|verbose`).
fn take_discriminator(group: &GroupSpec, args: &mut Value) -> Option<String> {
    let object = args.as_object_mut()?;
    if let Some(value) = object.get(group.discriminator).and_then(Value::as_str) {
        let value = normalize_branch(value);
        object.remove(group.discriminator);
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
    if find_branch(group, &candidate).is_some() {
        object.remove(other);
        return Some(candidate);
    }
    None
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
fn branch_usage(group: &GroupSpec, handlers: &[ToolHandler]) -> String {
    let mut lines = Vec::new();
    for spec in group.branches {
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
        lines.push(format!("{}: {}", spec.branch, names.join(", ")));
    }
    lines.join(" | ")
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
    let mut out = String::from(
        "# LeIndex tool guide\n\nLeIndex exposes four tools. Pick the tool, then the branch with \
         `mode` (explore, analyze) or `action` (edit, manage); every other argument is forwarded \
         unchanged to that branch. `project_path` is accepted everywhere (omit it to use the \
         server's project). `tier` (`l0` identity card, `l1` bounded overview — default, `l2` full \
         detail) is accepted everywhere.\n\nThe individual tool names (`leindex_search`, \
         `leindex_edit_apply`, ...) still work as direct calls.\n",
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
    fn test_public_surface_is_the_four_routers() {
        let tools = public_tools_json(&all_tool_handlers());
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(
            names,
            [
                "leindex_explore",
                "leindex_analyze",
                "leindex_edit",
                "leindex_manage"
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
        let (tool, _) = resolve_call("leindex_analyze", json!({"mode": "git_diff"})).unwrap();
        assert_eq!(tool, "leindex_git_diff");
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
    fn test_manage_phase_keeps_its_own_mode_argument() {
        let (tool, args) = resolve_call(
            "leindex_manage",
            json!({"action": "phase", "mode": "ultra", "phase": 3}),
        )
        .unwrap();
        assert_eq!(tool, "leindex_phase_analysis");
        assert_eq!(args, json!({"mode": "ultra", "phase": 3}));
        // `mode` alone (a phase output mode, not a branch) must not be eaten.
        let (tool, args) = resolve_call("leindex_manage", json!({"mode": "ultra"})).unwrap();
        assert_eq!(tool, "leindex_index");
        assert_eq!(args, json!({"mode": "ultra"}));
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
        let text = suggest_unknown_tool("leindex_git_dif").message_with_hint();
        assert!(
            text.contains("leindex_analyze") && text.contains("git_diff"),
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
        assert!(grouped <= 13_000, "tools/list grew to {grouped} bytes");
    }
}
