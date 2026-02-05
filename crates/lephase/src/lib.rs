#![warn(missing_docs)]

//! lephase - 5-phase analysis workflow for LeIndexer.

/// Summary cache helpers.
pub mod cache;
/// Shared runtime context and incremental preparation pipeline.
pub mod context;
/// Optional markdown/text analysis utilities.
pub mod docs;
/// Output formatting modes and truncation helpers.
pub mod format;
/// Freshness detection and generation hashing.
pub mod freshness;
/// User-facing analysis options.
pub mod options;
/// Orchestration engine and loop state.
pub mod orchestrate;
/// PDG merge/build helper functions.
pub mod pdg_utils;
/// Phase 1 structural scan.
pub mod phase1;
/// Phase 2 dependency map.
pub mod phase2;
/// Phase 3 logic flow.
pub mod phase3;
/// Phase 4 critical path.
pub mod phase4;
/// Phase 5 optimization synthesis.
pub mod phase5;
/// Recommendation models.
pub mod recommendations;
/// Shared file/path utility helpers.
pub mod utils;

use anyhow::Result;
use cache::PhaseCache;
use context::PhaseExecutionContext;
use format::TokenFormatter;
use serde::{Deserialize, Serialize};

pub use format::FormatMode;
pub use options::{DocsMode, PhaseOptions};
pub use phase1::Phase1Summary;
pub use phase2::Phase2Summary;
pub use phase3::Phase3Summary;
pub use phase4::Phase4Summary;
pub use phase5::Phase5Summary;

/// Which phase(s) to execute.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PhaseSelection {
    /// Run one specific phase 1..=5.
    Single(u8),
    /// Run all phases in order.
    All,
}

impl PhaseSelection {
    /// Validate a raw phase number.
    pub fn from_number(phase: u8) -> Option<Self> {
        if (1..=5).contains(&phase) {
            Some(Self::Single(phase))
        } else {
            None
        }
    }
}

/// Top-level phase-analysis report payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseAnalysisReport {
    /// Project id.
    pub project_id: String,
    /// Freshness generation hash.
    pub generation: String,
    /// Executed phase list.
    pub executed_phases: Vec<u8>,
    /// Whether any summary cache hit occurred.
    pub cache_hit: bool,
    /// Changed file count.
    pub changed_files: usize,
    /// Deleted file count.
    pub deleted_files: usize,

    /// Phase 1 summary.
    pub phase1: Option<Phase1Summary>,
    /// Phase 2 summary.
    pub phase2: Option<Phase2Summary>,
    /// Phase 3 summary.
    pub phase3: Option<Phase3Summary>,
    /// Phase 4 summary.
    pub phase4: Option<Phase4Summary>,
    /// Phase 5 summary.
    pub phase5: Option<Phase5Summary>,

    /// Human-readable report text.
    pub formatted_output: String,
}

/// Run phase analysis for a project with incremental freshness and shared context.
pub fn run_phase_analysis(
    options: PhaseOptions,
    selection: PhaseSelection,
) -> Result<PhaseAnalysisReport> {
    let options = options.normalized();
    let context = PhaseExecutionContext::prepare(&options)?;
    let cache = PhaseCache::new(&context.root);

    let mut executed_phases = Vec::new();
    let mut cache_hit = false;

    let mut phase1_summary = None;
    let mut phase2_summary = None;
    let mut phase3_summary = None;
    let mut phase4_summary = None;
    let mut phase5_summary = None;

    if should_run(1, selection) {
        if let Some(cached) =
            cache.load::<Phase1Summary>(&context.project_id, &context.generation_hash, 1)?
        {
            phase1_summary = Some(cached.payload);
            cache_hit = true;
        } else {
            let value = phase1::run(&context);
            cache.save(&context.project_id, &context.generation_hash, 1, &value)?;
            phase1_summary = Some(value);
        }
        executed_phases.push(1);
    }

    if should_run(2, selection) {
        if let Some(cached) =
            cache.load::<Phase2Summary>(&context.project_id, &context.generation_hash, 2)?
        {
            phase2_summary = Some(cached.payload);
            cache_hit = true;
        } else {
            let value = phase2::run(&context);
            cache.save(&context.project_id, &context.generation_hash, 2, &value)?;
            phase2_summary = Some(value);
        }
        executed_phases.push(2);
    }

    if should_run(3, selection) {
        let phase3_key = options_hash_for_phase(3, &options);
        if let Some(cached) = cache.load_with_options::<Phase3Summary>(
            &context.project_id,
            &context.generation_hash,
            3,
            phase3_key.as_deref(),
        )? {
            phase3_summary = Some(cached.payload);
            cache_hit = true;
        } else {
            let value = phase3::run(&context, &options);
            cache.save_with_options(
                &context.project_id,
                &context.generation_hash,
                3,
                phase3_key.as_deref(),
                &value,
            )?;
            phase3_summary = Some(value);
        }
        executed_phases.push(3);
    }

    if should_run(4, selection) {
        let phase4_key = options_hash_for_phase(4, &options);
        if let Some(cached) = cache.load_with_options::<Phase4Summary>(
            &context.project_id,
            &context.generation_hash,
            4,
            phase4_key.as_deref(),
        )? {
            phase4_summary = Some(cached.payload);
            cache_hit = true;
        } else {
            let value = phase4::run(&context, &options);
            cache.save_with_options(
                &context.project_id,
                &context.generation_hash,
                4,
                phase4_key.as_deref(),
                &value,
            )?;
            phase4_summary = Some(value);
        }
        executed_phases.push(4);
    }

    if should_run(5, selection) {
        let p1 = phase1_summary
            .clone()
            .unwrap_or_else(|| phase1::run(&context));
        let p2 = phase2_summary
            .clone()
            .unwrap_or_else(|| phase2::run(&context));
        let p3 = phase3_summary
            .clone()
            .unwrap_or_else(|| phase3::run(&context, &options));
        let p4 = phase4_summary
            .clone()
            .unwrap_or_else(|| phase4::run(&context, &options));

        let phase5_key = options_hash_for_phase(5, &options);
        if let Some(cached) = cache.load_with_options::<Phase5Summary>(
            &context.project_id,
            &context.generation_hash,
            5,
            phase5_key.as_deref(),
        )? {
            phase5_summary = Some(cached.payload);
            cache_hit = true;
        } else {
            let value = phase5::run(&context, &p1, &p2, &p3, &p4);
            cache.save_with_options(
                &context.project_id,
                &context.generation_hash,
                5,
                phase5_key.as_deref(),
                &value,
            )?;
            phase5_summary = Some(value);
        }
        executed_phases.push(5);
    }

    let formatted_output = format_report(
        &context,
        &executed_phases,
        phase1_summary.as_ref(),
        phase2_summary.as_ref(),
        phase3_summary.as_ref(),
        phase4_summary.as_ref(),
        phase5_summary.as_ref(),
        options.max_output_chars,
    );

    Ok(PhaseAnalysisReport {
        project_id: context.project_id,
        generation: context.generation_hash,
        executed_phases,
        cache_hit,
        changed_files: context.changed_files.len(),
        deleted_files: context.deleted_files.len(),
        phase1: phase1_summary,
        phase2: phase2_summary,
        phase3: phase3_summary,
        phase4: phase4_summary,
        phase5: phase5_summary,
        formatted_output,
    })
}

fn should_run(phase: u8, selection: PhaseSelection) -> bool {
    match selection {
        PhaseSelection::Single(p) => p == phase,
        PhaseSelection::All => true,
    }
}

fn options_hash_for_phase(phase: u8, options: &PhaseOptions) -> Option<String> {
    let key = match phase {
        3 => format!(
            "phase3:top_n={}:max_focus_files={}",
            options.top_n, options.max_focus_files
        ),
        4 => format!("phase4:top_n={}", options.top_n),
        5 => format!(
            "phase5:top_n={}:max_focus_files={}",
            options.top_n, options.max_focus_files
        ),
        _ => return None,
    };

    Some(blake3::hash(key.as_bytes()).to_hex().to_string()[..8].to_string())
}

fn format_report(
    context: &PhaseExecutionContext,
    executed_phases: &[u8],
    phase1: Option<&Phase1Summary>,
    phase2: Option<&Phase2Summary>,
    phase3: Option<&Phase3Summary>,
    phase4: Option<&Phase4Summary>,
    phase5: Option<&Phase5Summary>,
    max_chars: usize,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "5-Phase Analysis :: project={} generation={} phases={:?}",
        context.project_id, context.generation_hash, executed_phases
    ));
    lines.push(format!(
        "freshness: changed={} deleted={} inventory={}",
        context.changed_files.len(),
        context.deleted_files.len(),
        context.file_inventory.len()
    ));

    if let Some(p1) = phase1 {
        let avg_completeness = if p1.parser_completeness.is_empty() {
            0.0
        } else {
            p1.parser_completeness
                .iter()
                .map(|entry| entry.score)
                .sum::<f32>()
                / p1.parser_completeness.len() as f32
        };

        lines.push(format!(
            "phase1: files={} parsed={} failures={} signatures={} parser_completeness_avg={:.2}",
            p1.total_files, p1.parsed_files, p1.parse_failures, p1.signatures, avg_completeness
        ));
    }

    if let Some(p2) = phase2 {
        lines.push(format!(
            "phase2: import_edges internal={} external={} unresolved_modules={}",
            p2.internal_import_edges, p2.external_import_edges, p2.unresolved_modules
        ));
    }

    if let Some(p3) = phase3 {
        lines.push(format!(
            "phase3: entry_points={} impacted_nodes={} focus_files={}",
            p3.entry_points.len(),
            p3.impacted_nodes,
            p3.focus_files.len()
        ));
    }

    if let Some(p4) = phase4 {
        lines.push(format!("phase4: hotspots={}", p4.hotspots.len()));
    }

    if let Some(p5) = phase5 {
        lines.push(format!(
            "phase5: recommendations={} public_symbol_hints={}",
            p5.recommendations.len(),
            p5.public_symbol_hints
        ));
    }

    if let Some(docs) = &context.docs_summary {
        lines.push(format!(
            "docs: files={} headings={} todos={}",
            docs.files_scanned, docs.heading_count, docs.todo_count
        ));
    }

    TokenFormatter::truncate(&lines.join("\n"), max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn phase_selection_validation_works() {
        assert_eq!(
            PhaseSelection::from_number(1),
            Some(PhaseSelection::Single(1))
        );
        assert_eq!(
            PhaseSelection::from_number(5),
            Some(PhaseSelection::Single(5))
        );
        assert_eq!(PhaseSelection::from_number(0), None);
        assert_eq!(PhaseSelection::from_number(6), None);
    }

    #[test]
    fn single_phase_report_contains_only_requested_phase() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn f()->i32{1}\n").expect("write");

        let report = run_phase_analysis(
            PhaseOptions {
                root: dir.path().to_path_buf(),
                ..PhaseOptions::default()
            },
            PhaseSelection::Single(2),
        )
        .expect("phase run");

        assert_eq!(report.executed_phases, vec![2]);
        assert!(report.phase2.is_some());
        assert!(report.phase1.is_none());
        assert!(report.phase3.is_none());
    }

    #[test]
    fn phase3_cache_key_changes_with_top_n() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn a(){}\npub fn b(x:i32){}\npub fn c(x:i32,y:i32){}\n",
        )
        .expect("write");

        let first = run_phase_analysis(
            PhaseOptions {
                root: dir.path().to_path_buf(),
                top_n: 1,
                ..PhaseOptions::default()
            },
            PhaseSelection::Single(3),
        )
        .expect("first run");
        assert!(!first.cache_hit);

        let second = run_phase_analysis(
            PhaseOptions {
                root: dir.path().to_path_buf(),
                top_n: 2,
                ..PhaseOptions::default()
            },
            PhaseSelection::Single(3),
        )
        .expect("second run");

        assert!(
            !second.cache_hit,
            "phase 3 cache must miss when top_n changes"
        );
    }
}
