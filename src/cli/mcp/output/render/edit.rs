//! Edit-preview / edit-apply / rename rendering.
//!
//! Kept out of `render/mod.rs` so that file stays under the repository's
//! line budget. All functions here are `pub(super)` — `render/mod.rs`
//! re-exports them for the CLI and test surfaces.

use crate::cli::mcp::output::diff::render_diff_value;
use crate::cli::mcp::output::{
    BOLD, DIM, LIGHT_GREEN, LIGHT_RED, LIGHT_YELLOW, RESET, truncate_chars,
};
use serde_json::Value;

use super::field;

fn render_meta_line(label: &str, value: &str, color: bool) -> String {
    format!(
        "  {}{}:{} {}",
        if color { BOLD } else { "" },
        label,
        if color { RESET } else { "" },
        value,
    )
}

fn append_meta_lines(mut out: String, meta_lines: &[String]) -> String {
    if meta_lines.is_empty() {
        return out;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&meta_lines.join("\n"));
    out.push('\n');
    out
}

/// Render edit-preview output: `preview_token` on the FIRST line (§3.4 —
/// the apply-by-token path starts from the text surface), then the unified
/// diff, then metadata. §3.6: the no-information `Risk level: low` /
/// `Change count: N` lines are gone; a `risk` line appears only when the
/// handler computed a real finding, carrying its reason.
pub(super) fn render_edit_preview(data: &Value, color: bool) -> String {
    let mut out = String::new();
    if let Some(token) = data.get("preview_token").and_then(|v| v.as_str()) {
        out.push_str(&format!(
            "{}preview_token: {token}{} — apply with preview_token + project_path\n",
            if color { LIGHT_GREEN } else { "" },
            if color { RESET } else { "" },
        ));
    }

    let mut meta_lines = Vec::new();
    if let Some(symbols) = data.get("affected_symbols").and_then(|v| v.as_array())
        && !symbols.is_empty()
    {
        let names: Vec<&str> = symbols.iter().filter_map(|v| v.as_str()).collect();
        meta_lines.push(render_meta_line(
            "Affected symbols",
            &names.join(", "),
            color,
        ));
    }
    if let Some(files) = data.get("affected_files").and_then(|v| v.as_array())
        && !files.is_empty()
    {
        let names: Vec<&str> = files.iter().filter_map(|v| v.as_str()).collect();
        meta_lines.push(render_meta_line("Affected files", &names.join(", "), color));
    }
    // §3.6: real-signal risk only — the handler emits `risk` solely when a
    // finding exists, and its reason already names the file.
    if let Some(risk) = data.get("risk") {
        let level = risk.get("level").and_then(|v| v.as_str()).unwrap_or("?");
        let reason = risk.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        meta_lines.push(render_meta_line(
            "risk",
            &format!("{level} — {reason}"),
            color,
        ));
    }
    if let Some(breaks) = data.get("breaking_changes").and_then(|v| v.as_array()) {
        for description in breaks.iter().filter_map(|value| value.as_str()) {
            // The literal "none" entry is a no-information line (§3.6).
            if description == "none" {
                continue;
            }
            meta_lines.push(render_meta_line("Breaking", description, color));
        }
    }

    // §3.6: the requested validation outcome renders when the handler put one
    // in the payload — a single summary line plus up to two concrete
    // failures per category, enough to see WHY a preview is unsafe without
    // dumping the raw validator report.
    if let Some(validation) = data.get("validation") {
        let is_valid = validation
            .get("is_valid")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut details: Vec<String> = Vec::new();
        for (key, label) in [
            ("syntax_errors", "syntax"),
            ("reference_issues", "reference"),
            ("semantic_drift", "semantic"),
        ] {
            if let Some(items) = validation.get(key).and_then(|v| v.as_array()) {
                for item in items.iter().take(2) {
                    let msg = item.get("message").and_then(|v| v.as_str()).unwrap_or("");
                    details.push(format!("{label}: {msg}"));
                }
            }
        }
        if is_valid && details.is_empty() {
            meta_lines.push(render_meta_line("Validation", "valid", color));
        } else {
            for detail in details {
                meta_lines.push(render_meta_line("Validation", &detail, color));
            }
        }
    }

    out.push_str(&append_meta_lines(
        render_diff_value(data, color),
        &meta_lines,
    ));
    out
}

/// Render rename-symbol output: multi-file diffs followed by metadata
/// (old_name, new_name, files_affected, preview_only).
pub(super) fn render_rename_symbol(data: &Value, color: bool) -> String {
    let mut meta_lines = Vec::new();

    if let (Some(old), Some(new)) = (
        data.get("old_name").and_then(|v| v.as_str()),
        data.get("new_name").and_then(|v| v.as_str()),
    ) {
        meta_lines.push(render_meta_line("Rename", &format!("{old} → {new}"), color));
    }
    if let Some(count) = data.get("files_affected").and_then(|v| v.as_u64()) {
        meta_lines.push(render_meta_line(
            "Files affected",
            &count.to_string(),
            color,
        ));
    }
    if let Some(diffs_more) = data
        .get("diffs_more")
        .and_then(|v| v.as_u64())
        .filter(|count| *count > 0)
    {
        meta_lines.push(render_meta_line(
            "Additional diffs",
            &format!("{diffs_more} more (not shown)"),
            color,
        ));
    }
    if data
        .get("preview_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        meta_lines.push(render_meta_line(
            "Preview only",
            "changes not applied",
            color,
        ));
    }

    append_meta_lines(render_diff_value(data, color), &meta_lines)
}

pub(super) fn render_edit_region(region_value: &Value, color: bool) -> String {
    // `edit_region` may be a source excerpt or a byte-range object retained by `trim_edit`.
    let region_text = if let Some(s) = region_value.as_str() {
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    } else if let Some(obj) = region_value.as_object() {
        let start = obj.get("start").and_then(|v| v.as_u64());
        let end = obj.get("end").and_then(|v| v.as_u64());
        match (start, end) {
            (Some(s), Some(e)) => Some(format!("bytes {s}..{e}")),
            (Some(s), None) => Some(format!("bytes {s}..")),
            (None, Some(e)) => Some(format!("bytes ..{e}")),
            (None, None) => Some("bytes ?".to_string()),
        }
    } else {
        None
    };
    let Some(text) = region_text else {
        return String::new();
    };

    let mut out = String::from("\n");
    if region_value.is_string() {
        out.push_str(&format!(
            "  {}Surrounding region:{}\n",
            if color { DIM } else { "" },
            if color { RESET } else { "" },
        ));
        for line in text.lines() {
            out.push_str(&format!(
                "      {}{}{}\n",
                if color { DIM } else { "" },
                truncate_chars(line, 160),
                if color { RESET } else { "" },
            ));
        }
    } else {
        out.push_str(&format!(
            "  {}Surrounding region:{} {}\n",
            if color { DIM } else { "" },
            if color { RESET } else { "" },
            text,
        ));
    }
    out
}

pub(super) fn render_edit_apply(data: &Value, color: bool) -> String {
    // `EditApplyHandler` returns a confirmation payload with shape
    // (see `src/cli/mcp/edit_apply_handler.rs::EditApplyHandler::
    // execute`):
    //   { success, changes_applied, file_path, edit_region,
    //     affected_symbols, affected_files, breaking_changes,
    //     [validation], [message] (no-op only) }
    // The handler never emits `diff` / `diffs` / `diff_text`, so
    // `render_diff_value` returns an empty string for the apply
    // response and the CLI prints nothing for a successful (or
    // no-op) apply. Surface the success status, the change count,
    // the file path, the affected-symbol/file summary, breaking
    // changes, and the surrounding-region excerpt.
    let mut out = String::new();
    let success = data
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let is_noop = data
        .get("changes_applied")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        == 0;
    // With `applied_lines` present the one-line echo already says
    // "Applied: …" — a separate status label would only duplicate it. The
    // label remains for failure / dry-run / no-op shapes without the echo.
    let has_echo = data.get("applied_lines").is_some();
    if !has_echo || !success || is_noop {
        let (status_label, status_color) = edit_apply_status(success, data, color);
        out.push_str(&format!(
            "{}{}{}\n",
            status_color,
            status_label,
            if color { RESET } else { "" },
        ));
    }
    // §4.3: the apply result is ONE line per file —
    // `Applied: path:L802-828 (+27/-0) in fn name`. The handler computes
    // it (`applied_lines`); the field block below remains only for shapes
    // without it (legacy payloads, no-op confirmations).
    if let Some(lines) = data.get("applied_lines").and_then(|v| v.as_array()) {
        for line in lines.iter().filter_map(|v| v.as_str()) {
            out.push_str(&format!(
                "  {}{}{}\n",
                if color { LIGHT_GREEN } else { "" },
                line,
                if color { RESET } else { "" },
            ));
        }
        // §3.6: requested post-apply validation echoes (`validate:"syntax"`
        // / "check"`) ride on every changed-file response. They must be
        // surfaced BEFORE the early return, or a validated edit that failed
        // its own check renders identically to a clean one (Codex round 1,
        // id 4653371664).
        render_validation_echoes(&mut out, data);
        return out;
    }
    if let Some(path) = data.get("file_path").and_then(|v| v.as_str()) {
        out.push_str(&field("File", path, color));
    }
    if let Some(msg) = data.get("message").and_then(|v| v.as_str()) {
        out.push_str(&field("Message", msg, color));
    }
    if let Some(arr) = data.get("affected_symbols").and_then(|v| v.as_array())
        && !arr.is_empty()
    {
        out.push_str(&field("Affected symbols", &arr.len().to_string(), color));
    }
    if let Some(arr) = data.get("affected_files").and_then(|v| v.as_array())
        && !arr.is_empty()
    {
        out.push_str(&field("Affected files", &arr.len().to_string(), color));
    }
    if let Some(bc) = data.get("breaking_changes").and_then(|v| v.as_array())
        && !bc.is_empty()
    {
        out.push_str(&field("Breaking changes", &bc.len().to_string(), color));
    }
    out.push_str(
        &data
            .get("edit_region")
            .map_or_else(String::new, |region| render_edit_region(region, color)),
    );
    out
}

/// Render the post-apply validation echoes (`validate:"syntax"` /
/// `validate:"check"`): one line per edited file's syntax outcome, then one
/// line per scoped-check report. Present only when the caller requested them,
/// so a missing field renders nothing.
fn render_validation_echoes(out: &mut String, data: &Value) {
    if let Some(entries) = data.get("syntax").and_then(Value::as_array) {
        for entry in entries {
            // `attach_syntax_validation` emits `{ path, summary, errors }`;
            // keying on that keeps the renderer correct against the handler's
            // actual payload (Codex round 9, id 6095...).
            let Some(path) = entry.get("path").and_then(Value::as_str) else {
                continue;
            };
            let summary = entry.get("summary").and_then(Value::as_str).unwrap_or("");
            out.push_str(&format!("  Syntax {path}: {summary}\n"));
        }
    }
    if let Some(check) = data.get("check") {
        let Some(files) = check.get("files").and_then(Value::as_array) else {
            return;
        };
        for entry in files {
            let Some(path) = entry.get("file").and_then(Value::as_str) else {
                continue;
            };
            let report = match entry.get("error").and_then(Value::as_str) {
                Some(error) => format!("check failed: {error}"),
                None => match (
                    entry.get("headline").and_then(Value::as_str),
                    entry.get("error_count"),
                ) {
                    (Some(headline), Some(count)) => {
                        format!("{headline} ({count} errors)")
                    }
                    (Some(headline), None) => headline.to_string(),
                    _ => "checked".to_string(),
                },
            };
            out.push_str(&format!("  Check {path}: {report}\n"));
        }
    }
}

/// Pick the first status line of the edit-apply render: failure, dry-run
/// preview, no-op, or successful apply. A dry run always reports zero
/// applied changes by design; labeling it "No-op (content identical)"
/// misdescribes a real preview.
fn edit_apply_status(success: bool, data: &Value, color: bool) -> (&'static str, &'static str) {
    if !success {
        ("Edit apply failed", if color { LIGHT_RED } else { "" })
    } else if data.get("dry_run").and_then(|v| v.as_bool()) == Some(true) {
        (
            "Dry run (no changes written)",
            if color { LIGHT_YELLOW } else { "" },
        )
    } else if data
        .get("changes_applied")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        == 0
    {
        (
            "No-op (content identical)",
            if color { LIGHT_YELLOW } else { "" },
        )
    } else {
        ("Applied", if color { LIGHT_GREEN } else { "" })
    }
}
