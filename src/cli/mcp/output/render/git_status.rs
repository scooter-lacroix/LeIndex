//! Git status rendering: summary counts, file lists, changed symbols,
//! impact, and PDG enrichment status. Extracted from the main render module.

use super::*;

fn render_git_summary(data: &Value, color: bool) -> String {
    let Some(summary) = data.get("summary").and_then(|v| v.as_object()) else {
        return String::new();
    };
    let modified = summary
        .get("modified")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let staged = summary.get("staged").and_then(|v| v.as_u64()).unwrap_or(0);
    let untracked = summary
        .get("untracked")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if modified == 0 && staged == 0 && untracked == 0 {
        return String::new();
    }
    let (dim, reset) = if color { (DIM, RESET) } else { ("", "") };
    format!("  {dim}{modified} modified, {staged} staged, {untracked} untracked{reset}\n")
}

fn git_changed_symbol_icon(symbol_type: &str, color: bool) -> (&'static str, &'static str) {
    if !color {
        return ("•", "");
    }
    match symbol_type {
        "function" | "fn" => ("ƒ", LIGHT_GREEN),
        "method" => ("m", LIGHT_CYAN),
        "struct" => ("S", LIGHT_MAGENTA),
        _ => ("•", WHITE),
    }
}

fn render_git_changed_symbols(data: &Value, color: bool) -> String {
    let Some(entries) = data.get("changed_symbols").and_then(|v| v.as_array()) else {
        return String::new();
    };
    if entries.is_empty() {
        return String::new();
    }
    let (bold, yellow, cyan, dim, reset) = if color {
        (BOLD, LIGHT_YELLOW, LIGHT_CYAN, DIM, RESET)
    } else {
        ("", "", "", "", "")
    };
    let mut out = format!("\n  {bold}Changed Symbols:{reset}\n");
    for entry in entries.iter().take(20) {
        let file = entry.get("file").and_then(|v| v.as_str()).unwrap_or("?");
        let status = entry
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("modified");
        let symbols = entry
            .get("symbols")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        out.push_str(&format!(
            "    {yellow}{file}{reset} {dim}({status}){reset}\n"
        ));
        for symbol in symbols.iter().take(10) {
            let name = symbol.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let symbol_type = symbol
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("symbol");
            let caller_count = symbol
                .get("caller_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let impact = symbol
                .get("forward_impact_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let (icon, icon_color) = git_changed_symbol_icon(symbol_type, color);
            out.push_str(&format!(
                "      {icon_color}{icon}{reset} {cyan}{name}{reset} {dim}{caller_count} callers, {impact} impact{reset}\n"
            ));
        }
        if symbols.is_empty() {
            out.push_str(&format!("      {dim}No indexed symbols{reset}\n"));
        }
    }
    out
}

fn render_git_impact_summary(data: &Value, color: bool) -> String {
    let Some(impact) = data.get("impact_summary").and_then(|v| v.as_object()) else {
        return String::new();
    };
    let total = impact
        .get("total_affected_symbols")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if total == 0 {
        return String::new();
    }
    let affected = impact
        .get("affected_files")
        .and_then(|v| v.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    let (bold, reset) = if color { (BOLD, RESET) } else { ("", "") };
    format!("\n  {bold}Impact:{reset} {total} affected symbols across {affected} files\n")
}

fn render_git_pdg_enrichment(data: &Value, color: bool) -> String {
    let Some(pdg) = data.get("pdg_enrichment") else {
        return String::new();
    };
    let available = pdg
        .get("available")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let (icon, icon_color, status) = if available {
        ("✓", LIGHT_GREEN, "available")
    } else {
        ("⚠", LIGHT_YELLOW, "unavailable")
    };
    let (bold, dim, reset, displayed_icon_color) = if color {
        (BOLD, DIM, RESET, icon_color)
    } else {
        ("", "", "", "")
    };
    let mut out =
        format!("\n  {bold}PDG Enrichment:{reset} {displayed_icon_color}{icon}{reset} {status}\n");
    if !available {
        if let Some(reason) = pdg.get("reason").and_then(|v| v.as_str()) {
            out.push_str(&format!("    {dim}Reason:{reset} {reason}\n"));
        }
    }
    out
}

pub(super) fn render_git_status(data: &Value, color: bool) -> String {
    let mut out = header("Git Status", color);
    out.push('\n');
    if let Some(b) = data.get("branch").and_then(|v| v.as_str()) {
        out.push_str(&field("Branch", b, color));
    }

    // Summary counts, changed symbols, impact, and PDG enrichment are each
    // rendered by dedicated helpers (which return empty strings when the
    // corresponding section is absent, including their own leading newlines).
    out.push_str(&render_git_summary(data, color));

    // File lists — handler returns string arrays under
    // modified_files / staged_files / untracked_files
    git_status_file_list(
        data,
        "modified_files",
        "Modified",
        "~",
        LIGHT_YELLOW,
        color,
        &mut out,
    );
    git_status_file_list(
        data,
        "staged_files",
        "Staged",
        "+",
        LIGHT_GREEN,
        color,
        &mut out,
    );
    git_status_file_list(
        data,
        "untracked_files",
        "Untracked",
        "?",
        LIGHT_GREY,
        color,
        &mut out,
    );

    out.push_str(&render_git_changed_symbols(data, color));
    out.push_str(&render_git_impact_summary(data, color));
    out.push_str(&render_git_pdg_enrichment(data, color));

    out
}

/// Render a file list section for git status. The handler returns
/// arrays of strings (file paths), not objects with a `path` field.
fn git_status_file_list(
    data: &Value,
    key: &str,
    label: &str,
    marker: &str,
    marker_color: &str,
    color: bool,
    out: &mut String,
) {
    if let Some(arr) = data.get(key).and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push('\n');
            out.push_str(&format!("  {} ({}):\n", label, arr.len(),));
            for f in arr.iter().take(30) {
                // Files are plain strings in the handler output
                let path = f
                    .as_str()
                    .unwrap_or_else(|| f.get("path").and_then(|v| v.as_str()).unwrap_or("?"));
                out.push_str(&format!(
                    "    {}{}{} {}\n",
                    if color { marker_color } else { "" },
                    marker,
                    if color { RESET } else { "" },
                    path,
                ));
            }
            if arr.len() > 30 {
                out.push_str(&format!(
                    "    {}… {} more{}\n",
                    if color { DIM } else { "" },
                    arr.len() - 30,
                    if color { RESET } else { "" },
                ));
            }
        }
    }
}
