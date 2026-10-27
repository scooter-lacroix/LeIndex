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
    /// Freshness generation hash. Serialized as `analysis_fingerprint`: it
    /// is a content hash over the analyzed inventory, NOT the store's
    /// monotonic generation counter that the freshness footer reports —
    /// using the same key for both made payloads carry two different
    /// "generation" values (N-07).
    #[serde(rename = "analysis_fingerprint")]
    pub generation: String,
    /// Executed phase list.
    pub executed_phases: Vec<u8>,
    /// Whether any summary cache hit occurred.
    pub cache_hit: bool,
    /// Changed file count.
    pub changed_files: usize,
    /// Deleted file count.
    pub deleted_files: usize,

    /// Whether every requested phase actually ran.
    ///
    /// A wall-clock ceiling stops stages from *starting*, so a truncated run
    /// still produces a report that is a truthful summary of the phases that
    /// ran and a silent lie about the rest. The human-readable `INCOMPLETE`
    /// line in `formatted_output` does not help a machine consumer that
    /// deserializes this payload and sees `Ok` plus a normal report. So the
    /// truncation has to be a field.
    ///
    /// `serde(default)` gives `false` to a payload that predates this field,
    /// which is the only safe direction: "unknown" must never read as
    /// "complete".
    #[serde(default)]
    pub complete: bool,

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

    /// Wall time per pipeline stage, in milliseconds.
    ///
    /// Paid for the hard way: a phase call on a 40-file project once took
    /// 518 s with every MCP sub-timing reporting 0, because nothing below
    /// `handler_ms` was measured. Without a breakdown the only available
    /// diagnosis was a guess, so the cold graph path and the cache path are
    /// now timed separately.
    pub stage_ms: std::collections::BTreeMap<String, u64>,
}

/// Run phase analysis for a project with incremental freshness and shared context.
pub fn run_phase_analysis(
    options: PhaseOptions,
    selection: PhaseSelection,
) -> Result<PhaseAnalysisReport> {
    let options = options.normalized();
    // Anchors both the per-stage timings and the wall-clock budget.
    let run_started = std::time::Instant::now();
    // Split the pipeline's wall time into stages so a slow run is diagnosable
    // from the response itself, without attaching a profiler to a live daemon.
    let mut stage_ms = std::collections::BTreeMap::new();
    macro_rules! timed {
        ($label:literal, $body:expr) => {{
            let started = std::time::Instant::now();
            let value = $body;
            stage_ms.insert($label.to_string(), started.elapsed().as_millis() as u64);
            value
        }};
    }
    let mut context = timed!("prepare", PhaseExecutionContext::prepare(&options))?;
    let cache = PhaseCache::new(&context.root);
    let mut executed_phases = Vec::new();
    let mut cache_hit = false;

    let mut phase1_summary = None;
    let mut phase2_summary = None;
    let mut phase3_summary = None;
    let mut phase4_summary = None;
    let mut phase5_summary = None;

    // Say which stages ran and why any did not, rather than letting a budget
    // truncation look like a complete report.
    let mut budget_exhausted = false;
    macro_rules! take_budget {
        () => {
            if budget_exhausted {
                false
            } else if options
                .budget_ms
                .is_some_and(|budget| run_started.elapsed().as_millis() as u64 >= budget)
            {
                budget_exhausted = true;
                false
            } else {
                true
            }
        };
    }

    if should_run(1, selection) && take_budget!() {
        let (summary, hit) = timed!("phase1", run_phase1(&cache, &mut context))?;
        phase1_summary = Some(summary);
        cache_hit |= hit;
        executed_phases.push(1);
    }
    if should_run(2, selection) && take_budget!() {
        let (summary, hit) = timed!("phase2", run_phase2(&cache, &mut context))?;
        phase2_summary = Some(summary);
        cache_hit |= hit;
        executed_phases.push(2);
    }
    if should_run(3, selection) && take_budget!() {
        let (summary, hit) = timed!("phase3", run_phase3(&cache, &mut context, &options))?;
        phase3_summary = Some(summary);
        cache_hit |= hit;
        executed_phases.push(3);
    }
    if should_run(4, selection) && take_budget!() {
        let (summary, hit) = timed!("phase4", run_phase4(&cache, &mut context, &options))?;
        phase4_summary = Some(summary);
        cache_hit |= hit;
        executed_phases.push(4);
    }
    if should_run(5, selection) && take_budget!() {
        let (summary, hit) = timed!(
            "phase5",
            run_phase5(
                &cache,
                &mut context,
                &options,
                phase1_summary.as_ref(),
                phase2_summary.as_ref(),
                phase3_summary.as_ref(),
                phase4_summary.as_ref(),
            )
        )?;
        phase5_summary = Some(summary);
        cache_hit |= hit;
        executed_phases.push(5);
    }

    // `prepare` only defers the graph; if every phase hit the cache the graph
    // was never built, so report that honestly rather than as a 0 ms stage.
    let graph_pending = context.graph_is_pending();
    let mut formatted_output = timed!(
        "format",
        format_report(
            &context,
            &executed_phases,
            phase1_summary.as_ref(),
            phase2_summary.as_ref(),
            phase3_summary.as_ref(),
            phase4_summary.as_ref(),
            phase5_summary.as_ref(),
            options.max_output_chars,
        )
    );
    if graph_pending {
        stage_ms.insert("graph".to_string(), 0);
    }
    // The report is the only thing a caller sees, so the breakdown has to be
    // in it: a stage that is elided from the text is a stage nobody profiles.
    let breakdown = stage_ms
        .iter()
        .map(|(stage, ms)| format!("{stage}={ms}ms"))
        .collect::<Vec<_>>()
        .join(" ");
    formatted_output.push_str(&format!("\nstage_ms: {breakdown}"));
    if budget_exhausted {
        // A truncated run must never read as a complete one. Name the ceiling,
        // the stages that actually ran, and how to lift it.
        let skipped = (1..=5)
            .filter(|phase| should_run(*phase, selection) && !executed_phases.contains(phase))
            .map(|phase| phase.to_string())
            .collect::<Vec<_>>()
            .join(",");
        formatted_output.push_str(&format!(
            "\nINCOMPLETE: stopped after {0}ms of a {1}ms budget; ran phases [{2}], \
             skipped [{3}]. Raise LEINDEX_PHASE_BUDGET_MS or run the remaining phases \
             individually with `phase`.",
            run_started.elapsed().as_millis(),
            options.budget_ms.unwrap_or_default(),
            executed_phases
                .iter()
                .map(|phase| phase.to_string())
                .collect::<Vec<_>>()
                .join(","),
            skipped
        ));
    }
    Ok(PhaseAnalysisReport {
        project_id: context.project_id,
        generation: context.generation_hash,
        executed_phases,
        cache_hit,
        changed_files: context.changed_files.len(),
        deleted_files: context.deleted_files.len(),
        complete: !budget_exhausted,
        phase1: phase1_summary,
        phase2: phase2_summary,
        phase3: phase3_summary,
        phase4: phase4_summary,
        phase5: phase5_summary,
        formatted_output,
        stage_ms,
    })
}

fn run_phase1(
    cache: &PhaseCache,
    context: &mut PhaseExecutionContext,
) -> Result<(Phase1Summary, bool)> {
    if let Some(cached) =
        cache.load::<Phase1Summary>(&context.project_id, &context.generation_hash, 1)?
    {
        return Ok((cached.payload, true));
    }

    context.ensure_graph()?;
    let summary = phase1::run(context);
    cache.save(&context.project_id, &context.generation_hash, 1, &summary)?;
    Ok((summary, false))
}

fn run_phase2(
    cache: &PhaseCache,
    context: &mut PhaseExecutionContext,
) -> Result<(Phase2Summary, bool)> {
    if let Some(cached) =
        cache.load::<Phase2Summary>(&context.project_id, &context.generation_hash, 2)?
    {
        return Ok((cached.payload, true));
    }

    context.ensure_graph()?;
    let summary = phase2::run(context);
    cache.save(&context.project_id, &context.generation_hash, 2, &summary)?;
    Ok((summary, false))
}

fn run_phase3(
    cache: &PhaseCache,
    context: &mut PhaseExecutionContext,
    options: &PhaseOptions,
) -> Result<(Phase3Summary, bool)> {
    let key = options_hash_for_phase(3, options);
    if let Some(cached) = cache.load_with_options::<Phase3Summary>(
        &context.project_id,
        &context.generation_hash,
        3,
        key.as_deref(),
    )? {
        return Ok((cached.payload, true));
    }

    context.ensure_graph()?;
    let summary = phase3::run(context, options);
    cache.save_with_options(
        &context.project_id,
        &context.generation_hash,
        3,
        key.as_deref(),
        &summary,
    )?;
    Ok((summary, false))
}

fn run_phase4(
    cache: &PhaseCache,
    context: &mut PhaseExecutionContext,
    options: &PhaseOptions,
) -> Result<(Phase4Summary, bool)> {
    let key = options_hash_for_phase(4, options);
    if let Some(cached) = cache.load_with_options::<Phase4Summary>(
        &context.project_id,
        &context.generation_hash,
        4,
        key.as_deref(),
    )? {
        return Ok((cached.payload, true));
    }

    context.ensure_graph()?;
    let summary = phase4::run(context, options);
    cache.save_with_options(
        &context.project_id,
        &context.generation_hash,
        4,
        key.as_deref(),
        &summary,
    )?;
    Ok((summary, false))
}

fn run_phase5(
    cache: &PhaseCache,
    context: &mut PhaseExecutionContext,
    options: &PhaseOptions,
    phase1_summary: Option<&Phase1Summary>,
    phase2_summary: Option<&Phase2Summary>,
    phase3_summary: Option<&Phase3Summary>,
    phase4_summary: Option<&Phase4Summary>,
) -> Result<(Phase5Summary, bool)> {
    let key = options_hash_for_phase(5, options);

    // Check the cache FIRST: a hit means phase1-4 never need to run.
    if let Some(cached) = cache.load_with_options::<Phase5Summary>(
        &context.project_id,
        &context.generation_hash,
        5,
        key.as_deref(),
    )? {
        return Ok((cached.payload, true));
    }

    context.ensure_graph()?;
    let phase1_summary = match phase1_summary {
        Some(summary) => summary.clone(),
        None => compute_and_cache_phase1(cache, context)?,
    };
    let phase2_summary = match phase2_summary {
        Some(summary) => summary.clone(),
        None => compute_and_cache_phase2(cache, context)?,
    };
    let phase3_summary = match phase3_summary {
        Some(summary) => summary.clone(),
        None => compute_and_cache_phase3(cache, context, options)?,
    };
    let phase4_summary = match phase4_summary {
        Some(summary) => summary.clone(),
        None => compute_and_cache_phase4(cache, context, options)?,
    };

    let summary = phase5::run(
        context,
        &phase1_summary,
        &phase2_summary,
        &phase3_summary,
        &phase4_summary,
    );
    cache.save_with_options(
        &context.project_id,
        &context.generation_hash,
        5,
        key.as_deref(),
        &summary,
    )?;
    Ok((summary, false))
}

fn should_run(phase: u8, selection: PhaseSelection) -> bool {
    match selection {
        PhaseSelection::Single(p) => p == phase,
        PhaseSelection::All => true,
    }
}

/// Compute a phase 1 summary and persist it to the cache.
///
/// Used as the fallback path in `run_phase5` when a caller-supplied summary
/// is not available. Without this persistence, a later request recomputes
/// the same summary unnecessarily.
fn compute_and_cache_phase1(
    cache: &PhaseCache,
    context: &PhaseExecutionContext,
) -> Result<Phase1Summary> {
    if let Some(cached) =
        cache.load::<Phase1Summary>(&context.project_id, &context.generation_hash, 1)?
    {
        return Ok(cached.payload);
    }
    let summary = phase1::run(context);
    cache.save(&context.project_id, &context.generation_hash, 1, &summary)?;
    Ok(summary)
}

fn compute_and_cache_phase2(
    cache: &PhaseCache,
    context: &PhaseExecutionContext,
) -> Result<Phase2Summary> {
    if let Some(cached) =
        cache.load::<Phase2Summary>(&context.project_id, &context.generation_hash, 2)?
    {
        return Ok(cached.payload);
    }
    let summary = phase2::run(context);
    cache.save(&context.project_id, &context.generation_hash, 2, &summary)?;
    Ok(summary)
}

fn compute_and_cache_phase3(
    cache: &PhaseCache,
    context: &PhaseExecutionContext,
    options: &PhaseOptions,
) -> Result<Phase3Summary> {
    let key = options_hash_for_phase(3, options);
    if let Some(cached) = cache.load_with_options::<Phase3Summary>(
        &context.project_id,
        &context.generation_hash,
        3,
        key.as_deref(),
    )? {
        return Ok(cached.payload);
    }
    let summary = phase3::run(context, options);
    cache.save_with_options(
        &context.project_id,
        &context.generation_hash,
        3,
        key.as_deref(),
        &summary,
    )?;
    Ok(summary)
}

fn compute_and_cache_phase4(
    cache: &PhaseCache,
    context: &PhaseExecutionContext,
    options: &PhaseOptions,
) -> Result<Phase4Summary> {
    let key = options_hash_for_phase(4, options);
    if let Some(cached) = cache.load_with_options::<Phase4Summary>(
        &context.project_id,
        &context.generation_hash,
        4,
        key.as_deref(),
    )? {
        return Ok(cached.payload);
    }
    let summary = phase4::run(context, options);
    cache.save_with_options(
        &context.project_id,
        &context.generation_hash,
        4,
        key.as_deref(),
        &summary,
    )?;
    Ok(summary)
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

        if p1.cache_hit {
            lines.push(format!(
                "phase1: files={} cache_hit=true (incremental: no files changed, PDG loaded from cache) signatures_from_pdg={} parser_completeness_avg={:.2}",
                p1.total_files,
                p1.signatures,
                avg_completeness
            ));
        } else {
            lines.push(format!(
                "phase1: files={} parsed={} failures={} signatures={} parser_completeness_avg={:.2}",
                p1.total_files, p1.parsed_files, p1.parse_failures, p1.signatures, avg_completeness
            ));
        }
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
        // Any run that reaches `ensure_graph` runs the precision ingest pass,
        // which reads process-global `LEINDEX_SCIP_*` vars and spawns an
        // external indexer. Hold the process-wide lock so a test that rewires
        // those vars cannot overlap with this one (see `lock_flag_tests`).
        let _scip_guard = crate::feature_flags::lock_flag_tests();
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
        // Reaches precision ingest via `ensure_graph`; see
        // `single_phase_report_contains_only_requested_phase`.
        let _scip_guard = crate::feature_flags::lock_flag_tests();
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

    #[test]
    fn exhausted_budget_skips_remaining_phases_and_says_so() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn f()->i32{1}\n").expect("write");

        // A zero-nanosecond ceiling is already spent by the time `prepare`
        // returns, so the gate must refuse to start phase 1. This is the whole
        // contract: no further stage starts once the ceiling is passed.
        let report = run_phase_analysis(
            PhaseOptions {
                root: dir.path().to_path_buf(),
                budget_ms: Some(0),
                ..PhaseOptions::default()
            },
            PhaseSelection::All,
        )
        .expect("phase run");

        assert!(
            report.executed_phases.is_empty(),
            "no phase may start once the budget is spent, got {:?}",
            report.executed_phases
        );
        assert!(report.phase1.is_none());
        assert!(
            !report.complete,
            "an exhausted budget must set the structured `complete` flag, not only \
             bury a marker in formatted_output: a machine consumer that reads the \
             payload would otherwise see Ok plus a normal report"
        );
        assert!(
            report.formatted_output.contains("INCOMPLETE"),
            "a truncated run must not read as a complete one: {}",
            report.formatted_output
        );
    }

    #[test]
    fn unbounded_budget_runs_every_requested_phase() {
        // Runs all five phases, so it reaches precision ingest; see
        // `single_phase_report_contains_only_requested_phase`.
        let _scip_guard = crate::feature_flags::lock_flag_tests();
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn f()->i32{1}\n").expect("write");

        // The default is unbounded, so all five phases run and nothing is
        // reported as incomplete. This is the guard against a truncation
        // sneaking into the default path.
        let report = run_phase_analysis(
            PhaseOptions {
                root: dir.path().to_path_buf(),
                budget_ms: None,
                ..PhaseOptions::default()
            },
            PhaseSelection::All,
        )
        .expect("phase run");

        assert_eq!(report.executed_phases, vec![1, 2, 3, 4, 5]);
        assert!(
            report.complete,
            "a run that executed every requested phase must report complete, or the \
             flag is useless as a signal"
        );
        assert!(!report.formatted_output.contains("INCOMPLETE"));
    }
}
