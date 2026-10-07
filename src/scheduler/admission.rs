//! Global admission decisions (§5.3).
//!
//! The scheduler gates heavy work (index chunks, maintenance) through an
//! admission decision before dequeueing it. The invariant enforced here — and
//! by the full [`AdmissionController`] that builds on this module — is that
//! admission NEVER errors: it returns exactly one of [`Admission::Admit`],
//! [`Admission::Defer`], or [`Admission::Reduce`]. A memory cap must prevent
//! overlapping peaks, not convert valid work into errors (spec §5.3,
//! anti-cheat §2.1 #10).
//!
//! The [`AdmissionController`] is the daemon's single memory-admission point:
//! it reads three independent factors before deciding a heavy step's fate —
//! (1) this process's RSS, (2) the resident set of the mmap'd generation
//! working set, and (3) the provider (embed-worker) model reserve. On near-cap
//! pressure it first triggers idle-cache eviction (deferral is the fallback,
//! never the first response), then admits, reduces the batch, or defers.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The outcome of an admission decision for a unit of work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The work may proceed now.
    Admit,
    /// The work is deferred (held for a later tick) — never rejected or erased.
    Defer,
    /// The work may proceed, but with a reduced batch shape (smaller chunk).
    Reduce {
        /// The reduced per-step item ceiling for the batch.
        batch_shape: BatchShape,
    },
}

/// The reduced shape of a batch under memory pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchShape {
    /// Maximum items the reduced batch may process per step.
    pub max_items: usize,
}

impl BatchShape {
    /// A batch shape that permits no items (fully shrunk). Used as the floor
    /// for reduction; a job would effectively defer at this shape.
    pub fn empty() -> Self {
        Self { max_items: 0 }
    }

    /// A batch shape that halves the given item ceiling.
    pub fn half_of(items: usize) -> Self {
        Self {
            max_items: items / 2,
        }
    }

    /// A batch shape that scales `default_items` by the fraction of the next
    /// step's estimated byte footprint (`estimate_mb`) that fits within the
    /// available `headroom_mb`. Clamped to `default_items` when the estimate
    /// fits, and to zero when no headroom exists.
    pub fn scaled_for_bytes(default_items: usize, headroom_mb: u64, estimate_mb: u64) -> Self {
        if estimate_mb == 0 || estimate_mb <= headroom_mb {
            return Self {
                max_items: default_items,
            };
        }
        let scaled = (default_items as u128)
            .saturating_mul(headroom_mb as u128)
            .checked_div(estimate_mb as u128)
            .unwrap_or(0);
        Self {
            max_items: scaled.min(default_items as u128) as usize,
        }
    }
}

/// A decidable admission policy. The scheduler calls
/// `decide(estimated_next_bytes)` immediately before dequeueing an index chunk
/// or maintenance item.
pub type AdmissionDecider = Box<dyn Fn(usize) -> Admission + Send + Sync>;

/// An admission policy that always admits (used when the full admission
/// controller is not wired in, and as the default).
pub fn always_admit() -> AdmissionDecider {
    Box::new(|_| Admission::Admit)
}

/// An admission policy that defers every index chunk and maintenance item above
/// `defer_above_bytes`. Useful for wiring the run loop's admission gate into a
/// memory-aware controller without the full RSS reader.
pub fn defer_above(defer_above_bytes: usize) -> AdmissionDecider {
    Box::new(move |estimate| {
        if estimate > defer_above_bytes {
            Admission::Defer
        } else {
            Admission::Admit
        }
    })
}

/// `bytes` → megabytes, rounded up so any non-zero estimate counts as at least
/// one MB of projected footprint (a memory-cap account cannot under-count).
fn ceil_mb(bytes: u64) -> u64 {
    const MB: u64 = 1024 * 1024;
    if bytes == 0 { 0 } else { bytes.div_ceil(MB) }
}

/// Read the resident set of the mmap'd generation working set (Linux), in MB.
///
/// Parses `/proc/self/smaps` and sums the `Rss:` lines of file-backed
/// mappings (the mmap'd CAS blobs). Returns 0 when the file is unavailable
/// (non-Linux, sandboxed procfs) so admission degrades to RSS-only rather than
/// erroring — the "never error" invariant holds for the reader too.
fn read_mmap_resident_mb() -> u64 {
    let Ok(content) = std::fs::read_to_string("/proc/self/smaps") else {
        return 0;
    };
    let mut rss_kb: u64 = 0;
    let mut in_file_mapping = false;
    for line in content.lines() {
        let trimmed = line.trim();
        // VMA headers look like "55a1b2c3d000-55a1b2c4e000 r--p ... <pathname>".
        if !trimmed.is_empty()
            && trimmed
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_hexdigit())
            && trimmed.contains('-')
        {
            let fields: Vec<&str> = trimmed.split_whitespace().collect();
            // File-backed mappings carry a pathname after the inode field.
            in_file_mapping = fields.len() > 5 && !fields[5].starts_with('[');
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Rss:") {
            if in_file_mapping {
                if let Ok(kb) = rest.split_whitespace().next().unwrap_or("0").parse::<u64>() {
                    rss_kb += kb;
                }
            }
        }
    }
    rss_kb / 1024
}

/// Read the current process RSS in MB (Linux `/proc/self/status`).
///
/// Mirrors `cli::memory_cap::current_rss_mb` but lives in the scheduler module
/// so admission stays compiled independent of the `cli` feature. Returns `Err`
/// when procfs is unavailable (the caller degrades to a 0 baseline — never a
/// hard failure).
fn read_self_rss_mb() -> Result<u64, String> {
    let status = std::fs::read_to_string("/proc/self/status")
        .map_err(|e| format!("cannot read /proc/self/status: {e}"))?;
    for line in status.lines() {
        if line.starts_with("VmRSS:") {
            let kb = line
                .split_whitespace()
                .nth(1)
                .ok_or_else(|| "malformed VmRSS line".to_string())?
                .parse::<u64>()
                .map_err(|_| "non-numeric VmRSS value".to_string())?;
            return Ok(kb / 1024);
        }
    }
    Err("VmRSS not found in /proc/self/status".to_string())
}

/// A global memory admission controller (§5.3).
///
/// Reads three factors for every decision:
///   1. `rss_reader` — this process's RSS (MB);
///   2. `mmap_resident` — resident pages of the mmap'd generation working set
///      (MB), maintained by a sampler through the shared [`AtomicU64`];
///   3. `provider_reserve` — the embed worker's model footprint (MB) that must
///      be reserved while indexing.
///
/// The decision compares `rss + mmap + provider + next_estimate` against the
/// cap. Over-cap decisions trigger the `on_pressure` eviction hook first
/// (idle-cache eviction); deferral is the fallback, never the first response.
/// `decide` is infallible — it returns [`Admission::Admit`], [`Admission::Defer`],
/// or [`Admission::Reduce`] for any input, never an error.
pub struct AdmissionController {
    rss_reader: Box<dyn Fn() -> Result<u64, String> + Send + Sync>,
    /// Resident mmap working set in MB (sampler-updated).
    mmap_resident: Arc<AtomicU64>,
    /// Embed worker model reserve in MB.
    provider_reserve: u64,
    /// Memory cap in MB (from `--max-memory` or config).
    cap: u64,
    /// Fraction of the cap reserved so a reduced batch has room to make
    /// progress: below this headroom, reduction would stall, so we Defer.
    reduce_min_headroom_mb: u64,
    /// Nominal per-step item ceiling used to scale `Reduce` batches.
    default_max_items: usize,
    /// Pressure-response hook: evicts idle project caches (and re-samples the
    /// memory facts) before a Defer is emitted.
    on_pressure: Box<dyn Fn() + Send + Sync>,
}

impl AdmissionController {
    /// Create a controller with an injected RSS reader (tests, or a reader
    /// that includes the embed worker) and no eviction hook. The mmap
    /// resident cell starts at 0.
    pub fn new(
        cap_mb: u64,
        provider_reserve_mb: u64,
        rss_reader: impl Fn() -> Result<u64, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            rss_reader: Box::new(rss_reader),
            mmap_resident: Arc::new(AtomicU64::new(0)),
            provider_reserve: provider_reserve_mb,
            cap: cap_mb,
            reduce_min_headroom_mb: cap_mb / 10,
            default_max_items: 1000,
            on_pressure: Box::new(|| {}),
        }
    }

    /// A controller wired to the real process memory facts: RSS from
    /// `/proc/self/status` and mmap resident from `/proc/self/smaps`.
    /// `provider_reserve_mb` is the embed worker's model footprint. The mmap
    /// resident cell is sampled once at construction; the daemon's periodic
    /// sampler should keep it fresh via `mmap_resident_cell()`.
    pub fn real(cap_mb: u64, provider_reserve_mb: u64) -> Self {
        let controller = Self::new(cap_mb, provider_reserve_mb, read_self_rss_mb);
        controller
            .mmap_resident
            .store(read_mmap_resident_mb(), Ordering::SeqCst);
        controller
    }

    /// The shared cell holding the resident mmap working set (MB). A sampler
    /// (e.g. the daemon's periodic memory sampler) updates this.
    pub fn mmap_resident_cell(&self) -> Arc<AtomicU64> {
        self.mmap_resident.clone()
    }

    /// Attach the pressure-response hook (idle-cache eviction). Called when a
    /// decision projects usage over the cap, before deferring.
    pub fn with_eviction(mut self, on_pressure: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_pressure = Box::new(on_pressure);
        self
    }

    /// Override the nominal per-step item ceiling used to scale reductions.
    pub fn with_default_items(mut self, items: usize) -> Self {
        self.default_max_items = items;
        self
    }

    /// The configured cap in MB.
    pub fn cap_mb(&self) -> u64 {
        self.cap
    }

    /// Combined current footprint: RSS + resident mmap working set + provider
    /// reserve (MB). All three factors feed every decision (VAL-SCHED-009).
    fn base_usage_mb(&self) -> u64 {
        let rss = (self.rss_reader)().unwrap_or(0);
        rss.saturating_add(self.mmap_resident.load(Ordering::SeqCst))
            .saturating_add(self.provider_reserve)
    }

    /// Decide whether a unit of work projecting `next_estimate` bytes may run.
    ///
    /// Infallible: returns exactly one of `Admit` / `Defer` / `Reduce`. When
    /// the projected footprint exceeds the cap it first fires the pressure
    /// hook (idle-cache eviction) and re-samples; only if the work still does
    /// not fit does it defer or reduce.
    pub fn decide(&self, next_estimate: usize) -> Admission {
        let estimate_mb = ceil_mb(next_estimate as u64);
        let base = self.base_usage_mb();
        if base.saturating_add(estimate_mb) <= self.cap {
            return Admission::Admit;
        }

        // Projected over cap: pressure response FIRST (evict idle caches),
        // deferral is the fallback.
        (self.on_pressure)();
        let base_after = self.base_usage_mb();
        if base_after.saturating_add(estimate_mb) <= self.cap {
            // Eviction freed enough headroom.
            return Admission::Admit;
        }

        let headroom = self.cap.saturating_sub(base_after);
        if headroom <= self.reduce_min_headroom_mb {
            // At/near the cap even after eviction: no batch can make safe
            // progress. Hold the work for a later tick.
            return Admission::Defer;
        }

        // Room exists for a smaller batch: shrink the per-step ceiling so the
        // projected footprint fits within the cap.
        let shape = BatchShape::scaled_for_bytes(self.default_max_items, headroom, estimate_mb);
        if shape.max_items == 0 {
            Admission::Defer
        } else {
            Admission::Reduce { batch_shape: shape }
        }
    }

    /// Wrap this controller as an [`AdmissionDecider`] for the scheduler's
    /// admission gate (the daemon's single admission point).
    pub fn decider(self: &Arc<Self>) -> AdmissionDecider {
        let controller = self.clone();
        Box::new(move |estimate| controller.decide(estimate))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use proptest::prelude::*;

    const MB: u64 = 1024 * 1024;

    fn ok_reader(rss_mb: u64) -> impl Fn() -> Result<u64, String> {
        move || Ok(rss_mb)
    }

    /// Build a controller whose only memory fact is the RSS reader (mmap and
    /// provider both 0).
    fn rss_only(cap_mb: u64, rss_mb: u64) -> AdmissionController {
        AdmissionController::new(cap_mb, 0, ok_reader(rss_mb))
    }

    // VAL-SCHED-008: the AdmissionController returns only Admit/Defer/Reduce
    // for ANY input — random RSS (0 to 1 TiB), random mmap resident, random
    // provider reserve, and random estimates (0 to 16 GiB). Zero error cases.
    proptest! {
        #[test]
        fn test_admission_never_errors(
            cap in 16u64..=(1 << 20),
            rss in 0u64..=(1 << 30),
            mmap in 0u64..=(1 << 30),
            provider in 0u64..=(1 << 30),
            estimate_bytes in 0usize..=(1usize << 34),
        ) {
            let controller = AdmissionController {
                rss_reader: Box::new(move || Ok(rss)),
                mmap_resident: Arc::new(AtomicU64::new(mmap)),
                provider_reserve: provider,
                cap,
                reduce_min_headroom_mb: cap / 10,
                default_max_items: 1000,
                on_pressure: Box::new(|| {}),
            };
            let decision = controller.decide(estimate_bytes);
            match decision {
                Admission::Admit => {}
                Admission::Defer => {}
                Admission::Reduce { batch_shape } => {
                    prop_assert!(batch_shape.max_items <= 1000);
                }
            }
        }
    }

    /// The real RSS/mmap readers do not panic and return plausible values.
    #[test]
    fn test_real_readers_do_not_panic() {
        let controller = AdmissionController::real(1 << 20, 0);
        // Infallible decision against the live process.
        let decision = controller.decide(MB as usize);
        matches!(
            decision,
            Admission::Admit | Admission::Defer | Admission::Reduce { .. }
        );
        // The reader fns themselves are sane on Linux.
        #[cfg(target_os = "linux")]
        {
            let rss = read_self_rss_mb().expect("RSS readable on Linux");
            assert!(rss > 0);
        }
    }

    /// VAL-SCHED-009: all three factors (RSS, mmap resident, provider reserve)
    /// independently influence the decision. Vary each one while holding the
    /// other two constant and observe the outcome flip.
    #[test]
    fn test_admission_reads_all_factors() {
        let cap = 1024;

        // (a) RSS alone: low → Admit, near-cap → Defer, with mmap+provider 0.
        let low = rss_only(cap, 100).decide(100 * MB as usize);
        assert_eq!(low, Admission::Admit, "low RSS admits");
        let near = rss_only(cap, 980).decide(256 * MB as usize);
        assert_eq!(
            near,
            Admission::Defer,
            "near-cap RSS + large estimate defers"
        );

        // (b) mmap resident alone (RSS and provider constant at 0).
        let controller = AdmissionController::new(cap, 0, ok_reader(0));
        let mmap_cell = controller.mmap_resident_cell();
        mmap_cell.store(100, Ordering::SeqCst);
        assert_eq!(
            controller.decide(100 * MB as usize),
            Admission::Admit,
            "small mmap working set admits"
        );
        mmap_cell.store(980, Ordering::SeqCst);
        assert_eq!(
            controller.decide(256 * MB as usize),
            Admission::Defer,
            "large mmap working set defers with RSS+provider constant"
        );

        // (c) provider reserve alone (RSS and mmap constant at 0).
        let low_provider = AdmissionController::new(cap, 100, ok_reader(0));
        assert_eq!(
            low_provider.decide(100 * MB as usize),
            Admission::Admit,
            "small provider reserve admits"
        );
        let high_provider = AdmissionController::new(cap, 980, ok_reader(0));
        assert_eq!(
            high_provider.decide(256 * MB as usize),
            Admission::Defer,
            "large provider reserve defers with RSS+mmap constant"
        );
    }

    /// VAL-SCHED-010: when `current_usage + next_estimate > cap` the decision
    /// is `Defer`, never an error. After pressure subsides (eviction lowers
    /// usage), the same job is readmitted and eventually completes.
    #[test]
    fn test_admission_defer_not_error() {
        let cap = 1024;
        let rss = Arc::new(AtomicU64::new(972)); // 95% of cap
        let rss_for_reader = rss.clone();
        let controller =
            AdmissionController::new(cap, 0, move || Ok(rss_for_reader.load(Ordering::SeqCst)));

        // Near-cap RSS + a large estimate → Defer (not Err, not Admit).
        let estimate = 512 * MB as usize;
        assert_eq!(
            controller.decide(estimate),
            Admission::Defer,
            "near-cap + large estimate defers"
        );

        // Pressure subsides: RSS drops well under the cap → the same work is
        // readmitted.
        rss.store(400, Ordering::SeqCst);
        assert_eq!(
            controller.decide(estimate),
            Admission::Admit,
            "after memory frees, the deferred job is admitted"
        );
    }

    /// VAL-SCHED-011: under near-cap pressure the controller triggers
    /// idle-cache eviction BEFORE deferring; the eviction hook is called, and
    /// after it frees memory the work is admitted (defer is the fallback, not
    /// the first response).
    #[test]
    fn test_admission_evicts_under_pressure() {
        let cap = 1024;
        let evictions = Arc::new(AtomicU64::new(0));
        let evictions_hook = evictions.clone();

        // The controller's mmap resident cell starts at 600 (two idle projects'
        // resident pages). Idle-cache eviction clears it.
        let controller = AdmissionController::new(cap, 200, ok_reader(200));
        let mmap_cell = controller.mmap_resident_cell();
        mmap_cell.store(600, Ordering::SeqCst);
        let hook_mmap = mmap_cell.clone();
        let controller = controller.with_eviction(move || {
            evictions_hook.fetch_add(1, Ordering::SeqCst);
            // Evicting idle caches releases the resident mmap pages.
            hook_mmap.store(0, Ordering::SeqCst);
        });

        // 200 (rss) + 600 (mmap) + 200 (provider) + 300 estimate = 1300 > 1024.
        let estimate = 300 * MB as usize;
        let decision = controller.decide(estimate);
        // Eviction fired (first response to pressure).
        assert_eq!(
            evictions.load(Ordering::SeqCst),
            1,
            "eviction must be the first pressure response"
        );
        // After eviction freed the mmap working set, usage is 200+0+200+300=700
        // which fits — the work is admitted, not deferred.
        assert_eq!(
            decision,
            Admission::Admit,
            "after eviction frees headroom the work is admitted"
        );
    }

    /// When eviction cannot free enough memory, deferral is the fallback.
    #[test]
    fn test_admission_defers_when_eviction_not_enough() {
        let cap = 1024;
        let evictions = Arc::new(AtomicU64::new(0));
        let evictions_hook = evictions.clone();
        // The eviction hook frees only a little (2 projects evicted, 1 large
        // project remains resident).
        let controller =
            AdmissionController::new(cap, 200, ok_reader(900)).with_eviction(move || {
                evictions_hook.fetch_add(1, Ordering::SeqCst);
            });
        let decision = controller.decide(100 * MB as usize);
        assert_eq!(
            evictions.load(Ordering::SeqCst),
            1,
            "eviction attempted before deferring"
        );
        assert_eq!(
            decision,
            Admission::Defer,
            "still over cap after eviction → defer, never error"
        );
    }

    /// Reduce: with meaningful headroom and a batch too large to fit, the
    /// decision shrinks the batch so the projected footprint fits the cap.
    #[test]
    fn test_admission_reduces_batch_to_fit() {
        let cap = 1024;
        // base = 200 (rss) + 0 + 0 = 200 → headroom 824 MB (>= 10% of cap).
        let controller = AdmissionController::new(cap, 0, ok_reader(200)).with_default_items(1000);
        // estimate = 1 GiB would push projected to 1224 > 1024.
        let estimate = 1024 * MB as usize;
        match controller.decide(estimate) {
            Admission::Reduce { batch_shape } => {
                assert!(
                    batch_shape.max_items < 1000,
                    "reduced batch must shrink the item ceiling, got {}",
                    batch_shape.max_items
                );
                assert!(
                    batch_shape.max_items > 0,
                    "a fitting batch still makes progress"
                );
            }
            other => panic!("expected Reduce, got {other:?}"),
        }
    }

    /// BatchShape byte scaling is clamped and total.
    #[test]
    fn test_batch_shape_scaled_for_bytes() {
        // Estimate fits headroom → full ceiling.
        assert_eq!(BatchShape::scaled_for_bytes(1000, 800, 500).max_items, 1000);
        // Estimate larger than headroom → proportional shrink.
        let shape = BatchShape::scaled_for_bytes(1000, 400, 800);
        assert_eq!(shape.max_items, 500);
        // No headroom → empty floor.
        assert_eq!(BatchShape::scaled_for_bytes(1000, 0, 800).max_items, 0);
        // Zero estimate never shrinks.
        assert_eq!(BatchShape::scaled_for_bytes(1000, 10, 0).max_items, 1000);
    }

    /// The controller can be wrapped as an AdmissionDecider for the scheduler.
    #[test]
    fn test_controller_as_decider() {
        let controller = Arc::new(rss_only(1024, 100));
        let decider = controller.decider();
        assert_eq!(decider(100 * MB as usize), Admission::Admit);
        let near = Arc::new(rss_only(1024, 990));
        assert_eq!(near.decider()(512 * MB as usize), Admission::Defer);
    }

    /// The minimal in-memory policies keep their historical contracts.
    #[test]
    fn test_defer_above_policy() {
        let decider = defer_above(64);
        assert_eq!(decider(10), Admission::Admit);
        assert_eq!(decider(1000), Admission::Defer);
    }
}
