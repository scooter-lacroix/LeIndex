//! The per-file scan and windowing pipeline for text search, kept out of
//! `engine.rs` so each file stays under the repository's line budget.
//!
//! Everything here operates on the shared types in `super`; nothing in this
//! module is public API. `engine.rs` re-exports the four fns so `search`
//! calls them exactly as before.

use super::*;
use std::collections::BTreeMap;

pub(crate) fn page_collect_bound(options: &SearchOptions) -> usize {
    match (options.collect_hits, options.limit) {
        (false, _) => 0,
        (true, Some(limit)) => {
            // On a continuation page the per-file resume cursors skip hits
            // that earlier pages delivered; those skips must be buffered too,
            // or the page finds nothing left to show. The largest cursor
            // bounds every file, and plain pages (no cursors) are unchanged.
            let continuation_slack = options.skip_in_file.values().copied().max().unwrap_or(0);
            options
                .offset
                .saturating_add(limit)
                .saturating_add(continuation_slack)
        }
        (true, None) => 0,
    }
}

pub(crate) fn scan_file(
    candidate: &Candidate,
    compiled: &Compiled,
    options: &SearchOptions,
    index: Option<&TextIndex>,
    root_canonical: Option<&std::path::Path>,
) -> Option<FileResult> {
    if !index_candidate_contained(candidate, root_canonical) {
        return None;
    }
    // Text search keeps its historical shape: a file it cannot read is a
    // non-match, not an incomplete result. `has_more` for this axis is built
    // from the index plan, not from per-file read outcomes, so a refusal here
    // is dropped rather than escalated.
    let data = read_scan_file(&candidate.abs).ok()?;
    let symbol_index = resolve_symbol_index(index, candidate, options);
    let mut result = FileResult {
        rel: candidate.rel.clone(),
        symbols_stale: options.want_symbols && candidate.dirty && candidate.id.is_some(),
        ..Default::default()
    };
    let mut tallies: BTreeMap<(String, &'static str), usize> = BTreeMap::new();
    // Spans keyed by (name, span_start): same-named methods in separate
    // `impl` blocks each resolve to their own span, and `or_insert` on the
    // bare name kept only the first — the second method's range was lost.
    let mut symbol_spans: BTreeMap<(String, u32), u32> = BTreeMap::new();
    let (mut line_no, mut counted_to, mut last_line_start) = (1u32, 0usize, usize::MAX);
    let collect_bound = page_collect_bound(options);
    for found in compiled.regex.find_iter(&data) {
        let start = found.start();
        let line_start = memchr::memrchr(b'\n', &data[..start]).map_or(0, |p| p + 1);
        if line_start == last_line_start {
            continue;
        }
        last_line_start = line_start;
        line_no += memchr::memchr_iter(b'\n', &data[counted_to..line_start]).count() as u32;
        counted_to = line_start;
        result.match_lines += 1;

        let symbol = symbol_index
            .and_then(|(index, id)| index.enclosing_symbol(id, start))
            .map(|(name, kind, span_start, span_end)| {
                *tallies.entry((name.clone(), kind)).or_default() += 1;
                symbol_spans
                    .entry((name.clone(), span_start))
                    .or_insert(span_end);
                (name, kind)
            });
        let reportable =
            options.collect_hits && (collect_bound == 0 || result.hits.len() < collect_bound);
        if !reportable {
            continue;
        }
        result.hits.push(build_hit(
            &data, start, line_start, line_no, options, symbol,
        ));
    }
    if result.match_lines == 0 {
        return None;
    }
    // A page limit stopped collection while matches remained: the caller
    // must report has_more even if the buffered hits alone did not exceed
    // the window (the defaults' collect_bound == window_end shape). There
    // is no per_file_cap term: the cap shapes what a page SHOWS, not the
    // stream (see `window_file_hits`), so matches beyond the buffer are
    // exactly the ones a later offset must reach — flagging them is the
    // whole point.
    result.collected_truncated = collect_bound > 0 && result.match_lines > result.hits.len();
    // Byte spans → 1-based line numbers for the enclosing-symbol ranges the
    // grouped renderer prints (`path › name (L2336-2428): N hits`); the same
    // conversion shape `symbols.rs` uses for its hits. Newline offsets are
    // collected once and reused, so each span resolves without rescanning
    // its source prefix.
    let newlines: Vec<usize> = memchr::memchr_iter(b'\n', &data).collect();
    let line_of = |at: usize| {
        let idx = newlines.partition_point(|&nl| nl < at);
        1 + idx as u32
    };
    result.symbol_ranges = symbol_spans
        .into_iter()
        .map(|((name, start), end)| {
            (
                name,
                line_of(start as usize),
                line_of(end.saturating_sub(1).max(start) as usize),
            )
        })
        .collect();
    result.symbols = tallies
        .into_iter()
        .map(|((name, kind), count)| (name, kind, count))
        .collect();
    Some(result)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn window_file_hits(
    file: &mut FileResult,
    seen_hits: &mut usize,
    root_index: usize,
    options: &SearchOptions,
    window_end: Option<usize>,
    page_hits: &mut usize,
    cursors: &mut BTreeMap<String, usize>,
    probe_found: &mut bool,
) {
    // The per-file cap shapes what a page SHOWS without shrinking the
    // pagination stream: every buffered hit occupies a position in the
    // stream (`seen_hits` walks the uncapped sequence on every request), so
    // plain-offset pages tile it exactly once. Dropping past-cap hits from
    // the stream (the previous behavior) made them appear on no page while
    // `match_lines` kept counting them — a silent drop the caller could
    // never page into.
    //
    // On a continuation page (`skip_in_file` non-empty) the global offset
    // no longer windows: tails of capped files sit at stream positions
    // BELOW the next offset whenever files interleave. Each file skips its
    // resume cursor instead, shows up to the cap of NEW hits, and the page
    // stops after `limit` shown hits. Files visited here keep or advance
    // their cursor in `cursors`; exhausted files drop out.
    let continuation = !options.skip_in_file.is_empty();
    let key = format!("{root_index}:{}", file.rel);
    let skip = options.skip_in_file.get(&key).copied().unwrap_or(0);
    let mut skipped = 0usize;
    let mut tail = 0usize;
    let mut cap_held = 0usize;
    let per_file_cap = options.per_file_cap;
    file.total_hits = file.hits.len();
    file.shown = 0;
    let mut windowed = Vec::with_capacity(file.hits.len());
    for hit in file.hits.drain(..) {
        let position = *seen_hits;
        *seen_hits += 1;
        let mut shown = false;
        if continuation {
            if skipped < skip {
                // Delivered by an earlier page.
                skipped += 1;
            } else if options.limit.is_some_and(|limit| *page_hits >= limit) {
                // Page budget spent: this hit and every remaining hit of
                // the file stay undelivered — they must NOT advance the
                // cursor, or the next page would skip over them. Reaching
                // this branch is itself the evidence that an undelivered
                // hit exists, so a final page is never announced over hits
                // that were not delivered.
                *probe_found = true;
                break;
            } else if per_file_cap == 0 || file.shown < per_file_cap {
                shown = true;
            } else {
                // The file's cap is spent; keep scanning so the next page
                // resumes past everything shown so far. This is a genuine
                // cap exclusion — count it as withheld.
                cap_held += 1;
            }
        } else {
            let in_window =
                position >= options.offset && window_end.is_none_or(|end| position < end);
            if in_window {
                shown = per_file_cap == 0 || file.shown < per_file_cap;
                if !shown {
                    cap_held += 1;
                }
            }
            // The tail is every buffered hit at or after the window start
            // that this page did not show — capped in-window or beyond the
            // window. Hits BEFORE the offset were delivered by earlier pages
            // and must not resurrect the stream (a stale tail there would
            // make bare-offset paging loop forever).
            if !shown && position >= options.offset {
                tail += 1;
            }
        }
        if shown {
            file.shown += 1;
            *page_hits += 1;
            windowed.push(hit);
        }
    }
    file.hits = windowed;
    // cap_withheld reports ONLY matches withheld by the per-file cap. Hits
    // withheld because the global window overflowed are not cap-withheld —
    // they are ordinary next-page hits — so they must not inflate the cap
    // signal (overflow already drives has_more through the window check).
    file.cap_withheld = if per_file_cap == 0 { 0 } else { cap_held };
    // §6.8: the resume cursor is what this file has already DELIVERED —
    // shown hits plus everything earlier pages took (skip on a
    // continuation; the pre-offset window on a first page). Every visited
    // file gets an entry, not only capped ones: the next page must skip a
    // fully-delivered file just as surely as it must resume a capped one.
    // A file absent from the map was never visited (budget break) and is
    // served fresh.
    let cursor = if continuation {
        skip + file.shown
    } else {
        file.total_hits - tail
    };
    if cursor > 0 || file.cap_withheld > 0 {
        cursors.insert(key, cursor);
    } else {
        cursors.remove(&key);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn ingest_scanned_files(
    scanned: Vec<Option<FileResult>>,
    root_index: usize,
    root_out: &mut RootOutput,
    output: &mut SearchOutput,
    options: &SearchOptions,
    seen_hits: &mut usize,
    window_end: Option<usize>,
    page_hits: &mut usize,
    cursors: &mut BTreeMap<String, usize>,
    probe_found: &mut bool,
) -> bool {
    let mut truncated = false;
    for mut file in scanned.into_iter().flatten() {
        output.stats.files_matched += 1;
        output.stats.match_lines += file.match_lines;
        truncated |= file.collected_truncated;
        window_file_hits(
            &mut file,
            seen_hits,
            root_index,
            options,
            window_end,
            page_hits,
            cursors,
            probe_found,
        );
        // Summed after windowing: `window_file_hits` is what sets
        // `cap_withheld`.
        output.cap_withheld += file.cap_withheld;
        output.returned += file.hits.len();
        if !options.collect_hits || !file.hits.is_empty() {
            root_out.files.push(file);
        }
    }
    truncated
}
