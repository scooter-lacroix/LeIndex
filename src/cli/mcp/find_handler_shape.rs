//! Response-shaping helpers for `leindex_find`, kept out of
//! `find_handler.rs` so each file stays under the repository's line budget.
//!
//! Everything here is private to the handler; `find_handler.rs` re-exports
//! the fns it needs.

use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) fn shape_text_result(
    pattern: &str,
    output_mode: &str,
    primary: &[PrimaryRoot],
    result: SearchOutput,
    window: &FindWindow,
    regex_note: Option<&str>,
    case_insensitive_hits: usize,
    query_args: &Value,
) -> Value {
    let stats = &result.stats;
    // `callsites` is a hit page with definition lines dropped afterwards;
    // it shares the `matches` shaping end to end.
    let hit_page = output_mode == "matches" || output_mode == "callsites";
    let (mut files_json, symbol_totals, file_rows, cursor_keys) =
        collect_file_rows(&result, primary, output_mode, window.include_tests);
    let zero_hit = stats.match_lines == 0;

    let mut value = json!({
        "pattern": pattern,
        "target": "text",
        "output": output_mode,
        "roots": primary.iter().map(|r| json!({
            "path": r.root.to_string_lossy(),
            "indexed": r.indexed,
        })).collect::<Vec<_>>(),
        "total_files": stats.files_matched,
        "total_matches": stats.match_lines,
        "complete": result.complete,
        "source_freshness": "live",
        "stats": {
            "candidates": stats.candidates,
            "scanned": stats.scanned,
            "changed_since_index": stats.dirty,
            "globs_excluded": stats.glob_excluded,
            "millis": stats.millis as u64,
        },
    });
    // Whether the engine's has_more was suppressed because the continuation
    // cannot advance (matches mode only; the summary modes compute their own
    // in `shape_rows_page`). A zero-hit page ends the stream too — its
    // next_offset would re-serve the identical page — but only the ceiling
    // case is a truncation; the note explains the rest.
    let ceiling_ended =
        hit_page && result.has_more && continuation_past_ceiling(window.offset + result.returned);
    let zero_hit_ended = hit_page && result.has_more && result.returned == 0 && !ceiling_ended;
    match output_mode {
        "matches" | "callsites" => {
            if output_mode == "callsites" {
                // §4.2f: select callsites BEFORE the result limit applies — a
                // definition that consumes a page slot must not crowd out a
                // call. The engine fetched a slack window (see `run`), so the
                // calls that remain still fill the requested page.
                filter_callsites_rows(
                    &mut files_json,
                    pattern,
                    Some(window.limit.unwrap_or(MAX_LIMIT)),
                );
            }
            // §4.2c: test files rank after source files on every page.
            let mut zipped: Vec<(Value, String)> =
                files_json.into_iter().zip(cursor_keys).collect();
            if window.include_tests {
                zipped.sort_by_key(|(file, _)| is_test_path(file_path_of(file)));
            }
            let (files_json, cursor_keys): (Vec<Value>, Vec<String>) = zipped.into_iter().unzip();
            let visible_returned = (output_mode == "callsites").then(|| {
                files_json
                    .iter()
                    .filter_map(|file| file.get("hits").and_then(Value::as_array).map(Vec::len))
                    .sum()
            });
            shape_matches_page(
                &mut value,
                &result,
                window.offset,
                ceiling_ended,
                zero_hit_ended,
                files_json,
                cursor_keys,
                window.token_budget.unwrap_or(0),
                query_args,
                visible_returned,
            );
        }
        "files" => shape_rows_page(
            &mut value,
            file_rows,
            window.offset,
            window.limit,
            "files",
            |(file, matches)| json!({ "file": file, "matches": matches }),
            query_args,
        ),
        "symbols" => {
            let mut rows: Vec<_> = symbol_totals.into_iter().collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            shape_rows_page(
                &mut value,
                rows,
                window.offset,
                window.limit,
                "symbols",
                |entry| {
                    let ((name, kind, file), matches) = entry;
                    json!({ "name": name, "kind": kind, "file": file, "matches": matches })
                },
                query_args,
            );
        }
        _ => {}
    }
    let value_notes_ceiling = ceiling_ended || value.get("truncated_by_ceiling").is_some();
    // `count` carries no has_more at all — it is not pageable — so a
    // missing key means false, not true.
    let response_has_more = value
        .get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let notes = match text_result_notes(&result, value_notes_ceiling, response_has_more, regex_note)
    {
        Some(note) => vec![note],
        None => Vec::new(),
    };
    attach_notes(
        &mut value,
        notes,
        zero_hit,
        regex_note.is_some(),
        case_insensitive_hits,
        primary,
        &result,
    );
    value
}

/// The file path a shaped file entry carries (its `file` key).
pub(super) fn file_path_of(file: &Value) -> &str {
    file.get("file").and_then(Value::as_str).unwrap_or("")
}

/// §4.2b: merge overlapping and ADJACENT context windows so no line prints
/// twice. Hits arrive in line order; the previous hit's after-context gives
/// up any line at or beyond the next match line, and the next hit's
/// before-context gives up everything the previous window already printed.
pub(super) fn merge_hit_context(hits: &[Value]) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::with_capacity(hits.len());
    for hit in hits {
        let line = hit.get("line").and_then(Value::as_u64).unwrap_or(0);
        let before_len = hit["before"].as_array().map_or(0, Vec::len) as u64;
        let mut entry = hit.clone();
        if let Some(prev) = merged.last_mut() {
            let prev_line = prev.get("line").and_then(Value::as_u64).unwrap_or(0);
            let prev_end = prev_line + prev["after"].as_array().map_or(0, Vec::len) as u64;
            if line.saturating_sub(before_len) <= prev_end + 1 {
                // The previous window owns the lines up to prev_end: its
                // after-context keeps only the lines strictly between the
                // two matches, so the upcoming match line prints once.
                let keep_after = line.saturating_sub(prev_line + 1) as usize;
                if let Some(prev_obj) = prev.as_object_mut()
                    && let Some(arr) = prev_obj.get_mut("after").and_then(Value::as_array_mut)
                {
                    // Keep the FIRST `keep_after` lines — the ones immediately
                    // after the previous match, up to the next match — and
                    // drop the tail, which belongs to the next window. A
                    // truncate keeps the near lines; the old front-drain kept
                    // the far ones and duplicated the next hit's context.
                    arr.truncate(keep_after);
                }
                // This hit's before-context keeps only the lines past the
                // previous window's end.
                let keep_before = line.saturating_sub(prev_end + 1).min(before_len) as usize;
                let skip = before_len as usize - keep_before;
                if skip > 0
                    && let Some(arr) = hit["before"].as_array()
                {
                    entry["before"] = Value::Array(arr[skip..].to_vec());
                }
            }
        }
        merged.push(entry);
    }
    merged
}

/// §4.2c: the path conventions test files follow, across the common
/// languages. `*_test.rs` / `*_tests.rs`, a `tests/` or `test/` directory,
/// `test_*` / `*_test` / `*_spec` / `*.spec.*` / `*.test.*` names, and
/// Python's `test_*.py` / `*_test.py`.
pub(super) fn is_test_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let segments: Vec<&str> = normalized.split('/').collect();
    // A tests/ or test/ DIRECTORY anywhere in the path.
    if segments[..segments.len().saturating_sub(1)]
        .iter()
        .any(|segment| *segment == "tests" || *segment == "test")
    {
        return true;
    }
    let name = segments.last().copied().unwrap_or("");
    let lower = name.to_ascii_lowercase();
    lower.starts_with("test_")
        || lower.ends_with("_test")
        || lower.ends_with("_test.rs")
        || lower.ends_with("_tests.rs")
        || lower.ends_with("_test.py")
        || lower.ends_with("_test.go")
        || lower.ends_with("_test.ts")
        || lower.ends_with("_test.tsx")
        || lower.ends_with("_test.js")
        || lower.ends_with("_test.mjs")
        || lower.ends_with(".test.js")
        || lower.ends_with(".test.ts")
        || lower.ends_with(".test.tsx")
        || lower.ends_with(".test.mjs")
        || lower.ends_with(".spec.js")
        || lower.ends_with(".spec.ts")
        || lower.ends_with(".spec.tsx")
        || lower.ends_with(".spec.mjs")
}

/// Gather the per-root file results into the shape each output mode needs:
/// rendered match pages (context merged, cap tails announced), per-file
/// symbol totals, or plain (file, count) rows.
pub(super) fn collect_file_rows(
    result: &SearchOutput,
    primary: &[PrimaryRoot],
    output_mode: &str,
    include_tests: bool,
) -> (
    Vec<Value>,
    BTreeMap<(String, &'static str, String), usize>,
    Vec<(String, usize)>,
    Vec<String>,
) {
    let mut files_json = Vec::new();
    let mut symbol_totals: BTreeMap<(String, &'static str, String), usize> = BTreeMap::new();
    let mut file_rows: Vec<(String, usize)> = Vec::new();
    // Engine resume-cursor keys (`{root_index}:{rel}`), parallel to
    // `files_json` so the page can tell WHICH rows were delivered after a
    // token-budget trim (empty for rows that have no engine file — the
    // collapsed test summaries).
    let mut cursor_keys = Vec::new();
    let mut collapsed_tests: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    // `callsites` shares the matches pipeline (it is the same scan; only the
    // definition lines are dropped afterwards), so its pages must carry hits.
    let hit_pages = output_mode == "matches" || output_mode == "callsites";
    let collapse = output_mode == "matches";
    for (root_id, root) in result.roots.iter().enumerate() {
        for file in &root.files {
            let shown = display_path(primary, root_id, &file.rel);
            if hit_pages {
                // §4.2c: test files collapse to one summary line unless
                // the caller asked for them in full (matches pages only —
                // callsites filtering needs every hit to filter on).
                if collapse && !include_tests && is_test_path(&shown) {
                    let entry = collapsed_tests.entry(shown).or_default();
                    entry.0 += file.match_lines;
                    entry.1 += file.total_hits;
                    continue;
                }
                let mut value = json!({
                    "file": shown,
                    "matches": file.match_lines,
                    "symbols_stale": file.symbols_stale,
                    "hits": merge_hit_context(
                        &file.hits.iter().map(|hit| {
                            let mut value = json!({ "line": hit.line, "text": hit.text });
                            if hit.col > 1 { value["col"] = json!(hit.col); }
                            if let Some((name, kind)) = &hit.symbol {
                                value["symbol"] = json!(name);
                                value["kind"] = json!(kind);
                            }
                            if !hit.before.is_empty() { value["before"] = json!(hit.before); }
                            if !hit.after.is_empty() { value["after"] = json!(hit.after); }
                            value
                        }).collect::<Vec<_>>(),
                    ),
                });
                // §4.2e: the tail is REACHABLE on the next page; say how
                // much of this file is still unpaged.
                if file.cap_withheld > 0 {
                    value["cap_line"] = json!(format!(
                        "+{} more in this file (next page)",
                        file.cap_withheld
                    ));
                }
                // §4.2a: enclosing-symbol ranges for the grouped header.
                if !file.symbol_ranges.is_empty() {
                    let mut ranges = file.symbol_ranges.clone();
                    ranges.sort_by_key(|range| range.1);
                    value["symbol_ranges"] = json!(
                        ranges
                            .iter()
                            .map(|(name, start, end)| json!({
                                "name": name,
                                "start": start,
                                "end": end,
                            }))
                            .collect::<Vec<_>>()
                    );
                }
                files_json.push(value);
                cursor_keys.push(format!("{root_id}:{}", file.rel));
            } else if output_mode == "symbols" {
                for (name, kind, count) in &file.symbols {
                    *symbol_totals
                        .entry((name.clone(), kind, shown.clone()))
                        .or_default() += count;
                }
            } else {
                file_rows.push((shown, file.match_lines));
            }
        }
    }
    // §4.2c: the collapsed test files ride at the END of the page as one
    // line each — present, counted, and not in the way.
    for (path, (match_lines, hits)) in collapsed_tests {
        files_json.push(json!({
            "file": path,
            "matches": match_lines,
            "collapsed": format!("tests: {match_lines} more hits in this file"),
            "total_shown": hits,
        }));
        cursor_keys.push(String::new());
    }
    (files_json, symbol_totals, file_rows, cursor_keys)
}

/// §4.2d: trim the page to the token budget greedily by rank (files arrive
/// in rank order; test summaries count as one line). Returns the row count
/// kept and the total rows offered.
pub(super) fn trim_to_budget(files: &mut Vec<Value>, token_budget: usize) {
    if token_budget == 0 {
        return;
    }
    let budget = token_budget.saturating_mul(CHARS_PER_TOKEN);
    let mut used = 0usize;
    let mut kept = 0usize;
    for file in files.iter() {
        let cost = file.to_string().len();
        if used + cost > budget && kept > 0 {
            break;
        }
        if used + cost > budget {
            // The very first entry alone exceeds the budget: keep it anyway
            // (an empty page helps nobody) and stop here.
            kept = 1;
            break;
        }
        used += cost;
        kept += 1;
    }
    files.truncate(kept);
}

/// Per-file resume cursors from a continuation (`resume_cursors`, injected
/// by `find_paging::merge` from the token). Anything that is not a
/// map of `string → usize` is ignored, so a malformed token degrades to a
/// plain first page instead of erroring.
pub(super) fn parse_resume_cursors(args: &Value) -> BTreeMap<String, usize> {
    args.get("resume_cursors")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    value.as_u64().map(|shown| (key.clone(), shown as usize))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `matches` response page: hit lists plus the paging/truncation signals.
pub(super) fn shape_matches_page(
    value: &mut Value,
    result: &SearchOutput,
    offset: usize,
    ceiling_ended: bool,
    zero_hit_ended: bool,
    mut files: Vec<Value>,
    cursor_keys: Vec<String>,
    token_budget: usize,
    query_args: &Value,
    visible_returned: Option<usize>,
) {
    let offered = files.len();
    trim_to_budget(&mut files, token_budget);
    let shown = files.len();
    let budget_ended = shown < offered;
    let stream_ended = ceiling_ended || zero_hit_ended;
    value["returned"] = json!(result.returned);
    value["offset"] = json!(offset);
    value["files"] = Value::Array(files);
    let more = result.has_more && !stream_ended;
    value["has_more"] = json!(more || budget_ended);
    if more || budget_ended {
        // §4.2d/§6.8: the token carries per-file resume cursors, and they
        // must reflect ONLY what this page actually delivered. Rows the
        // token budget trimmed away were NOT delivered, so their resume
        // position must not advance — a cursor that claims delivery would
        // make the next page skip hits the client never saw.
        let kept_keys: std::collections::HashSet<&str> =
            cursor_keys.iter().take(shown).map(String::as_str).collect();
        let cursors = if result.resume.is_empty() {
            Value::Null
        } else {
            let map: serde_json::Map<String, Value> = result
                .resume
                .iter()
                .filter(|(key, _)| kept_keys.contains(key.as_str()))
                .map(|(key, value)| (key.clone(), json!(value)))
                .collect();
            if map.is_empty() {
                Value::Null
            } else {
                json!(map)
            }
        };
        // The stream moves by the hits actually delivered. For callsites the
        // engine's returned count includes definitions that were dropped, so
        // the handler's own filtered count rules there.
        let delivered = visible_returned.unwrap_or(result.returned);
        let next_offset = if budget_ended {
            offset + shown
        } else {
            offset + delivered
        };
        let token = crate::cli::mcp::find_paging::issue_with_cursors(
            "leindex_find",
            query_args,
            next_offset,
            &cursors,
        );
        if budget_ended {
            // A budget cut leaves GAPS in the page (rows were dropped): a
            // bare `next_offset` cannot express them, so the continuation is
            // advertised only through the token and its per-file cursors.
            if !token.is_empty() {
                value["next"] = json!(token);
                value["budget_note"] = json!(format!(
                    "+{} more (token_budget reached; pass next to continue)",
                    offered - shown
                ));
            }
        } else {
            value["next_offset"] = json!(next_offset);
            if !token.is_empty() {
                value["next"] = json!(token);
            }
        }
    }
    if ceiling_ended {
        value["truncated_by_ceiling"] = json!(true);
    }
    if result.cap_withheld > 0 {
        value["withheld_by_cap"] = json!(result.cap_withheld);
    }
}

/// A summarised (`files`/`symbols`) response page: window the sorted rows and
/// attach the same honest paging/truncation signals the hit pages carry.
fn shape_rows_page<T>(
    value: &mut Value,
    rows: Vec<T>,
    offset: usize,
    limit: Option<usize>,
    key: &str,
    render: impl Fn(&T) -> Value,
    query_args: &Value,
) {
    let cap = limit.unwrap_or(usize::MAX);
    let total = rows.len();
    let page: Vec<_> = rows.into_iter().skip(offset).take(cap).collect();
    let proposed_next = offset + page.len();
    let ceiling_ended = proposed_next < total && continuation_past_ceiling(proposed_next);
    value[key] = json!(page.iter().map(render).collect::<Vec<_>>());
    value["has_more"] = json!(proposed_next < total && !ceiling_ended);
    if proposed_next < total && !ceiling_ended {
        let token = crate::cli::mcp::find_paging::issue("leindex_find", query_args, proposed_next);
        value["next_offset"] = json!(proposed_next);
        if !token.is_empty() {
            value["next"] = json!(token);
        }
    }
    if ceiling_ended {
        value["truncated_by_ceiling"] = json!(true);
    }
}

/// Trailing hints for any text payload: why a complete-looking response may
/// still be incomplete, and (§6.7) why a zero result is zero.
/// `ceiling_ended` is the caller-computed truncation flag (it needs the
/// request offset); the ceiling sentence takes precedence because it is the
/// binding cause — the page's continuation is unservable regardless of the
/// time budget. Whether the scan ALSO stopped at the deadline is answered by
/// the engine's `stopped_by_deadline`, not by `complete`: a window stop
/// pairs `has_more` with `complete = false` itself, so keying the budget
/// sentence on `complete` mislabeled ceiling-ended pages "time budget".
pub(super) fn text_result_notes(
    result: &SearchOutput,
    ceiling_ended: bool,
    response_has_more: bool,
    regex_note: Option<&str>,
) -> Option<String> {
    let mut note = String::new();
    if let Some(interpreted) = regex_note {
        note.push_str(interpreted);
    }
    if ceiling_ended {
        if !note.is_empty() {
            note.push(' ');
        }
        note.push_str(&ceiling_sentence());
    }
    // Keyed on the RESPONSE's has_more, not the engine's: a page whose
    // continuation was refused reads as final to the client, so a
    // deadline-stopped scan must still say its totals are provisional.
    if result.stopped_by_deadline && !response_has_more {
        if !note.is_empty() {
            note.push(' ');
        }
        note.push_str("Stopped at the time budget; raise timeout_ms or narrow the search");
    }
    if result.cap_withheld > 0 {
        if !note.is_empty() {
            note.push(' ');
        }
        note.push_str(&format!(
            "{} shown matches sit past per_file_cap on THIS page; the next offset \
             (or next token) displays them — the cap bounds display, not reach",
            result.cap_withheld
        ));
    }
    (!note.is_empty()).then_some(note)
}

/// §6.7: every zero result names its reason. Checked in the order a caller
/// can act on them: literal-vs-regex (already retried by §3.8), globs,
/// ignore rules, index state, case sensitivity.
pub(super) fn zero_reasons(
    primary: &[PrimaryRoot],
    result: &SearchOutput,
    pattern_had_metachars: bool,
    case_insensitive_hits: usize,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if pattern_had_metachars {
        reasons.push(
            "the pattern looks like a regex and was also tried as one; neither reading matched"
                .to_string(),
        );
    }
    if case_insensitive_hits > 0 {
        reasons.push(format!(
            "0 case-sensitive hits, {case_insensitive_hits} case-insensitive; \
             pass case:\"insensitive\" to see them"
        ));
    }
    if result.stats.glob_excluded > 0 {
        reasons.push(format!(
            "{} file(s) were excluded by include_globs/exclude_globs/scope",
            result.stats.glob_excluded
        ));
    }
    if result.stats.candidates == 0 {
        if primary.iter().all(|r| r.indexed) {
            reasons.push(
                "no candidate files came from the index; the paths you want may be \
                 git-ignored — pass paths=[\"...\"] to search them anyway"
                    .to_string(),
            );
        } else if primary.iter().all(|r| !r.indexed) {
            reasons.push(
                "no index was consulted (raw path search); the directory may be empty \
                 or hold only binary/oversized files"
                    .to_string(),
            );
        }
    }
    reasons
}

/// Attach the note(s): the reason list on a zero result, the truncation
/// notes otherwise. `note` stays a single line; the zero-reasons ride in
/// `zero_reasons` so a compact renderer prints both without nesting.
pub(super) fn attach_notes(
    value: &mut Value,
    notes: Vec<String>,
    zero_hit: bool,
    pattern_had_metachars: bool,
    case_insensitive_hits: usize,
    primary: &[PrimaryRoot],
    result: &SearchOutput,
) {
    if zero_hit {
        value["zero_reasons"] = json!(zero_reasons(
            primary,
            result,
            pattern_had_metachars,
            case_insensitive_hits
        ));
    }
    if let Some(first) = notes.first() {
        value["note"] = json!(first);
    }
}

/// §3.8: what the regex retry means. A zero-hit regex reading is only
/// conclusive when the scan COMPLETED — a deadline-stopped scan proves
/// nothing, so it must not be reported as a clean miss.
pub(super) fn retry_note(retried: &SearchOutput) -> Option<&'static str> {
    if retried.stats.match_lines > 0 {
        return Some("(interpreted as regex)");
    }
    if retried.stopped_by_deadline || !retried.complete {
        return Some(
            "(regex reading did not finish within timeout_ms; raise timeout_ms or narrow the search)",
        );
    }
    Some("literal and regex readings both matched nothing")
}

/// §3.8: does this literal pattern plausibly read as a regex? Only
/// metacharacters that CHANGE meaning trigger the retry — a lone `.` or a
/// plain word never does, so ordinary literal searches never pay a second
/// read. A dot next to another metachar (`.*`, `a.b`) still counts: the
/// caller who typed it in a literal almost certainly meant the regex.
pub(super) fn looks_like_regex(pattern: &str) -> bool {
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.peek() {
                // \b, \d, \w … are regex classes: as a literal they are
                // backslash-plus-letter text, so the two readings differ.
                Some(next) if next.is_alphanumeric() => return true,
                // \. and friends escape a metachar: the literal and the
                // escaped-regex readings match the same text, so no retry.
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            // Alternation, repetition-with-a-partner, anchors, classes and
            // groups are the shapes that make a literal reading useless.
            // A trailing lone `.` is a plain literal (no retry); a dot with
            // anything after it changes the regex reading.
            '|' | '^' | '$' | '[' | '(' => return true,
            '.' if chars.peek().is_some() => return true,
            _ => {}
        }
    }
    false
}

/// §4.2f `output=exists`: a yes/no answer that stops at the first hit.
pub(super) async fn exists_result(
    found: FindPattern,
    specs: Vec<RootSpec>,
    primary: &[PrimaryRoot],
    window: FindWindow,
) -> Result<Value, JsonRpcError> {
    let compiled = found.compiled;
    let options = SearchOptions {
        offset: 0,
        limit: Some(1),
        per_file_cap: 1,
        skip_in_file: BTreeMap::new(),
        context: 0,
        max_line_chars: 200,
        deadline: search_deadline(window.timeout_ms),
        collect_hits: true,
        want_symbols: false,
    };
    let result = tokio::task::spawn_blocking(move || search(&specs, &compiled, &options)).await;
    let result: SearchOutput =
        result.map_err(|e| JsonRpcError::internal_error(format!("find failed: {e}")))?;
    let exists = result.stats.match_lines > 0;
    let mut first = json!(null);
    for (root_id, root) in result.roots.iter().enumerate() {
        for file in &root.files {
            if let Some(hit) = file.hits.first() {
                first = json!({
                    // `display_path` matches the matches response: explicit
                    // file roots resolve to their own absolute path, so a
                    // caller can act on it.
                    "file": display_path(primary, root_id, &file.rel),
                    "line": hit.line,
                    "text": hit.text,
                });
                break;
            }
        }
        if !first.is_null() {
            break;
        }
    }
    // Also carry the hit as a content row so the compact (`L1`) renderer
    // prints the answer — it iterates `files[].hits[]` and would otherwise
    // print nothing for an `exists` payload.
    let files: Vec<Value> = result
        .roots
        .iter()
        .enumerate()
        .flat_map(|(root_id, root)| {
            root.files.iter().map(move |file| {
                json!({
                    "file": display_path(primary, root_id, &file.rel),
                    "matches": 1,
                    "hits": file.hits.iter().map(|hit| {
                        json!({ "line": hit.line, "text": hit.text })
                    }).collect::<Vec<_>>(),
                })
            })
        })
        .collect();
    Ok(json!({
        "pattern": found.text,
        "output": "exists",
        "exists": exists,
        "first": first,
        "files": files,
        "roots": primary.iter().map(|r| json!({
            "path": r.root.to_string_lossy(),
            "indexed": r.indexed,
        })).collect::<Vec<_>>(),
        "complete": result.complete,
    }))
}
