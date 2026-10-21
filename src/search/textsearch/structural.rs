//! Structural (AST-pattern) search: ast-grep patterns over LeIndex's grammars.
//!
//! This is the third search axis next to the trigram/regex text index and the
//! PDG symbol lookup: it matches *code shape* (`fn $NAME($$$ARGS) { $$$BODY }`)
//! rather than text or names, so whitespace, comments and formatting cannot
//! change the answer.
//!
//! ## Grammars are LeIndex's, not ast-grep's
//!
//! [`StructuralLang`] implements [`ast_grep_core::Language`] on top of the
//! `tree_sitter::Language` handles returned by [`LanguageId::from_cache`] — the
//! same grammar cache every other parser uses. Nothing here declares a grammar
//! of its own, so the crate keeps exactly one tree-sitter/grammar generation
//! (`ast-grep-core` 0.45 needs tree-sitter 0.26, which `parse` already pins).
//!
//! ## The expando char, and why `$` alone is not enough
//!
//! ast-grep metavariables are written `$NAME` / `$$$NAME`. In languages whose
//! lexer cannot start an identifier with `$` (Rust, Python, Go, Java, Swift,
//! C, …) that text parses as error-recovery nodes, so the pattern silently
//! matches nothing. ast-grep's answer is the *expando char*: rewrite `$` in the
//! pattern to a character the grammar does accept as an identifier start, and
//! read metavariables back with the same char.
//!
//! A matrix probe over Rust, Python, Go, JavaScript, TypeScript, C, Java, Lua
//! and Bash (see `structural_test`) found `$` yields zero matches for
//! Rust/Python/Go while the expando yields the correct matches for all nine.
//! One expando for every language is therefore both simpler and strictly more
//! correct than special-casing `$`.
//!
//! ## Scanning
//!
//! Files are enumerated with [`engine::list_files_matching`] (gitignore-aware,
//! live — no index required), with the same [`FileFilter`] the text search uses
//! plus the pattern's own language applied inside the walk, so
//! [`MAX_CANDIDATE_FILES`] bounds eligible files rather than raw tree order.
//! Why the listing stopped short is folded into
//! [`StructuralOutput::has_more`], because a budget that expires during
//! enumeration is as partial as one that expires during parsing; the *cause*
//! decides [`StructuralOutput::stopped_by_deadline`], which may only cite the
//! time budget. Contents are
//! read through [`engine::read_scannable`], so the binary and size limits stay
//! the engine's rather than becoming a second policy that could drift.

use super::engine::{list_files_matching, read_scannable};
use super::glob::FileFilter;
use crate::parse::grammar::LanguageId;
use ast_grep_core::Language;
use ast_grep_core::matcher::{Pattern, PatternBuilder, PatternError};
use ast_grep_core::meta_var::MetaVariable;
use ast_grep_core::tree_sitter::{LanguageExt, StrDoc, TSLanguage};
use rayon::prelude::*;
use std::borrow::Cow;
use std::path::Path;
use std::time::Instant;

/// Character substituted for `$` in a pattern before parsing, and used to
/// recognise metavariables afterwards.
///
/// `µ` is alphabetic, hence a valid identifier start in every grammar LeIndex
/// ships — including the ones that reject `$`.
const EXPANDO_CHAR: char = 'µ';

/// Hits buffered per file when the per-file cap is disabled. Bounds the memory
/// one pathological file may cost; the file still reports `capped` so callers
/// know its contribution was truncated.
const UNLIMITED_FILE_BUFFER: usize = 10_000;

/// Maximum pattern size. ast-grep parses the pattern as source, so an
/// oversized pattern is a parse cost rather than a backtracking hazard; the
/// limit stops a caller streaming an entire file in as a "pattern".
pub const MAX_PATTERN_BYTES: usize = 4_096;

/// `per_file_cap` value meaning "no cap".
pub const NO_CAP: usize = 0;

/// Upper bound on files a single structural search will examine, matching
/// [`engine::MAX_INDEX_FILES`] so the two search axes see the same tree.
const MAX_CANDIDATE_FILES: usize = 300_000;

/// One AST-pattern match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralHit {
    /// 1-based line the matched node starts on.
    pub line: u32,
    /// 1-based byte column the matched node starts at.
    pub col: u32,
    /// Byte offset of the matched node's start within the file.
    pub start_byte: usize,
    /// Byte offset of the matched node's end within the file.
    pub end_byte: usize,
    /// Tree-sitter node kind (`function_item`, `function_definition`, …).
    pub kind: String,
    /// Matched source text, clipped to [`StructuralOptions::max_match_chars`].
    pub text: String,
    /// Name of the pattern's first capture, when it has one (`NAME`).
    pub capture: Option<String>,
    /// Text bound to [`StructuralHit::capture`], when it has one.
    pub captured: Option<String>,
}

/// Matches in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralFile {
    /// Root-relative path (forward slashes).
    pub rel: String,
    /// Language the file was matched against.
    pub lang: &'static str,
    /// Matches found in the file before the per-file cap.
    pub total: usize,
    /// Matches reported for this file on this page.
    pub hits: Vec<StructuralHit>,
    /// The per-file cap withheld matches that appear on no page.
    pub capped: bool,
}

/// Options shaping a structural search.
#[derive(Debug, Clone)]
pub struct StructuralOptions {
    /// Maximum hits returned across all files.
    pub limit: usize,
    /// Hits to skip before collecting.
    pub offset: usize,
    /// Matches reported per file; [`NO_CAP`] reports all (up to
    /// [`UNLIMITED_FILE_BUFFER`]). Capped-out matches appear on no page.
    pub per_file_cap: usize,
    /// Longest matched-text snippet kept per hit.
    pub max_match_chars: usize,
    /// Wall-clock budget; `None` disables it.
    pub deadline: Option<Instant>,
}

impl Default for StructuralOptions {
    fn default() -> Self {
        Self {
            limit: 100,
            offset: 0,
            per_file_cap: 20,
            max_match_chars: 200,
            deadline: None,
        }
    }
}

/// Totals for one structural search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructuralStats {
    /// Files admitted by language selection and the glob filter.
    pub candidates: usize,
    /// Files parsed (read and handed to ast-grep).
    pub scanned: usize,
    /// Candidates whose scan did not complete before the deadline. Non-zero
    /// means the result is partial: an unexamined file may still hold matches,
    /// so `has_more` is forced true rather than reporting a clean page.
    pub skipped_deadline: usize,
    /// Files with at least one match, including files paged out by `offset`.
    pub files_matched: usize,
    /// Matches found, before the per-file cap and paging.
    pub total_matches: usize,
    /// Files whose match count exceeded the per-file cap.
    pub files_capped: usize,
    /// Wall time.
    pub millis: u128,
}

/// Everything a structural search returns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructuralOutput {
    /// Files with at least one reported hit, in path order.
    pub files: Vec<StructuralFile>,
    /// Hits returned on this page.
    pub returned: usize,
    /// Matches exist beyond `offset + limit` (cap-withheld matches excluded:
    /// they appear on no page, so paging cannot reach them).
    pub has_more: bool,
    /// The time budget expired, so results may be incomplete.
    pub stopped_by_deadline: bool,
    /// Totals.
    pub stats: StructuralStats,
}

impl StructuralOutput {
    /// Every match seen, reported or not.
    pub fn total_matches(&self) -> usize {
        self.stats.total_matches
    }
}

/// A pattern compiled for one language.
///
/// Compiling once and running over many files is the whole point: ast-grep
/// parses the pattern into a tree, which costs far more than matching it.
pub struct CompiledPattern {
    lang: StructuralLang,
    pattern: Pattern,
    name: &'static str,
}

impl std::fmt::Debug for CompiledPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledPattern")
            .field("lang", &self.name)
            .finish_non_exhaustive()
    }
}

/// Errors a structural search raises before scanning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructuralError {
    /// The language name/extension has no LeIndex grammar.
    UnsupportedLanguage(String),
    /// The language exists as a document flavour with no AST.
    GrammarUnavailable(&'static str),
    /// The pattern did not parse into a single meaningful node.
    InvalidPattern {
        /// Language the pattern was compiled for.
        lang: &'static str,
        /// ast-grep's diagnosis.
        detail: String,
    },
    /// The pattern was empty or absurdly large.
    UnusablePattern(String),
}

impl std::fmt::Display for StructuralError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedLanguage(name) => write!(
                f,
                "structural search does not support language '{name}' — pass one LeIndex \
                 has a tree-sitter grammar for (e.g. rust, python, go, typescript)"
            ),
            Self::GrammarUnavailable(name) => write!(
                f,
                "language '{name}' is parsed as a document flavour and has no tree-sitter \
                 grammar, so structural search cannot match it"
            ),
            Self::InvalidPattern { lang, detail } => {
                write!(f, "pattern is not valid {lang} code: {detail}")
            }
            Self::UnusablePattern(detail) => write!(f, "unusable pattern: {detail}"),
        }
    }
}

impl std::error::Error for StructuralError {}

/// ast-grep language over a LeIndex grammar handle.
#[derive(Clone)]
struct StructuralLang {
    id: LanguageId,
    grammar: TSLanguage,
}

impl StructuralLang {
    fn new(id: LanguageId) -> Result<Self, StructuralError> {
        if is_document_flavour(id) {
            return Err(StructuralError::GrammarUnavailable(
                id.config().name.as_str(),
            ));
        }
        let grammar = id
            .from_cache()
            .map_err(|_| StructuralError::GrammarUnavailable(id.config().name.as_str()))?;
        Ok(Self { id, grammar })
    }
}

impl Language for StructuralLang {
    fn pre_process_pattern<'q>(&self, query: &'q str) -> Cow<'q, str> {
        // See the module docs: without this, patterns match nothing in every
        // grammar whose lexer cannot start an identifier with `$`.
        if query.contains('$') {
            Cow::Owned(query.replace('$', "\u{b5}"))
        } else {
            Cow::Borrowed(query)
        }
    }

    fn expando_char(&self) -> char {
        EXPANDO_CHAR
    }

    fn kind_to_id(&self, kind: &str) -> u16 {
        self.grammar.id_for_node_kind(kind, true)
    }

    fn field_to_id(&self, field: &str) -> Option<u16> {
        self.grammar.field_id_for_name(field).map(|f| f.get())
    }

    fn build_pattern(&self, builder: &PatternBuilder<'_>) -> Result<Pattern, PatternError> {
        builder.build(|src| StrDoc::try_new(src, self.clone()))
    }
}

impl LanguageExt for StructuralLang {
    fn get_ts_language(&self) -> TSLanguage {
        self.grammar.clone()
    }
}

/// True for the doc flavours: they are parsed by a hand-written document
/// parser, so `LanguageId::load_language` returns a stand-in grammar that must
/// never be handed to ast-grep.
fn is_document_flavour(id: LanguageId) -> bool {
    matches!(
        id,
        LanguageId::Markdown | LanguageId::Rst | LanguageId::Adoc | LanguageId::PlainText
    )
}

/// Every language that can take part in a structural search.
const STRUCTURAL_LANGUAGES: [LanguageId; 37] = [
    LanguageId::Python,
    LanguageId::JavaScript,
    LanguageId::TypeScript,
    LanguageId::Go,
    LanguageId::Rust,
    LanguageId::Java,
    LanguageId::Cpp,
    LanguageId::CSharp,
    LanguageId::Ruby,
    LanguageId::Php,
    LanguageId::Lua,
    LanguageId::Scala,
    LanguageId::C,
    LanguageId::Bash,
    LanguageId::Json,
    LanguageId::Swift,
    LanguageId::Kotlin,
    LanguageId::Dart,
    LanguageId::Html,
    LanguageId::Css,
    LanguageId::Scss,
    LanguageId::Yaml,
    LanguageId::Cmake,
    LanguageId::Elixir,
    LanguageId::Erlang,
    LanguageId::Haskell,
    LanguageId::Perl,
    LanguageId::R,
    LanguageId::Zig,
    LanguageId::Graphql,
    LanguageId::Hcl,
    LanguageId::Make,
    LanguageId::Elisp,
    LanguageId::Julia,
    LanguageId::D,
    LanguageId::Glsl,
    LanguageId::EmbeddedTemplate,
];

/// Resolve a language name, display name, or extension to a [`LanguageId`].
///
/// Accepts `"rust"`, `"rs"` and `"Rust"`. Extensions come from the registry so
/// the two can never disagree; display names are matched against
/// [`LanguageConfig::name`](crate::parse::traits::LanguageConfig), which is the
/// same string `parallel.rs` resolves parsers by.
pub fn resolve_language(language: &str) -> Option<LanguageId> {
    let key = language.trim();
    if key.is_empty() {
        return None;
    }
    if let Some(id) = LanguageId::from_extension(key) {
        return Some(id);
    }
    STRUCTURAL_LANGUAGES.into_iter().find(|id| {
        id.config().name.eq_ignore_ascii_case(key)
            || id
                .config()
                .extensions
                .iter()
                .any(|ext| ext.eq_ignore_ascii_case(key))
    })
}

/// The structural-search language a path belongs to, if any.
pub fn language_for_path(path: &Path) -> Option<LanguageId> {
    let ext = path.extension()?.to_str()?;
    LanguageId::from_extension(ext).filter(|id| !is_document_flavour(*id))
}

impl CompiledPattern {
    /// Compile `pattern` for `language` (a name, display name, or extension).
    pub fn new(language: &str, pattern: &str) -> Result<Self, StructuralError> {
        let id = resolve_language(language)
            .ok_or_else(|| StructuralError::UnsupportedLanguage(language.to_string()))?;
        Self::for_language(id, pattern)
    }

    /// Compile `pattern` for an already-resolved [`LanguageId`].
    pub fn for_language(id: LanguageId, pattern: &str) -> Result<Self, StructuralError> {
        let name = id.config().name.as_str();
        let lang = StructuralLang::new(id)?;
        if pattern.trim().is_empty() {
            return Err(StructuralError::UnusablePattern(
                "pattern must not be empty".to_string(),
            ));
        }
        if pattern.len() > MAX_PATTERN_BYTES {
            return Err(StructuralError::UnusablePattern(format!(
                "pattern is {} bytes; the maximum is {MAX_PATTERN_BYTES}",
                pattern.len()
            )));
        }
        let pattern = Pattern::try_new(pattern, lang.clone()).map_err(|e| {
            StructuralError::InvalidPattern {
                lang: name,
                detail: format!("{e:?}"),
            }
        })?;
        Ok(Self {
            lang,
            pattern,
            name,
        })
    }

    /// Canonical language name this pattern was compiled for.
    pub fn language(&self) -> &'static str {
        self.name
    }

    /// The language id this pattern was compiled for.
    pub fn language_id(&self) -> LanguageId {
        self.lang.id
    }

    /// Match `source` and collect its hits, honouring the per-file cap.
    fn scan(&self, rel: &str, source: &str, options: &StructuralOptions) -> Option<StructuralFile> {
        let doc = self.lang.ast_grep(source);
        let buffer_cap = if options.per_file_cap == NO_CAP {
            UNLIMITED_FILE_BUFFER
        } else {
            options.per_file_cap
        };
        let mut hits = Vec::new();
        let mut total = 0usize;
        for matched in doc.root().find_all(&self.pattern) {
            total += 1;
            if hits.len() >= buffer_cap {
                continue;
            }
            let node = matched.get_node();
            let (line, byte_column) = node.start_pos().byte_point();
            let range = node.range();
            let (capture, captured) = capture_of(&matched, options.max_match_chars);
            hits.push(StructuralHit {
                line: u32::try_from(line + 1).unwrap_or(u32::MAX),
                col: u32::try_from(byte_column + 1).unwrap_or(u32::MAX),
                start_byte: range.start,
                end_byte: range.end,
                kind: node.kind().to_string(),
                text: clip(&node.text(), options.max_match_chars),
                capture,
                captured,
            });
        }
        if total == 0 {
            return None;
        }
        Some(StructuralFile {
            rel: rel.to_string(),
            lang: self.name,
            total,
            hits,
            capped: total > buffer_cap,
        })
    }
}

/// The pattern's first named capture, if it has one.
///
/// Only a pattern that actually captured something can report one; the first
/// is the one a reader looks for (`$NAME` in `fn $NAME`).
fn capture_of(
    matched: &ast_grep_core::NodeMatch<'_, StrDoc<StructuralLang>>,
    max_chars: usize,
) -> (Option<String>, Option<String>) {
    let env = matched.get_env();
    let Some(name) = env.get_matched_variables().find_map(|var| match var {
        MetaVariable::Capture(name, _) | MetaVariable::MultiCapture(name) => Some(name),
        MetaVariable::Multiple | MetaVariable::Dropped(_) => None,
    }) else {
        return (None, None);
    };
    let captured = env
        .get_match(&name)
        .map(|node| clip(&node.text(), max_chars));
    (Some(name), captured)
}

/// Clip `text` to `max_chars`, appending an ellipsis when it was cut.
fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// Run `pattern` over `root`, honouring `filter` and `options`.
///
/// Enumerating with [`engine::list_files`] means a directory without a trigram
/// index is searched live and correctly, exactly like the text search: the
/// index is a speed-up, never a correctness requirement.
pub fn search(
    root: &Path,
    pattern: &CompiledPattern,
    filter: &FileFilter,
    options: &StructuralOptions,
) -> StructuralOutput {
    let started = Instant::now();
    let mut output = StructuralOutput::default();

    // The pattern's own language and the glob filter are applied INSIDE the
    // walk, so `MAX_CANDIDATE_FILES` bounds the files that can actually match. A
    // post-hoc filter over a truncated listing substitutes "the first N paths in
    // the tree" for "the first N eligible files": a large tree whose Rust files
    // sort past the cutoff reported zero candidates for a Rust pattern. Same
    // defect as the language-inference cutoff in `structural_handler`.
    let (rels, truncation) =
        list_files_matching(root, MAX_CANDIDATE_FILES, options.deadline, |rel| {
            filter.allows(rel) && language_for_path(Path::new(rel)) == Some(pattern.language_id())
        });
    output.stats.candidates = rels.len();

    // Two things
    // never produce a `StructuralFile`, and only one of them means the answer is
    // incomplete:
    //   * the worker hit the deadline before opening the file — that file may
    //     still hold matches, so the result is partial; and
    //   * `read_scannable` refused the file (binary, oversized, unreadable) —
    //     legitimately not matched, and counting it as incomplete would pin
    //     `has_more` forever on any tree containing a binary.
    // They are distinguished here rather than lumped into `None`, or the
    // deadline fix becomes a permanent false alarm.
    let deadline_skips = std::sync::atomic::AtomicUsize::new(0);
    let read_failures = std::sync::atomic::AtomicUsize::new(0);
    let deadline = options.deadline;
    let scanned: Vec<Option<StructuralFile>> = rels
        .par_iter()
        .map(|rel| {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                deadline_skips.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return None;
            }
            let source = match read_scannable(&root.join(rel)) {
                Some(source) => source,
                None => {
                    read_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return None;
                }
            };
            pattern.scan(rel, &source, options)
        })
        .collect();

    let skipped_deadline = deadline_skips.into_inner();
    output.stats.scanned = rels
        .len()
        .saturating_sub(skipped_deadline + read_failures.into_inner());
    output.stats.skipped_deadline = skipped_deadline;

    let mut files: Vec<StructuralFile> = scanned.into_iter().flatten().collect();
    // Deterministic order keeps `offset`/`limit` paging stable even if the
    // scan strategy changes.
    files.sort_by(|a, b| a.rel.cmp(&b.rel));

    // `offset` pages over the per-file-capped stream only: cap-withheld matches
    // appear on no page, so they must not inflate `has_more` forever. Hits the
    // offset already skipped are behind the caller, so they are not counted as
    // "more" — otherwise every page after the first reports `has_more` even
    // when it holds the last match in the tree.
    let mut paged: usize = 0;
    let mut skipped = options.offset;
    for file in files {
        output.stats.total_matches += file.total;
        output.stats.files_matched += 1;
        if file.capped {
            output.stats.files_capped += 1;
        }
        let available = file.hits.len();
        let drop = skipped.min(available);
        skipped -= drop;
        paged += available - drop;
        let room = options.limit.saturating_sub(output.returned);
        let keep = (available - drop).min(room);
        if keep == 0 {
            continue;
        }
        let hits = file.hits[drop..drop + keep].to_vec();
        output.returned += hits.len();
        output.files.push(StructuralFile { hits, ..file });
    }
    // `stopped_by_deadline` claims the TIME BUDGET cut this scan, so it may only
    // cite deadline evidence: files the workers skipped on expiry, or a walk
    // that stopped on expiry. A listing cut by the file count limit is not time
    // pressure — reporting it as one makes a large tree look time-starved and
    // would hide a genuine deadline skip behind the wrong explanation.
    output.stopped_by_deadline = output.stats.skipped_deadline > 0 || truncation.deadline;
    // `has_more`, by contrast, only needs *some* tree left unexamined — a limit
    // truncation counts, because those files were never candidates.
    output.has_more =
        output.returned < paged || output.stopped_by_deadline || truncation.truncated();
    output.stats.millis = started.elapsed().as_millis();
    output
}
