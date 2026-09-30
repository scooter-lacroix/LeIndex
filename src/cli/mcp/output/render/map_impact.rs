use super::*;

pub(super) fn render_project_map(data: &Value, color: bool) -> String {
    let mut out = header("Project Structure", color);
    out.push('\n');
    if data.get("group_by").and_then(|v| v.as_str()) == Some("community") {
        out.push_str(&render_community_groups(data, color));
    } else if let Some(tree) = data.get("tree").and_then(|v| v.as_array()) {
        out.push_str(&render_tree(tree, color));
    } else if let Some(roots) = data.get("root").map(|v| vec![v.clone()]) {
        out.push_str(&render_tree(&roots, color));
    } else if let Some(files) = data.get("files").and_then(|v| v.as_array()) {
        let tree = build_tree_from_files(files);
        if tree.is_empty() {
            // No directory info is available in the file entries (the
            // handler ships basenames, not full paths), so render a flat
            // ranked list rather than fabricating fake directories.
            out.push_str(&render_flat_files(files, color));
        } else {
            out.push_str(&render_tree(&tree, color));
        }
    }
    if let Some(stats) = data.get("stats") {
        out.push('\n');
        if let Some(v) = stats.get("total_files").and_then(|v| v.as_u64()) {
            out.push_str(&field("Files", &v.to_string(), color));
        }
        if let Some(v) = stats.get("total_symbols").and_then(|v| v.as_u64()) {
            out.push_str(&field("Symbols", &v.to_string(), color));
        }
        if let Some(v) = stats.get("avg_complexity").and_then(|v| v.as_f64()) {
            out.push_str(&field("Avg complexity", &format!("{:.1}", v), color));
        }
        if let Some(v) = stats.get("total_loc").and_then(|v| v.as_u64()) {
            out.push_str(&field("Lines of code", &v.to_string(), color));
        }
    }
    // Also show total_files_in_scope from the handler output
    // (the handler puts this at top level, not under "stats")
    let scoped = data
        .get("total_files_in_scope")
        .and_then(|v| v.as_u64())
        .is_some_and(|count| count > 0);
    if let Some(v) = data.get("total_files_in_scope").and_then(|v| v.as_u64()) {
        if data.get("stats").is_none() {
            out.push('\n');
        }
        // N-14: state the count basis — "Files in scope" counts source files
        // under the scoped path, which is deliberately different from the
        // indexed-file total shown by diagnostics (skip lists, exclusions,
        // and scoping all apply).
        out.push_str(&field(
            "Files in scope",
            &format!("{} (source files under the scoped path)", v),
            color,
        ));
    }
    // N-14: one legend line for the bracket annotations — `[out→in]` is
    // outgoing→incoming dependency counts and `[N symbols]` is the file's
    // indexed symbol count. Previously nowhere documented.
    if scoped || data.get("tree").is_some() || data.get("root").is_some() {
        out.push_str(&format!(
            "\n  {}Legend: [N symbols] = indexed symbol count; [out→in] = outgoing→incoming dependencies{}\n",
            if color { DIM } else { "" },
            if color { RESET } else { "" },
        ));
    }
    out
}

pub(super) fn render_community_groups(data: &Value, color: bool) -> String {
    let Some(communities) = data.get("communities").and_then(|v| v.as_array()) else {
        return "  (no community assignments)\n".to_string();
    };
    if communities.is_empty() {
        let note = data
            .get("note")
            .and_then(|v| v.as_str())
            .map(|value| format!(" — {value}"))
            .unwrap_or_default();
        return format!("  (no communities{note})\n");
    }

    let mut out = String::new();
    out.push_str(&format!(
        "  {}Community groups ({}):{}\n",
        if color { BOLD } else { "" },
        communities.len(),
        if color { RESET } else { "" },
    ));
    for community in communities {
        let id = community
            .get("community")
            .and_then(|v| v.as_i64())
            .map(|value| value.to_string())
            .unwrap_or_else(|| "?".to_string());
        let label = community
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("unlabeled");
        let file_count = community
            .get("file_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        out.push_str(&format!(
            "\n    {}Community {}{} — {} ({} files){}\n",
            if color { LIGHT_MAGENTA } else { "" },
            id,
            if color { RESET } else { "" },
            label,
            file_count,
            if color { RESET } else { "" },
        ));
        if let Some(files) = community.get("files").and_then(|v| v.as_array()) {
            for file in files {
                let path = file
                    .get("relative_path")
                    .and_then(|v| v.as_str())
                    .or_else(|| file.get("path").and_then(|v| v.as_str()))
                    .unwrap_or("?");
                let symbols = file
                    .get("symbol_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                out.push_str(&format!(
                    "      {}•{} {}{}{}  [{} symbols]\n",
                    if color { LIGHT_YELLOW } else { "" },
                    if color { RESET } else { "" },
                    if color { LIGHT_YELLOW } else { "" },
                    path,
                    if color { RESET } else { "" },
                    symbols,
                ));
            }
        }
    }
    if let Some(files) = data.get("ungrouped_files").and_then(|v| v.as_array()) {
        if !files.is_empty() {
            out.push_str(&format!(
                "\n    {}Ungrouped ({} files):{}\n",
                if color { BOLD } else { "" },
                files.len(),
                if color { RESET } else { "" },
            ));
            for file in files {
                let path = file
                    .get("relative_path")
                    .and_then(|v| v.as_str())
                    .or_else(|| file.get("path").and_then(|v| v.as_str()))
                    .unwrap_or("?");
                out.push_str(&format!("      • {path}\n"));
            }
        }
    }
    out
}

pub(super) fn render_flat_files(files: &[Value], color: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  {}(flat list — files ordered as returned){}\n",
        if color { DIM } else { "" },
        if color { RESET } else { "" },
    ));
    for (i, f) in files.iter().enumerate() {
        let path = f
            .get("path")
            .and_then(|v| v.as_str())
            .or_else(|| f.get("relative_path").and_then(|v| v.as_str()))
            .unwrap_or("?");
        let syms = f.get("symbol_count").and_then(|v| v.as_u64()).unwrap_or(0);
        let cx = f
            .get("total_complexity")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let deps = f
            .get("incoming_dependencies")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            + f.get("outgoing_dependencies")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
        // The complexity value is shown as a colour-coded integer
        // (`cx:{N}`); the human-readable label ("low" / "med" /
        // "high") was previously computed alongside the colour but
        // never rendered, so simplify the conditional to just the
        // colour string. When colour is disabled the colour string
        // is the empty literal.
        let cx_color = if color {
            match cx {
                0..=20 => LIGHT_GREEN,
                21..=60 => LIGHT_YELLOW,
                _ => LIGHT_RED,
            }
        } else {
            ""
        };
        out.push_str(&format!(
            "  {}{:>3}.{} {}{}{}  {}{} sym  cx:{}{}{}  deps:{}\n",
            if color { BOLD } else { "" },
            i + 1,
            if color { RESET } else { "" },
            if color { LIGHT_YELLOW } else { "" },
            path,
            if color { RESET } else { "" },
            if color { DIM } else { "" },
            syms,
            cx_color,
            cx,
            if color { RESET } else { "" },
            deps,
        ));
    }
    out
}

/// Convert a flat list of `{path, relative_path, symbol_count, ...}`
/// entries into a nested directory tree suitable for `render_tree`.
/// Returns an empty Vec if the file entries don't carry directory
/// information (caller falls back to flat rendering).
pub(super) fn build_tree_from_files(files: &[Value]) -> Vec<Value> {
    use std::collections::BTreeMap;

    // Bail out unless at least one entry has a path with a directory
    // separator — otherwise we'd fabricate a meaningless single-level
    // tree from basenames.
    let any_with_dir = files.iter().any(|f| {
        f.get("relative_path")
            .and_then(|v| v.as_str())
            .or_else(|| f.get("path").and_then(|v| v.as_str()))
            .map(|p| p.contains('/') || p.contains('\\'))
            .unwrap_or(false)
    });
    if !any_with_dir {
        return Vec::new();
    }

    // A `Node` here is a tiny tree of name -> (entry, children).
    struct Node {
        entry: Option<Value>,
        children: BTreeMap<String, Node>,
    }

    impl Node {
        fn new() -> Self {
            Self {
                entry: None,
                children: BTreeMap::new(),
            }
        }
        /// Convert a directory node to a `{name, type, children}` JSON
        /// value. File nodes pass through their entry. Each child is
        /// converted using its own key as the name so nested directory
        /// labels stay distinct instead of inheriting the parent's
        /// segment.
        fn into_value(self, name: &str) -> Value {
            let children: Vec<Value> = self
                .children
                .into_iter()
                .map(|(child_name, child)| child.into_value(&child_name))
                .collect();
            if let Some(mut entry) = self.entry {
                if let Some(obj) = entry.as_object_mut() {
                    if !children.is_empty() {
                        obj.insert("children".to_string(), Value::Array(children));
                    }
                }
                entry
            } else {
                serde_json::json!({
                    "name": name,
                    "type": "directory",
                    "children": children,
                })
            }
        }
    }

    let mut root = Node::new();
    for file in files {
        let rel = file
            .get("relative_path")
            .and_then(|v| v.as_str())
            .or_else(|| file.get("path").and_then(|v| v.as_str()))
            .unwrap_or("?");
        // Normalize to forward slashes for stable tree building.
        let rel = rel.replace('\\', "/");
        let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
        let mut node = &mut root;
        for (i, part) in parts.iter().enumerate() {
            let key = part.to_string();
            let is_file = i + 1 == parts.len();
            let child = node.children.entry(key.clone()).or_insert_with(Node::new);
            if is_file {
                let mut entry = file.clone();
                if let Some(obj) = entry.as_object_mut() {
                    obj.entry("name".to_string())
                        .or_insert(Value::String((*part).to_string()));
                    obj.entry("type".to_string())
                        .or_insert(Value::String("file".to_string()));
                }
                child.entry = Some(entry);
            }
            node = child;
        }
    }

    // The root is a virtual container with no name of its own — return
    // its top-level children directly so `render_tree` shows them as
    // siblings rather than nested under a fabricated "root" node.
    let mut top: Vec<Value> = Vec::new();
    for (name, child) in root.children.into_iter() {
        top.push(child.into_value(&name));
    }
    top
}

pub(super) fn render_impact_risk(data: &Value, color: bool) -> String {
    let Some(risk) = data.get("risk_level").and_then(|v| v.as_str()) else {
        return String::new();
    };
    let (icon, risk_color) = if color {
        match risk.to_lowercase().as_str() {
            "high" => ("●", LIGHT_RED),
            "medium" => ("●", LIGHT_YELLOW),
            "low" => ("●", LIGHT_GREEN),
            _ => ("○", WHITE),
        }
    } else {
        ("●", "")
    };
    format!(
        "  {}Risk:{} {risk_color}{icon} {risk}{}\n",
        if color { BOLD } else { "" },
        if color { RESET } else { "" },
        if color { RESET } else { "" },
    )
}

pub(super) fn render_impact_list(
    data: &Value,
    key: &str,
    title: &str,
    arrow: &str,
    item_color: &str,
    limit: usize,
    show_more: bool,
    color: bool,
) -> String {
    let Some(items) = data.get(key).and_then(|v| v.as_array()) else {
        return String::new();
    };
    if items.is_empty() {
        return String::new();
    }
    let (bold, dim, reset, line_color) = if color {
        (BOLD, DIM, RESET, item_color)
    } else {
        ("", "", "", "")
    };
    let mut out = format!("\n  {bold}{title} ({}):{reset}\n", items.len());
    for item in items.iter().take(limit) {
        let name = item
            .as_str()
            .or_else(|| item.get("name").and_then(|v| v.as_str()))
            .unwrap_or("?");
        out.push_str(&format!("    {line_color}{arrow} {name}{reset}\n"));
    }
    if show_more && items.len() > limit {
        out.push_str(&format!("    {dim}… {} more{reset}\n", items.len() - limit));
    }
    out
}

pub(super) fn render_impact_communities(data: &Value, color: bool) -> String {
    let Some(breakdown) = data.get("community_breakdown") else {
        return String::new();
    };
    if breakdown.is_null() {
        return String::new();
    }
    let same = breakdown
        .get("same_community")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let crossing = breakdown
        .get("crossing")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let mut out = String::from("\n");
    out.push_str(&format!(
        "  {}Community boundaries:{}\n",
        if color { BOLD } else { "" },
        if color { RESET } else { "" },
    ));
    out.push_str(&format!("    Same community: {same}\n"));
    out.push_str(&format!("    Crossing communities: {crossing}\n"));
    if let Some(boundaries) = breakdown.get("boundaries").and_then(|v| v.as_array()) {
        for boundary in boundaries {
            let from = boundary
                .get("from")
                .and_then(|v| v.as_u64())
                .map(|value| value.to_string())
                .unwrap_or_else(|| "?".to_string());
            let to = boundary
                .get("to")
                .and_then(|v| v.as_u64())
                .map(|value| value.to_string())
                .unwrap_or_else(|| "?".to_string());
            let symbols = boundary
                .get("symbols")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            out.push_str(&format!(
                "    {}{} → {}{}: {} symbols\n",
                if color { LIGHT_MAGENTA } else { "" },
                from,
                to,
                if color { RESET } else { "" },
                symbols,
            ));
        }
    }
    out
}

pub(super) fn render_impact_counts(data: &Value, color: bool) -> String {
    let affected_files = data
        .get("transitive_affected_files")
        .and_then(|v| v.as_u64());
    let transitive_callers = data.get("transitive_callers").and_then(|v| v.as_u64());
    if affected_files.is_none() && transitive_callers.is_none() {
        return String::new();
    }

    let mut out = String::from("\n");
    if let Some(count) = affected_files {
        out.push_str(&field("Affected files", &count.to_string(), color));
    }
    if let Some(count) = transitive_callers {
        out.push_str(&field("Transitive callers", &count.to_string(), color));
    }
    out
}

pub(super) fn render_impact(data: &Value, color: bool) -> String {
    let mut out = header("Impact Analysis", color);
    out.push('\n');
    if let Some(sym) = data.get("symbol").and_then(|v| v.as_str()) {
        out.push_str(&field("Symbol", sym, color));
    }
    if let Some(file) = data.get("file").and_then(|v| v.as_str()) {
        out.push_str(&field("File", file, color));
    }
    if let Some(ct) = data.get("change_type").and_then(|v| v.as_str()) {
        out.push_str(&field("Change type", ct, color));
    }
    out.push_str(&render_impact_risk(data, color));

    out.push_str(&render_impact_list(
        data,
        "direct_callers",
        "Direct callers",
        "←",
        LIGHT_CYAN,
        20,
        false,
        color,
    ));

    out.push_str(&render_impact_list(
        data,
        "transitive_affected_symbols",
        "Transitive affected symbols",
        "→",
        LIGHT_YELLOW,
        30,
        true,
        color,
    ));

    // Summary with numeric counts
    if let Some(s) = data.get("summary").and_then(|v| v.as_str()) {
        out.push('\n');
        out.push_str(&format!(
            "  {}Summary:{} {}\n",
            if color { BOLD } else { "" },
            if color { RESET } else { "" },
            s,
        ));
    }

    out.push_str(&render_impact_counts(data, color));
    out.push_str(&render_impact_communities(data, color));

    out
}
