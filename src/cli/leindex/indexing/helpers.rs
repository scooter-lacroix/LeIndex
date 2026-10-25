//! Indexing-pipeline free helpers: phase-injection guards, progress output,
//! checkpoint reuse, and scan/parse/PDG-stat computation. Extracted from the
//! main pipeline module to keep it under the line-count gate.

use super::IndexPipelineState;
use crate::cli::index_job::{
    CheckpointStore, FileFingerprint, LexicalCheckpoint, ParseCheckpoint, PdgCheckpoint,
    ScanCheckpoint,
};
use crate::cli::memory_cap::{CapStatus, MemoryCapGuard};
use anyhow::{Result, bail};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tracing::warn;
pub(super) fn injected_phase_failure(phase: &str) -> Result<()> {
    if std::env::var("LEINDEX_INJECT_FAILURE_PHASE")
        .ok()
        .is_some_and(|value| value.eq_ignore_ascii_case(phase))
    {
        bail!("injected indexing failure after reusable {phase} artifact")
    }
    Ok(())
}

/// Read a checkpoint artifact only when its on-disk blake3 hash matches the
/// hash recorded in the checkpoint state. Returns `None` when the artifact is
/// absent or stale (hash mismatch / unreadable). Factored out of `run_scan` so
/// the parse/lexical/neural resume lookups share one verify path.
pub(super) fn read_verified_artifact<T>(
    state: Option<&crate::cli::index_job::JobCheckpointState>,
    artifact: &str,
    artifact_path: &std::path::Path,
    read: impl Fn(&CheckpointStore) -> Result<Option<T>>,
    store: &CheckpointStore,
) -> Option<T> {
    state
        .and_then(|state| state.artifact_hashes.get(artifact).cloned())
        .and_then(|expected_hash| {
            let bytes = std::fs::read(artifact_path).ok()?;
            (blake3::hash(&bytes).to_hex().as_str() == expected_hash).then_some(())?;
            read(store).ok().flatten()
        })
}

fn path_contains_component_boundary(value: &str, component: &str) -> bool {
    let value = value.replace('\\', "/");
    let component = component.trim_matches('/');
    value == component
        || value.strip_prefix(&format!("{component}/")).is_some()
        || value.strip_suffix(&format!("/{component}")).is_some()
        || value.contains(&format!("/{component}/"))
}

pub(super) fn add_submodule_summary_nodes(
    pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
    project_path: &std::path::Path,
) {
    let Ok(summaries) = crate::cli::git::submodule_summaries(project_path) else {
        return;
    };
    for summary in summaries {
        let relative = summary
            .path
            .strip_prefix(project_path)
            .unwrap_or(&summary.path)
            .to_string_lossy()
            .replace('\\', "/");
        let node_id = format!("submodule:{relative}:{}", summary.commit_oid);
        if pdg
            .node_indices()
            .filter_map(|index| pdg.get_node(index))
            .any(|node| node.id == node_id)
        {
            continue;
        }
        let name = summary
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&relative)
            .to_string();
        let module_index = pdg.add_node(crate::graph::pdg::Node {
            id: node_id,
            node_type: crate::graph::pdg::NodeType::Module,
            name,
            file_path: std::sync::Arc::from(summary.path.to_string_lossy().to_string()),
            byte_range: (0, 0),
            complexity: 0,
            language: format!("git-submodule:{}", summary.commit_oid),
        });
        let importers = pdg
            .node_indices()
            .filter(|index| *index != module_index)
            .filter_map(|index| pdg.get_node(index).map(|node| (index, node)))
            .filter(|(_, node)| {
                node.node_type == crate::graph::pdg::NodeType::External
                    && (path_contains_component_boundary(&node.name, &relative)
                        || path_contains_component_boundary(&node.id, &relative))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for importer in importers {
            pdg.add_edge(
                importer,
                module_index,
                crate::graph::pdg::Edge {
                    edge_type: crate::graph::pdg::EdgeType::Import,
                    metadata: crate::graph::pdg::EdgeMetadata::empty(),
                },
            );
        }
    }
}

/// Write a progress line to stderr if stderr is a terminal.
/// Uses `\r` to overwrite the current line (no newline).
/// This is a no-op when stderr is not a terminal (e.g., MCP/stdio mode).
pub(super) fn progress_stderr(msg: &str) {
    use std::io::{IsTerminal, Write};
    let stderr = std::io::stderr();
    if stderr.is_terminal() {
        let mut handle = stderr.lock();
        // Clear the line first, then write the new content
        let _ = write!(handle, "\r\x1b[K{}", msg);
        let _ = handle.flush();
    }
}

/// Cumulative (hits, misses) of the client-side embed caches for this
/// process — surfaces cache effectiveness in neural-phase progress lines.
///
/// Both routes share one store and one progress line, so their counters are
/// summed: a row served from the remote cache is exactly as "not embedded" as
/// one served from the local worker cache.
#[cfg(feature = "onnx")]
pub(super) fn neural_cache_counters() -> (u64, u64) {
    #[allow(unused_mut)]
    let (mut hits, mut misses) = crate::search::onnx::embed_cache_frontend::counters();
    #[cfg(feature = "remote-embeddings")]
    {
        let (remote_hits, remote_misses, _) = crate::search::onnx::remote_cache::counters();
        hits += remote_hits;
        misses += remote_misses;
    }
    (hits, misses)
}

#[cfg(not(feature = "onnx"))]
pub(super) fn neural_cache_counters() -> (u64, u64) {
    (0, 0)
}

/// Clear the progress line on stderr (when terminal).
pub(super) fn progress_clear() {
    use std::io::{IsTerminal, Write};
    let stderr = std::io::stderr();
    if stderr.is_terminal() {
        let mut handle = stderr.lock();
        let _ = write!(handle, "\r\x1b[K");
        let _ = handle.flush();
    }
}

/// Throttled writer for intra-phase progress into `index-state.json`.
///
/// A phase can run for over a minute (the parse and neural phases own ~98% of
/// a cold run's wall time), and `mark_index_phase` only fires when a phase
/// *starts*. Without this, a status reader sees one phase name for the whole
/// duration and cannot tell forward progress from a hang.
///
/// Why a reported count cannot simply be written per file: `save_health` is an
/// atomic write that fsyncs both the file and its directory. Doing that
/// thousands of times per parse would turn a progress indicator into a
/// measurable share of the run. Writes here are therefore gated on
/// [`Self::INTERVAL`] and on the reported value actually changing.
///
/// Writes are also best-effort: a failed status write must never fail an
/// index. The snapshot is advisory until the final publish replaces it.
pub(super) struct PhaseProgress {
    storage_path: PathBuf,
    phase: crate::cli::leindex::IndexPhase,
    total: usize,
    last_write: std::cell::Cell<std::time::Instant>,
    last_done: std::cell::Cell<usize>,
}

impl PhaseProgress {
    /// Begin reporting for `phase` out of `total` items.
    ///
    /// Writes immediately so the total reaches readers before the first
    /// interval elapses — otherwise a reader polling right after the phase
    /// starts sees the previous phase's stale count.
    pub(super) fn start(
        storage_path: PathBuf,
        phase: crate::cli::leindex::IndexPhase,
        total: usize,
    ) -> Self {
        let progress = Self {
            storage_path,
            phase,
            total,
            // Long enough ago that the first `report(0)` always writes.
            last_write: std::cell::Cell::new(
                std::time::Instant::now() - Self::INTERVAL - std::time::Duration::from_secs(1),
            ),
            last_done: std::cell::Cell::new(usize::MAX),
        };
        progress.report(0);
        progress
    }

    /// Record that `done` items have finished. Cheap: usually a no-op.
    pub(super) fn report(&self, done: usize) {
        if done == self.last_done.get() || self.last_write.get().elapsed() < Self::INTERVAL {
            return;
        }
        self.last_done.set(done);
        self.last_write.set(std::time::Instant::now());
        // `load_health` can return None for a project that has never published
        // (a cold first index). In that case there is no generation to attach
        // progress to and `mark_index_phase` already wrote a default snapshot
        // with no total, so skip rather than fabricate a zero generation.
        let Some(mut health) = crate::cli::index_freshness::load_health(&self.storage_path) else {
            return;
        };
        // Do not attribute a count to a phase the snapshot no longer reports:
        // a concurrent publish may have moved on, and "42 of 900" under the
        // wrong phase name is worse than no number at all.
        if health.phase != self.phase {
            return;
        }
        health.progress_total = Some(self.total);
        health.progress_done = done;
        let _ = crate::cli::index_freshness::save_health(&self.storage_path, &health);
    }

    /// Clear the counters once the phase is over, so a finished generation
    /// never shows a frozen "900 of 900" bar.
    pub(super) fn finish(&self) {
        let Some(mut health) = crate::cli::index_freshness::load_health(&self.storage_path) else {
            return;
        };
        if health.phase != self.phase {
            return;
        }
        health.progress_total = None;
        health.progress_done = 0;
        let _ = crate::cli::index_freshness::save_health(&self.storage_path, &health);
    }
}

impl PhaseProgress {
    /// Minimum gap between durable writes for one phase.
    pub(super) const INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
}

/// Drives a [`PhaseProgress`] from a parser's per-file completion callback.
///
/// Two constraints shape this, and they are why it is not just an `Arc`:
///
/// 1. `PhaseProgress::report` uses non-`Sync` interior mutability for its
///    throttle, while the parse callback runs on rayon worker threads. So the
///    workers may only bump a counter; exactly ONE other thread samples it and
///    performs the write. That keeps the snapshot single-writer.
/// 2. The sampler must never outlive the phase, so [`Self::finish`] takes
///    `self` by value — the thread is stopped and joined by construction, and a
///    caller cannot forget it while still holding the handle.
///
/// The result is a handful of atomic fsync writes for a several-hundred-file
/// parse, instead of one per file.
pub(super) struct ParseProgressHeartbeat {
    done: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ParseProgressHeartbeat {
    /// Begin reporting for `phase` out of `total`, with a sampler thread that
    /// wakes every [`PhaseProgress::INTERVAL`].
    pub(super) fn start(
        storage_path: PathBuf,
        phase: crate::cli::leindex::IndexPhase,
        total: usize,
    ) -> Self {
        let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reporter = PhaseProgress::start(storage_path, phase, total);
        let (counter, stop_thread) = (done.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("index-parse-progress".to_owned())
            .spawn(move || {
                while !stop_thread.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(PhaseProgress::INTERVAL);
                    reporter.report(counter.load(std::sync::atomic::Ordering::Relaxed));
                }
                // Final sample, then close. The last interval may have ended
                // just short of the total, and a bar frozen at "397 of 400"
                // reads as a truncated run.
                reporter.report(counter.load(std::sync::atomic::Ordering::Relaxed));
                reporter.finish();
            })
            .ok();
        Self { done, stop, thread }
    }

    /// The closure to hand a parser. It only stores — see the type docs.
    ///
    /// `use<>` promises the returned closure captures nothing from `&self`:
    /// it clones the `Arc`, so the caller can hand out a callback and then
    /// still move the heartbeat into `finish`.
    pub(super) fn callback(&self) -> impl Fn(usize) + Sync + use<> {
        let counter = self.done.clone();
        move |finished: usize| counter.store(finished, std::sync::atomic::Ordering::Relaxed)
    }

    /// Stop and join the sampler. Consumes `self` so it cannot be skipped.
    pub(super) fn finish(self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread {
            let _ = thread.join();
        }
    }
}

/// Dispatch a parse over the requested route.
///
/// `on_progress` is called with the running count of files that have finished
/// parsing; `None` keeps every caller (and the tests) unchanged. Lives here
/// rather than beside the pipeline because it is a pure function of its
/// arguments — no `LeIndex` state — and the pipeline module has a line budget.
pub(super) fn parse_files_for_route(
    files: Vec<PathBuf>,
    scan: &ScanCheckpoint,
    route: super::streaming::routes::ParseRoute,
    on_progress: Option<&(dyn Fn(usize) + Sync)>,
) -> Vec<crate::parse::parallel::ParsingResult> {
    use super::streaming;
    match route {
        streaming::routes::ParseRoute::Streaming => {
            // Byte sizes come from the scan inventory (single hashing
            // authority); anything missing sizes to 0 and still parses.
            let file_sizes: HashMap<PathBuf, u64> = scan
                .files
                .iter()
                .map(|file| (file.canonical_path.clone(), file.bytes))
                .collect();
            let bounded = files
                .into_iter()
                .map(|path| {
                    let bytes = file_sizes.get(&path).copied().unwrap_or(0);
                    (path, bytes)
                })
                .collect();
            streaming::parse::stream_parse_parallel(
                bounded,
                &streaming::parse::ParseBudget::default(),
                on_progress,
            )
        }
        streaming::routes::ParseRoute::Legacy => {
            crate::parse::parallel::ParallelParser::new().parse_files_counting(files, on_progress)
        }
    }
}

/// Observe RSS against the memory cap at an indexing phase boundary.
///
/// VAL-SCHED-015: this is NOT an error path anymore. Over-cap RSS reports
/// `CapStatus::OverCap` (a deferral signal already logged by the guard) and
/// indexing continues; capacity decisions belong to the global admission
/// controller (`scheduler::admission`), which defers/reduces instead of
/// erroring. A cap prevents overlapping peaks — never valid-work failures.
pub(super) fn check_memory_cap(cap_guard: &mut Option<&mut MemoryCapGuard>) -> Result<()> {
    if let Some(guard) = cap_guard.as_mut() {
        match guard.check_now() {
            CapStatus::Ok => {}
            CapStatus::OverCap => {
                // Deferral signal: the guard has already logged the pressure.
                // The phase continues; heavy work is gated by admission.
            }
        }
    }
    Ok(())
}

pub(super) fn load_resumed_pdg(
    store: &CheckpointStore,
    scan: &ScanCheckpoint,
    resumed_scan: bool,
    artifact_hash: Option<String>,
) -> Option<(PdgCheckpoint, crate::graph::pdg::ProgramDependenceGraph)> {
    if !resumed_scan {
        return None;
    }
    let hash = artifact_hash?;
    let metadata = store.read_pdg_checkpoint().ok().flatten();
    let checkpoint = match metadata {
        Some(checkpoint)
            if checkpoint.scan_hash == scan.input_hash && checkpoint.artifact_hash == hash =>
        {
            checkpoint
        }
        Some(_) => return None,
        None => PdgCheckpoint {
            scan_hash: scan.input_hash.clone(),
            artifact_path: store.paths.pdg(),
            artifact_hash: hash,
            nodes: 0,
            edges: 0,
        },
    };
    let pdg = store
        .read_pdg_artifact(&checkpoint.artifact_hash)
        .ok()
        .flatten()?;
    Some((
        PdgCheckpoint {
            nodes: if checkpoint.nodes == 0 {
                pdg.node_count()
            } else {
                checkpoint.nodes
            },
            edges: if checkpoint.edges == 0 {
                pdg.edge_count()
            } else {
                checkpoint.edges
            },
            ..checkpoint
        },
        pdg,
    ))
}

pub(super) fn valid_lexical_checkpoint(checkpoint: &LexicalCheckpoint) -> bool {
    checkpoint.snapshot_path.is_file()
        && checkpoint.tfidf_path.is_file()
        && checkpoint
            .snapshot_path
            .metadata()
            .is_ok_and(|metadata| metadata.len() > 0)
        && checkpoint
            .tfidf_path
            .metadata()
            .is_ok_and(|metadata| metadata.len() > 0)
}

pub(super) fn scan_checkpoint(source_files_with_hashes: &[(PathBuf, String)]) -> ScanCheckpoint {
    let files = source_files_with_hashes
        .iter()
        .map(|(path, hash)| FileFingerprint {
            canonical_path: path.clone(),
            blake3: hash.clone(),
            bytes: std::fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or_default(),
            language: path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("unknown")
                .to_string(),
        })
        .collect::<Vec<_>>();
    ScanCheckpoint {
        input_hash: crate::cli::index_job::scan_hash(&files),
        files,
    }
}

pub(super) struct ParsePlan {
    pub(super) source_file_hashes: HashMap<String, String>,
    pub(super) current_file_paths: HashSet<String>,
    pub(super) files_to_parse: Vec<PathBuf>,
    pub(super) unchanged_files: HashSet<String>,
    pub(super) deleted_files: Vec<String>,
}

pub(super) fn parse_plan(state: &IndexPipelineState) -> ParsePlan {
    let source_file_hashes = state
        .source_files_with_hashes
        .iter()
        .map(|(path, hash)| (path.display().to_string(), hash.clone()))
        .collect::<HashMap<_, _>>();
    let current_file_paths = state
        .source_files_with_hashes
        .iter()
        .map(|(path, _)| path.display().to_string())
        .collect::<HashSet<_>>();
    let mut files_to_parse = Vec::new();
    let mut unchanged_files = HashSet::new();
    for (path, hash) in &state.source_files_with_hashes {
        let path_str = path.display().to_string();
        if state.force
            || !state.indexed_files.contains_key(&path_str)
            || state.indexed_files.get(&path_str) != Some(hash)
        {
            files_to_parse.push(path.clone());
        } else {
            unchanged_files.insert(path_str);
        }
    }
    let deleted_files = state
        .indexed_files
        .keys()
        .filter(|path| !current_file_paths.contains(*path))
        .cloned()
        .collect();
    ParsePlan {
        source_file_hashes,
        current_file_paths,
        files_to_parse,
        unchanged_files,
        deleted_files,
    }
}

pub(super) fn reuse_parse_results(
    resumed_scan: bool,
    resumed_parse: Option<&ParseCheckpoint>,
    store: &CheckpointStore,
    source_file_hashes: &HashMap<String, String>,
    files_to_parse: &mut Vec<PathBuf>,
) -> Result<Vec<crate::parse::parallel::ParsingResult>> {
    if !resumed_scan {
        return Ok(Vec::new());
    }
    let Some(parse_checkpoint) = resumed_parse else {
        return Ok(Vec::new());
    };
    let mut results = Vec::new();
    let mut reused_paths = HashSet::new();
    for path in files_to_parse.iter() {
        let path_key = path.display().to_string();
        let Some(source_hash) = source_file_hashes.get(&path_key) else {
            continue;
        };
        let Some(expected_hash) = parse_checkpoint.artifact_hashes.get(source_hash) else {
            continue;
        };
        let parsed = match store.read_parsed_for_path_verified(source_hash, expected_hash, path) {
            Ok(Some(parsed)) => parsed,
            Ok(None) => continue,
            Err(error) => {
                warn!(
                    "Unable to reuse parsed checkpoint for '{}': {}; reparsing",
                    path.display(),
                    error
                );
                continue;
            }
        };
        // Re-read the file per chunk (VAL-STREAM-012: no cross-phase cache).
        let source_bytes = match crate::cli::index_builder::read_file_once(path) {
            Ok(bytes) => bytes.1.as_ref().clone(),
            Err(error) => {
                warn!(
                    "Unable to reuse source bytes for '{}': {}; reparsing",
                    path.display(),
                    error
                );
                continue;
            }
        };
        results.push(crate::parse::parallel::ParsingResult {
            file_path: parsed.file_path,
            language: Some(parsed.language),
            signatures: parsed.signatures,
            source_bytes: Some(source_bytes),
            error: None,
            parse_time_ms: parsed.parse_time_ms,
        });
        reused_paths.insert(path.clone());
    }
    files_to_parse.retain(|path| !reused_paths.contains(path));
    Ok(results)
}

pub(super) struct PdgParseStats {
    pub(super) files_parsed: usize,
    pub(super) successful: usize,
    pub(super) failed: usize,
    pub(super) total_sigs: usize,
    pub(super) all_signatures: Vec<(String, crate::parse::prelude::SignatureInfo)>,
}

pub(super) fn pdg_parse_stats(results: &[crate::parse::parallel::ParsingResult]) -> PdgParseStats {
    let successful = results.iter().filter(|result| result.is_success()).count();
    let failed = results.iter().filter(|result| result.is_failure()).count();
    let total_sigs = results.iter().map(|result| result.signatures.len()).sum();
    for result in results.iter().filter(|result| result.is_failure()) {
        warn!(
            "Parse failure for '{}' during indexing: {}",
            result.file_path.display(),
            result
                .error
                .as_deref()
                .filter(|error| !error.is_empty())
                .unwrap_or("unknown error")
        );
    }
    if failed > 0 {
        warn!(
            "Indexing completed with {} parse failure(s) out of {} file(s)",
            failed,
            successful + failed
        );
    }
    let all_signatures = results
        .iter()
        .filter(|result| result.is_success())
        .flat_map(|result| {
            let file_path = result.file_path.display().to_string();
            result
                .signatures
                .iter()
                .cloned()
                .map(move |signature| (file_path.clone(), signature))
        })
        .collect();
    PdgParseStats {
        files_parsed: results.len(),
        successful,
        failed,
        total_sigs,
        all_signatures,
    }
}

pub(super) fn git_tree_oid(project_path: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD^{tree}"])
        .current_dir(project_path)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_contains_component_boundary_normalizes_separators() {
        assert!(path_contains_component_boundary(
            "src\\submodule\\lib.rs",
            "src/submodule"
        ));
        assert!(path_contains_component_boundary(
            "src/submodule/lib.rs",
            "src/submodule"
        ));
        assert!(!path_contains_component_boundary(
            "src/submodule-extra/lib.rs",
            "src/submodule"
        ));
        assert!(!path_contains_component_boundary(
            "src/not-submodule/lib.rs",
            "src/submodule"
        ));
    }
}
