//! Native text search: a mmap'd trigram index (the technique behind Zoekt and
//! Google Code Search) with live verification, in-process and dependency-light.
//!
//! * `index` — the on-disk index and its reader,
//! * `trigram` — case-folded trigram extraction,
//! * `plan` — regex → required-trigram formula → candidate files,
//! * `glob` — include/exclude filtering,
//! * `engine` — inventory, freshness, scanning and result windowing.
//!
//! Anything the index has not seen (files edited since, new files, whole
//! directories outside the workspace) is scanned live, so callers never need
//! an index to get a correct answer — only to get it faster.

pub mod engine;
pub mod glob;
pub mod index;
pub mod plan;
pub mod trigram;

pub use engine::{
    BuildStats, CaseMode, Compiled, FileResult, Hit, Query, RootOutput, RootSpec, SearchOptions,
    SearchOutput, SearchStats, SymbolHit, build_index, invalidate_freshness, list_files, search,
    search_symbols,
};
pub use glob::FileFilter;
pub use index::{SymbolSpan, TextIndex, kind_code};
