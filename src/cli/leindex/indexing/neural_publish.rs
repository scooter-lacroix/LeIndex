use super::*;
use std::path::{Component, Path};

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

    /// Start the heartbeat, optionally mirroring the row count into the
    /// durable `index-state.json` snapshot on every tick.
    ///
    /// The stderr heartbeat is invisible to any other process: an MCP status
    /// call, the TUI Status pane, or a second terminal all read the snapshot,
    /// and during a neural pass they otherwise see one unchanging phase name
    /// for the entire duration. Reusing this heartbeat rather than starting a
    /// second thread is deliberate — it already ticks at [`Self::INTERVAL`],
    /// which is the throttle `PhaseProgress` wants, and already tracks the
    /// same counter.
    ///
    /// `progress: None` is the caller that has no snapshot to attribute rows
    /// to (and is what the heartbeat's own test drives).
    fn start_reporting(total: usize, progress: Option<super::PhaseProgress>) -> Self {
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
                    // Mirror the same count into the durable snapshot so other
                    // processes see the phase advancing, not just this stderr.
                    if let Some(progress) = &progress {
                        progress
                            .report(rows_t.load(std::sync::atomic::Ordering::Relaxed));
                    }
                    for _ in 0..(Self::INTERVAL.as_millis() / 100) {
                        if stop_t.load(std::sync::atomic::Ordering::Relaxed) {
                            // Close the counters on the way out: a finished
                            // pass must not leave the snapshot frozen at a
                            // mid-flight count.
                            if let Some(progress) = &progress {
                                progress.finish();
                            }
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

/// Document title for a neural node: the source path as it will be sent to a
/// remote provider as `{title}`.
///
/// It MUST be project-relative. Some nodes carry an absolute path (submodule
/// summaries store `summary.path` verbatim), and sending that as the title
/// leaks the local checkout path to a remote provider and makes title-aware
/// embeddings differ between two identical checkouts (CodeRabbit
/// Security/Privacy). Note that only the TITLE is relativized — the node's
/// `file_path` is still the absolute path used to READ the source.
///
/// Three cases where there is no usable relative path, each with its own
/// answer — the distinction matters, because they leak different things:
///
/// * it succeeds with an EMPTY remainder — the node's path IS `project_path`,
///   e.g. a project summary node at the checkout root. The answer is the
///   project's own directory name, when that name is provably the project's
///   (see [`project_title`]);
/// * it fails outright — a path outside `project_path`, like a submodule
///   summary. Only the file name is kept; see [`leaf_title`] for why nothing
///   above it may be;
/// * the path has no components at all (a bare root, or one ending in `..`).
///   The symbol name stands in.
///
/// The empty remainder needs the explicit `is_empty` check below rather than a
/// plain `Ok(relative)` arm: `Ok("")` is a SUCCESS, so matching the `Result`
/// alone sends it straight through as an empty title.
///
/// A lexical `strip_prefix` is also not a containment check: it happily returns
/// `../private/x.rs` for a path that escapes the root, and `..` is neither empty
/// nor an error. Such a remainder is not a relative path at all, so it takes the
/// fallback like any other path outside the project.
///
/// The node ID may NOT be the fallback: it is `{file_path}:{qname}@{range}`
/// (see `graph::extraction`), so it carries the absolute path this exists to
/// strip.
fn document_title(file_path: &str, project_path: &Path, node_name: &str) -> String {
    let path = Path::new(file_path);
    if let Ok(relative) = path.strip_prefix(project_path) {
        if relative.as_os_str().is_empty() {
            return project_title(project_path, node_name);
        }
        if !relative.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            // Separators normalized so the title is the same string on either
            // host for the same file: without this a Windows-style path is one
            // opaque `Normal` component whose text still carries `\` and its
            // drive.
            return relative
                .to_string_lossy()
                .replace('\\', "/")
                .trim_matches('/')
                .to_string();
        }
    }
    leaf_title(path, node_name)
}

/// Title for a node that sits AT the project root: the project's own directory
/// name.
///
/// Guarded on depth, because the name is only the project's when there is a
/// path above it that could have been the machine-specific part. A one- or
/// two-component project (`/home/alice`, `/mnt/data`, `/work`) is as likely to
/// BE the home directory or a mount point as a checkout, and then its name is
/// precisely the component that must not leave the machine. Below that depth the
/// symbol name is the safer title.
fn project_title(project_path: &Path, node_name: &str) -> String {
    let depth = project_path
        .components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count();
    if depth <= 2 {
        return node_name.to_string();
    }
    project_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| node_name.to_string())
}

/// Title for a node whose path could not be made project-relative: the file
/// name, or `node_name` when the path has none.
///
/// There is no sound way to keep a fixed-length suffix of an ABSOLUTE path.
/// `strip_prefix` failed, so this code cannot tell which components are the
/// checkout and which are the project — and every component above the file is a
/// candidate for carrying a username (`/home/alice/src/main.rs`) or a mount
/// point. Taking "the last three" therefore shipped `alice` to the remote
/// provider, which is the leak [`document_title`] exists to prevent, merely
/// moved.
///
/// So the fallback keeps ONLY the file name. Two consequences, both accepted:
/// the title loses the directory context that used to separate `a/src/lib.rs`
/// from `b/src/lib.rs`, and this changes the title text — and therefore the
/// remote wire text — for out-of-project nodes relative to earlier versions.
/// Identity does not include the title, so cached vectors stay keyed correctly;
/// what changes is the text the provider sees. A cosmetic collision is a fair
/// price for never putting a local username on the wire.
fn leaf_title(path: &Path, node_name: &str) -> String {
    let leaf = path
        .components()
        .rev()
        .find_map(|component| match component {
            // One component can still be a whole Windows path when parsed on a
            // Unix host (`C:\a\b.rs` has no `/` separator there), so split it
            // rather than trusting the component boundary.
            Component::Normal(name) => Some(name.to_string_lossy().replace('\\', "/")),
            _ => None,
        })
        .and_then(|segment| {
            segment
                .split('/')
                .rfind(|part| !part.is_empty() && *part != "..")
                .map(str::to_string)
        });
    leaf.filter(|title| !title.is_empty())
        .unwrap_or_else(|| node_name.to_string())
}

/// Nominal byte target for one neural chunk. Must stay far below
/// `index_builder::cap_neural_text`'s 64 KiB truncation point, so that capping
/// each chunk is a no-op and no chunk is ever silently truncated on its way to
/// the embedder. The chunker's own accounting can overshoot this target by a few
/// bytes (interior line terminators), which is why the bound that actually
/// matters is the truncation point, not this number.
///
/// This is the ONE place the chunk size is defined. It is derived from
/// [`index_builder::NEURAL_TEXT_CAP_BYTES`] rather than repeating a literal, so a
/// future change to the cap cannot leave the chunker above it — which would
/// silently reintroduce the truncation this splitter exists to prevent.
///
/// # Measured: the chunk size is NOT a speed lever — do not re-tune it
///
/// 2 KiB (`/32`) was tried, then reverted. Isolated server probes were
/// encouraging — at a constant ~94k tokens, 2048-byte inputs gave 33,968 tok/s
/// against 20,631 tok/s at 8192 bytes — because llama.cpp forms its physical
/// batch across SEQUENCES, so many short inputs fill the batch where a single
/// long input occupies it alone.
///
/// Those numbers did not survive contact with the real pipeline. An interleaved
/// A/B over freshly-generated never-embedded corpora (so the embedding cache
/// could not mask the effect) measured `median 29.17s` at 8 KiB against
/// `median 30.04s` at 2 KiB — **0.97x, i.e. no gain, slightly worse**.
///
/// The reason is that this splitter almost never fires. Across a real tree the
/// mean node is 882 bytes with a p90 of 1801; only 7.8% of nodes exceed 2 KiB
/// and 0.1% exceed 8 KiB. A threshold sitting above the 90th percentile cannot
/// move end-to-end time no matter which way it is tuned.
///
/// So it stays at `/8`. Do not re-tune it expecting a speedup: the threshold
/// is above the 90th percentile of real node sizes, so it is close to inert.
/// Measure the node-size distribution before trusting any claim about it — an
/// earlier revision of this comment stated a token figure taken from a
/// synthetic probe with 220-character inputs and mistook characters for
/// tokens, understating real nodes by ~5x.
const NEURAL_CHUNK_BYTES: usize = index_builder::NEURAL_TEXT_CAP_BYTES / 8;

/// Budget for the neural streaming pass.
///
/// Centralised rather than spelled out at the call site so the ceilings that
/// interact — per-text sequence length, UTF-8 bytes, estimated tokens, and the
/// chunk size above — are checked against each other by the assertions below
/// instead of drifting apart silently.
fn neural_batch_budget() -> streaming::BatchBudget {
    streaming::BatchBudget {
        // Measured request-size curve on the managed server at 2048 chars/text,
        // constant total tokens, median of two runs each:
        //   64 texts -> 34,908 tok/s
        //  128 texts -> 34,973 tok/s
        //  256 texts -> 31,794 tok/s
        //  512 texts -> 28,652 tok/s
        // Throughput peaks at 128 and decays as the request grows, so the old
        // 256 sat past the knee. `max_estimated_tokens` alone did not bound this:
        // a 120k-node run logged 48 batches of 252 texts, i.e. the text cap was
        // binding, not the token budget.
        max_texts: 128,
        // A chunk is at most NEURAL_CHUNK_BYTES plus a small terminator
        // overshoot; 2x leaves headroom without admitting a whole 64 KiB node as
        // a single input.
        max_seq_len: index_builder::NEURAL_TEXT_CAP_BYTES,
        ..streaming::BatchBudget::default()
    }
}

/// The chunk size and the budget must agree, or a chunk is silently skipped.
///
/// `BatchBudget::can_accept` rejects any text longer than `max_seq_len`, and an
/// over-length input is counted as skipped and given no row. `chunk_code` may
/// overshoot its nominal target by a few bytes, so the only safe relationship is
/// `NEURAL_CHUNK_BYTES` strictly below `max_seq_len` with room for that overshoot.
/// Pinned as a compile-time assertion because both values are constants.
const _: () = assert!(
    NEURAL_CHUNK_BYTES * 2 <= index_builder::NEURAL_TEXT_CAP_BYTES,
    "a chunk must have headroom under the per-text cap, or overshoot is skipped"
);

/// Split one node's bounded semantic text into embedder-sized chunks.
///
/// Why this exists: [`index_builder::cap_neural_text`] truncates the text at
/// 64 KiB, so a node larger than that had its tail dropped from the neural
/// vector — the tail was never embedded at all. Emitting the text as a sequence
/// of chunks (each far under the cap) and mean-pooling the chunk vectors covers
/// the whole node instead of just its head.
///
/// The split reuses the fragment chunker rather than inventing a byte splitter.
/// `chunk_code` parses the text with tree-sitter when a grammar is available for
/// the path's extension and falls back to a UTF-8-safe naive line walk otherwise.
/// For a node's own source range the semantic fragments are contiguous slices of
/// that range (`split_node` grows each fragment from the node start through its
/// children), so no part of the node is skipped.
///
/// These chunks are NOT fragments: fragments are `owner`-addressed rows for a
/// content-hash store, whereas these exist only to be recombined into the single
/// vector a PDG node stores.
fn split_neural_chunks(text: &str, path: &std::path::Path) -> Vec<String> {
    let chunks: Vec<String> =
        index_builder::fragment::chunk_code(text, path, NEURAL_CHUNK_BYTES, true)
            .into_iter()
            .map(|fragment| fragment.content.to_owned())
            .collect();
    if chunks.is_empty() {
        // Nothing to split (empty source, or a path with no chunkable content):
        // embed the text as-is so the node still gets a vector.
        vec![text.to_owned()]
    } else {
        chunks
    }
}

/// Mean-pool every chunk vector of one node into the single row that node stores.
///
/// Means are accumulated as running sums, so a node whose chunks straddle a batch
/// boundary is still aggregated correctly: the partial sum carries in `open`
/// until the node's last chunk lands.
///
/// Cosine scoring is norm-invariant (`search::vector::cosine_similarity` normalizes
/// both sides), so the pooled vector is deliberately left unnormalized — no extra
/// sqrt pass and no change in rank order. Empty chunk vectors are dropped rather
/// than averaged in, so a chunk the embedder refused does not dilute the mean
/// toward the origin.
struct MeanPoolNeuralRowWriter<W: streaming::neural::NeuralRowWriter> {
    inner: W,
    open_node_id: Option<String>,
    open_sum: Vec<f32>,
    open_chunks: usize,
    /// A chunk of the open node arrived in a different vector space, so the
    /// running sum no longer represents the whole node.
    open_dim_mismatch: bool,
    /// Nodes abandoned because their chunks disagreed on dimension. Reported
    /// once at the end of the pass rather than warned per node.
    dim_mismatch_nodes: usize,
    /// Rows the wrapped writer has accepted since the previous `write_batch`
    /// call (or `finish`). `write_batch` returns this accumulated count —
    /// including the mid-batch flushes `fold` triggers — so the streaming
    /// stats and the checkpoint see every stored node, not just the trailing
    /// flush.
    accepted: usize,
}

impl<W: streaming::neural::NeuralRowWriter> MeanPoolNeuralRowWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            open_node_id: None,
            open_sum: Vec::new(),
            open_chunks: 0,
            open_dim_mismatch: false,
            dim_mismatch_nodes: 0,
            accepted: 0,
        }
    }

    /// Add one chunk vector to the open node's running sum.
    fn fold(&mut self, node_id: &str, embedding: &[f32]) -> Result<()> {
        if embedding.is_empty() {
            return Ok(());
        }
        if self.open_node_id.as_deref() != Some(node_id) {
            // The streaming pass emits a node's chunks consecutively, so a
            // different id means the previous node is complete.
            self.finish_open()?;
            self.open_node_id = Some(node_id.to_owned());
            self.open_sum.clear();
            self.open_chunks = 0;
            self.open_dim_mismatch = false;
        }
        if self.open_sum.is_empty() {
            self.open_sum = embedding.to_vec();
        } else if self.open_sum.len() == embedding.len() {
            for (slot, value) in self.open_sum.iter_mut().zip(embedding) {
                *slot += value;
            }
        } else {
            // One node must never mix vector spaces, and a mean over only the
            // chunks that agreed would silently represent a fraction of the
            // node's text. Abandon the whole node instead: `finish_open` then
            // writes nothing and the miss is counted.
            self.open_dim_mismatch = true;
            return Ok(());
        }
        self.open_chunks += 1;
        Ok(())
    }

    /// Number of nodes abandoned for a chunk dimension mismatch, consuming the
    /// count so the caller reports it exactly once.
    fn take_dim_mismatches(&mut self) -> usize {
        std::mem::take(&mut self.dim_mismatch_nodes)
    }

    /// Write the open node's mean through to the wrapped writer, returning the
    /// number of rows the wrapped writer actually ACCEPTED (0 or 1).
    ///
    /// The count must come back from the wrapped writer, not be assumed. Wrapped
    /// writers may legitimately reject a row — `SearchNeuralRowWriter` dedupes on
    /// node id and returns the accepted count from its own `write_batch`. Reporting
    /// a hardcoded `1` for a rejected row inflates `NeuralStats::rows_written`,
    /// which is what the phase's success/degraded checks and the checkpoint gate
    /// read, so a discarded row would be accounted as persisted.
    fn finish_open(&mut self) -> Result<usize> {
        let Some(node_id) = self.open_node_id.take() else {
            return Ok(0);
        };
        // A node whose chunks spanned two vector spaces has no valid mean; drop
        // it entirely and account for it instead of persisting a partial one.
        if self.open_dim_mismatch {
            self.dim_mismatch_nodes += 1;
            self.open_dim_mismatch = false;
            self.open_sum.clear();
            self.open_chunks = 0;
            return Ok(0);
        }
        let written = if self.open_chunks > 0 {
            let divisor = self.open_chunks as f32;
            let mean: Vec<f32> = self.open_sum.iter().map(|sum| sum / divisor).collect();
            let written = self.inner.write_batch(vec![(node_id, mean)])?;
            // Count it toward the next `write_batch`/`finish` return, so an
            // accepted row is never discarded just because the flush happened
            // mid-batch rather than at the batch's end (PR #105).
            self.accepted += written;
            written
        } else {
            0
        };
        self.open_sum.clear();
        self.open_chunks = 0;
        Ok(written)
    }

    /// Flush the still-open node. `enrich_neural_streaming` has no finish hook,
    /// so the last node of every run is still open when it returns — without
    /// this call that node silently loses its row.
    fn finish(&mut self) -> Result<usize> {
        self.finish_open()?;
        Ok(std::mem::take(&mut self.accepted))
    }
}

impl<W: streaming::neural::NeuralRowWriter> streaming::neural::NeuralRowWriter
    for MeanPoolNeuralRowWriter<W>
{
    fn write_row(&mut self, node_id: &str, embedding: &[f32]) -> Result<()> {
        // A writer that bypasses `write_batch` still gets pooling applied.
        self.fold(node_id, embedding)
    }

    fn write_batch(&mut self, rows: Vec<(String, Vec<f32>)>) -> Result<usize> {
        let mut latest = None;
        for (node_id, embedding) in rows {
            self.fold(&node_id, &embedding)?;
            latest = Some(node_id);
        }
        // The wrapped writer's `write_batch` dedupes on node id (see
        // `SearchNeuralRowWriter`), so a partially aggregated node must NOT be
        // written yet — doing so would make the final mean look like a duplicate
        // and be dropped. Only fully accumulated nodes are flushed; the wrapped
        // writer sees exactly one row per node.
        if self.open_node_id.as_deref() != latest.as_deref() {
            self.finish_open()?;
        }
        // Return every row accepted since the previous call, including the
        // flush(s) that happened while folding this batch. Discarding those
        // counts made a run of N nodes report ~1 accepted row overall, which
        // poisoned the checkpoint count and forced re-embedding on resume
        // (PR #105).
        Ok(std::mem::take(&mut self.accepted))
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

    // `filter_map` + `flatten`, not `from_fn`: one node now yields one input PER
    // CHUNK (see `split_neural_chunks`), so the per-node work has to produce a
    // sequence rather than a single item.
    std::iter::from_fn(move || node_indices.next())
        .filter_map(move |node_idx| {
            let node = pdg.get_node(node_idx)?;
            if node.node_type == crate::graph::pdg::NodeType::External
                || !admitted_node_ids.contains(&node.id)
            {
                return None;
            }
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
            // The project-relative source path rides along as the document title
            // so a title-aware provider gets a real `{title}` instead of `none`
            // (EmbeddingGemma2 trains on title+text prompts).
            //
            // This is NOT parity with the legacy route, and the difference is
            // deliberate: `embed_pending_neural_batch` goes through
            // `embed_neural_batch_blocking`, which passes no title, so
            // `apply_doc_template` renders `{title}` as `none` there and as a
            // path here. Only this route has a PDG node to derive a path from.
            //
            // The two flavours CAN coexist in one store, and nothing enforces
            // otherwise. `index_project`'s lexical phase uses the
            // `index_nodes_tfidf_only*` routes, which pass `_allow_neural =
            // false` — but the public `LeIndex::reload_vector_from_pdg` calls
            // `index_builder::index_nodes`, which passes `true`, so
            // `admit_prepared_node` builds a neural embedder and defers to the
            // title-less batch on the SAME resident engine this phase later
            // enriches. A process that rebuilds vectors that way, then indexes,
            // mixes both flavours in one generation.
            //
            // The risk that carries is retrieval quality, not cache corruption:
            // titles join the doc template, so title-aware rows and `none` rows
            // are embedded from different prompts and score against each other
            // slightly worse. Cache identity is unaffected — the template
            // fingerprint covers it. Accepted, and pinned here so it is not
            // mistaken for a guarantee.
            // `document_title` owns the leak guarantees; see its doc.
            let title = document_title(&node.file_path, project_path, &node.name);
            let node_id = node.id.clone();
            // A node too large for one embedder request is split rather than
            // truncated; `MeanPoolNeuralRowWriter` recombines the chunk vectors
            // into the single row the node stores. Short nodes produce exactly
            // one chunk and are unchanged from before.
            let chunks = split_neural_chunks(&text, Path::new(&*node.file_path));
            Some(chunks.into_iter().map(move |chunk| {
                Ok(streaming::neural::NeuralInput {
                    node_id: node_id.clone(),
                    text: index_builder::cap_neural_text(&chunk).to_owned(),
                    title: Some(title.clone()),
                })
            }))
        })
        .flatten()
}

/// How many admitted nodes this neural pass failed to cover.
///
/// `None` means full coverage. Split out of `enrich_neural_for_route` because
/// that function needs a live `SearchEngine` and PDG, and the policy — "a pass
/// that covers less than it admitted is a WARN, not a clean finish" — is the
/// part worth pinning. Zero admitted nodes is full coverage by definition: an
/// empty slice is not a failure.
fn neural_coverage_gap(embedded: usize, admitted: usize) -> Option<usize> {
    (embedded < admitted).then(|| admitted - embedded)
}

fn enrich_neural_for_route(
    engine: &mut crate::search::search::SearchEngine,
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    embedder: &index_builder::GenerationNeuralEmbedder,
    admitted_node_ids: &HashSet<String>,
    project_path: &std::path::Path,
) -> Result<usize> {
    let (cache_hits_before, cache_misses_before) = neural_cache_counters();
    // The heartbeat covers the whole pass (see NeuralProgress): liveness even
    // while the first embed batch is in flight, plus the CPU-fallback watch.
    // It also mirrors its row counter into `index-state.json`, which is the
    // only surface a DIFFERENT process (MCP status, TUI Status pane) can read.
    let progress = NeuralProgress::start_reporting(
        admitted_node_ids.len(),
        Some(super::PhaseProgress::start(
            crate::cli::leindex::resolve_existing_storage_path(project_path)
                .unwrap_or_else(|| project_path.join(".leindex")),
            crate::cli::leindex::IndexPhase::Neural,
            admitted_node_ids.len(),
        )),
    );
    let streaming_embedder = HybridStreamingEmbedder { inner: embedder };
    let writer = SearchNeuralRowWriter {
        engine,
        seen_node_ids: HashSet::new(),
    };
    let mut inputs = streaming_neural_inputs(pdg, admitted_node_ids, project_path);
    let budget = neural_batch_budget();
    // Pooling wraps the row writer so every chunk vector is folded into the
    // owning node's single mean BEFORE the id-deduping writer sees it —
    // otherwise all but the first chunk of a node would be dropped.
    let mut pooling = MeanPoolNeuralRowWriter::new(writer);
    let stats = streaming::neural::enrich_neural_streaming(
        &mut inputs,
        &streaming_embedder,
        &mut ProgressNeuralRowWriter {
            inner: &mut pooling,
            rows_reported: &progress.rows,
        },
        &budget,
    )?;
    // `enrich_neural_streaming` has no finish hook, so the last node's chunks are
    // still pooled here when it returns. Flush it or that node silently loses its
    // row.
    let mut stats = stats;
    stats.rows_written += pooling.finish()?;
    let dim_mismatch_nodes = pooling.take_dim_mismatches();
    drop(progress);
    let (cache_hits_after, cache_misses_after) = neural_cache_counters();
    let cache_hits = cache_hits_after.saturating_sub(cache_hits_before);
    // A DELTA, not the cumulative counter: `counters()` is process-wide, so
    // reading it raw here reported every miss from every earlier phase of the
    // same process as if this pass had embedded them. On a resumed or
    // multi-route run "N embedded" could exceed the rows on screen.
    let cache_misses = cache_misses_after.saturating_sub(cache_misses_before);
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
    } else if let Some(gap) = neural_coverage_gap(stats.rows_written, admitted_node_ids.len()) {
        // Partial coverage is the failure mode a single blip now produces
        // instead of a total freeze: the generation survives, the rest of the
        // run embeds normally, and the ONLY trace is a store that is quietly
        // missing rows — resume skips those nodes forever, because nothing
        // ever retries them. Never let that pass as a clean finish.
        warn!(
            admitted = admitted_node_ids.len(),
            embedded = stats.rows_written,
            gap,
            skipped = stats.rows_skipped,
            "neural enrichment covered only {}/{} admitted nodes; the remaining nodes have no \
             neural vector and resume will NOT re-embed them. Re-run the index (or delete \
             .leindex/neural_embeddings.bin) to fill the gap.",
            stats.rows_written,
            admitted_node_ids.len(),
        );
    }
    // A partially-degraded generation must not report clean success. `rows_skipped`
    // counts admitted nodes the embedder refused to vectorise (empty vectors,
    // over-length text) and `dim_mismatch_nodes` counts nodes whose chunks spanned
    // two vector spaces and were therefore abandoned whole. Either way those nodes
    // get NO neural row: semantic search cannot retrieve them, and because the
    // generation still completes and writes its checkpoint, a resume will not
    // revisit them either. Name the count so the gap is never mistaken for a
    // complete neural layer.
    if stats.rows_skipped > 0 || dim_mismatch_nodes > 0 {
        warn!(
            written = stats.rows_written,
            skipped = stats.rows_skipped,
            dim_mismatch = dim_mismatch_nodes,
            "neural enrichment finished with rows it could not embed. Those nodes have no neural \
             row, so semantic search cannot retrieve them and a resume will not retry them. Force \
             a re-embed (`leindex index --force`) after fixing the reported provider/model error."
        );
    }
    progress_stderr(&format!(
        "Indexing: neural done — {} rows ({} from embed cache, {} embedded, {} skipped, {} batches){}...",
        stats.rows_written,
        cache_hits,
        cache_misses,
        stats.rows_skipped,
        stats.batches,
        if dim_mismatch_nodes > 0 {
            format!(", {dim_mismatch_nodes} dropped on dimension mismatch")
        } else {
            String::new()
        },
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
        // loaded is derived from the restored row count, never from the
        // checkpoint: a resume that yields rows is loaded even when the
        // checkpoint recorded none, and a checkpoint that recorded rows
        // guarantees nothing unless the mmap actually yields them.
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
                // skipped for re-embedding (Codex P2).
                //
                // The check below runs only for a checkpoint that RECORDED rows:
                // `checkpointed > 0 && rows != checkpointed`. A checkpoint with
                // `rows == 0` records that nothing was embedded, so there is no
                // count to compare and the comparison is skipped entirely. That
                // exemption is deliberate (a zero-row checkpoint is a legitimate
                // no-neural-work marker, and treating a nonzero restore against
                // it as a mismatch would re-embed on every resume), but it is
                // narrower than "the restored total matches what was
                // checkpointed": the count is unverified whenever the checkpoint
                // recorded none, and a generation whose checkpoint recorded rows
                // but whose mmaps yield a different total is discarded wholesale
                // rather than partially trusted. Changing either rule would need
                // its own decision.
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
        let (mut neural_rows, mut neural_resume_loaded) = self.restore_neural_checkpoint(
            state.resumed_neural.as_ref(),
            &lexical_hash,
            &current_embed_model,
        );
        self.validate_neural_resume(&mut neural_rows, &mut neural_resume_loaded, &state);
        if let Some(neural_embedder) = neural_embedder
            .as_ref()
            .filter(|_| neural_rows == 0 && !neural_resume_loaded)
        {
            neural_rows = self.enrich_admitted_nodes(&state, neural_embedder)?;
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

    /// A checkpoint can be internally consistent yet still cover only a
    /// fraction of the lexical admission set (for example, a killed or degraded
    /// 1/866 pass). Never treat that partial layer as reusable: clear it and
    /// run enrichment again so missing nodes are retried.
    fn validate_neural_resume(
        &mut self,
        neural_rows: &mut usize,
        neural_resume_loaded: &mut bool,
        state: &IndexPipelineState,
    ) {
        if !*neural_resume_loaded || *neural_rows >= state.admitted_node_ids.len() {
            return;
        }
        warn!(
            restored = *neural_rows,
            admitted = state.admitted_node_ids.len(),
            "discarding partial neural checkpoint; re-embedding admitted nodes"
        );
        self.search_engine.clear_neural_embeddings();
        *neural_rows = 0;
        *neural_resume_loaded = false;
    }

    /// Embed every admitted node through the configured embedder, returning the
    /// number of neural rows produced.
    fn enrich_admitted_nodes(
        &mut self,
        state: &IndexPipelineState,
        neural_embedder: &index_builder::GenerationNeuralEmbedder,
    ) -> Result<usize> {
        let pdg = self
            .pdg
            .as_ref()
            .context("PDG is resident before neural enrichment")?;
        progress_stderr(&format!(
            "Indexing: neural embedding {} admitted nodes...",
            state.admitted_node_ids.len()
        ));
        enrich_neural_for_route(
            &mut self.search_engine,
            pdg,
            neural_embedder,
            &state.admitted_node_ids,
            &self.project_path,
        )
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
                // Publishing a Complete generation ends the run: any counters
                // a mid-flight phase left behind would otherwise survive into
                // a finished snapshot and read as a stuck bar.
                progress_done: 0,
                progress_total: None,
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

/// `neural_publish` is declared unconditionally by `indexing/mod.rs`, and the code
/// under test here — `split_neural_chunks`, `MeanPoolNeuralRowWriter`,
/// `neural_batch_budget` — carries no `onnx`/`remote-embeddings` gate of its own.
/// The module is reachable whenever the `cli` feature is on, which is the only
/// gate that matters for it.
///
/// So this module's tests carry `test` alone. Tying them to `onnx` claimed a gate
/// the production code does not have: in a `cli`-enabled build without `onnx` the
/// chunk splitter and mean-pool writer still compile and ship, and their tests
/// would have been compiled out. A test's gate has to match its subject's, or the
/// coverage silently disappears exactly where nobody is looking.
///
/// Tests needing a neural backend keep their own tighter gate.
#[cfg(test)]
#[path = "neural_publish_test.rs"]
mod tests;
