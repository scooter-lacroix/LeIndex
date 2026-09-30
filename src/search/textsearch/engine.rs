//! Search execution: candidates from the trigram index (plus anything that
//! changed since it was built, plus anything never indexed), verified against
//! the live file contents with the real regex.
//!
//! Correctness rules:
//! * Hits always come from the file on disk *now*; the index only decides which
//!   files are worth reading. A file edited after indexing is detected by
//!   size/mtime and scanned directly, so results are never stale.
//! * A root with no index (any directory outside the workspace) is scanned
//!   live, in parallel, with no side effects and no setup.
//! * Order is deterministic (root order, then path, then line), so
//!   `offset`/`limit` paging is stable.

use super::glob::FileFilter;
use super::index::{FLAG_ALWAYS_SCAN, FLAG_BINARY, FileInput, SymbolSpan, TextIndex, write_index};
use super::plan::{self, Expr};
use super::trigram::Extractor;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Files larger than this are not trigram-indexed (they are always scanned).
pub const MAX_INDEXED_FILE_BYTES: u64 = 1 << 20;
/// Files larger than this are never read by a search.
pub const MAX_SCAN_FILE_BYTES: u64 = 16 << 20;
/// Upper bound on files recorded in one index.
pub const MAX_INDEX_FILES: usize = 300_000;
const BINARY_SNIFF: usize = 8192;

/// Directories never worth searching, at any depth.
const NEVER_WALK: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".leindex",
    "node_modules",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
];

/// Worker threads for scans: `LEINDEX_FIND_THREADS`, else `min(4, cores)`.
pub fn worker_threads() -> usize {
    std::env::var("LEINDEX_FIND_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(2, usize::from)
                .min(4)
        })
}

fn pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(worker_threads())
            .thread_name(|i| format!("leindex-find-{i}"))
            .build()
            .expect("failed to build the text-search thread pool")
    })
}

fn signature(meta: &std::fs::Metadata) -> (u64, i64) {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos().min(i64::MAX as u128) as i64);
    (meta.len(), mtime)
}

// ── Inventory ────────────────────────────────────────────────────────────────

fn git_inventory(root: &Path) -> Option<Vec<String>> {
    let output = std::process::Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut files: Vec<String> = output
        .stdout
        .split(|b| *b == 0)
        .filter(|raw| !raw.is_empty())
        .filter_map(|raw| std::str::from_utf8(raw).ok())
        .filter(|rel| !rel.split('/').any(|seg| seg == ".leindex" || seg == ".git"))
        .map(str::to_string)
        .collect();
    files.sort_unstable();
    files.dedup();
    Some(files)
}

fn walk_inventory(root: &Path, limit: usize, deadline: Option<Instant>) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        if out.len() >= limit || deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let rel = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            if kind.is_dir() {
                if !NEVER_WALK.contains(&name) {
                    stack.push((entry.path(), rel));
                }
            } else if kind.is_file() {
                out.push(rel);
            }
        }
    }
    out.sort_unstable();
    out
}

/// Files under `dir`, relative to it, sorted, forward slashes. Uses git's own
/// ignore rules inside a repository; elsewhere a plain walk that skips VCS
/// metadata and package caches.
pub fn list_files(dir: &Path, limit: usize, deadline: Option<Instant>) -> Vec<String> {
    let mut files = git_inventory(dir).unwrap_or_else(|| walk_inventory(dir, limit, deadline));
    files.truncate(limit);
    files
}

// ── Index building ───────────────────────────────────────────────────────────

/// What a build produced.
#[derive(Debug, Clone, Default)]
pub struct BuildStats {
    /// Files in the table.
    pub files: usize,
    /// Files with trigrams.
    pub indexed: usize,
    /// Binary files (recorded, never searched).
    pub binary: usize,
    /// Oversized files (recorded, always scanned).
    pub large: usize,
    /// Distinct trigrams.
    pub trigrams: usize,
    /// Index size on disk.
    pub bytes: usize,
    /// Build wall time.
    pub millis: u128,
}

fn looks_binary(data: &[u8]) -> bool {
    data[..data.len().min(BINARY_SNIFF)].contains(&0)
}

/// Build `out` for `root`. `symbols` maps root-relative paths (forward
/// slashes) to their spans; pass an empty map when none are known.
pub fn build_index(
    root: &Path,
    out: &Path,
    symbols: HashMap<String, Vec<SymbolSpan>>,
) -> io::Result<BuildStats> {
    let started = Instant::now();
    let rels = list_files(root, MAX_INDEX_FILES, None);
    let has_symbols = !symbols.is_empty();
    let symbols = Arc::new(symbols);
    let inputs: Vec<Option<FileInput>> = pool().install(|| {
        rels.par_iter()
            .map_init(Extractor::new, |extractor, rel| {
                let path = root.join(rel);
                let meta = std::fs::metadata(&path).ok()?;
                if !meta.is_file() {
                    return None;
                }
                let (size, mtime_ns) = signature(&meta);
                let mut input = FileInput {
                    rel_path: rel.clone(),
                    size,
                    mtime_ns,
                    flags: 0,
                    trigrams: Vec::new(),
                    symbols: symbols.get(rel).cloned().unwrap_or_default(),
                };
                if size > MAX_INDEXED_FILE_BYTES {
                    input.flags = FLAG_ALWAYS_SCAN;
                    return Some(input);
                }
                let data = std::fs::read(&path).ok()?;
                if looks_binary(&data) {
                    input.flags = FLAG_BINARY;
                    input.symbols.clear();
                } else {
                    input.trigrams = extractor.distinct(&data);
                }
                Some(input)
            })
            .collect()
    });
    let files: Vec<FileInput> = inputs.into_iter().flatten().collect();
    write_index(out, &files, has_symbols)?;
    let index = TextIndex::open(out)?;
    Ok(BuildStats {
        files: files.len(),
        indexed: files.iter().filter(|f| f.flags == 0).count(),
        binary: files.iter().filter(|f| f.flags & FLAG_BINARY != 0).count(),
        large: files
            .iter()
            .filter(|f| f.flags & FLAG_ALWAYS_SCAN != 0)
            .count(),
        trigrams: index.trigram_count() as usize,
        bytes: index.bytes(),
        millis: started.elapsed().as_millis(),
    })
}

// ── Freshness ────────────────────────────────────────────────────────────────

type DirtyCache = Mutex<HashMap<PathBuf, DirtyEntry>>;

struct DirtyEntry {
    at: Instant,
    /// How long computing it took: cheap checks are never cached, so a file
    /// saved a moment ago is always seen.
    cost: Duration,
    files: Arc<Vec<String>>,
}

/// Reuse a freshness result only if computing it was this slow...
const DIRTY_CACHE_MIN_COST: Duration = Duration::from_millis(50);
/// ...and it is at most this old (coalesces bursts on very large trees).
const DIRTY_CACHE_TTL: Duration = Duration::from_millis(500);

fn dirty_cache() -> &'static DirtyCache {
    static CACHE: OnceLock<DirtyCache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Forget any cached freshness for `root`. Call after this process changes
/// files under it (edits, writes) so the very next search sees them.
pub fn invalidate_freshness(root: &Path) {
    if let Ok(mut guard) = dirty_cache().lock() {
        guard.retain(|cached, _| !cached.starts_with(root) && !root.starts_with(cached));
    }
}

/// Relative paths that are new, or differ in size/mtime from the index.
pub fn dirty_files(root: &Path, index: &TextIndex) -> Arc<Vec<String>> {
    if let Ok(guard) = dirty_cache().lock() {
        if let Some(entry) = guard.get(root) {
            if entry.cost >= DIRTY_CACHE_MIN_COST && entry.at.elapsed() < DIRTY_CACHE_TTL {
                return Arc::clone(&entry.files);
            }
        }
    }
    let started = Instant::now();
    let rels = list_files(root, MAX_INDEX_FILES * 2, None);
    let dirty: Vec<String> = pool().install(|| {
        rels.par_iter()
            .filter(|rel| {
                let Some(id) = index.id_of(rel) else {
                    return true; // never indexed
                };
                let Some(known) = index.file(id) else {
                    return true;
                };
                match std::fs::metadata(root.join(rel.as_str())) {
                    Ok(meta) => signature(&meta) != (known.size, known.mtime_ns),
                    Err(_) => false, // deleted: nothing to search
                }
            })
            .cloned()
            .collect()
    });
    let files = Arc::new(dirty);
    if let Ok(mut guard) = dirty_cache().lock() {
        guard.insert(
            root.to_path_buf(),
            DirtyEntry {
                at: Instant::now(),
                cost: started.elapsed(),
                files: Arc::clone(&files),
            },
        );
    }
    files
}

// ── Query ────────────────────────────────────────────────────────────────────

/// Case handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaseMode {
    /// Insensitive unless the pattern contains an uppercase letter.
    #[default]
    Smart,
    /// Exact case.
    Sensitive,
    /// Ignore case.
    Insensitive,
}

/// What to look for.
#[derive(Debug, Clone)]
pub struct Query {
    /// Literal text, or a regex when `regex` is set.
    pub pattern: String,
    /// Interpret `pattern` as a regular expression.
    pub regex: bool,
    /// Case rule.
    pub case: CaseMode,
    /// Match whole words only.
    pub word: bool,
}

/// A compiled query.
pub struct Compiled {
    regex: regex::bytes::Regex,
    expr: Expr,
    /// The case-insensitivity actually applied.
    pub case_insensitive: bool,
}

fn has_uppercase(pattern: &str, is_regex: bool) -> bool {
    let mut escaped = false;
    for c in pattern.chars() {
        if is_regex && escaped {
            escaped = false;
            continue;
        }
        if is_regex && c == '\\' {
            escaped = true;
            continue;
        }
        if c.is_uppercase() {
            return true;
        }
    }
    false
}

impl Query {
    /// Compile the regex and the trigram plan.
    pub fn compile(&self) -> Result<Compiled, String> {
        if self.pattern.is_empty() {
            return Err("pattern must not be empty".to_string());
        }
        let case_insensitive = match self.case {
            CaseMode::Insensitive => true,
            CaseMode::Sensitive => false,
            CaseMode::Smart => !has_uppercase(&self.pattern, self.regex),
        };
        let body = if self.regex {
            self.pattern.clone()
        } else {
            regex::escape(&self.pattern)
        };
        let full = if self.word {
            format!(r"\b(?:{body})\b")
        } else {
            body
        };
        let regex = regex::bytes::RegexBuilder::new(&full)
            .case_insensitive(case_insensitive)
            .multi_line(true)
            .size_limit(64 << 20)
            .build()
            .map_err(|e| format!("invalid pattern: {e}"))?;
        let expr =
            plan::plan(&full, case_insensitive).map_err(|e| format!("invalid pattern: {e}"))?;
        Ok(Compiled {
            regex,
            expr,
            case_insensitive,
        })
    }
}

/// A place to search.
pub struct RootSpec {
    /// Directory (or single file) to search.
    pub root: PathBuf,
    /// Its trigram index, if one exists.
    pub index: Option<Arc<TextIndex>>,
    /// Include/exclude/scope filter over root-relative paths.
    pub filter: FileFilter,
}

/// Result-shaping options.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Hits to skip.
    pub offset: usize,
    /// Hits to return (`None` = all).
    pub limit: Option<usize>,
    /// Reported hits per file (`0` = unlimited); further matches are counted.
    pub per_file_cap: usize,
    /// Context lines around each hit.
    pub context: usize,
    /// Maximum characters of a hit line.
    pub max_line_chars: usize,
    /// Wall-clock budget.
    pub deadline: Option<Instant>,
    /// Extract hit text (false for count-only modes).
    pub collect_hits: bool,
    /// Resolve enclosing symbols (indexed, unmodified files only).
    pub want_symbols: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: Some(50),
            per_file_cap: 50,
            context: 0,
            max_line_chars: 200,
            deadline: None,
            collect_hits: true,
            want_symbols: true,
        }
    }
}

/// One matching line.
#[derive(Debug, Clone)]
pub struct Hit {
    /// 1-based line.
    pub line: u32,
    /// 1-based byte column of the first match on the line.
    pub col: u32,
    /// The line (trimmed around the match when very long).
    pub text: String,
    /// Context before.
    pub before: Vec<String>,
    /// Context after.
    pub after: Vec<String>,
    /// Enclosing symbol `(name, kind)`.
    pub symbol: Option<(String, &'static str)>,
}

/// Matches in one file.
#[derive(Debug, Clone, Default)]
pub struct FileResult {
    /// Root-relative path.
    pub rel: String,
    /// Matching lines in the file (may exceed `hits.len()`).
    pub match_lines: usize,
    /// Reported hits.
    pub hits: Vec<Hit>,
    /// Enclosing-symbol tallies `(name, kind, matching lines)`.
    pub symbols: Vec<(String, &'static str, usize)>,
    /// Symbols were unavailable because the file changed after indexing.
    pub symbols_stale: bool,
}

/// Results for one root.
#[derive(Debug, Default)]
pub struct RootOutput {
    /// The searched root.
    pub root: PathBuf,
    /// A trigram index narrowed the scan.
    pub used_index: bool,
    /// Files with at least one match, in path order.
    pub files: Vec<FileResult>,
}

/// Whole-search totals.
#[derive(Debug, Default, Clone)]
pub struct SearchStats {
    /// Files considered after index/filter narrowing.
    pub candidates: usize,
    /// Files actually read.
    pub scanned: usize,
    /// Files modified/added since their index was built.
    pub dirty: usize,
    /// Files with a match.
    pub files_matched: usize,
    /// Matching lines found.
    pub match_lines: usize,
    /// Wall time.
    pub millis: u128,
}

/// Everything a search returns.
#[derive(Debug, Default)]
pub struct SearchOutput {
    /// Per-root results (hit lists already windowed by offset/limit).
    pub roots: Vec<RootOutput>,
    /// Every candidate was scanned (no early stop, no deadline).
    pub complete: bool,
    /// More hits exist beyond `offset + limit`.
    pub has_more: bool,
    /// Hits returned.
    pub returned: usize,
    /// Totals.
    pub stats: SearchStats,
}

struct Candidate {
    rel: String,
    abs: PathBuf,
    id: Option<u32>,
    dirty: bool,
}

fn root_candidates(
    spec: &RootSpec,
    compiled: &Compiled,
    deadline: Option<Instant>,
    stats: &mut SearchStats,
) -> Vec<Candidate> {
    let mut set: BTreeMap<String, Candidate> = BTreeMap::new();
    let is_file_root = spec.root.is_file();

    if is_file_root {
        let name = spec
            .root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        set.insert(
            name.clone(),
            Candidate {
                rel: name,
                abs: spec.root.clone(),
                id: None,
                dirty: true,
            },
        );
    } else if let Some(index) = &spec.index {
        let narrowed = plan::candidates(index, &compiled.expr);
        let add = |id: u32, set: &mut BTreeMap<String, Candidate>| {
            if let Some(meta) = index.file(id) {
                if meta.flags & FLAG_BINARY == 0 {
                    set.insert(
                        meta.path.to_string(),
                        Candidate {
                            rel: meta.path.to_string(),
                            abs: spec.root.join(meta.path),
                            id: Some(id),
                            dirty: false,
                        },
                    );
                }
            }
        };
        match narrowed {
            Some(ids) => ids.into_iter().for_each(|id| add(id, &mut set)),
            None => index.file_ids().for_each(|id| add(id, &mut set)),
        }
        for id in index.file_ids() {
            if index
                .file(id)
                .is_some_and(|m| m.flags & FLAG_ALWAYS_SCAN != 0)
            {
                add(id, &mut set);
            }
        }
        let dirty = dirty_files(&spec.root, index);
        stats.dirty += dirty.len();
        for rel in dirty.iter() {
            set.insert(
                rel.clone(),
                Candidate {
                    rel: rel.clone(),
                    abs: spec.root.join(rel),
                    id: index.id_of(rel),
                    dirty: true,
                },
            );
        }
    } else {
        for rel in list_files(&spec.root, 1_000_000, deadline) {
            set.insert(
                rel.clone(),
                Candidate {
                    abs: spec.root.join(&rel),
                    rel,
                    id: None,
                    dirty: true,
                },
            );
        }
    }
    set.into_values()
        .filter(|c| is_file_root || spec.filter.allows(&c.rel))
        .collect()
}

fn clip_line(line: &[u8], col: usize, max_chars: usize) -> String {
    let trimmed = if line.ends_with(b"\r") {
        &line[..line.len() - 1]
    } else {
        line
    };
    if trimmed.len() <= max_chars {
        return String::from_utf8_lossy(trimmed).into_owned();
    }
    let start = col.saturating_sub(max_chars / 3).min(trimmed.len());
    let end = (start + max_chars).min(trimmed.len());
    let mut text = String::from_utf8_lossy(&trimmed[start..end]).into_owned();
    if start > 0 {
        text.insert(0, '…');
    }
    if end < trimmed.len() {
        text.push('…');
    }
    text
}

fn context_lines(
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

fn scan_file(
    candidate: &Candidate,
    compiled: &Compiled,
    options: &SearchOptions,
    index: Option<&TextIndex>,
) -> Option<FileResult> {
    let meta = std::fs::metadata(&candidate.abs).ok()?;
    if !meta.is_file() || meta.len() > MAX_SCAN_FILE_BYTES {
        return None;
    }
    let data = std::fs::read(&candidate.abs).ok()?;
    if looks_binary(&data) {
        return None;
    }
    let symbol_index = match (index, candidate.id, candidate.dirty) {
        (Some(index), Some(id), false) if options.want_symbols && index.has_symbols() => {
            Some((index, id))
        }
        _ => None,
    };
    let mut result = FileResult {
        rel: candidate.rel.clone(),
        symbols_stale: options.want_symbols && candidate.dirty && candidate.id.is_some(),
        ..Default::default()
    };
    let mut tallies: BTreeMap<(String, &'static str), usize> = BTreeMap::new();
    let (mut line_no, mut counted_to, mut last_line_start) = (1u32, 0usize, usize::MAX);
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
            .map(|(name, kind, _, _)| (name, kind));
        if let Some(symbol) = &symbol {
            *tallies.entry(symbol.clone()).or_default() += 1;
        }
        let reportable = options.collect_hits
            && (options.per_file_cap == 0 || result.hits.len() < options.per_file_cap);
        if !reportable {
            continue;
        }
        let line_end = memchr::memchr(b'\n', &data[start..]).map_or(data.len(), |p| start + p);
        let (before, after) = if options.context > 0 {
            context_lines(
                &data,
                line_start,
                line_end,
                options.context,
                options.max_line_chars,
            )
        } else {
            (Vec::new(), Vec::new())
        };
        result.hits.push(Hit {
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
        });
    }
    if result.match_lines == 0 {
        return None;
    }
    result.symbols = tallies
        .into_iter()
        .map(|((name, kind), count)| (name, kind, count))
        .collect();
    Some(result)
}

/// Run `query` over `roots`.
pub fn search(roots: &[RootSpec], compiled: &Compiled, options: &SearchOptions) -> SearchOutput {
    let started = Instant::now();
    let mut output = SearchOutput {
        complete: true,
        ..Default::default()
    };
    let window_end = options
        .limit
        .map(|limit| options.offset.saturating_add(limit));
    let mut seen_hits = 0usize;
    let chunk_size = worker_threads() * 8;

    'roots: for spec in roots {
        let mut root_out = RootOutput {
            root: spec.root.clone(),
            used_index: spec.index.is_some() && !spec.root.is_file(),
            files: Vec::new(),
        };
        let candidates = root_candidates(spec, compiled, options.deadline, &mut output.stats);
        output.stats.candidates += candidates.len();

        for chunk in candidates.chunks(chunk_size) {
            if options.deadline.is_some_and(|d| Instant::now() >= d) {
                output.complete = false;
                output.roots.push(std::mem::take(&mut root_out));
                break 'roots;
            }
            let scanned: Vec<Option<FileResult>> = pool().install(|| {
                chunk
                    .par_iter()
                    .map(|c| scan_file(c, compiled, options, spec.index.as_deref()))
                    .collect()
            });
            output.stats.scanned += chunk.len();
            for mut file in scanned.into_iter().flatten() {
                output.stats.files_matched += 1;
                output.stats.match_lines += file.match_lines;
                // Window the reported hits by offset/limit.
                let mut windowed = Vec::with_capacity(file.hits.len());
                for hit in file.hits.drain(..) {
                    let position = seen_hits;
                    seen_hits += 1;
                    if position >= options.offset && window_end.is_none_or(|end| position < end) {
                        windowed.push(hit);
                    }
                }
                // Hits past the per-file cap still occupy no positions.
                file.hits = windowed;
                output.returned += file.hits.len();
                let keep = !options.collect_hits || !file.hits.is_empty();
                if keep {
                    root_out.files.push(file);
                }
            }
            if options.collect_hits && window_end.is_some_and(|end| seen_hits > end) {
                output.has_more = true;
                output.complete = false;
                output.roots.push(std::mem::take(&mut root_out));
                break 'roots;
            }
        }
        output.roots.push(root_out);
    }
    if !output.has_more && !output.complete {
        // Stopped by the deadline, not by the window.
        output.has_more = true;
    }
    output.stats.millis = started.elapsed().as_millis();
    output
}

// ── Symbol-definition search ─────────────────────────────────────────────────

/// A symbol whose *name* matched.
#[derive(Debug, Clone)]
pub struct SymbolHit {
    /// Root index in the `roots` slice.
    pub root: usize,
    /// Root-relative file.
    pub rel: String,
    /// Symbol name.
    pub name: String,
    /// Kind (`function`, `class`, ...).
    pub kind: &'static str,
    /// First line (1-based); `0` when the file could not be read.
    pub line: u32,
    /// Last line (1-based).
    pub end_line: u32,
    /// The file changed after indexing, so the line numbers may be off.
    pub stale: bool,
    /// 0 exact name, 1 prefix, 2 other.
    pub rank: u8,
}

/// Find symbol definitions by name across indexed roots. Symbols come from the
/// index, so this needs no PDG and no parsing; exact matches sort first.
/// Returns `(window, total)`.
pub fn search_symbols(
    roots: &[RootSpec],
    compiled: &Compiled,
    pattern_lower: &str,
    kinds: &[String],
    offset: usize,
    limit: Option<usize>,
) -> (Vec<SymbolHit>, usize) {
    let mut hits: Vec<SymbolHit> = Vec::new();
    for (root_id, spec) in roots.iter().enumerate() {
        let Some(index) = &spec.index else { continue };
        let dirty_list = dirty_files(&spec.root, index);
        let dirty: std::collections::HashSet<&str> =
            dirty_list.iter().map(String::as_str).collect();
        for id in index.file_ids() {
            let Some(meta) = index.file(id) else { continue };
            if !spec.filter.allows(meta.path) {
                continue;
            }
            for (name, kind, start, end) in index.symbols(id) {
                if !kinds.is_empty() && !kinds.iter().any(|k| k.eq_ignore_ascii_case(kind)) {
                    continue;
                }
                if !compiled.regex.is_match(name.as_bytes()) {
                    continue;
                }
                let lowered = name.to_ascii_lowercase();
                let rank = if lowered == pattern_lower {
                    0
                } else if lowered.starts_with(pattern_lower) {
                    1
                } else {
                    2
                };
                hits.push(SymbolHit {
                    root: root_id,
                    rel: meta.path.to_string(),
                    name,
                    kind,
                    line: start,
                    end_line: end,
                    stale: dirty.contains(meta.path),
                    rank,
                });
            }
        }
    }
    hits.sort_by(|a, b| (a.rank, a.root, &a.rel, a.line).cmp(&(b.rank, b.root, &b.rel, b.line)));
    let total = hits.len();
    let mut window: Vec<SymbolHit> = hits
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    // Byte offsets -> lines, only for the returned window.
    for hit in &mut window {
        let path = roots[hit.root].root.join(&hit.rel);
        if let Ok(data) = std::fs::read(&path) {
            let (start, end) = (hit.line as usize, hit.end_line as usize);
            let line_of = |at: usize| {
                1 + memchr::memchr_iter(b'\n', &data[..at.min(data.len())]).count() as u32
            };
            hit.line = line_of(start);
            hit.end_line = line_of(end.saturating_sub(1).max(start));
        } else {
            hit.line = 0;
            hit.end_line = 0;
        }
    }
    (window, total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn corpus() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "src/lib.rs",
            "pub fn parse_config() {}\npub fn other() {}\n",
        );
        write(
            dir.path(),
            "src/main.rs",
            "fn main() {\n    parse_config();\n    // TODO: later\n}\n",
        );
        write(
            dir.path(),
            "notes/todo.txt",
            "todo: nothing\nParse_Config is documented\n",
        );
        write(dir.path(), "assets/blob.bin", "PK\0\0parse_config\0binary");
        dir
    }

    fn q(pattern: &str) -> Query {
        Query {
            pattern: pattern.into(),
            regex: false,
            case: CaseMode::Smart,
            word: false,
        }
    }

    fn built(dir: &Path) -> Arc<TextIndex> {
        let out = dir.join(".leindex/textindex/index.bin");
        build_index(dir, &out, HashMap::new()).unwrap();
        Arc::new(TextIndex::open(&out).unwrap())
    }

    fn spec(dir: &Path, index: Option<Arc<TextIndex>>) -> RootSpec {
        RootSpec {
            root: dir.to_path_buf(),
            index,
            filter: FileFilter::default(),
        }
    }

    fn paths(output: &SearchOutput) -> Vec<String> {
        output
            .roots
            .iter()
            .flat_map(|r| r.files.iter().map(|f| f.rel.clone()))
            .collect()
    }

    #[test]
    fn test_indexed_and_live_search_agree() {
        let dir = corpus();
        let index = built(dir.path());
        let compiled = q("parse_config").compile().unwrap();
        let opts = SearchOptions::default();
        let indexed = search(&[spec(dir.path(), Some(index))], &compiled, &opts);
        let live = search(&[spec(dir.path(), None)], &compiled, &opts);
        assert_eq!(paths(&indexed), paths(&live));
        assert_eq!(
            paths(&indexed),
            ["notes/todo.txt", "src/lib.rs", "src/main.rs"]
        );
        assert!(indexed.roots[0].used_index && !live.roots[0].used_index);
        assert!(indexed.stats.candidates <= live.stats.candidates);
    }

    #[test]
    fn test_binary_files_are_never_reported() {
        let dir = corpus();
        let live = search(
            &[spec(dir.path(), None)],
            &q("parse_config").compile().unwrap(),
            &SearchOptions::default(),
        );
        assert!(!paths(&live).iter().any(|p| p.ends_with(".bin")));
    }

    #[test]
    fn test_smart_case_and_explicit_case() {
        let dir = corpus();
        let opts = SearchOptions::default();
        let lower = search(
            &[spec(dir.path(), None)],
            &q("parse_config").compile().unwrap(),
            &opts,
        );
        assert_eq!(
            lower.stats.files_matched, 3,
            "lowercase pattern ignores case"
        );
        let upper = search(
            &[spec(dir.path(), None)],
            &q("Parse_Config").compile().unwrap(),
            &opts,
        );
        assert_eq!(
            paths(&upper),
            ["notes/todo.txt"],
            "an uppercase letter makes it exact"
        );
        let forced = Query {
            case: CaseMode::Sensitive,
            ..q("parse_config")
        };
        let exact = search(&[spec(dir.path(), None)], &forced.compile().unwrap(), &opts);
        assert_eq!(exact.stats.files_matched, 2);
    }

    #[test]
    fn test_hits_carry_line_column_and_context() {
        let dir = corpus();
        let opts = SearchOptions {
            context: 1,
            ..Default::default()
        };
        let out = search(
            &[spec(dir.path(), None)],
            &q("TODO: later").compile().unwrap(),
            &opts,
        );
        let hit = &out.roots[0].files[0].hits[0];
        assert_eq!((hit.line, hit.col), (3, 8));
        assert_eq!(hit.text, "    // TODO: later");
        assert_eq!(hit.before, ["    parse_config();"]);
        assert_eq!(hit.after, ["}"]);
    }

    #[test]
    fn test_edits_after_indexing_are_found_without_reindexing() {
        let dir = corpus();
        let index = built(dir.path());
        std::thread::sleep(Duration::from_millis(20));
        write(dir.path(), "src/lib.rs", "pub fn brand_new_symbol() {}\n");
        write(dir.path(), "src/added.rs", "fn brand_new_symbol_two() {}\n");
        let compiled = q("brand_new_symbol").compile().unwrap();
        let out = search(
            &[spec(dir.path(), Some(index))],
            &compiled,
            &SearchOptions::default(),
        );
        assert_eq!(paths(&out), ["src/added.rs", "src/lib.rs"]);
        assert!(out.stats.dirty >= 2);
    }

    #[test]
    fn test_removed_content_is_not_reported_from_a_stale_index() {
        let dir = corpus();
        let index = built(dir.path());
        std::thread::sleep(Duration::from_millis(20));
        write(dir.path(), "src/lib.rs", "pub fn other() {}\n");
        let out = search(
            &[spec(dir.path(), Some(index))],
            &q("parse_config").compile().unwrap(),
            &SearchOptions::default(),
        );
        assert!(!paths(&out).contains(&"src/lib.rs".to_string()));
    }

    #[test]
    fn test_regex_word_and_filters() {
        let dir = corpus();
        let re = Query {
            regex: true,
            ..q(r"fn \w+\(\)")
        };
        let out = search(
            &[spec(dir.path(), None)],
            &re.compile().unwrap(),
            &SearchOptions::default(),
        );
        assert_eq!(paths(&out), ["src/lib.rs", "src/main.rs"]);
        let word = Query {
            word: true,
            ..q("other")
        };
        assert_eq!(
            search(
                &[spec(dir.path(), None)],
                &word.compile().unwrap(),
                &SearchOptions::default()
            )
            .stats
            .files_matched,
            1
        );
        let mut only_notes = spec(dir.path(), None);
        only_notes.filter = FileFilter::new(&["*.txt".into()], &[], None);
        let out = search(
            &[only_notes],
            &q("parse_config").compile().unwrap(),
            &SearchOptions::default(),
        );
        assert_eq!(paths(&out), ["notes/todo.txt"]);
    }

    #[test]
    fn test_invalid_and_empty_patterns_are_errors() {
        assert!(
            Query {
                regex: true,
                ..q("(oops")
            }
            .compile()
            .is_err()
        );
        assert!(q("").compile().is_err());
    }

    #[test]
    fn test_paging_is_stable_and_reports_has_more() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            write(dir.path(), &format!("f{i}.txt"), "needle one\nneedle two\n");
        }
        let compiled = q("needle").compile().unwrap();
        let mut collected = Vec::new();
        let mut offset = 0;
        loop {
            let opts = SearchOptions {
                offset,
                limit: Some(3),
                ..Default::default()
            };
            let out = search(&[spec(dir.path(), None)], &compiled, &opts);
            for root in &out.roots {
                for file in &root.files {
                    for hit in &file.hits {
                        collected.push((file.rel.clone(), hit.line));
                    }
                }
            }
            if !out.has_more {
                break;
            }
            offset += 3;
        }
        assert_eq!(collected.len(), 10, "{collected:?}");
        let mut sorted = collected.clone();
        sorted.sort();
        assert_eq!(collected, sorted, "pages concatenate in path/line order");
        let unbounded = search(
            &[spec(dir.path(), None)],
            &compiled,
            &SearchOptions {
                limit: None,
                ..Default::default()
            },
        );
        assert_eq!(unbounded.returned, 10);
        assert!(unbounded.complete && !unbounded.has_more);
    }

    #[test]
    fn test_per_file_cap_bounds_output_but_not_counts() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "big.txt", &"needle\n".repeat(100));
        let opts = SearchOptions {
            per_file_cap: 5,
            limit: None,
            ..Default::default()
        };
        let out = search(
            &[spec(dir.path(), None)],
            &q("needle").compile().unwrap(),
            &opts,
        );
        assert_eq!(out.roots[0].files[0].hits.len(), 5);
        assert_eq!(out.roots[0].files[0].match_lines, 100);
    }

    #[test]
    fn test_count_only_mode_reads_no_text() {
        let dir = corpus();
        let opts = SearchOptions {
            collect_hits: false,
            limit: None,
            ..Default::default()
        };
        let out = search(
            &[spec(dir.path(), None)],
            &q("parse_config").compile().unwrap(),
            &opts,
        );
        assert_eq!(out.stats.files_matched, 3);
        assert!(
            out.roots[0]
                .files
                .iter()
                .all(|f| f.hits.is_empty() && f.match_lines > 0)
        );
    }

    #[test]
    fn test_long_lines_are_windowed_around_the_match() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "long.txt",
            &format!("{}needle{}\n", "a".repeat(500), "b".repeat(500)),
        );
        let opts = SearchOptions {
            max_line_chars: 40,
            ..Default::default()
        };
        let out = search(
            &[spec(dir.path(), None)],
            &q("needle").compile().unwrap(),
            &opts,
        );
        let text = &out.roots[0].files[0].hits[0].text;
        assert!(
            text.contains("needle") && text.starts_with('…') && text.ends_with('…'),
            "{text}"
        );
        assert!(text.chars().count() <= 44);
    }

    #[test]
    fn test_single_file_root_and_missing_root() {
        let dir = corpus();
        let file = RootSpec {
            root: dir.path().join("src/main.rs"),
            index: None,
            filter: FileFilter::default(),
        };
        let out = search(
            &[file],
            &q("parse_config").compile().unwrap(),
            &SearchOptions::default(),
        );
        assert_eq!(out.stats.files_matched, 1);
        let missing = spec(&dir.path().join("nope"), None);
        let out = search(
            &[missing],
            &q("x1y2").compile().unwrap(),
            &SearchOptions::default(),
        );
        assert_eq!(out.stats.files_matched, 0);
    }

    #[test]
    fn test_symbol_search_ranks_exact_first_and_reports_lines() {
        let dir = tempfile::tempdir().unwrap();
        let text = "line one\nfn parse() {}\nfn parse_config() {}\nstruct Parser;\n";
        write(dir.path(), "a.rs", text);
        let span = |needle: &str, kind: &str, name: &str| {
            let start = text.find(needle).unwrap() as u32;
            SymbolSpan {
                start,
                end: start + needle.len() as u32,
                kind: super::super::index::kind_code(kind),
                name: name.into(),
            }
        };
        let mut symbols = HashMap::new();
        symbols.insert(
            "a.rs".to_string(),
            vec![
                span("fn parse_config() {}", "function", "parse_config"),
                span("fn parse() {}", "function", "parse"),
                span("struct Parser;", "struct", "Parser"),
            ],
        );
        let out = dir.path().join(".leindex/textindex/index.bin");
        build_index(dir.path(), &out, symbols).unwrap();
        let specs = [spec(
            dir.path(),
            Some(Arc::new(TextIndex::open(&out).unwrap())),
        )];
        let compiled = q("parse").compile().unwrap();
        let (hits, total) = search_symbols(&specs, &compiled, "parse", &[], 0, None);
        assert_eq!(total, 3);
        let names: Vec<_> = hits.iter().map(|h| (h.name.as_str(), h.line)).collect();
        assert_eq!(names, [("parse", 2), ("parse_config", 3), ("Parser", 4)]);
        assert_eq!(hits[0].rank, 0);
        let (functions, _) =
            search_symbols(&specs, &compiled, "parse", &["function".into()], 0, None);
        assert_eq!(functions.len(), 2);
        let (page, _) = search_symbols(&specs, &compiled, "parse", &[], 1, Some(1));
        assert_eq!(page[0].name, "parse_config");
        assert!(hits.iter().all(|h| !h.stale));
    }

    #[test]
    fn test_symbols_annotate_hits_in_clean_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let text = "fn outer() {\n    needle();\n}\n";
        write(dir.path(), "a.rs", text);
        let mut symbols = HashMap::new();
        symbols.insert(
            "a.rs".to_string(),
            vec![SymbolSpan {
                start: 0,
                end: text.len() as u32,
                kind: 0,
                name: "outer".into(),
            }],
        );
        let out = dir.path().join(".leindex/textindex/index.bin");
        build_index(dir.path(), &out, symbols).unwrap();
        let index = Arc::new(TextIndex::open(&out).unwrap());
        let compiled = q("needle").compile().unwrap();
        let found = search(
            &[spec(dir.path(), Some(index.clone()))],
            &compiled,
            &SearchOptions::default(),
        );
        assert_eq!(
            found.roots[0].files[0].hits[0].symbol,
            Some(("outer".to_string(), "function"))
        );
        assert_eq!(
            found.roots[0].files[0].symbols,
            [("outer".to_string(), "function", 1)]
        );

        std::thread::sleep(Duration::from_millis(20));
        write(
            dir.path(),
            "a.rs",
            "// shifted\nfn outer() {\n    needle();\n}\n",
        );
        let stale = search(
            &[spec(dir.path(), Some(index))],
            &compiled,
            &SearchOptions::default(),
        );
        let file = &stale.roots[0].files[0];
        assert!(
            file.symbols_stale && file.hits[0].symbol.is_none(),
            "stale byte offsets must not be trusted"
        );
    }

    #[test]
    fn test_oversized_files_are_scanned_but_not_indexed() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "huge.txt",
            &format!(
                "{}\nneedle_in_huge\n",
                "x".repeat(MAX_INDEXED_FILE_BYTES as usize + 10)
            ),
        );
        let index = built(dir.path());
        let out = search(
            &[spec(dir.path(), Some(index))],
            &q("needle_in_huge").compile().unwrap(),
            &SearchOptions::default(),
        );
        assert_eq!(paths(&out), ["huge.txt"]);
    }
}
