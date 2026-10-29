use super::*;

pub(super) fn render_symbol_lookup(data: &Value, color: bool) -> String {
    // `lookup_single_symbol` returns the shape (see
    // `src/cli/mcp/symbol_lookup_handler.rs::lookup_single_symbol`):
    //   { symbol, type, file, byte_range, complexity, language,
    //     callers, callees, impact_radius, [source] }
    // where each caller/callee entry is { name, file, type } (no
    // `line` field). Older renderers read `file_path` / `line` /
    // `symbol_type` / `signature` and end up emitting mostly blanks.
    //
    // Batch mode (`lookup_symbols_batch`) returns the wrapper
    //   { batch: true, count, results: [ ...singleSymbolEntries ] }
    // The previous renderer silently dropped the wrapper and
    // emitted a header followed by nothing when `symbol` /
    // `file` / `type` were absent at the top level. Branch on
    // `batch:true` and recurse into each entry.
    if data.get("batch").and_then(|v| v.as_bool()) == Some(true) {
        let count = data.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
        let mut out = header("Symbol Lookup (batch)", color);
        out.push('\n');
        out.push_str(&field("Count", &count.to_string(), color));
        if let Some(arr) = data.get("results").and_then(|v| v.as_array()) {
            if arr.is_empty() {
                out.push_str("  (no results)\n");
                return out;
            }
            for (idx, entry) in arr.iter().enumerate() {
                out.push('\n');
                out.push_str(&format!(
                    "  {}{}#{}{} {}\n",
                    if color { BOLD } else { "" },
                    if color { DIM } else { "" },
                    idx + 1,
                    if color { RESET } else { "" },
                    entry.get("symbol").and_then(|v| v.as_str()).unwrap_or("?"),
                ));
                out.push_str(&render_symbol_lookup_single(entry, color));
            }
        }
        return out;
    }
    let mut out = header("Symbol Lookup", color);
    out.push('\n');
    out.push_str(&render_symbol_lookup_single(data, color));
    out
}

/// Render a single symbol entry (the inner shape returned by
/// `lookup_single_symbol`). Lifted out of `render_symbol_lookup` so
/// the batch wrapper can recurse into each entry.
pub(super) fn render_symbol_source(data: &Value, color: bool) -> String {
    let Some(source) = data.get("source").and_then(|v| v.as_str()) else {
        return String::new();
    };

    let mut out = String::from("\n");
    let mut shown = 0usize;
    for line in source.lines() {
        if line.trim().is_empty() {
            continue;
        }
        out.push_str(&format!(
            "      {}{}{}\n",
            if color { DIM } else { "" },
            truncate_chars(line, 160),
            if color { RESET } else { "" },
        ));
        shown += 1;
        if shown >= 12 {
            break;
        }
    }
    out
}

pub(super) fn render_symbol_relationships(
    data: &Value,
    key: &str,
    truncated_key: &str,
    title: &str,
    arrow: &str,
    color: bool,
) -> String {
    let Some(entries) = data.get(key).and_then(|v| v.as_array()) else {
        return String::new();
    };
    if entries.is_empty() {
        return String::new();
    }

    let mut out = format!("\n  {title}:\n");
    for entry in entries.iter().take(50) {
        let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let file = entry.get("file").and_then(|v| v.as_str()).unwrap_or("");
        let typ = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!(
            "    {arrow} {}{}{} {}{}{}{}\n",
            if color { LIGHT_CYAN } else { "" },
            name,
            if color { RESET } else { "" },
            if color { DIM } else { "" },
            file,
            if typ.is_empty() {
                String::new()
            } else {
                format!(" [{}]", typ)
            },
            if color { RESET } else { "" },
        ));
    }
    if data
        .get(truncated_key)
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        out.push_str(&format!(
            "    {}... (showing 50 or more){}\n",
            if color { DIM } else { "" },
            if color { RESET } else { "" },
        ));
    }
    out
}

pub(super) fn render_symbol_lookup_single(data: &Value, color: bool) -> String {
    let mut out = String::new();
    if let Some(sym) = data.get("symbol").and_then(|v| v.as_str()) {
        out.push_str(&field("Symbol", sym, color));
    }
    if let Some(file) = data.get("file").and_then(|v| v.as_str()) {
        out.push_str(&field("File", file, color));
    }
    if let Some(typ) = data.get("type").and_then(|v| v.as_str()) {
        out.push_str(&field("Type", typ, color));
    }
    if let Some(lang) = data.get("language").and_then(|v| v.as_str()) {
        out.push_str(&field("Language", lang, color));
    }
    if let Some(br) = data.get("byte_range").and_then(|v| v.as_array()) {
        if br.len() == 2 {
            let start = br[0].as_u64().unwrap_or(0);
            let end = br[1].as_u64().unwrap_or(0);
            if end > start {
                out.push_str(&field("Range", &format!("bytes {}-{}", start, end), color));
            }
        }
    }
    if let Some(cx) = data.get("complexity").and_then(|v| v.as_u64()) {
        out.push_str(&field("Complexity", &cx.to_string(), color));
    }
    if let Some(ir) = data.get("impact_radius").and_then(|v| v.as_object()) {
        let syms = ir
            .get("affected_symbols")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let files = ir
            .get("affected_files")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        out.push_str(&field(
            "Impact",
            &format!("{} symbols / {} files", syms, files),
            color,
        ));
    }
    // N-15 honest degradation: a zero-impact figure from a stale index or a
    // degraded graph must not read as fact.
    if let Some(note) = data.get("impact_note").and_then(|v| v.as_str()) {
        out.push_str(&field("Note", &format!("⚠ {note}"), color));
    }
    if let Some(freshness) = data.get("index_freshness").and_then(|v| v.as_str()) {
        if freshness != "fresh" {
            out.push_str(&field("Index freshness", freshness, color));
        }
    }
    out.push_str(&render_symbol_source(data, color));

    out.push_str(&render_symbol_relationships(
        data,
        "callers",
        "callers_truncated",
        "Callers",
        "→",
        color,
    ));
    out.push_str(&render_symbol_relationships(
        data,
        "callees",
        "callees_truncated",
        "Callees",
        "←",
        color,
    ));
    out
}

pub(super) fn render_phase_section(data: &Value, phase: u8, color: bool) -> String {
    let key = format!("phase{phase}");
    let Some(value) = data.get(&key) else {
        return String::new();
    };
    let (bold, dim, reset) = if color {
        (BOLD, DIM, RESET)
    } else {
        ("", "", "")
    };
    match phase {
        1 => {
            let files = value
                .get("total_files")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let signatures = value
                .get("signatures")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let label = if value
                .get("cache_hit")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                "cache hit"
            } else {
                "parsed"
            };
            format!(
                "\n  {bold}Phase 1:{reset} {files} files, {signatures} signatures ({label}){reset}\n"
            )
        }
        2 => {
            let internal = value
                .get("internal_import_edges")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let external = value
                .get("external_import_edges")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let unresolved = value
                .get("unresolved_modules")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            format!(
                "  {bold}Phase 2:{reset} {internal} internal, {external} external, {unresolved} unresolved modules{reset}\n"
            )
        }
        3 => {
            let entries = value
                .get("entry_points")
                .and_then(|v| v.as_array())
                .map(|items| items.len())
                .unwrap_or(0);
            let impacted = value
                .get("impacted_nodes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            format!(
                "  {bold}Phase 3:{reset} {entries} entry points, {impacted} impacted nodes{reset}\n"
            )
        }
        4 => {
            let hotspots = value
                .get("hotspots")
                .and_then(|v| v.as_array())
                .map(Vec::len)
                .unwrap_or(0);
            let mut out = format!("  {bold}Phase 4:{reset} {hotspots} hotspots{reset}\n");
            if let Some(items) = value.get("hotspots").and_then(|v| v.as_array()) {
                for (index, hotspot) in items.iter().take(5).enumerate() {
                    let name = hotspot
                        .get("node_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let score = hotspot.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let complexity = hotspot
                        .get("complexity")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    out.push_str(&format!(
                        "    {}. {name} {dim}(score: {score:.2}, complexity: {complexity}){reset}\n",
                        index + 1,
                    ));
                }
            }
            out
        }
        5 => {
            let recommendations = value
                .get("recommendations")
                .and_then(|v| v.as_array())
                .map(Vec::len)
                .unwrap_or(0);
            format!("  {bold}Phase 5:{reset} {recommendations} recommendations{reset}\n")
        }
        _ => String::new(),
    }
}

pub(super) fn render_phase_formatted(data: &Value, color: bool) -> String {
    let Some(formatted) = data.get("formatted_output").and_then(|v| v.as_str()) else {
        return String::new();
    };
    if formatted.is_empty() {
        return String::new();
    }
    let (dim, reset) = if color { (DIM, RESET) } else { ("", "") };
    let mut out = String::from("\n");
    for line in truncate_chars(formatted, 2000).lines() {
        out.push_str(&format!("  {dim}{line}{reset}\n"));
    }
    out
}

pub(super) fn render_phase(data: &Value, color: bool) -> String {
    let mut out = header("Phase Analysis", color);
    out.push('\n');

    // Show executed phases
    if let Some(ep) = data.get("executed_phases").and_then(|v| v.as_array()) {
        let nums: Vec<String> = ep
            .iter()
            .filter_map(|v| v.as_u64().map(|n| n.to_string()))
            .collect();
        if !nums.is_empty() {
            out.push_str(&field("Executed phases", &nums.join(", "), color));
        }
    }

    // Show cache hit status
    if let Some(ch) = data.get("cache_hit").and_then(|v| v.as_bool()) {
        out.push_str(&field(
            "Cache hit",
            if ch { "true" } else { "false" },
            color,
        ));
    }

    // Show the analysis fingerprint (a content hash over the analyzed
    // inventory — distinct from the store's generation counter in the
    // freshness footer; N-07).
    if let Some(fingerprint) = data
        .get("analysis_fingerprint")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("generation").and_then(|v| v.as_str()))
    {
        out.push_str(&field("Analysis fingerprint", fingerprint, color));
    }

    out.push_str(&render_phase_section(data, 1, color));

    out.push_str(&render_phase_section(data, 2, color));

    out.push_str(&render_phase_section(data, 3, color));

    out.push_str(&render_phase_section(data, 4, color));

    out.push_str(&render_phase_section(data, 5, color));
    out.push_str(&render_phase_formatted(data, color));

    out
}
