//! `leindex_manage` actions `run` and `how_to_test`: argument handling and
//! JSON shaping only; the work lives in the sibling modules.
//!
//! `run` has four modes, chosen by the first argument present:
//! `log` (read a stored log by handle), `check` (scoped check of a file),
//! `diagnostics` (stored diagnostics for a file), else execute `argv`/`command`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::parsers::Summary;
use super::{diagnostics, how_to_test, run_log, sandbox};

const DEFAULT_TAIL: usize = 200;
const MAX_TAIL: usize = 2000;
const DEFAULT_CHECK_MAX: usize = 10;

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn usize_arg(args: &Value, key: &str) -> Option<usize> {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn summary_json(action: &str, summary: &Summary) -> Value {
    let handle = summary
        .log_path
        .as_deref()
        .and_then(Path::file_stem)
        .and_then(|stem| stem.to_str())
        .map(run_log::handle);
    json!({
        "action": action,
        "summary": summary.render(),
        "exit_code": summary.exit_code,
        "duration_ms": summary.duration_ms,
        "log": handle,
    })
}

/// `action=run`.
pub(crate) async fn run_action(root: &Path, args: &Value) -> Result<Value, String> {
    let storage = diagnostics::storage_for(root);

    if let Some(handle) = str_arg(args, "log") {
        return read_log(&storage, handle, usize_arg(args, "tail"));
    }
    if let Some(file) = str_arg(args, "check") {
        let max = usize_arg(args, "max").unwrap_or(DEFAULT_CHECK_MAX);
        let (root, file) = (root.to_path_buf(), PathBuf::from(file));
        let summary =
            tokio::task::spawn_blocking(move || diagnostics::check_scoped(&root, &file, max))
                .await
                .map_err(|error| format!("check task failed: {error}"))?
                .map_err(|error| error.to_string())?;
        return Ok(summary_json("run", &summary));
    }
    if let Some(file) = str_arg(args, "diagnostics") {
        let from = usize_arg(args, "from").unwrap_or(1);
        let to = usize_arg(args, "to").unwrap_or(usize::MAX);
        let found = diagnostics::diagnostics_for(root, Path::new(file), from..=to);
        return Ok(json!({
            "action": "run",
            "file": file,
            "count": found.len(),
            "diagnostics": found,
            "note": if found.is_empty() {
                Some("no stored diagnostics for this file/range: either the last check was clean for it or none ran; run action=run check=<file> first")
            } else {
                None
            },
        }));
    }

    let argv = match args.get("argv") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .map(|item| item.as_str().map(str::to_string))
                .collect::<Option<Vec<String>>>()
                .ok_or("argv must be an array of strings")?,
        ),
        Some(_) => return Err("argv must be an array of strings".to_string()),
    };
    let cwd = str_arg(args, "cwd").map(PathBuf::from);
    let summary = sandbox::run(sandbox::RunRequest {
        project_root: root,
        storage: &storage,
        argv: argv.as_deref(),
        command: str_arg(args, "command"),
        cwd: cwd.as_deref(),
        timeout_secs: args.get("timeout_secs").and_then(Value::as_u64),
        name: str_arg(args, "name"),
    })
    .await
    .map_err(|error| error.to_string())?;
    Ok(summary_json("run", &summary))
}

fn read_log(storage: &Path, handle: &str, tail: Option<usize>) -> Result<Value, String> {
    let text = run_log::read_by_handle(storage, handle)
        .map_err(|error| format!("cannot read {handle}: {error}"))?;
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let shown = tail.unwrap_or(DEFAULT_TAIL).clamp(1, MAX_TAIL).min(total);
    let start = total - shown;
    Ok(json!({
        "action": "run",
        "log": handle,
        "lines_total": total,
        "lines_shown": format!("{}-{}", start + 1, total),
        "text": lines[start..].join("\n"),
        "note": if start > 0 {
            Some(format!("{start} earlier lines omitted; pass tail=N (max {MAX_TAIL}) for more"))
        } else {
            None
        },
    }))
}

/// `action=how_to_test`.
pub(crate) fn how_to_test_action(root: &Path, args: &Value) -> Result<Value, String> {
    let path = str_arg(args, "path").unwrap_or(".");
    let guidance =
        how_to_test::how_to_test(root, Path::new(path)).map_err(|error| error.to_string())?;
    Ok(json!({
        "action": "how_to_test",
        "path": path,
        "command": guidance.command,
        "cwd": guidance.cwd,
        "source": guidance.source,
        "also": guidance.also,
    }))
}

#[cfg(test)]
#[path = "actions_test.rs"]
mod actions_tests;
