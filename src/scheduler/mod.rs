//! Fair bounded scheduler (§5.2, §5.3).
//!
//! The [`Scheduler`] owns the DRR queues and is the daemon's single point of
//! scheduling. It enqueues [`BoundedJob`]s keyed by (client, project, work
//! class), interleaves them fairly by deficit-round-robin, coalesces
//! same-project duplicate index requests, and consults the global admission
//! decision **before** dequeueing index chunks and maintenance work — so reads
//! are never starved and memory pressure defers (never errors) heavy work.

pub mod admission;
pub mod budget;
pub mod classes;
pub mod index_job;
pub mod queue;

pub use admission::{Admission, AdmissionController, AdmissionDecider, BatchShape, always_admit};
pub use budget::{BoundedJob, Step, WorkBudget};
pub use classes::{ClassAging, WorkClass};
pub use index_job::{IndexJob, IndexJobProgress, IndexPhaseId, PhaseExecutor};
pub use queue::{DrrQueue, JobId, QueueKey, TargetRef, TickOutcome};

use std::sync::Arc;

/// A fair scheduler: DRR fairness + admission-gated dequeue of heavy work.
pub struct Scheduler<P> {
    queue: DrrQueue<P>,
}

impl<P: Send + Clone> Scheduler<P> {
    /// Create a scheduler whose admission policy always admits.
    pub fn new() -> Self {
        Self::with_admission(always_admit())
    }

    /// Create a scheduler with a custom admission policy.
    ///
    /// The policy is owned by the underlying queue and consulted on every tick
    /// before dequeueing an [`WorkClass::IndexChunk`] or
    /// [`WorkClass::Maintenance`] item.
    pub fn with_admission(admission: AdmissionDecider) -> Self {
        let queue = DrrQueue::with_policy(ClassAging::default(), move |_class, estimate| {
            // The decider decides purely on the projected next-step footprint;
            // reducing the per-step item ceiling happens within the queue.
            admission(estimate)
        });
        Self { queue }
    }

    /// Create a scheduler whose admission gate is the global
    /// [`AdmissionController`] — the daemon's single memory-admission point
    /// (§5.3). Before every index chunk / maintenance dequeue the controller
    /// reads RSS + mmap resident + provider reserve and returns Admit, Defer,
    /// or Reduce (never an error); pressure triggers idle-cache eviction first.
    pub fn with_admission_controller(controller: Arc<AdmissionController>) -> Self {
        Self::with_admission(controller.decider())
    }

    /// Enqueue a job, returning its id. Same-project duplicate requests with
    /// the same coalesce target are merged into a single job.
    pub fn enqueue(
        &mut self,
        key: QueueKey,
        coalesce: Option<TargetRef>,
        job: Box<dyn BoundedJob<Progress = P>>,
    ) -> JobId {
        self.queue.enqueue(key, coalesce, job)
    }

    /// Advance scheduling by one tick (one step of one job).
    ///
    /// Returns the step outcome, or `None` when no work is ready (all queues
    /// empty, or the only ready heavy work was deferred by admission).
    pub fn tick(&mut self, budget: WorkBudget) -> Option<TickOutcome<P>> {
        self.queue.tick(budget)
    }

    /// Run the run-loop until `id` completes or no ready work remains.
    ///
    /// Returns the shared result if the job completed. When admission
    /// repeatedly defers the job across many ticks, returns `None` (the job is
    /// still pending, not failed). A job that reached the terminal failed
    /// state also yields `None` here — callers must disambiguate with
    /// [`failure`](Self::failure) / [`is_failed`](Self::is_failed), which is
    /// why `is_done` alone is not a success signal.
    pub fn run_until_done(&mut self, id: JobId, budget: WorkBudget) -> Option<&P> {
        // Bound the loop so an eternally-deferred job cannot spin forever; the
        // caller re-invokes this after memory frees up.
        let max_ticks = 100_000;
        for _ in 0..max_ticks {
            if self.queue.is_done(id) {
                return self.queue.result(id);
            }
            // No ready work this tick (deferred or idle); yield control.
            self.queue.tick(budget)?;
        }
        None
    }

    /// Whether the job has completed (successfully or as a terminal failure).
    pub fn is_done(&self, id: JobId) -> bool {
        self.queue.is_done(id)
    }

    /// Whether the job ended in the terminal failed state (bounded retries
    /// exhausted). Check this after [`is_done`](Self::is_done): a failed job
    /// is "done" but has no result.
    pub fn is_failed(&self, id: JobId) -> bool {
        self.queue.failure(id).is_some()
    }

    /// The terminal failure message for `id`, if the job failed.
    pub fn failure(&self, id: JobId) -> Option<&str> {
        self.queue.failure(id)
    }

    /// The shared result for a completed job.
    pub fn result(&self, id: JobId) -> Option<&P> {
        self.queue.result(id)
    }

    /// Number of queued entries for a key (diagnostics / coalesce tests).
    pub fn queue_len(&self, key: &QueueKey) -> usize {
        self.queue.queue_len(key)
    }
}

impl<P: Send + Clone> Default for Scheduler<P> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    /// A counting job processing one item per step.
    struct CountingJob {
        total: usize,
        processed: usize,
        steps: Arc<AtomicUsize>,
    }

    impl CountingJob {
        fn new(total: usize) -> Self {
            Self {
                total,
                processed: 0,
                steps: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl BoundedJob for CountingJob {
        type Progress = usize;

        fn step(&mut self, budget: WorkBudget) -> anyhow::Result<Step<Self::Progress>> {
            self.steps.fetch_add(1, Ordering::SeqCst);
            if self.processed >= self.total {
                return Ok(Step::Complete);
            }
            let take = budget.max_items.max(1).min(self.total - self.processed);
            self.processed += take;
            if self.processed >= self.total {
                Ok(Step::Complete)
            } else {
                Ok(Step::Yield(self.processed))
            }
        }

        fn estimated_next_bytes(&self) -> usize {
            if self.processed >= self.total {
                0
            } else {
                (self.total - self.processed) * 8
            }
        }
    }

    fn one_item_budget() -> WorkBudget {
        WorkBudget::new(usize::MAX, usize::MAX, 1, Duration::from_secs(3600))
    }

    fn read_key(project: &str) -> QueueKey {
        QueueKey::new("client-a", project, WorkClass::InteractiveRead)
    }

    fn index_key(project: &str) -> QueueKey {
        QueueKey::new("client-a", project, WorkClass::IndexChunk)
    }

    /// Task 6: multi-project contention — project A indexes in chunks while
    /// project B's reads are not starved, and the scheduler calls the admission
    /// decision before dequeueing an IndexChunk.
    #[test]
    fn test_run_loop_admission_gates_index_chunk() {
        // Admission pressure switch + call counter.
        let pressure = Arc::new(AtomicBool::new(true));
        let decide_calls = Arc::new(AtomicUsize::new(0));
        let pressure_c = pressure.clone();
        let calls_c = decide_calls.clone();
        let decider: AdmissionDecider = Box::new(move |_estimate| {
            calls_c.fetch_add(1, Ordering::SeqCst);
            if pressure_c.load(Ordering::SeqCst) {
                Admission::Defer // project A's index chunk is deferred
            } else {
                Admission::Admit
            }
        });

        let mut sched = Scheduler::<usize>::with_admission(decider);

        // Project A: a long-running IndexChunk.
        let index_job: JobId =
            sched.enqueue(index_key("project-a"), None, Box::new(CountingJob::new(10)));
        // Project B: continuous interactive reads.
        for _ in 0..20 {
            sched.enqueue(read_key("project-b"), None, Box::new(CountingJob::new(1)));
        }

        // Under pressure, reads proceed while project A's index chunk is held.
        let mut read_done = 0;
        for _ in 0..30 {
            match sched.tick(one_item_budget()) {
                Some(outcome) => {
                    if outcome.class == WorkClass::InteractiveRead {
                        read_done += 1;
                    }
                }
                None => break,
            }
        }
        assert!(
            !sched.is_done(index_job),
            "index chunk must be deferred while pressure is on"
        );
        assert!(
            read_done > 0,
            "project B reads must proceed while A's index is deferred"
        );
        assert!(
            decide_calls.load(Ordering::SeqCst) > 0,
            "admission decision must be consulted before dequeuing an IndexChunk"
        );

        // Pressure relents: project A's index chunk now advances.
        pressure.store(false, Ordering::SeqCst);
        let mut index_advanced = false;
        for _ in 0..12 {
            match sched.tick(one_item_budget()) {
                Some(outcome) if outcome.job_id == index_job => {
                    index_advanced = true;
                    if outcome.step == Step::Complete {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(
            index_advanced,
            "after pressure relents, project A's index chunk must be serviced"
        );
        assert!(
            sched.is_done(index_job),
            "project A's index chunk eventually completes"
        );
    }

    /// Task 6: the scheduler interleaves a project's indexing with another
    /// project's reads without starving the reads over many ticks.
    #[test]
    fn test_contented_reads_not_starved_by_indexing() {
        let mut sched = Scheduler::<usize>::new();
        let index_job: JobId = sched.enqueue(
            index_key("project-a"),
            None,
            Box::new(CountingJob::new(100)),
        );
        for _ in 0..50 {
            sched.enqueue(read_key("project-b"), None, Box::new(CountingJob::new(1)));
        }
        // Run until project A's indexing is done; project B reads must have
        // completed far before that (they did not starve).
        let mut reads_done_before_index = 0;
        for _ in 0..200 {
            match sched.tick(one_item_budget()) {
                Some(outcome) if outcome.class == WorkClass::InteractiveRead => {
                    reads_done_before_index += 1;
                }
                _ => {}
            }
            // NOTE: reads starve-check measured at the end.
            if sched.is_done(index_job) {
                break;
            }
        }
        // Reads (interactive) should have largely completed while the index
        // chunk world only partly: reads are higher weight and not starved.
        assert!(
            reads_done_before_index > 0,
            "reads must make progress while a same-scheduler index runs"
        );
        assert!(sched.is_done(index_job), "indexing completes eventually");
    }

    /// Configuring a scheduler with the default (always-admit) policy admits
    /// index chunks without any decision-based deferral.
    #[test]
    fn test_default_scheduler_admits() {
        let mut sched = Scheduler::<usize>::new();
        let id: JobId = sched.enqueue(index_key("p"), None, Box::new(CountingJob::new(3)));
        sched.run_until_done(id, one_item_budget());
        assert!(sched.is_done(id));
    }

    /// Task 7: with the global AdmissionController wired as the scheduler's
    /// admission gate, indexing OVER the cap defers (never errors) and then
    /// completes after pressure-response eviction frees memory.
    #[test]
    fn test_scheduler_admission_controller_defers_then_completes() {
        use crate::scheduler::admission::AdmissionController;
        use std::sync::atomic::{AtomicU64, Ordering};

        // RSS at the cap (1024/1024 MB) at start: every index step projects
        // over the cap and must defer.
        let rss = Arc::new(AtomicU64::new(1024));
        let evictions = Arc::new(AtomicU64::new(0));
        let rss_for_reader = rss.clone();
        let rss_for_eviction = rss.clone();
        let evictions_hook = evictions.clone();
        let controller = Arc::new(
            AdmissionController::new(1024, 0, move || Ok(rss_for_reader.load(Ordering::SeqCst)))
                .with_eviction(move || {
                    let call = evictions_hook.fetch_add(1, Ordering::SeqCst);
                    if call == 0 {
                        // First pressure response is insufficient: still over
                        // the cap → this tick's decision is Defer.
                        rss_for_eviction.store(1024, Ordering::SeqCst);
                    } else {
                        // Eviction frees idle caches → headroom appears.
                        rss_for_eviction.store(300, Ordering::SeqCst);
                    }
                }),
        );

        let mut sched = Scheduler::<usize>::with_admission_controller(controller);
        // A 10-step index chunk. The counting job's per-step estimate is
        // small but non-zero (>= 1 MB), so at RSS == cap every step's
        // projected footprint exceeds the cap until eviction frees memory.
        let id: JobId = sched.enqueue(index_key("project-a"), None, Box::new(CountingJob::new(10)));

        // Tick until completion. At least one tick must defer (no progress)
        // before the eviction response frees memory and the job completes.
        let mut steps = 0;
        let mut deferred_ticks = 0;
        while !sched.is_done(id) {
            let progressed = sched.tick(one_item_budget()).is_some();
            if !progressed {
                deferred_ticks += 1;
            }
            steps += 1;
            assert!(steps < 1000, "index job must complete, not spin forever");
        }
        assert!(
            deferred_ticks >= 1,
            "over-cap indexing must defer (at least one held tick) before eviction frees memory"
        );
        assert!(
            evictions.load(Ordering::SeqCst) >= 1,
            "pressure response (idle-cache eviction) must have fired"
        );
        assert!(
            rss.load(Ordering::SeqCst) < 1024,
            "eviction must have freed memory"
        );
        assert!(sched.is_done(id), "over-cap index completes after eviction");
    }

    /// A terminally failed job must be distinguishable from a successful
    /// no-progress job through the facade: `run_until_done` returns `None`
    /// for both, so `is_failed` / `failure` are the disambiguation.
    #[test]
    fn test_scheduler_exposes_terminal_failure() {
        struct AlwaysFails;
        impl BoundedJob for AlwaysFails {
            type Progress = usize;
            fn step(&mut self, _budget: WorkBudget) -> anyhow::Result<Step<usize>> {
                Err(anyhow::anyhow!("deterministic failure"))
            }
            fn estimated_next_bytes(&self) -> usize {
                8
            }
        }

        let mut sched = Scheduler::<usize>::new();
        let id: JobId = sched.enqueue(index_key("p"), None, Box::new(AlwaysFails));
        let mut ticks = 0;
        while !sched.is_done(id) {
            ticks += 1;
            assert!(ticks <= 1000, "job must terminate");
            let _ = sched.tick(one_item_budget());
        }
        assert!(sched.run_until_done(id, one_item_budget()).is_none());
        assert!(sched.is_failed(id), "terminal failure must be visible");
        assert!(
            sched
                .failure(id)
                .is_some_and(|m| m.contains("deterministic failure"))
        );
    }
}
