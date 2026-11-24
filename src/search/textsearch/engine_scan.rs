//! Per-file read and hit-construction primitives for text search, kept out
//! of `engine.rs` so each file stays under the repository's line budget.
//!
//! All types come from `super`; `engine.rs` re-exports the public pieces.

use super::*;
use std::path::Path;

pub(crate) fn context_lines(
    data: &[u8],
    line_start: usize,
    line_end: usize,
    n: usize,
    max: usize,
) -> (Vec<String>, Vec<String>) {
    let mut before = Vec::new();
    let mut cursor = line_start;
    while before.len() < n && cursor > 0 {
        let prev_end = cursor - 1;
        let prev_start = memchr::memrchr(b'\n', &data[..prev_end]).map_or(0, |p| p + 1);
        before.push(clip_line(&data[prev_start..prev_end], 0, max));
        cursor = prev_start;
    }
    before.reverse();
    let mut after = Vec::new();
    let mut cursor = line_end + 1;
    while after.len() < n && cursor <= data.len() && line_end < data.len() {
        let next_end = memchr::memchr(b'\n', &data[cursor.min(data.len())..])
            .map_or(data.len(), |p| cursor + p);
        after.push(clip_line(&data[cursor.min(data.len())..next_end], 0, max));
        cursor = next_end + 1;
        if next_end >= data.len() {
            break;
        }
    }
    (before, after)
}

/// Why a candidate yielded no bytes to scan.
///
/// The two halves mean opposite things to a caller: [`NotScannable`] input is
/// *legitimately* unmatched — nothing a pattern could match lives in a binary
/// or an oversized file, so it is a clean miss — while [`Unreadable`] names a
/// file this scan never examined, whose matches are unknown rather than absent.
/// Collapsing them into one `None` let a structural search answer
/// `has_more: false` over a file it had failed to open (Codex P2).
///
/// [`NotScannable`]: ScanRefusal::NotScannable
/// [`Unreadable`]: ScanRefusal::Unreadable
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanRefusal {
    /// Policy says this input is not source we scan: not a regular file,
    /// oversized, binary, or not valid UTF-8.
    NotScannable,
    /// The file could not be read — a stat or open failure such as denied
    /// permissions or a transient I/O error. Its contents are unknown, so a
    /// caller must not treat it as having been searched and come up empty.
    Unreadable,
}

pub(crate) fn read_scan_file(path: &Path) -> Result<Vec<u8>, ScanRefusal> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        // A path that no longer exists has no contents to find, so it is not an
        // unknown file: reporting it unreadable would pin a caller's
        // "incomplete" flag forever on a broken symlink or a file that moved
        // between the walk and this open.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ScanRefusal::NotScannable);
        }
        Err(_) => return Err(ScanRefusal::Unreadable),
    };
    if !meta.is_file() || meta.len() > MAX_SCAN_FILE_BYTES {
        return Err(ScanRefusal::NotScannable);
    }
    let data = std::fs::read(path).map_err(|error| {
        // Same race as the stat above: a file that vanished between the walk
        // and this read has no contents to find, so it is a clean miss, not an
        // unknown file whose absence must pin `has_more`.
        if error.kind() == std::io::ErrorKind::NotFound {
            ScanRefusal::NotScannable
        } else {
            ScanRefusal::Unreadable
        }
    })?;
    if looks_binary(&data) {
        return Err(ScanRefusal::NotScannable);
    }
    Ok(data)
}

/// Read a file as text under the same policy a scan uses: regular file, no
/// bigger than [`MAX_SCAN_FILE_BYTES`], not binary.
///
/// Structural search needs the source as `&str` (ast-grep's `StrDoc`), so the
/// bytes from [`read_scan_file`] are converted here rather than duplicating the
/// size and binary rules in a second module where they could drift.
/// Non-UTF-8 files are skipped: they cannot be handed to ast-grep.
///
/// Use [`read_scannable_reason`] when the caller must tell a policy refusal
/// from a file it failed to read.
pub fn read_scannable(path: &Path) -> Option<String> {
    read_scannable_reason(path).ok()
}

/// [`read_scannable`], keeping the reason so a caller can distinguish policy
/// refusals from files it could not open.
pub fn read_scannable_reason(path: &Path) -> Result<String, ScanRefusal> {
    match String::from_utf8(read_scan_file(path)?) {
        Ok(text) => Ok(text),
        // Valid UTF-8 is a precondition of parsing, not a read failure: the
        // bytes were read fine and simply cannot hold an ast-grep match.
        Err(_) => Err(ScanRefusal::NotScannable),
    }
}

pub(crate) fn resolve_symbol_index<'a>(
    index: Option<&'a TextIndex>,
    candidate: &Candidate,
    options: &SearchOptions,
) -> Option<(&'a TextIndex, u32)> {
    match (index, candidate.id, candidate.dirty) {
        (Some(index), Some(id), false) if options.want_symbols && index.has_symbols() => {
            Some((index, id))
        }
        _ => None,
    }
}

pub(crate) fn build_hit(
    data: &[u8],
    start: usize,
    line_start: usize,
    line_no: u32,
    options: &SearchOptions,
    symbol: Option<(String, &'static str)>,
) -> Hit {
    let line_end = memchr::memchr(b'\n', &data[start..]).map_or(data.len(), |p| start + p);
    let (before, after) = if options.context > 0 {
        context_lines(
            data,
            line_start,
            line_end,
            options.context,
            options.max_line_chars,
        )
    } else {
        (Vec::new(), Vec::new())
    };
    Hit {
        line: line_no,
        col: (start - line_start + 1) as u32,
        text: clip_line(
            &data[line_start..line_end],
            start - line_start,
            options.max_line_chars,
        ),
        before,
        after,
        symbol,
    }
}

/// Whether `path` still resolves inside the canonical `root`.
///
/// Index-seeded paths were recorded at inventory time and may have been
/// re-pointed since; a tracked regular file swapped for a symlink (at the
/// leaf or via a parent component) would otherwise be followed into content
/// outside the root. Canonicalization never follows a trailing symlink whose
/// target is missing, and a resolution that escapes `root` fails the prefix
/// check — both fail closed.
pub(crate) fn path_stays_inside(
    path: &std::path::Path,
    root_canonical: Option<&std::path::Path>,
) -> bool {
    root_canonical.is_some_and(|root| {
        std::fs::canonicalize(path).is_ok_and(|resolved| resolved.starts_with(root))
    })
}

/// Whether an index-seeded candidate may still be read: its path was
/// recorded at inventory time and may have been re-pointed since, and a
/// tracked regular file swapped for a symlink (at the leaf or via a parent
/// component) would be followed into content outside the root. Fresh-walk
/// candidates cannot escape (the walk does not follow links) and explicit
/// file roots are caller-requested paths, so only index-seeded candidates
/// are revalidated.
pub(crate) fn index_candidate_contained(
    candidate: &Candidate,
    root_canonical: Option<&std::path::Path>,
) -> bool {
    match (candidate.id, root_canonical) {
        (Some(_), Some(root)) => path_stays_inside(&candidate.abs, Some(root)),
        _ => true,
    }
}
