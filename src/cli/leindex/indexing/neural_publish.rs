use super::*;
use std::path::Path;

struct HybridStreamingEmbedder<'a> {
    inner: &'a index_builder::GenerationNeuralEmbedder,
}

impl streaming::neural::StreamingEmbedder for HybridStreamingEmbedder<'_> {
    fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
        let untitled: Vec<Option<String>> = std::iter::repeat_n(None, texts.len()).collect();
        self.embed_batch_titled(texts, &untitled)
    }

    fn embed_batch_titled(&self, texts: &[String], titles: &[Option<String>]) -> Vec<Vec<f32>> {
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        {
            if titles.len() != texts.len() {
                // Aligned-slices contract: a misshapen call is the same
                // embedder-violation class as the count mismatch below.
                return Vec::new();
            }
            // Dedup key is (title, capped_text): the same body under two
            // titles must embed twice — the title joins the remote doc
            // template, so the wire texts differ.
            let mut unique_texts: Vec<&str> = Vec::new();
            let mut unique_titles: Vec<Option<String>> = Vec::new();
            let mut positions = HashMap::new();
            for (text, title) in texts.iter().zip(titles) {
                let capped = index_builder::cap_neural_text(text);
                if let std::collections::hash_map::Entry::Vacant(slot) =
                    positions.entry((title, capped))
                {
                    slot.insert(unique_texts.len());
                    unique_texts.push(capped);
                    unique_titles.push(title.clone());
                }
            }
            let embeddings = self
                .inner
                .embed_neural_batch_blocking_titled(&unique_texts, unique_titles);
            if embeddings.len() != unique_texts.len() {
                // Count mismatch = embedder contract violation. Return a
                // length that cannot match `texts.len()` so `flush_batch`'s
                // hard-error fires and run_neural's failure path handles it —
                // a masked mismatch (empty vectors => rows_skipped) would
                // persist partial batches and resume would skip those rows
                // forever (PR #90 review, cluster E).
                return Vec::new();
            }
            let embeddings_by_text: Vec<Option<Vec<f32>>> = embeddings;
            texts
                .iter()
                .zip(titles)
                .map(|(text, title)| {
                    positions
                        .get(&(title, index_builder::cap_neural_text(text)))
                        .and_then(|&position| embeddings_by_text[position].clone())
                        .unwrap_or_default()
                })
                .collect()
        }
        #[cfg(not(any(feature = "onnx", feature = "remote-embeddings")))]
        {
            let _ = (self.inner, texts, titles);
            Vec::new()
        }
    }
}

struct SearchNeuralRowWriter<'a> {
    engine: &'a mut crate::search::search::SearchEngine,
    seen_node_ids: HashSet<String>,
}

impl streaming::neural::NeuralRowWriter for SearchNeuralRowWriter<'_> {
    fn write_row(&mut self, node_id: &str, embedding: &[f32]) -> Result<()> {
        self.write_batch(vec![(node_id.to_owned(), embedding.to_vec())])?;
        Ok(())
    }

    fn write_batch(&mut self, mut rows: Vec<(String, Vec<f32>)>) -> Result<usize> {
        rows.retain(|(node_id, _)| self.seen_node_ids.insert(node_id.clone()));
        Ok(self.engine.update_neural_embeddings(rows))
    }
}

/// Neural-phase progress: a background heartbeat line plus a row counter the
/// streaming writer bumps per completed batch.
///
/// Why a heartbeat: the "neural embedding N admitted nodes" line is the LAST
/// output before enrichment, which on a CPU-fallen-back embedder takes tens of
/// minutes for a real project — reading as a hang. The heartbeat prints
/// immediately (before the first batch completes, which alone can take
/// minutes) and then every `INTERVAL`, so the phase is visibly alive. Real
/// `n/N` counts come from `ProgressNeuralRowWriter` around the streaming
/// writer, so the counter advances as rows land rather than only at
/// completion. Stopped/joined on drop — never outlives enrichment.
///
/// The heartbeat also watches for a CPU-fallen-back embedder when a GPU
/// provider was requested, and announces it on this surface: the worker's own
/// WARN lands in `~/.leindex/logs/leindex-embed-daemon.log`, invisible to
/// someone watching the CLI sit still. The watch runs concurrently with the
/// phase because the daemon is spawned by the first embed call — a
/// pre-phase poll would see nothing (and blocking on it would stall a cold
/// start for no gain). After the fallback is observed, every heartbeat line
/// carries the warning suffix so it cannot scroll away.
struct NeuralProgress {
    rows: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Whether the configured execution provider names a GPU backend whose
/// silent CPU fallback should be surfaced. `cpu`/`auto`/anything else is a
/// deliberate or runtime-chosen configuration — no warning.
#[cfg(feature = "onnx")]
fn is_gpu_provider(configured: &str) -> bool {
    matches!(configured, "migraphx" | "cuda" | "rocm")
}

/// Explain a neural phase that is about to run with no embedder.
///
/// A missing embedder is a deliberate, silent no-op everywhere downstream:
/// `restore_neural_checkpoint` and the enrichment `if let` both yield zero
/// rows, the phase finishes in milliseconds, and the run prints a normal
/// success summary. The only user-visible clue is a store that stayed
/// TF-IDF-only. Say why, once.
///
/// The usual cause is a `leindex.toml` whose `[neural] enabled` was never set
/// — it defaults to false so that upgrading an existing config cannot silently
/// opt a user into a worker download — while `[indexing.embeddings]` (remote
/// endpoint, provider preset) *is* configured and looks authoritative. Naming
/// the actual gate is the whole point of the two branches: "neural is off"
/// and "neural is on but every backend failed" have completely different
/// fixes, and the second one is invisible without this.
fn warn_if_missing_neural_embedder(embedder: Option<&index_builder::GenerationNeuralEmbedder>) {
    if embedder.is_some() {
        return;
    }
    let config = crate::config::LeIndexConfig::load_cached();
    if crate::feature_flags::FeatureFlag::NeuralSearch.is_enabled()
        && !crate::feature_flags::is_neural_enabled(config.neural.enabled)
    {
        warn!(
            "[neural] enabled = false in leindex.toml: skipping neural embedding entirely (this \
             run is TF-IDF only). Set `enabled = true` under [neural] to build the neural layer; \
             [indexing.embeddings] settings alone do not enable it."
        );
    } else {
        warn!(
            neural_enabled = config.neural.enabled,
            search_mode = %config.search.search_mode,
            "[neural] is enabled but no embedding backend could be constructed; this run is \
             TF-IDF only. Check the local ONNX worker and any remote endpoint with `leindex \
             diagnostics`; rerun with RUST_LOG=debug for the backend errors."
        );
    }
}

impl NeuralProgress {
    const INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

    /// Suffix for the heartbeat once the phase has run long enough that a
    /// pinned row counter is diagnostic rather than merely early.
    ///
    /// A remote provider that is unreachable costs `timeout_secs *
    /// max_retries` of *silence* before the generation fallback fires —
    /// measured 115s against a dead port with the defaults (30s × 3), and the
    /// per-request `WARN` only appears at the end. On a real project that
    /// window reads exactly like a hang, and the elapsed counter climbing
    /// proves nothing. Say what is happening as soon as the window is long
    /// enough to be suspicious (`WARN_AFTER`): before the first row lands the
    /// phase is either still connecting or already grinding through retries.
    fn stuck_hint(rows: usize, elapsed: std::time::Duration) -> &'static str {
        const WARN_AFTER: std::time::Duration = std::time::Duration::from_secs(45);
        if rows > 0 || elapsed < WARN_AFTER {
            return "";
        }
        " [no rows yet — provider slow or unreachable; each failed request waits \
timeout_secs × max_retries before falling back]"
    }

    fn start(total: usize) -> Self {
        let rows = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (rows_t, stop_t) = (rows.clone(), stop.clone());
        // Arm the CPU-fallback watch only when the LOCAL ONNX worker owns the
        // neural layer and a GPU provider was requested. When a remote
        // endpoint takes over (enable_hybrid with a resolved remote), the
        // local daemon may never spawn — probing it is meaningless, and a
        // stale CPU daemon left over from an earlier run must not raise a
        // false warning.
        #[cfg(feature = "onnx")]
        let gpu_requested = {
            let config = crate::config::LeIndexConfig::load_cached();
            config.indexing.embeddings.resolved_remote().is_none()
                && is_gpu_provider(&config.neural.execution_provider.to_ascii_lowercase())
        };
        #[cfg(not(feature = "onnx"))]
        let gpu_requested = false;
        // Best-effort: a failed spawn must not fail indexing — the counter
        // still works for the streaming writer, only the heartbeat is lost.
        let thread = std::thread::Builder::new()
            .name("neural-progress".to_owned())
            .spawn(move || {
                let started = std::time::Instant::now();
                // cfg-split decls: without `onnx` neither binding is ever
                // reassigned (the probe block below is feature-gated out),
                // so `mut` would trip unused_mut in narrow builds.
                #[cfg(feature = "onnx")]
                let mut cpu_fallback = false;
                #[cfg(not(feature = "onnx"))]
                let cpu_fallback = false;
                #[cfg(feature = "onnx")]
                let mut provider_resolved = !gpu_requested;
                #[cfg(not(feature = "onnx"))]
                let provider_resolved = !gpu_requested;
                loop {
                    // Probe the daemon once per heartbeat while the provider
                    // is still unknown. The health socket publishes the
                    // provider only after model init (observed ~45s cold:
                    // rocprof + MIGraphX probe), so this sees through both
                    // cold and warm starts without delaying the phase.
                    // Gated like `gpu_requested` above: without onnx there is
                    // no local daemon to probe, and provider_resolved starts
                    // true (`!gpu_requested`), so the gate never fires.
                    if !provider_resolved {
                        #[cfg(feature = "onnx")]
                        {
                            match crate::search::onnx::daemon_active_provider() {
                                Some((active, _phase)) if active.eq_ignore_ascii_case("cpu") => {
                                    cpu_fallback = true;
                                    provider_resolved = true;
                                    progress_stderr(
                                        "\nWarning: neural embedder fell back to CPU after a GPU \
                                         provider was requested; inference is 100-1000x slower — see \
                                         leindex.toml execution_provider.\nDetails: \
                                         ~/.leindex/logs/leindex-embed-daemon.log\n",
                                    );
                                }
                                Some(_) => provider_resolved = true,
                                None => {}
                            }
                        }
                    }
                    let suffix = if cpu_fallback {
                        " [CPU fallback — see leindex.toml execution_provider]"
                    } else {
                        ""
                    };
                    let stuck = Self::stuck_hint(
                        rows_t.load(std::sync::atomic::Ordering::Relaxed),
                        started.elapsed(),
                    );
                    progress_stderr(&format!(
                        "Indexing: neural embedding {}/{} nodes ({}s elapsed)...{suffix}{stuck}",
                        rows_t.load(std::sync::atomic::Ordering::Relaxed),
                        total,
                        started.elapsed().as_secs()
                    ));
                    for _ in 0..(Self::INTERVAL.as_millis() / 100) {
                        if stop_t.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            })
            .ok();
        Self { rows, stop, thread }
    }
}

impl Drop for NeuralProgress {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A `NeuralRowWriter` that advances a shared `NeuralProgress` row counter as
/// batches land, so the heartbeat shows real `n/N` progress on the streaming
/// route. Generic over the inner writer so tests drive it with
/// `VecNeuralRowWriter` (no live search engine required).
struct ProgressNeuralRowWriter<'a, W: streaming::neural::NeuralRowWriter> {
    inner: &'a mut W,
    rows_reported: &'a std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl<W: streaming::neural::NeuralRowWriter> streaming::neural::NeuralRowWriter
    for ProgressNeuralRowWriter<'_, W>
{
    fn write_row(&mut self, node_id: &str, embedding: &[f32]) -> Result<()> {
        self.inner.write_row(node_id, embedding)
    }

    fn write_batch(&mut self, rows: Vec<(String, Vec<f32>)>) -> Result<usize> {
        // The writer receives only successful rows; skipped texts are
        // accounted from the stats the caller prints at completion.
        self.rows_reported
            .fetch_add(rows.len(), std::sync::atomic::Ordering::Relaxed);
        self.inner.write_batch(rows)
    }
}

fn streaming_neural_inputs<'a>(
    pdg: &'a crate::graph::pdg::ProgramDependenceGraph,
    admitted_node_ids: &'a HashSet<String>,
    project_path: &'a std::path::Path,
) -> impl Iterator<Item = Result<streaming::neural::NeuralInput>> + 'a {
    let connectivity_config = crate::graph::pdg::TraversalConfig {
        max_depth: Some(1),
        max_nodes: Some(1000),
        allowed_edge_types: Some(&[
            crate::graph::pdg::EdgeType::Call,
            crate::graph::pdg::EdgeType::DataDependency,
        ]),
        excluded_node_types: Some(vec![crate::graph::pdg::NodeType::External]),
        min_complexity: None,
        min_edge_confidence: 0.0,
    };
    let file_summary_ctx = index_builder::FileSummaryContext::from_pdg(pdg);
    let mut node_indices = pdg.node_indices();
    let mut file_cache = index_builder::FileReadCache::per_chunk_scratch();

    std::iter::from_fn(move || {
        loop {
            let node_idx = node_indices.next()?;
            let Some(node) = pdg.get_node(node_idx) else {
                continue;
            };
            if node.node_type == crate::graph::pdg::NodeType::External
                || !admitted_node_ids.contains(&node.id)
            {
                continue;
            }
            let input = {
                let file_bytes = file_cache
                    .get_or_read(Path::new(&*node.file_path))
                    .unwrap_or_else(|error| {
                        warn!(
                            node_id = %node.id,
                            path = %node.file_path,
                            %error,
                            "Failed to read source for streaming neural node; using empty bytes"
                        );
                        std::sync::Arc::new(Vec::new())
                    });
                let text = index_builder::enriched_node_content(
                    pdg,
                    node_idx,
                    node,
                    &file_bytes,
                    &connectivity_config,
                    &file_summary_ctx,
                );
                Ok(streaming::neural::NeuralInput {
                    node_id: node.id.clone(),
                    text: index_builder::cap_neural_text(&text).to_owned(),
                    // Legacy-route parity: the project-relative source path
                    // rides along as the document title so the remote doc
                    // template ({title}) receives the same input shape on both
                    // routes (EmbeddingGemma2 trains on title+text prompts).
                    //
                    // It MUST be project-relative, not `node.file_path`: some
                    // nodes carry an absolute path (submodule summaries store
                    // `summary.path` verbatim), and sending that as the title
                    // leaks the local checkout path to a remote provider and
                    // makes title-aware embeddings differ between two identical
                    // checkouts (CodeRabbit Security/Privacy). Only the title is
                    // relativized — `node.file_path` above is still the
                    // absolute path used to READ the source.
                    //
                    // The fallback is the FILE NAME, never the raw path. Falling
                    // back to `node.file_path` reintroduced exactly the leak this
                    // documents, for precisely the nodes it describes: a path that
                    // fails `strip_prefix` (non-canonical through a symlink, or
                    // outside `project_path` like a submodule summary) was sent in
                    // full. The file name still carries template parity without
                    // identifying the checkout.
                    title: Some(
                        match std::path::Path::new(&*node.file_path).strip_prefix(project_path) {
                            Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
                            Err(_) => std::path::Path::new(&*node.file_path)
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                        },
                    ),
                })
            };
            return Some(input);
        }
    })
}

fn enrich_neural_for_route(
    engine: &mut crate::search::search::SearchEngine,
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    embedder: &index_builder::GenerationNeuralEmbedder,
    admitted_node_ids: &HashSet<String>,
    project_path: &std::path::Path,
) -> Result<usize> {
    let (cache_hits_before, _) = neural_cache_counters();
    // The heartbeat covers the whole pass (see NeuralProgress): liveness even
    // while the first embed batch is in flight, plus the CPU-fallback watch.
    let progress = NeuralProgress::start(admitted_node_ids.len());
    let streaming_embedder = HybridStreamingEmbedder { inner: embedder };
    let mut writer = SearchNeuralRowWriter {
        engine,
        seen_node_ids: HashSet::new(),
    };
    let mut inputs = streaming_neural_inputs(pdg, admitted_node_ids, project_path);
    let budget = streaming::BatchBudget {
        max_texts: 256,
        max_seq_len: 64 * 1024,
        ..streaming::BatchBudget::default()
    };
    let stats = streaming::neural::enrich_neural_streaming(
        &mut inputs,
        &streaming_embedder,
        &mut ProgressNeuralRowWriter {
            inner: &mut writer,
            rows_reported: &progress.rows,
        },
        &budget,
    )?;
    drop(progress);
    let (cache_hits_after, cache_misses) = neural_cache_counters();
    // Batches that COMPLETE but write no rows are the signature of a dead
    // embedder (remote endpoint unreachable, provider 4xx/5xx, or a worker
    // answering with empty vectors). Without this, such a run shows a live
    // "N/N" counter marching to completion while every batch contributes
    // zero rows, and the neural layer silently ends up empty.
    if stats.batches > 0 && stats.rows_written == 0 {
        warn!(
            batches = stats.batches,
            "neural enrichment completed with zero rows: the embedder returned no vectors for \
             every batch. Search falls back to TF-IDF only. Check `leindex setup --check` for \
             provider/model state and ~/.leindex/logs/leindex-embed-daemon.log for worker errors."
        );
    }
    progress_stderr(&format!(
        "Indexing: neural done — {} rows ({} from embed cache, {} embedded, {} skipped, {} batches)...",
        stats.rows_written,
        cache_hits_after - cache_hits_before,
        cache_misses,
        stats.rows_skipped,
        stats.batches,
    ));
    Ok(stats.rows_written)
}

fn reusable_fragment_rows(
    rows: HashMap<String, Vec<f32>>,
    persisted: Option<&index_builder::fragment::sync::FragmentFileManifest>,
    selected_identity: &str,
) -> HashMap<String, Vec<f32>> {
    if persisted
        .is_some_and(|manifest| manifest.extraction_identity.model_name == selected_identity)
    {
        rows
    } else {
        if !rows.is_empty() {
            warn!(
                "Fragment provider identity changed or is unknown; skipping old-space rows and forcing re-embedding"
            );
        }
        HashMap::new()
    }
}

/// The Matryoshka policy tag in force right now, as recorded in neural
/// checkpoints and compared at resume.
///
/// `effective_matryoshka()` already folds in the rollout flag, so the tag is
/// `off` whenever the capability is not actually active — including when a
/// non-Off policy is configured but `LEINDEX_FEATURE_MATRYOSHKA` is unset.
fn checkpoint_matryoshka_tag() -> &'static str {
    crate::config::LeIndexConfig::load_cached()
        .indexing
        .embeddings
        .effective_matryoshka()
        .tag()
}

impl LeIndex {
    #[cfg(feature = "onnx")]
    pub(super) fn configured_neural_embedder(
        &self,
    ) -> Result<Option<index_builder::HybridEmbedder>> {
        let tfidf = self
            .embedder
            .as_ref()
            .context("core embedder is set before neural enrichment")?
            .tfidf()
            .clone();
        // B1: enrichment construction mirrors the lexical-route ladder —
        // remote endpoint first (helper logs its own degradation), then local
        // ONNX, never hard-failing to TF-IDF-only.
        #[cfg(feature = "remote-embeddings")]
        let mut embedder = index_builder::try_remote_neural_embedder(
            tfidf.clone(),
            &crate::config::LeIndexConfig::load_cached()
                .indexing
                .embeddings,
        )
        .or_else(|| {
            index_builder::HybridEmbedder::hybrid_local(
                tfidf.clone(),
                Some(crate::config::LeIndexConfig::load_cached().neural_weight_f32()),
            )
            .ok()
        });
        #[cfg(not(feature = "remote-embeddings"))]
        let mut embedder = index_builder::HybridEmbedder::hybrid_local(
            tfidf,
            Some(crate::config::LeIndexConfig::load_cached().neural_weight_f32()),
        )
        .ok();
        if let Some(reason) = embedder
            .as_ref()
            .and_then(index_builder::HybridEmbedder::cpu_fallback_reason)
        {
            tracing::warn!("{}", reason);
            embedder = None;
        }
        if crate::config::LeIndexConfig::load_cached()
            .search
            .search_mode
            == "text"
        {
            embedder = None;
        }
        // Feature-flag rollout gate: a deployment can kill neural indexing via
        // LEINDEX_FEATURE_NEURAL_SEARCH=false even when config enables it.
        // `is_neural_enabled` ANDs the runtime flag (default-on for this GA
        // feature) with the config knob, so a disabled config stays disabled.
        if !crate::feature_flags::is_neural_enabled(
            crate::config::LeIndexConfig::load_cached().neural.enabled,
        ) {
            embedder = None;
        }
        Ok(embedder)
    }

    #[cfg(not(feature = "onnx"))]
    pub(super) fn configured_neural_embedder(
        &self,
    ) -> Result<Option<index_builder::HybridEmbedder>> {
        Ok(None)
    }

    pub(super) fn restore_neural_checkpoint(
        &mut self,
        checkpoint: Option<&NeuralCheckpoint>,
        lexical_hash: &str,
        current_model: &str,
    ) -> (usize, bool) {
        // A checkpoint taken under a different Matryoshka policy describes a
        // different on-disk row shape (sliced vs native), so it cannot be
        // resumed: the row count would match while the vectors mean something
        // else. An untagged checkpoint predates this field and is accepted.
        let policy_tag = checkpoint_matryoshka_tag();
        let requested = checkpoint.is_some_and(|checkpoint| {
            checkpoint.lexical_hash == lexical_hash
                && checkpoint.model == current_model
                && checkpoint
                    .matryoshka
                    .as_deref()
                    .is_none_or(|tag| tag == policy_tag)
                && (checkpoint.rows == 0 || checkpoint.mmap_path.is_file())
        });
        let rows = 0;
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        let mut rows = rows;
        // loaded stays false until the mmap restore actually yields rows; a
        // checkpoint with rows==0 means nothing was loaded, not a successful restore.
        let loaded = false;
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        let mut loaded = loaded;
        if requested {
            #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
            {
                // Restore EVERY persisted tier, not just the canonical file. A
                // Dynamic or fixed sub-native generation writes some (or all) of
                // its rows to sidecars, so accepting the checkpoint on the
                // canonical row count alone resumed with the resident engine
                // missing every tier-homed node — and those nodes were then
                // skipped for re-embedding (Codex P2). Accept the checkpoint only
                // when the restored total matches what was checkpointed.
                //
                // Gated on the effective Matryoshka setting, like the hydration
                // path: a generation checkpointed while MRL was armed must not
                // keep serving sliced rows after the kill switch is set or the
                // policy changes, so the rollback actually rolls back (Codex P1).
                let artifact_path = self.project_path.join(".leindex");
                // Borrow the canonical index rather than consuming it: its
                // identity trailer is the generation marker for the tier gate
                // below, and moving it would drop that information before the
                // sidecars are considered.
                let canonical = index_builder::try_load_neural_mmap_embeddings(&self.project_path);
                let canonical_identity = canonical.as_ref().and_then(|mmap| mmap.identity());
                if let Some(neural_mmap) = canonical.as_ref() {
                    rows = self.search_engine.restore_neural_embeddings(neural_mmap);
                }
                if crate::config::LeIndexConfig::load_cached()
                    .indexing
                    .embeddings
                    .effective_matryoshka()
                    != crate::config::MatryoshkaSetting::Off
                {
                    for mmap in index_builder::load_neural_tier_mmaps_from_storage(
                        &artifact_path,
                        canonical_identity,
                    ) {
                        rows += self.search_engine.restore_neural_embeddings(&mmap);
                    }
                }
                let checkpointed = checkpoint.map_or(0, |checkpoint| checkpoint.rows);
                if checkpointed > 0 && rows != checkpointed {
                    warn!(
                        restored = rows,
                        checkpointed,
                        "Neural checkpoint row count mismatch; discarding partial restore and \
                         re-embedding"
                    );
                    // Discard resident rows too: re-embedding may be unavailable
                    // or fail, and a later persist must not publish a partial restore.
                    self.search_engine.clear_neural_embeddings();
                    rows = 0;
                } else {
                    loaded = rows > 0;
                }
            }
        }
        (rows, loaded)
    }

    pub(super) fn persist_neural_snapshot(
        &mut self,
        _state: &IndexPipelineState,
        rows: usize,
        embedder: Option<index_builder::HybridEmbedder>,
        document_embedding_stopped: bool,
    ) -> Result<()> {
        if rows == 0 {
            return Ok(());
        }
        if embedder.is_some() {
            self.embedder = embedder;
        }
        // Fragment layer (Task 7): incremental sync before the snapshot
        // persist. On failure the layer is CLEARED (not just logged) — see
        // `sync_fragment_layer_or_clear` (Codex wave-4 P2).
        if document_embedding_stopped {
            warn!(
                "Node generation stopped remote documents; skipping fragment embedding for this generation"
            );
            self.search_engine.set_fragment_embeddings(Vec::new());
        } else {
            self.sync_fragment_layer_or_clear();
        }
        // Neural rows attach to an already-persisted graph; derive the
        // identity from the resident graph as the layer reconstructs it
        // (collapse) so hydration's fingerprint check agrees.
        let (identity_nodes, identity_edges, identity_fingerprint) =
            index_builder::persisted_search_identity_from_graph(
                self.pdg.as_ref().expect("PDG resident after lexical"),
            );
        index_builder::persist_search_snapshot(
            &self.search_engine,
            &self.project_path,
            identity_nodes,
            identity_edges,
            identity_fingerprint,
        )
    }

    /// Persist the neural mmap only when embeddings were freshly produced
    /// (not resumed) AND there are rows to write — never persist an empty mmap.
    /// Extracted from run_neural to keep that function's branch count bounded.
    pub(super) fn persist_neural_mmap(
        &self,
        _neural_resume_loaded: bool,
        _neural_rows: usize,
        _identity: Option<&str>,
    ) -> Result<()> {
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        if !_neural_resume_loaded && _neural_rows > 0 {
            index_builder::persist_neural_embeddings_to_mmap(
                &self.search_engine,
                &self.project_path,
                _identity,
                crate::config::LeIndexConfig::load_cached()
                    .indexing
                    .embeddings
                    .effective_matryoshka(),
            )?;
        }
        Ok(())
    }

    /// Finalize neural persistence and return the model/provider labels used by
    /// the checkpoint. Extracted from run_neural to keep its branch count
    /// under the CCN-15 gate.
    fn persist_neural_outputs(
        &mut self,
        state: &IndexPipelineState,
        neural_rows: usize,
        neural_resume_loaded: bool,
        neural_embedder: Option<index_builder::GenerationNeuralEmbedder>,
    ) -> Result<(String, &'static str)> {
        let selected_model = neural_embedder
            .as_ref()
            .map(index_builder::GenerationNeuralEmbedder::identity)
            .unwrap_or_else(|| "unavailable".to_string());
        let document_embedding_stopped = neural_embedder
            .as_ref()
            .is_some_and(index_builder::GenerationNeuralEmbedder::document_embedding_stopped);
        let neural_embedder =
            neural_embedder.map(index_builder::GenerationNeuralEmbedder::into_embedder);
        let selected_provider = match neural_embedder.as_ref() {
            #[cfg(feature = "remote-embeddings")]
            Some(index_builder::HybridEmbedder::HybridRemote { .. }) => "remote",
            #[cfg(feature = "onnx")]
            Some(index_builder::HybridEmbedder::HybridLocal { .. }) => "onnx",
            _ => "unavailable",
        };
        self.persist_neural_mmap(neural_resume_loaded, neural_rows, Some(&selected_model))?;
        self.persist_neural_snapshot(
            state,
            neural_rows,
            neural_embedder,
            document_embedding_stopped,
        )?;
        Ok((selected_model, selected_provider))
    }

    /// against the persisted manifest, re-chunk ONLY changed files via the
    /// PDG (Tier-2 sub-symbol + Tier-3 orphans), embed ONLY content hashes
    /// missing from the store (batch-256 IPC), then update the store + root
    /// under a bumped generation and populate the engine's fragment vector
    /// index so `persist_search_snapshot` writes real fragment twins.
    ///
    /// Feature-off compatible: a no-op when `[search] fragment_index_enabled`
    /// is false, no PDG is resident, or no neural embedder is configured. A
    /// mid-build crash is handled by the generation guard in
    /// `fragment_layer_is_valid` — hydration serves the last complete root
    /// (i.e. keeps the fragment layer off) rather than a half-synced tree.
    /// Run the fragment-layer incremental sync, clearing the in-memory
    /// fragment rows on failure (Codex wave-4 P2).
    ///
    /// Every snapshot persist is preceded by a fragment sync; on sync failure
    /// the engine would otherwise keep the PREVIOUS generation's fragment
    /// index + owner refs, so a fresh node snapshot could rank/surface a
    /// changed symbol against deleted (pre-change) fragment content. Clearing
    /// makes the pre-existing "fragment layer disabled for this generation"
    /// warning honest and keeps the persisted snapshot fragment-free.
    /// `set_fragment_embeddings` with empty input drops the index, the owner
    /// refs, and the result cache in one call.
    pub(super) fn sync_fragment_layer_or_clear(&mut self) {
        if let Err(e) = self.sync_fragment_layer() {
            warn!(
                "Fragment layer sync failed (fragment layer disabled for this generation): {e:#}"
            );
            self.search_engine.set_fragment_embeddings(Vec::new());
        }
    }

    pub(super) fn sync_fragment_layer(&mut self) -> Result<()> {
        let cfg = crate::config::LeIndexConfig::load_cached();
        if !cfg.search.fragment_index_enabled
            || self.pdg.is_none()
            || !crate::feature_flags::is_neural_enabled(cfg.neural.enabled)
            || cfg.search.search_mode == "text"
        {
            return Ok(());
        }
        // The fragment layer only produces rows when a neural embedder is
        // configured; without one (e.g. text-only builds) it is a no-op.
        let embedder = match self
            .embedder
            .as_ref()
            .filter(|embedder| embedder.has_neural())
        {
            Some(embedder) => Some(embedder.clone()),
            None => self.configured_neural_embedder()?,
        }
        .map(index_builder::GenerationNeuralEmbedder::pinned);
        let Some(embedder) = embedder else {
            return Ok(());
        };
        let selected_identity = embedder.identity();
        let files = self.collect_source_files_with_hashes(false)?;
        if files.is_empty() {
            return Ok(());
        }

        let mut store =
            index_builder::fragment::FragmentStore::load_from_storage(&self.project_path)?
                .unwrap_or_default();
        let max_bytes = cfg.search.fragment_max_bytes as usize;
        let orphan_enabled = cfg.search.fragment_orphan_enabled;
        let naive_fallback = cfg.search.fragment_naive_fallback;
        // Codex P1: persist the model + fragment-knob identity so a model or
        // knob change while sources are byte-identical forces a fragment
        // re-sync (mirrors the node-level `NeuralCheckpoint.model` discipline;
        // without it the source-hash skip would silently serve stale rows).
        let extraction_identity = index_builder::fragment::sync::FragmentExtractionIdentity::new(
            &selected_identity,
            max_bytes,
            orphan_enabled,
            naive_fallback,
        );

        // P2-4 (Codex review): detect a missing/corrupt fragment embeddings mmap
        // BEFORE the sync so unchanged files are NOT skipped. With the mmap gone
        // but the store+manifest intact, a normal run would embed nothing, install
        // an empty fragment index, and the snapshot path would remove the mmap
        // again — permanently disabling fragment retrieval. Recover by forcing a
        // full re-embed of every content hash.
        let pre_sync_mmap_rows: std::collections::HashMap<String, Vec<f32>> =
            index_builder::try_load_fragment_mmap_embeddings_from_storage(
                &self.project_path.join(".leindex"),
            )
            .map(|mmap| mmap.entries().unwrap_or_default().into_iter().collect())
            .unwrap_or_default();
        // An unrecorded or different provider cannot prove old rows belong
        // to this space. Do not merge them after partial re-embedding.
        let persisted_manifest = index_builder::fragment::sync::load_fragment_sync_manifest(
            &self.project_path.join(".leindex"),
        )?;
        let old_rows = reusable_fragment_rows(
            pre_sync_mmap_rows,
            persisted_manifest.as_ref(),
            &selected_identity,
        );
        let force_reembed = !store.is_empty() && old_rows.is_empty();

        // Scoped so the chunk closure (which borrows `self.pdg`) is dropped
        // before we mutate `self.search_engine` below.
        let (summary, new_embeddings) = {
            let pdg = self.pdg.as_ref().expect("checked above");
            let mut chunk_fn = |path: &std::path::Path, bytes: &[u8]| {
                index_builder::fragment::extract::extract_file_fragments(
                    pdg,
                    path,
                    bytes,
                    max_bytes,
                    orphan_enabled,
                    naive_fallback,
                )
            };
            let mut embed_fn = |texts: &[String]| -> Vec<Option<Vec<f32>>> {
                #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
                {
                    embedder.embed_neural_batch_blocking(texts)
                }
                #[cfg(not(any(feature = "onnx", feature = "remote-embeddings")))]
                {
                    let _ = (&embedder, texts);
                    vec![None; texts.len()]
                }
            };
            index_builder::fragment::sync::incremental_sync_fragments(
                &self.project_path,
                &mut store,
                &files,
                &mut chunk_fn,
                &mut embed_fn,
                force_reembed,
                &extraction_identity,
            )?
        };

        info!(
            files_scanned = summary.files_scanned,
            files_changed = summary.files_changed,
            fragments_total = summary.fragments_total,
            embedded = summary.embedded,
            reused = summary.reused,
            generation = summary.generation,
            "Fragment incremental sync complete"
        );
        let fresh_rows: std::collections::HashMap<String, Vec<f32>> =
            new_embeddings.into_iter().collect();
        self.populate_fragment_engine_from_sync(&store, fresh_rows, &old_rows);
        Ok(())
    }

    /// Merge freshly embedded rows with reused rows and install them plus
    /// owner refs on the search engine. Extracted from sync_fragment_layer
    /// to keep its branch count under the CCN-15 gate.
    fn populate_fragment_engine_from_sync(
        &mut self,
        store: &index_builder::fragment::FragmentStore,
        fresh_rows: std::collections::HashMap<String, Vec<f32>>,
        old_rows: &std::collections::HashMap<String, Vec<f32>>,
    ) {
        let mut rows: Vec<(String, Vec<f32>)> = Vec::with_capacity(store.len());
        for hash in store.content_hashes() {
            if let Some(embedding) = fresh_rows.get(hash).or_else(|| old_rows.get(hash)) {
                rows.push((hash.to_string(), embedding.clone()));
            }
        }
        self.search_engine.set_fragment_embeddings(rows);

        let refs: std::collections::HashMap<String, Vec<(String, (usize, usize))>> = store
            .content_hashes()
            .filter_map(|hash| {
                let owners: Vec<(String, (usize, usize))> = store
                    .get(hash)
                    .into_iter()
                    .flatten()
                    .filter_map(|meta| {
                        meta.owner
                            .as_ref()
                            .map(|owner| (owner.clone(), meta.byte_range))
                    })
                    .collect();
                (!owners.is_empty()).then(|| (hash.to_string(), owners))
            })
            .collect();
        self.search_engine.set_fragment_refs(refs);
    }

    pub(crate) fn run_neural(
        &mut self,
        _job: &JobPaths,
        lexical: &LexicalCheckpoint,
    ) -> Result<NeuralCheckpoint> {
        let mut state = self
            .pipeline
            .take()
            .context("neural phase started without pipeline state")?;
        let lexical_hash = state
            .lexical_hash
            .clone()
            .unwrap_or_else(|| lexical.pdg_hash.clone());
        let neural_embedder = self
            .configured_neural_embedder()?
            .map(index_builder::GenerationNeuralEmbedder::new);
        warn_if_missing_neural_embedder(neural_embedder.as_ref());
        if let (Some(generation), Some(checkpoint)) =
            (neural_embedder.as_ref(), state.resumed_neural.as_ref())
        {
            generation.adopt_checkpoint_dimension(&checkpoint.model);
        }
        // Remote identities include endpoint/model/templates/dimension; a
        // checkpoint from a pre-success Qwen fallback must not resume as remote.
        let current_embed_model = neural_embedder
            .as_ref()
            .map(index_builder::GenerationNeuralEmbedder::identity)
            .unwrap_or_else(|| "unavailable".to_string());
        let (mut neural_rows, neural_resume_loaded) = self.restore_neural_checkpoint(
            state.resumed_neural.as_ref(),
            &lexical_hash,
            &current_embed_model,
        );
        if let Some(neural_embedder) = neural_embedder
            .as_ref()
            .filter(|_| neural_rows == 0 && !neural_resume_loaded)
        {
            let pdg = self
                .pdg
                .as_ref()
                .context("PDG is resident before neural enrichment")?;
            progress_stderr(&format!(
                "Indexing: neural embedding {} admitted nodes...",
                state.admitted_node_ids.len()
            ));
            neural_rows = enrich_neural_for_route(
                &mut self.search_engine,
                pdg,
                neural_embedder,
                &state.admitted_node_ids,
                &self.project_path,
            )?;
        }
        let (selected_model, selected_provider) = self.persist_neural_outputs(
            &state,
            neural_rows,
            neural_resume_loaded,
            neural_embedder,
        )?;
        progress_stderr("Indexing: publishing enhanced generation...");
        let neural_checkpoint = self.neural_checkpoint_for(
            lexical_hash,
            neural_rows,
            selected_provider,
            selected_model,
        );
        let checkpoint_store = state
            .checkpoint_store
            .as_ref()
            .context("neural phase missing checkpoint store")?;
        let neural_hash = checkpoint_store.write_neural(&neural_checkpoint)?;
        self.checkpoint_state(checkpoint_store, "neural", neural_hash);
        injected_phase_failure("neural")?;
        state.neural_rows = neural_rows;
        state.neural_resume_loaded = neural_resume_loaded;
        state.neural_checkpoint = Some(neural_checkpoint.clone());
        self.pipeline = Some(state);
        Ok(neural_checkpoint)
    }

    /// Assemble the neural checkpoint from the phase outcome. Kept out of
    /// `run_neural` to bound its branch count (cyclomatic gate).
    fn neural_checkpoint_for(
        &self,
        lexical_hash: String,
        rows: usize,
        selected_provider: &str,
        model: String,
    ) -> NeuralCheckpoint {
        NeuralCheckpoint {
            lexical_hash,
            mmap_path: self.project_path.join(".leindex/neural_embeddings.bin"),
            rows,
            provider: if rows == 0 {
                "unavailable".to_string()
            } else {
                selected_provider.to_string()
            },
            model,
            matryoshka: Some(checkpoint_matryoshka_tag().to_string()),
        }
    }

    pub(crate) fn publish_generation(
        &mut self,
        _job: &JobPaths,
        neural: Option<&NeuralCheckpoint>,
    ) -> Result<PublishedGeneration> {
        let mut state = self
            .pipeline
            .take()
            .context("publication started without pipeline state")?;

        // Compute core health if it hasn't been set yet (consolidated single
        // publish path — the intermediate lexical-only publish was removed).
        if state.core_health.is_none() {
            self.update_last_indexed_timestamp()?;
            self.save_stats_to_storage()?;
            let generation = self.checkpoint_generation();
            let git_status = crate::cli::git::status(&self.project_path).ok();
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0);
            let indexed_paths: std::collections::HashSet<PathBuf> = state
                .source_files_with_hashes
                .iter()
                .map(|(path, _)| path.clone())
                .collect();
            let dirty_source_paths =
                git_status
                    .as_ref()
                    .map_or_else(std::collections::HashSet::new, |status| {
                        status
                            .modified
                            .iter()
                            .chain(status.staged.iter())
                            .chain(status.untracked.iter())
                            .chain(status.deleted.iter())
                            .map(|path| {
                                if path.is_absolute() {
                                    path.clone()
                                } else {
                                    self.project_path.join(path)
                                }
                            })
                            .filter(|path| {
                                path.extension()
                                    .and_then(|extension| extension.to_str())
                                    .is_some_and(|extension| {
                                        super::super::SOURCE_FILE_EXTENSIONS
                                            .iter()
                                            .any(|known| known.eq_ignore_ascii_case(extension))
                                    })
                            })
                            .collect::<std::collections::HashSet<_>>()
                    });
            let changed_unindexed_count = dirty_source_paths
                .iter()
                .filter(|path| !indexed_paths.contains(*path))
                .count();
            let mut health = super::super::IndexHealth {
                generation,
                phase: super::super::IndexPhase::Complete,
                status: super::super::ComponentStatus::Fresh,
                head_oid: git_status
                    .as_ref()
                    .and_then(|status| status.head_oid.clone()),
                tree_oid: git_tree_oid(&self.project_path),
                indexed_file_count: state.source_files_with_hashes.len(),
                dirty_file_count: dirty_source_paths.len(),
                changed_unindexed_count,
                indexed_at_unix_ms: Some(now_ms),
                last_failure_phase: None,
                last_failure: None,
            };
            let core_published =
                self.publish_generation_snapshot(generation, &mut health, false)?;
            crate::cli::index_freshness::save_health(self.storage_path(), &health)?;
            state.core_health = Some(health);

            // If no neural checkpoint, return the core generation immediately.
            if neural.is_none() {
                injected_phase_failure("lexical")?;
                self.pipeline = Some(state);
                return Ok(core_published);
            }
        }

        let core_health = state
            .core_health
            .clone()
            .context("neural publication missing core health")?;
        let published = if neural.is_some_and(|checkpoint| checkpoint.rows > 0) {
            let mut health = super::super::IndexHealth {
                generation: core_health.generation.saturating_add(1),
                ..core_health.clone()
            };
            let published =
                self.publish_generation_snapshot(health.generation, &mut health, true)?;
            crate::cli::index_freshness::save_health(self.storage_path(), &health)?;
            published
        } else {
            crate::cli::index_freshness::save_health(self.storage_path(), &core_health)?;
            PublishedGeneration {
                generation: core_health.generation,
                storage_path: self
                    .storage_path()
                    .join("generations")
                    .join(core_health.generation.to_string()),
                health: core_health,
            }
        };
        self.pipeline = Some(state);
        Ok(published)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::leindex::indexing::streaming::neural::NeuralRowWriter;

    #[test]
    fn test_stuck_hint_only_after_quiet_timeout_window() {
        use std::time::Duration;
        // Fresh phase: nothing to report.
        assert!(NeuralProgress::stuck_hint(0, Duration::from_secs(0)).is_empty());
        // Still inside the pre-fallback window (30s timeout x 3 retries).
        assert!(NeuralProgress::stuck_hint(0, Duration::from_secs(44)).is_empty());
        // Past it with zero rows: the provider is slow or unreachable.
        assert!(!NeuralProgress::stuck_hint(0, Duration::from_secs(45)).is_empty());
        // Rows landing means the provider is fine; never cry wolf.
        assert!(NeuralProgress::stuck_hint(1, Duration::from_secs(600)).is_empty());
    }

    /// A checkpoint whose recorded row count disagrees with what the mmap
    /// actually restored must report ZERO rows. The mismatch is the only
    /// signal that the persisted store is partial, and the caller's
    /// re-embed gate is `neural_rows == 0 && !neural_resume_loaded` — handing
    /// it the partial count closes that gate and publishes the partial store
    /// over the good artifacts, committing the exact corruption the row-count
    /// check exists to reject.
    #[test]
    #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
    fn test_checkpoint_row_mismatch_reports_no_rows() {
        use crate::search::search::{DEFAULT_EMBEDDING_DIMENSION, NodeInfo};

        let dir = tempfile::tempdir().expect("checkpoint fixture");
        let storage = dir.path().join(".leindex");
        std::fs::create_dir_all(&storage).expect("storage dir");
        let mmap_path = storage.join("neural_embeddings.bin");

        let node = |node_id: &str| {
            let mut tfidf_embedding = vec![0.0; DEFAULT_EMBEDDING_DIMENSION];
            tfidf_embedding[0] = 1.0;
            NodeInfo {
                node_id: node_id.to_string(),
                file_path: "fixture.rs".to_string(),
                symbol_name: "new".to_string(),
                language: "rust".to_string(),
                content: "fn new() {}".to_string(),
                byte_range: (0, 11),
                tfidf_embedding,
                neural_embedding: None,
                complexity: 1,
                signature: None,
                pre_tokenized: None,
            }
        };

        // 2 rows on disk, 2 matching nodes in the resident engine, but the
        // checkpoint claims 5 — the partial-store case.
        let rows = vec![
            ("fixture.rs:new".to_string(), vec![1.0, 0.0]),
            ("fixture.rs:other".to_string(), vec![0.0, 1.0]),
        ];
        crate::search::vector::write_mmap_embeddings(&mmap_path, &rows).expect("persist fixture");

        let mut index = LeIndex::new(dir.path()).expect("checkpoint index");
        index
            .search_engine
            .index_nodes(vec![node("fixture.rs:new"), node("fixture.rs:other")]);

        let checkpoint = NeuralCheckpoint {
            lexical_hash: "lexical".to_string(),
            mmap_path: mmap_path.clone(),
            rows: 5,
            provider: "remote".to_string(),
            model: "model".to_string(),
            matryoshka: None,
        };

        assert_eq!(
            index.restore_neural_checkpoint(Some(&checkpoint), "lexical", "model"),
            (0, false),
            "a partial restore must report zero rows so the caller re-embeds"
        );
    }

    #[test]
    fn test_neural_progress_writer_counts_rows_and_passes_through() {
        let rows = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut inner = streaming::neural::VecNeuralRowWriter::new();
        {
            let mut writer = ProgressNeuralRowWriter {
                inner: &mut inner,
                rows_reported: &rows,
            };
            let batch =
                |w: &mut ProgressNeuralRowWriter<'_, streaming::neural::VecNeuralRowWriter>,
                 base: usize,
                 n: usize| {
                    let batch: Vec<_> = (base..base + n)
                        .map(|i| (format!("node-{i}"), vec![i as f32, (i + 1) as f32]))
                        .collect();
                    w.write_batch(batch).expect("write_batch");
                };
            batch(&mut writer, 0, 2);
            assert_eq!(rows.load(std::sync::atomic::Ordering::Relaxed), 2);
            batch(&mut writer, 2, 3);
            assert_eq!(rows.load(std::sync::atomic::Ordering::Relaxed), 5);
        }
        assert_eq!(inner.rows.len(), 5, "all rows must reach the inner writer");
        assert_eq!(
            inner.rows[0],
            ("node-0".to_owned(), vec![0.0, 1.0]),
            "rows must pass through unmodified"
        );
        // Single-row path forwards too.
        let mut writer = ProgressNeuralRowWriter {
            inner: &mut inner,
            rows_reported: &rows,
        };
        writer
            .write_row("solo", &[9.0])
            .expect("write_row forwards");
        assert!(inner.rows.iter().any(|(id, _)| id == "solo"));
    }

    #[test]
    fn test_neural_progress_heartbeat_starts_and_stops() {
        // The heartbeat thread must start, print at least once, and exit on
        // drop — proving the phase can report liveness even while the first
        // embed batch is still in flight.
        let progress = NeuralProgress::start(10);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let observed = progress
            .rows
            .fetch_add(0, std::sync::atomic::Ordering::Relaxed);
        drop(progress);
        assert_eq!(observed, 0, "no rows reported by the heartbeat itself");
    }

    #[test]
    fn test_all_batches_empty_is_distinguishable_from_no_work() {
        // enrich_neural_for_route warns only when batches ran and produced no
        // rows. Pin the stats contract that condition reads: an embedder that
        // answers with only empty vectors must land in rows_skipped with
        // batches > 0, never be mistaken for "no nodes to embed".
        struct DeadEmbedder;
        impl streaming::neural::StreamingEmbedder for DeadEmbedder {
            fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
                vec![Vec::new(); texts.len()]
            }
        }
        let mut inputs = (0..4).map(|i| {
            Ok(streaming::neural::NeuralInput {
                node_id: format!("node-{i}"),
                text: format!("document-{i}"),
                title: None,
            })
        });
        let mut writer = streaming::neural::VecNeuralRowWriter::new();
        let budget = streaming::BatchBudget {
            max_texts: 2,
            ..Default::default()
        };
        let dead = streaming::neural::enrich_neural_streaming(
            &mut inputs,
            &DeadEmbedder,
            &mut writer,
            &budget,
        )
        .expect("empty vectors are a skip, not an error");
        assert!(dead.batches > 0, "batches ran");
        assert_eq!(dead.rows_written, 0, "no rows landed");
        assert_eq!(dead.rows_skipped, 4, "every input accounted for");
        assert!(writer.rows.is_empty());

        // Zero inputs is the other side of the same condition: no batches, so
        // the dead-embedder warning must not fire.
        let mut none = std::iter::empty();
        let idle = streaming::neural::enrich_neural_streaming(
            &mut none,
            &DeadEmbedder,
            &mut streaming::neural::VecNeuralRowWriter::new(),
            &budget,
        )
        .expect("empty input");
        assert_eq!(idle.batches, 0);
        assert_eq!(idle.rows_written, 0);
    }

    #[test]
    fn test_search_neural_row_writer_keeps_first_row_per_node_id() {
        use crate::search::search::{DEFAULT_EMBEDDING_DIMENSION, NodeInfo, SearchEngine};

        // append_nodes keeps the FIRST occurrence of a duplicate node_id while
        // update_neural_embeddings applies rows last-wins. A late duplicate row
        // must therefore not overwrite the retained node's own body.
        let node = |node_id: &str, file_path: &str| {
            let mut tfidf_embedding = vec![0.0; DEFAULT_EMBEDDING_DIMENSION];
            tfidf_embedding[0] = 1.0;
            NodeInfo {
                node_id: node_id.to_string(),
                file_path: file_path.to_string(),
                symbol_name: "new".to_string(),
                language: "rust".to_string(),
                content: "fn new() {}".to_string(),
                byte_range: (0, 11),
                tfidf_embedding,
                neural_embedding: None,
                complexity: 1,
                signature: None,
                pre_tokenized: None,
            }
        };
        let mut engine = SearchEngine::new();
        engine.index_nodes(vec![node("a.rs:new", "a.rs"), node("b.rs:new", "b.rs")]);

        let mut writer = SearchNeuralRowWriter {
            engine: &mut engine,
            seen_node_ids: HashSet::new(),
        };
        writer
            .write_batch(vec![
                ("a.rs:new".to_string(), vec![1.0, 0.0]),
                ("a.rs:new".to_string(), vec![0.0, 1.0]),
            ])
            .expect("first batch");
        writer
            .write_batch(vec![("a.rs:new".to_string(), vec![0.25, 0.75])])
            .expect("second batch");
        drop(writer);

        assert_eq!(
            engine.collect_neural_embeddings(),
            vec![("a.rs:new".to_string(), vec![1.0, 0.0])],
            "first occurrence per node_id kept, later duplicates dropped"
        );
    }

    #[test]
    fn test_fragment_provider_identity_prevents_old_space_merge() {
        let mut manifest = index_builder::fragment::sync::FragmentFileManifest::new();
        manifest.extraction_identity.model_name = "remote|old-template|768".into();
        let old_rows = HashMap::from([("content-hash".into(), vec![0.5; 768])]);
        assert_eq!(
            reusable_fragment_rows(old_rows.clone(), Some(&manifest), "remote|old-template|768")
                .len(),
            1
        );
        assert!(
            reusable_fragment_rows(old_rows.clone(), Some(&manifest), "remote|new-template|768")
                .is_empty()
        );
        assert!(
            reusable_fragment_rows(old_rows.clone(), Some(&manifest), "local|qwen|1024").is_empty()
        );
        assert!(reusable_fragment_rows(old_rows, None, "remote|old-template|768").is_empty());
    }

    #[test]
    #[cfg(feature = "remote-embeddings")]
    fn test_streaming_generation_remote_outage_persists_single_space() {
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server_requests = requests.clone();
        let endpoint = index_builder::remote_embed_test::spawn_mock_embeddings_server_responses(
            move |_| {
                if server_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    768
                } else {
                    0
                }
            },
            None,
            2,
        );
        let tfidf = index_builder::TfIdfEmbedder::build(&[("a.rs".into(), "fn a() {}".into())]);
        let generation = index_builder::GenerationNeuralEmbedder::new(
            index_builder::HybridEmbedder::hybrid_remote(
                tfidf,
                crate::search::onnx::RemoteEmbeddingConfig {
                    provider: crate::search::onnx::RemoteProvider::Custom { endpoint },
                    remote_dimension: Some(768),
                    ..crate::search::onnx::RemoteEmbeddingConfig::default()
                },
                None,
            )
            .expect("remote generation"),
        );
        let adapter = HybridStreamingEmbedder { inner: &generation };
        let mut inputs = (0..6).map(|i| {
            Ok(streaming::neural::NeuralInput {
                node_id: format!("node-{i}"),
                text: format!("document-{i}"),
                title: Some(format!("src/file-{i}.rs")),
            })
        });
        let mut writer = streaming::neural::VecNeuralRowWriter::new();
        let budget = streaming::BatchBudget {
            max_texts: 2,
            ..Default::default()
        };
        let stats =
            streaming::neural::enrich_neural_streaming(&mut inputs, &adapter, &mut writer, &budget)
                .expect("streaming generation");
        assert_eq!(stats.rows_written, 2);
        assert_eq!(stats.rows_skipped, 4);
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 2);
        let dir = tempfile::tempdir().expect("streaming mmap");
        let path = dir.path().join("neural_embeddings.bin");
        crate::search::vector::write_mmap_embeddings(&path, &writer.rows)
            .expect("persist streaming");
        let mmap = crate::search::vector::MmapEmbeddingIndex::open(&path).expect("open streaming");
        let rows = mmap.entries().expect("streaming stored rows");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|(_, row)| row.len() == 768));
    }

    #[test]
    fn test_streaming_neural_inputs_missing_source_uses_empty_bytes() {
        let dir = tempfile::tempdir().expect("neural source fixture");
        let project_path = dir.path().join("checkout");
        std::fs::create_dir_all(&project_path).expect("project root fixture");
        let file_path = project_path.join("missing.rs").display().to_string();
        let node_id = format!("{file_path}:missing_source");
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        pdg.add_node(crate::graph::pdg::Node {
            id: node_id.clone(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "missing_source".to_string(),
            file_path: std::sync::Arc::from(file_path),
            byte_range: (0, 1),
            complexity: 1,
            language: "rust".to_string(),
        });
        let admitted = HashSet::from([node_id.clone()]);

        let mut inputs = streaming_neural_inputs(&pdg, &admitted, &project_path);
        let input = inputs
            .next()
            .expect("admitted node should produce a neural input")
            .expect("missing source uses the empty-byte fallback");
        assert_eq!(input.node_id, node_id);
        // The title is the PROJECT-RELATIVE path: an absolute one would leak the
        // local checkout path to a remote provider and make title-aware
        // embeddings differ between identical checkouts.
        assert_eq!(input.title.as_deref(), Some("missing.rs"));
        assert!(inputs.next().is_none());
    }
}
