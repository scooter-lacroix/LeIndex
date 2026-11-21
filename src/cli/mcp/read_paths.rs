//! Read-path engine (brief §3.1, §4.1, §6.9).
//!
//! Everything the read tools need that must work WITHOUT a loaded index:
//!
//! * stable read handles (`path#symbol`, `path#L600-800`) — parse and render;
//! * elided reads — the focus symbol in full, sibling top-level items
//!   collapsed to one-line signatures with `expand:` handles;
//! * the per-MCP-session read ledger — bounded, keyed by session id, so an
//!   unchanged re-read can be answered with `unchanged since read #N`.
//!
//! Pure std + `serde_json` + `blake3`; the symbol spans come from the caller
//! (the live tree-sitter parse), never from the index.

use crate::storage::CatalogSymbol;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

// =============================================================================
// Read handles: `path`, `path#symbol`, `path#L600-800`
// =============================================================================

/// A parsed `file_path` argument: the file, plus an optional embedded
/// symbol or line-range target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadHandle {
    /// The file part. Relative paths are resolved against `project_root`
    /// when one was available at parse time.
    pub file_path: String,
    /// `path#symbol` — the symbol to expand.
    pub symbol: Option<String>,
    /// `path#L600-800` — the explicit line range (1-based, inclusive).
    pub line_range: Option<(usize, usize)>,
}

/// Split `path#fragment` into the file part and the raw fragment.
/// A `#` with an empty file part (`#foo`) stays in the file part.
pub(crate) fn split_read_fragment(raw: &str) -> (String, Option<String>) {
    match raw.split_once('#') {
        Some((file, fragment)) if !file.is_empty() => {
            (file.to_string(), Some(fragment.to_string()))
        }
        _ => (raw.to_string(), None),
    }
}

/// Classify a handle fragment: `L600-800` / `L600` / `600-800` → inclusive
/// 1-based range; anything else is a symbol name.
pub(crate) fn classify_fragment(fragment: &str) -> Fragment {
    if let Some(range) = parse_line_fragment(fragment) {
        return Fragment::LineRange(range);
    }
    let trimmed = fragment.trim();
    if trimmed.is_empty() {
        return Fragment::None;
    }
    Fragment::Symbol(trimmed.to_string())
}

/// A classified handle fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fragment {
    /// `path#L600-800`
    LineRange((usize, usize)),
    /// `path#symbol`
    Symbol(String),
    /// Empty fragment (`path#`)
    None,
}

/// Split a `file_path` argument into file, symbol and line-range parts.
///
/// `#` starts a fragment: `#symbol` or `#L<start>-<end>` (also `#L<start>`,
/// meaning the single line). The two escapes MCP clients actually produce for
/// spaces and for a literal `#` in a filename are decoded.
pub(crate) fn parse_read_handle(raw: &str, project_root: Option<&Path>) -> ReadHandle {
    // Split FIRST, decode second: `%23` in a path is an escaped literal `#`
    // (a filename like `a b#c.rs`), never a fragment separator — decoding
    // before the split would cut the file part at the escaped `#`.
    let (file_part, fragment) = split_read_fragment(raw);
    let decode = |part: &str| part.replace("%20", " ").replace("%23", "#");
    let decoded_file = decode(&file_part);
    let decoded_fragment = fragment.as_deref().map(decode);

    let (line_range, symbol) = match decoded_fragment.as_deref().map(classify_fragment) {
        Some(Fragment::LineRange(range)) => (Some(range), None),
        Some(Fragment::Symbol(symbol)) => (None, Some(symbol)),
        _ => (None, None),
    };

    let file_path = match project_root {
        Some(root) if !Path::new(&decoded_file).is_absolute() && !decoded_file.is_empty() => {
            root.join(&decoded_file).to_string_lossy().into_owned()
        }
        _ => decoded_file,
    };

    ReadHandle {
        file_path,
        symbol,
        line_range,
    }
}

/// `L600-800` / `L600` / `600-800` → inclusive 1-based range.
fn parse_line_fragment(fragment: &str) -> Option<(usize, usize)> {
    let spec = fragment
        .strip_prefix('L')
        .or_else(|| fragment.strip_prefix('l'))
        .unwrap_or(fragment);
    let (start, end) = match spec.split_once('-') {
        Some((s, e)) => (s, e),
        None => (spec, spec),
    };
    let start: usize = start.trim().parse().ok()?;
    let end: usize = end.trim().parse().ok()?;
    if start == 0 || end < start {
        return None;
    }
    Some((start, end))
}

/// Render the stable handle for a served read range: `path#L600-800`,
/// project-relative when the file lives under the root.
pub(crate) fn handle_for_range(
    file_path: &Path,
    project_root: Option<&Path>,
    start_line: usize,
    end_line: usize,
) -> String {
    format!(
        "{}#L{}-{}",
        display_path(file_path, project_root),
        start_line,
        end_line
    )
}

/// Project-relative display when the file is under the root, absolute path
/// otherwise. Handles read back identically via [`parse_read_handle`].
fn display_path(file_path: &Path, project_root: Option<&Path>) -> String {
    if let Some(root) = project_root
        && let Ok(rel) = file_path.strip_prefix(root)
    {
        return rel.to_string_lossy().into_owned();
    }
    file_path.to_string_lossy().into_owned()
}

// =============================================================================
// Numbered body rendering (`600 code`, no padding, no duplication)
// =============================================================================

/// Render `content` lines as `N code` starting at `start_line` (1-based).
/// The number appears exactly once — this is the §3.1 contract.
pub(crate) fn number_lines(content: &str, start_line: usize) -> String {
    let mut out = String::with_capacity(content.len() + 16);
    for (index, line) in content.lines().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&(start_line + index).to_string());
        out.push(' ');
        out.push_str(line);
    }
    out
}

// =============================================================================
// Elided reads (brief §4.1)
// =============================================================================

/// One collapsed sibling item.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CollapsedItem {
    /// `fn foo(x: u32) -> Bar` — the signature line without the opening brace.
    pub signature: String,
    /// Symbol name used in the expand handle.
    pub name: String,
    /// First line of the item, 1-based.
    pub start_line: usize,
    /// Total line span of the item.
    pub line_count: usize,
}

/// Result of an elided read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ElidedOutput {
    /// Numbered body: focus item in full, siblings collapsed.
    pub body: String,
    /// Handle that expands the focus item (`path#symbol`).
    pub expand: String,
    /// Focus item line range (1-based, inclusive).
    pub focus_start_line: usize,
    pub focus_end_line: usize,
    /// Sibling items that were collapsed.
    pub collapsed: Vec<CollapsedItem>,
}

/// A located symbol span inside a file (byte range + 1-based line range).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymbolSpan {
    pub name: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
}

/// Byte offset of the start of every line (len+1 entries; the last is
/// `content.len()`).
fn line_byte_offsets(content: &str) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(content.lines().count() + 1);
    let mut acc = 0usize;
    offsets.push(0usize);
    for chunk in content.split_inclusive('\n') {
        acc += chunk.len();
        offsets.push(acc);
    }
    offsets
}

/// Convert byte ranges to 1-based line ranges against `content`.
pub(crate) fn spans_with_lines(content: &str, symbols: &[CatalogSymbol]) -> Vec<SymbolSpan> {
    let offsets = line_byte_offsets(content);
    let total_lines = offsets.len().saturating_sub(1);
    let line_of = |byte: usize| -> usize {
        offsets
            .partition_point(|&off| off <= byte)
            .max(1)
            .min(total_lines.max(1))
    };

    symbols
        .iter()
        .map(|symbol| {
            let (start_byte, end_byte) = symbol.byte_range;
            SymbolSpan {
                name: symbol.symbol_name.clone(),
                start_byte,
                end_byte,
                start_line: line_of(start_byte),
                end_line: line_of(end_byte.saturating_sub(1).min(content.len())),
            }
        })
        .collect()
}

/// Top-level spans: spans that are not strictly contained in another span.
pub(crate) fn top_level_spans(spans: &[SymbolSpan]) -> Vec<SymbolSpan> {
    spans
        .iter()
        .filter(|span| {
            !spans.iter().any(|other| {
                other.name != span.name
                    && other.start_byte <= span.start_byte
                    && other.end_byte >= span.end_byte
                    && (other.end_byte - other.start_byte) > (span.end_byte - span.start_byte)
            })
        })
        .cloned()
        .collect()
}

/// Find the span for `focus` among `spans` (exact name, then suffix —
/// handles name the bare item, parses may carry a qualifier like
/// `Impl::build_x`).
pub(crate) fn find_span<'a>(spans: &'a [SymbolSpan], focus: &str) -> Option<&'a SymbolSpan> {
    spans
        .iter()
        .find(|span| span.name == focus)
        .or_else(|| spans.iter().find(|span| span.name.ends_with(focus)))
}

/// Build the elided read: focus span verbatim, every other top-level span
/// collapsed to `sig { … N lines }  [expand: path#item]`.
///
/// `file_label` is the path as it should appear inside `[expand: …]`
/// handles (project-relative when possible). Doc-comment / attribute lines
/// directly above a collapsed item collapse with it; the same block above
/// the focus item is kept. Output is capped at `max_lines` with an explicit
/// truncation note (zero silent truncation).
pub(crate) fn elide_around(
    content: &str,
    symbols: &[CatalogSymbol],
    focus: &str,
    file_label: &str,
    max_lines: usize,
) -> Result<ElidedOutput, String> {
    let spans = spans_with_lines(content, symbols);
    let focus_span = find_span(&spans, focus)
        .ok_or_else(|| format!("focus_symbol '{focus}' not found in file"))?
        .clone();
    let top_level = top_level_spans(&spans);
    let total_lines = content.lines().count();

    let mut collapsed: Vec<CollapsedItem> = Vec::new();
    // line number -> index into `collapsed` for the item whose one-line
    // signature must be emitted at that line.
    let mut signature_at: HashMap<usize, usize> = HashMap::new();
    // 1-based line numbers replaced by a collapsed sibling (body + its
    // directly-hanging doc/attr block).
    let mut collapsed_lines: Vec<usize> = Vec::new();

    for span in &top_level {
        if span.start_byte == focus_span.start_byte && span.end_byte == focus_span.end_byte {
            continue;
        }
        let line_count = span.end_line.saturating_sub(span.start_line) + 1;
        let first_line = content
            .lines()
            .nth(span.start_line - 1)
            .unwrap_or_default()
            .trim_end();
        let signature = match first_line.strip_suffix('{') {
            Some(sig) => sig.trim_end().to_string(),
            None => first_line.to_string(),
        };
        collapsed.push(CollapsedItem {
            signature,
            name: span.name.clone(),
            start_line: span.start_line,
            line_count,
        });
        signature_at.insert(span.start_line, collapsed.len() - 1);

        // Doc/attr block above the item: contiguous ///, //!, #[...] lines
        // ending at item_start-1.
        let mut block_start = span.start_line;
        while block_start > 1 {
            let line = content.lines().nth(block_start - 2).unwrap_or_default();
            let trimmed = line.trim_start();
            if trimmed.starts_with("///")
                || trimmed.starts_with("//!")
                || trimmed.starts_with("#[")
                || trimmed.starts_with("#![")
            {
                block_start -= 1;
            } else {
                break;
            }
        }
        // A block with the focus item starting inside it belongs to the
        // focus item, not to the sibling — keep it visible.
        let block_belongs_to_focus =
            focus_span.start_line < span.start_line && focus_span.start_line >= block_start;
        if !block_belongs_to_focus {
            collapsed_lines.extend(block_start..span.start_line);
        }
        collapsed_lines.extend(span.start_line..=span.end_line.min(total_lines));
    }
    collapsed_lines.sort_unstable();
    collapsed_lines.dedup();

    let in_focus =
        |line_no: usize| line_no >= focus_span.start_line && line_no <= focus_span.end_line;

    let mut out = String::new();
    let mut emitted = 0usize;
    let mut truncated = 0usize;
    for (idx, line) in content.lines().enumerate() {
        let line_no = idx + 1;
        if emitted >= max_lines {
            truncated = total_lines - emitted;
            break;
        }
        if let Some(&item_idx) = signature_at.get(&line_no) {
            let item = &collapsed[item_idx];
            let entry = if item.line_count > 1 {
                format!(
                    "{} {{ … {} lines }}  [expand: {}#{}]",
                    item.signature, item.line_count, file_label, item.name
                )
            } else {
                item.signature.clone()
            };
            out.push_str(&line_no.to_string());
            out.push(' ');
            out.push_str(&entry);
            out.push('\n');
            emitted += 1;
            continue;
        }
        if collapsed_lines.binary_search(&line_no).is_ok() && !in_focus(line_no) {
            continue;
        }
        out.push_str(&line_no.to_string());
        out.push(' ');
        out.push_str(line);
        out.push('\n');
        emitted += 1;
    }
    if truncated > 0 {
        out.push_str(&format!(
            "+{truncated} more lines (elided view truncated at max_lines={max_lines})"
        ));
    }
    Ok(ElidedOutput {
        body: out.trim_end_matches('\n').to_string(),
        expand: format!("{file_label}#{focus}"),
        focus_start_line: focus_span.start_line,
        focus_end_line: focus_span.end_line,
        collapsed,
    })
}

// =============================================================================
// Session read ledger (brief §4.1, §6.9)
// =============================================================================

/// Verdict for a consult against the session ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConsultVerdict {
    /// The exact requested range was already served from identical content.
    Unchanged {
        /// Read ordinal that served this content.
        since_ordinal: u64,
        /// Requested range that is fully covered.
        start_line: usize,
        end_line: usize,
    },
    /// Part of the requested range is new; only the unseen subranges need
    /// to be served.
    PartialOverlap {
        /// Merged, disjoint unseen subranges (1-based, inclusive).
        unseen: Vec<(usize, usize)>,
        /// Ordinal of the read that served the overlapping part.
        since_ordinal: u64,
    },
    /// Nothing useful is cached for this file/range.
    New,
}

/// Per-file ledger record for one session.
#[derive(Debug, Clone)]
struct FileRecord {
    /// blake3 hex of the full content last served.
    content_hash: String,
    /// (read ordinal, start line, end line) — bounded, oldest dropped first.
    reads: Vec<(u64, usize, usize)>,
    /// Set when an edit touched the file after the last recorded read.
    invalidated: bool,
}

/// Per-session state.
#[derive(Debug, Default)]
struct SessionState {
    files: HashMap<PathBuf, FileRecord>,
    /// Session-wide monotonically increasing read ordinal.
    next_ordinal: u64,
    /// (ordinal, path) edit events, oldest first, bounded.
    edits: Vec<(u64, PathBuf)>,
}

/// Bounded per-session ledger of served reads.
///
/// Keys are MCP session ids. When `max_sessions` distinct sessions are
/// resident, the oldest-inserted session is evicted to admit a new one; each
/// file remembers at most `max_reads_per_file` reads / edits. No structure
/// here grows with the number of tool calls.
pub(crate) struct SessionLedger {
    inner: Mutex<LedgerInner>,
}

struct LedgerInner {
    sessions: HashMap<Arc<str>, SessionState>,
    /// Session ids in insertion order (eviction order).
    order: VecDeque<Arc<str>>,
    max_sessions: usize,
    max_reads_per_file: usize,
}

impl Default for SessionLedger {
    fn default() -> Self {
        Self::new(64, 64)
    }
}

impl SessionLedger {
    /// `max_sessions` bounds distinct session ids; `max_reads_per_file`
    /// bounds the per-file read/edit lists.
    pub(crate) fn new(max_sessions: usize, max_reads_per_file: usize) -> Self {
        Self {
            inner: Mutex::new(LedgerInner {
                sessions: HashMap::new(),
                order: VecDeque::new(),
                max_sessions,
                max_reads_per_file,
            }),
        }
    }

    /// Consult whether `(session, path, hash, range)` was already served.
    pub(crate) fn consult(
        &self,
        session: &str,
        path: &Path,
        content_hash: &str,
        start_line: usize,
        end_line: usize,
    ) -> ConsultVerdict {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(state) = inner.sessions.get(session) else {
            return ConsultVerdict::New;
        };
        let Some(record) = state.files.get(path) else {
            return ConsultVerdict::New;
        };
        if record.invalidated || record.content_hash != content_hash {
            return ConsultVerdict::New;
        }
        let mut covering: Option<u64> = None;
        let mut overlap_ordinal: Option<u64> = None;
        // Start with the requested range; every recorded read eats the part
        // of it that it served.
        let mut unseen: Vec<(usize, usize)> = vec![(start_line, end_line)];
        for &(ordinal, read_start, read_end) in &record.reads {
            let mut next_unseen: Vec<(usize, usize)> = Vec::new();
            for &(s, e) in &unseen {
                if read_start <= s && e <= read_end {
                    covering = Some(covering.map_or(ordinal, |prev: u64| prev.min(ordinal)));
                } else if s <= read_end && read_start <= e {
                    if s < read_start {
                        next_unseen.push((s, read_start - 1));
                    }
                    if e > read_end {
                        next_unseen.push((read_end + 1, e));
                    }
                    overlap_ordinal = Some(overlap_ordinal.unwrap_or(ordinal));
                } else {
                    next_unseen.push((s, e));
                }
            }
            unseen = merge_ranges(next_unseen);
            if unseen.is_empty() {
                break;
            }
        }
        if let Some(ordinal) = covering {
            return ConsultVerdict::Unchanged {
                since_ordinal: ordinal,
                start_line,
                end_line,
            };
        }
        if let Some(ordinal) = overlap_ordinal {
            return ConsultVerdict::PartialOverlap {
                unseen,
                since_ordinal: ordinal,
            };
        }
        ConsultVerdict::New
    }

    /// Record a served read; returns the session-wide read ordinal. A
    /// content-hash change or a recorded edit resets the file's read list.
    pub(crate) fn record(
        &self,
        session: &str,
        path: &Path,
        content_hash: &str,
        start_line: usize,
        end_line: usize,
    ) -> u64 {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cap = inner.max_reads_per_file;
        let key: Arc<str> = Arc::from(session);
        if !inner.sessions.contains_key(&key) {
            // Admit at most `max_sessions` resident sessions: evict in
            // insertion order. `key` is not resident yet, so it can never
            // evict itself.
            while inner.sessions.len() >= inner.max_sessions {
                match inner.order.pop_front() {
                    Some(oldest) => {
                        inner.sessions.remove(oldest.as_ref());
                    }
                    None => break,
                }
            }
            inner.order.push_back(key.clone());
        }
        let state = inner.sessions.entry(key).or_default();
        state.next_ordinal += 1;
        let ordinal = state.next_ordinal;
        let entry = state.files.entry(path.to_path_buf()).or_insert(FileRecord {
            content_hash: content_hash.to_string(),
            reads: Vec::new(),
            invalidated: false,
        });
        if entry.content_hash != content_hash || entry.invalidated {
            entry.content_hash = content_hash.to_string();
            entry.reads.clear();
            entry.invalidated = false;
        }
        entry.reads.push((ordinal, start_line, end_line));
        if entry.reads.len() > cap {
            entry.reads.drain(0..entry.reads.len() - cap);
        }
        ordinal
    }

    /// Mark a file edited in a session: cached reads become invalid and the
    /// edit is listed by the session-reads resource. No-op for unknown
    /// sessions (nothing cached to invalidate).
    pub(crate) fn record_edit(&self, session: &str, path: &Path) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cap = inner.max_reads_per_file;
        let Some(state) = inner.sessions.get_mut(session) else {
            return;
        };
        state.next_ordinal += 1;
        let ordinal = state.next_ordinal;
        let entry = state.files.entry(path.to_path_buf()).or_insert(FileRecord {
            content_hash: String::new(),
            reads: Vec::new(),
            invalidated: false,
        });
        entry.invalidated = true;
        state.edits.push((ordinal, path.to_path_buf()));
        if state.edits.len() > cap {
            state.edits.drain(0..state.edits.len() - cap);
        }
    }

    /// Render the `leindex://session/reads` resource body for `session`.
    /// `None` (stdio, direct CLI) renders the process-default session.
    pub(crate) fn render(&self, session: Option<&str>) -> String {
        let (entries, edits) = self.summarize_session(session.unwrap_or("__process__"));
        render_session_reads(&entries, &edits)
    }

    /// Per-file read entries `(path, [(ordinal, start, end)])` in first-read
    /// order, plus the session's bounded edit list — the data behind the
    /// `leindex://session/reads` resource.
    pub(crate) fn summarize_session(
        &self,
        session: &str,
    ) -> (
        Vec<(PathBuf, Vec<(u64, usize, usize)>)>,
        Vec<(u64, PathBuf)>,
    ) {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(state) = inner.sessions.get(session) else {
            return (Vec::new(), Vec::new());
        };
        let mut files: Vec<(PathBuf, Vec<(u64, usize, usize)>)> = state
            .files
            .iter()
            .filter(|(_, record)| !record.reads.is_empty())
            .map(|(path, record)| (path.clone(), record.reads.clone()))
            .collect();
        files.sort_by_key(|(_, reads)| reads.first().map(|r| r.0).unwrap_or(0));
        (files, state.edits.clone())
    }
}

/// Merge overlapping/adjacent inclusive ranges into disjoint sorted ranges.
pub(crate) fn merge_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1.saturating_add(1) => {
                last.1 = last.1.max(end);
            }
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// Format the `leindex://session/reads` resource body: one line per file,
/// ranges merged, read ordinals attached, edits listed.
pub(crate) fn render_session_reads(
    entries: &[(PathBuf, Vec<(u64, usize, usize)>)],
    edits: &[(u64, PathBuf)],
) -> String {
    if entries.is_empty() && edits.is_empty() {
        return "Session has no recorded reads or edits yet.".to_string();
    }
    let mut out = String::from("# Reads and edits this session\n");
    for (path, reads) in entries {
        let ordinals: Vec<String> = reads.iter().map(|(o, _, _)| o.to_string()).collect();
        let ranges = merge_ranges(reads.iter().map(|&(_, s, e)| (s, e)).collect());
        let ranges_text = ranges
            .iter()
            .map(|(s, e)| {
                if s == e {
                    s.to_string()
                } else {
                    format!("{s}-{e}")
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&format!(
            "- {} reads#{} L{}\n",
            path.display(),
            ordinals.join(","),
            ranges_text
        ));
    }
    for (ordinal, path) in edits {
        out.push_str(&format!("- edited {} at #{}\n", path.display(), ordinal));
    }
    out
}

/// The process-wide ledger used by the MCP transports. Bounded: at most 64
/// live sessions, at most 64 reads remembered per file per session.
pub(crate) fn session_ledger() -> &'static SessionLedger {
    static LEDGER: OnceLock<SessionLedger> = OnceLock::new();
    LEDGER.get_or_init(SessionLedger::default)
}

/// The JSON shape of an `unchanged` verdict — bytes-free on purpose.
pub(crate) fn unchanged_payload(
    file_path: &str,
    since_ordinal: u64,
    start_line: usize,
    end_line: usize,
) -> Value {
    serde_json::json!({
        "file_path": file_path,
        "status": "unchanged",
        "note": format!("unchanged since read #{since_ordinal} (lines {start_line}-{end_line}); pass fresh:true to force"),
        "unchanged_since_read": since_ordinal,
        "lines": format!("{start_line}-{end_line}"),
    })
}

#[cfg(test)]
#[path = "read_paths_test.rs"]
mod tests;
