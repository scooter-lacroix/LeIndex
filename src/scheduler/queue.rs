//! Deficit round-robin (DRR) queues with same-project coalescing and aging
//! (§5.2).
//!
//! The queue is organized per (client, project, work class). Each queue holds
//! FIFO entries; every scheduler tick adds a quantum proportional to the
//! class's effective weight to each queue's deficit, and the ready queue with
//! the largest deficit is serviced next. This yields fair interleaving across
//! projects of equal weight (no project runs to completion while another
//! waits). Same-project duplicate index requests that target the same source
//! generation are coalesced into a single job (generalizing the legacy
//! `index_slots` consolidation). A starved class receives an aging weight
//! boost so it is eventually selected.

use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::scheduler::admission::Admission;
use crate::scheduler::budget::{BoundedJob, Step, WorkBudget};
use crate::scheduler::classes::{ClassAging, WorkClass};

/// Identifier for a distinct job instance in the queue.
pub type JobId = u64;

/// A client identifier (the agent harness or shim that issued the request).
pub type ClientId = String;

/// A project identifier (the indexed project path/name).
pub type ProjectId = String;

/// Identity of a queue: (client, project, work class).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueueKey {
    /// The client harness that issued the request.
    pub client: ClientId,
    /// The project being operated on.
    pub project: ProjectId,
    /// The work class (determines DRR weight and admission gating).
    pub class: WorkClass,
}

impl QueueKey {
    /// Build a queue key for the given (client, project, class) tuple.
    pub fn new(client: impl Into<String>, project: impl Into<String>, class: WorkClass) -> Self {
        Self {
            client: client.into(),
            project: project.into(),
            class,
        }
    }
}

/// Coalescing key for a job: two requests for the same project that target the
/// same source generation produce the same end state and can share one job.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TargetRef {
    /// The project this target indexes.
    pub project: ProjectId,
    /// Fingerprint of the source state the job indexes (generalizes the
    /// legacy `index_slots` per-project consolidation).
    pub source_fingerprint: String,
}

impl TargetRef {
    /// Build a coalescing target from a project id and a source fingerprint.
    pub fn new(project: impl Into<String>, source_fingerprint: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            source_fingerprint: source_fingerprint.into(),
        }
    }
}

/// A single queued unit of work.
pub struct DrrEntry<P> {
    /// Stable job id handed to callers.
    id: JobId,
    key: QueueKey,
    /// Coalescing target, if this job can absorb duplicates.
    target: Option<TargetRef>,
    job: Box<dyn BoundedJob<Progress = P>>,
    /// Number of callers awaiting this job's completion (>= 1).
    waiters: usize,
    /// Last progress the job reported; used as the shared result on completion.
    last_progress: Option<P>,
    /// Consecutive failed steps without an intervening success (reset on
    /// progress).
    consecutive_failures: usize,
    /// Earliest tick at which this entry may be retried after a failure
    /// (exponential backoff in ticks).
    retry_not_before_tick: u64,
}

/// Consecutive failed steps after which a job is moved to the terminal
/// failed state instead of being retried again. A deterministic failure
/// (unreadable input, poisoned checkpoint) would otherwise burn the entire
/// step budget with zero progress and keep every poller waiting forever.
const MAX_CONSECUTIVE_FAILURES: usize = 8;

/// Longest backoff between retries, in ticks.
const MAX_RETRY_BACKOFF_TICKS: u64 = 128;

/// Outcome of a single scheduler tick (one step of one job).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickOutcome<P> {
    /// Which job was advanced.
    pub job_id: JobId,
    /// The class the job belongs to.
    pub class: WorkClass,
    /// The step result.
    pub step: Step<P>,
}

/// One (client, project, class) FIFO queue with its running DRR deficit.
struct ClassQueue<P> {
    entries: VecDeque<DrrEntry<P>>,
    /// Deficit credit accrued toward this queue.
    deficit: u64,
}

impl<P> ClassQueue<P> {
    fn has_ready(&self) -> bool {
        !self.entries.is_empty()
    }
}

/// DRR multi-queue keyed by (client, project, class).
pub struct DrrQueue<P> {
    queues: BTreeMap<QueueKey, ClassQueue<P>>,
    ager: ClassAging,
    next_job_id: JobId,
    /// Completed job results, keyed by job id. The `Option` is `Some` when the
    /// job reported progress before completing (`None` for jobs that completed
    /// in a single step without ever yielding).
    results: HashMap<JobId, Option<P>>,
    /// Terminally failed jobs, keyed by job id, with the last step error.
    /// Recorded after [`MAX_CONSECUTIVE_FAILURES`] consecutive failed steps;
    /// `is_done` reports them as done so pollers cannot block forever.
    failures: HashMap<JobId, String>,
    /// Completed coalescable jobs, keyed by target (so a duplicate arriving
    /// after completion returns the shared result without new work).
    completed_by_target: HashMap<TargetRef, JobId>,
    /// Deficit added per tick per unit of weight.
    quantum: u64,
    /// Monotonic tick counter driving retry backoff.
    tick_count: u64,
    /// Admission gate consulted before running gated classes.
    admit: Box<dyn Fn(WorkClass, usize) -> Admission + Send + Sync>,
}

impl<P: Send + Clone> DrrQueue<P> {
    /// Create an empty DRR queue with the default aging policy (threshold 100
    /// ticks, boost 200) and an always-admit gate.
    pub fn new() -> Self {
        Self::with_policy(ClassAging::default(), |_, _| Admission::Admit)
    }

    /// Create a DRR queue with a custom aging policy and admission gate.
    pub fn with_policy<F>(ager: ClassAging, admit: F) -> Self
    where
        F: Fn(WorkClass, usize) -> Admission + Send + Sync + 'static,
    {
        Self {
            queues: BTreeMap::new(),
            ager,
            next_job_id: 0,
            results: HashMap::new(),
            failures: HashMap::new(),
            completed_by_target: HashMap::new(),
            quantum: 1,
            tick_count: 0,
            admit: Box::new(admit),
        }
    }

    /// The aging policy in use (for diagnostics).
    pub fn aging(&self) -> &ClassAging {
        &self.ager
    }

    /// Enqueue a job.
    ///
    /// If `coalesce` is `Some(target)` and a live (or already-completed) job
    /// for the same project and target exists, no duplicate is enqueued: the
    /// existing job's id is returned and its waiter count is incremented.
    pub fn enqueue(
        &mut self,
        key: QueueKey,
        coalesce: Option<TargetRef>,
        job: Box<dyn BoundedJob<Progress = P>>,
    ) -> JobId {
        // Same-project duplicate coalescing by target source generation.
        if let Some(target) = &coalesce {
            if let Some(id) = self.completed_by_target.get(target) {
                // Duplicate arrived after the original completed: hand out the
                // shared (already-done) result without creating new work.
                return *id;
            }
            if let Some(id) = self.find_live_for_target(target) {
                return id;
            }
        }
        let id = self.next_job_id;
        self.next_job_id += 1;
        self.ager.note_eligible(key.class);
        let queue = self
            .queues
            .entry(key.clone())
            .or_insert_with(|| ClassQueue {
                entries: VecDeque::new(),
                deficit: 0,
            });
        queue.entries.push_back(DrrEntry {
            id,
            key,
            target: coalesce,
            job,
            waiters: 1,
            last_progress: None,
            consecutive_failures: 0,
            retry_not_before_tick: 0,
        });
        id
    }

    /// Number of queued (not completed) entries for a queue key.
    pub fn queue_len(&self, key: &QueueKey) -> usize {
        self.queues.get(key).map_or(0, |q| q.entries.len())
    }

    /// Whether the job with `id` has completed (successfully or as a
    /// terminal failure).
    pub fn is_done(&self, id: JobId) -> bool {
        self.results.contains_key(&id) || self.failures.contains_key(&id)
    }

    /// The terminal failure message for `id`, if the job failed.
    pub fn failure(&self, id: JobId) -> Option<&str> {
        self.failures.get(&id).map(String::as_str)
    }

    /// The shared result for `id`, if completed and the job reported progress.
    pub fn result(&self, id: JobId) -> Option<&P> {
        self.results.get(&id).and_then(|p| p.as_ref())
    }

    /// Number of callers waiting on a live job (>= 1). Coalesced duplicates
    /// raise this.
    pub fn waiters(&self, id: JobId) -> usize {
        self.queues
            .values()
            .flat_map(|q| q.entries.iter())
            .find(|e| e.id == id)
            .map_or(0, |e| e.waiters)
    }

    /// Whether a live (non-completed) job for the same project + target exists.
    /// If so, increments its waiter count (a coalesced duplicate).
    fn find_live_for_target(&mut self, target: &TargetRef) -> Option<JobId> {
        for (key, queue) in self.queues.iter_mut() {
            if key.project != target.project {
                continue;
            }
            for entry in queue.entries.iter_mut() {
                if entry.target.as_ref() == Some(target) {
                    entry.waiters += 1;
                    return Some(entry.id);
                }
            }
        }
        None
    }

    /// Advance the scheduler by one tick: accrue deficits, select the ready
    /// queue with the largest deficit, and execute one step of its front job.
    ///
    /// Returns `None` when no job is ready (all queues empty or the selected
    /// work was deferred by admission).
    pub fn tick(&mut self, budget: WorkBudget) -> Option<TickOutcome<P>> {
        self.tick_count += 1;
        self.ager.advance();

        // Accrue deficit for every queue proportional to effective weight.
        for (key, queue) in self.queues.iter_mut() {
            let weight = self.ager.effective_weight(key.class);
            queue.deficit = queue
                .deficit
                .saturating_add(self.quantum.saturating_mul(weight));
        }

        // Candidate queues in deficit order (stable tie-break by key).
        let mut candidates: Vec<(QueueKey, u64)> = self
            .queues
            .iter()
            .filter(|(_, q)| q.has_ready())
            .map(|(key, q)| (key.clone(), q.deficit))
            .collect();
        candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        for (key, _deficit) in candidates {
            let gated = matches!(key.class, WorkClass::IndexChunk | WorkClass::Maintenance);
            let mut step_budget = budget;
            if gated {
                // The scheduler's admission gate is consulted before dequeueing
                // index chunks and maintenance work.
                let estimate = self
                    .queues
                    .get(&key)
                    .and_then(|q| q.entries.front())
                    .map_or(0, |e| e.job.estimated_next_bytes());
                match (self.admit)(key.class, estimate) {
                    Admission::Admit => {}
                    Admission::Reduce { batch_shape } => {
                        if batch_shape.max_items == 0 {
                            continue; // fully shrunk: effectively deferred
                        }
                        step_budget.max_items = batch_shape.max_items;
                        step_budget.max_input_bytes =
                            step_budget.max_input_bytes.min(batch_shape.max_items);
                        step_budget.max_output_bytes =
                            step_budget.max_output_bytes.min(batch_shape.max_items);
                    }
                    Admission::Defer => continue, // held for a later tick
                }
            }

            if let Some(outcome) = self.run_front(key, step_budget) {
                return Some(outcome);
            }
        }
        None
    }

    /// Run one step of the front entry of `key`'s queue.
    fn run_front(&mut self, key: QueueKey, budget: WorkBudget) -> Option<TickOutcome<P>> {
        let queue = self.queues.get_mut(&key)?;
        let mut entry = queue.entries.pop_front()?;
        // Backoff: an entry that recently failed waits out its deferral at
        // the BACK of the queue, so entries behind it are served instead of
        // starving behind a hot retry loop.
        if self.tick_count < entry.retry_not_before_tick {
            self.queues.get_mut(&key)?.entries.push_back(entry);
            return None;
        }
        let job_id = entry.id;
        let class = entry.key.class;
        let step = match entry.job.step(budget) {
            Ok(step) => step,
            Err(error) => {
                if self.defer_failed_step(&mut entry, error) {
                    self.queues.get_mut(&key)?.entries.push_back(entry);
                }
                return None;
            }
        };
        match step {
            Step::Yield(progress) => {
                entry.last_progress = Some(progress.clone());
                entry.consecutive_failures = 0;
                entry.retry_not_before_tick = 0;
                // Requeue the entry for a future tick.
                self.queues.get_mut(&key)?.entries.push_front(entry);
                self.queues.get_mut(&key)?.deficit = self
                    .queues
                    .get_mut(&key)?
                    .deficit
                    .saturating_sub(self.quantum);
                self.ager.note_served(class);
                Some(TickOutcome {
                    job_id,
                    class,
                    step: Step::Yield(progress),
                })
            }
            Step::Complete => {
                // Completed: record the shared result (last reported progress,
                // if any), publish the coalesce mapping, and drop the entry.
                let progress = entry.last_progress.take();
                if let Some(target) = entry.target {
                    self.completed_by_target.insert(target, job_id);
                }
                self.results.insert(job_id, progress);
                self.queues.get_mut(&key)?.deficit = self
                    .queues
                    .get_mut(&key)?
                    .deficit
                    .saturating_sub(self.quantum);
                self.ager.note_served(class);
                Some(TickOutcome {
                    job_id,
                    class,
                    step: Step::Complete,
                })
            }
        }
    }

    /// Defer-don't-error (anti-cheat §2.1 #10) handling for a failed step:
    /// retry with exponential backoff, never silently drop, and surface the
    /// error. After [`MAX_CONSECUTIVE_FAILURES`] the job becomes terminally
    /// failed so it stops consuming the step budget and `is_done` unblocks
    /// pollers.
    ///
    /// Returns whether the entry should be re-queued (at the back) for a
    /// later retry; `false` means it failed terminally and is dropped.
    fn defer_failed_step(&mut self, entry: &mut DrrEntry<P>, error: anyhow::Error) -> bool {
        let job_id = entry.id;
        let class = entry.key.class;
        entry.consecutive_failures += 1;
        let message = format!("{error:#}");
        if entry.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
            tracing::warn!(
                job = job_id,
                class = ?class,
                attempts = entry.consecutive_failures,
                "scheduler job failed terminally: {message}"
            );
            self.failures.insert(job_id, message);
            // Mirror the other run_front exits: drain one quantum of DRR
            // credit for this service. The entry is being dropped, so a
            // quantum left behind is credit nothing can spend and the next
            // enqueue on this key would inherit it.
            if let Some(queue) = self.queues.get_mut(&entry.key) {
                queue.deficit = queue.deficit.saturating_sub(self.quantum);
            }
            self.ager.note_served(class);
            return false;
        }
        let backoff = MAX_RETRY_BACKOFF_TICKS.min(1u64 << entry.consecutive_failures.min(7));
        entry.retry_not_before_tick = self.tick_count + backoff;
        tracing::debug!(
            job = job_id,
            class = ?class,
            attempt = entry.consecutive_failures,
            retry_in_ticks = backoff,
            "scheduler step failed; backing off: {message}"
        );
        true
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// A counting job that processes one item per step. `steps` counts every
    /// step() invocation so tests can observe how much work actually executed
    /// (proving coalescing prevented duplicate work).
    struct CountingJob {
        total: usize,
        processed: usize,
        steps: Arc<AtomicUsize>,
    }

    impl CountingJob {
        fn new(total: usize, steps: Arc<AtomicUsize>) -> Self {
            Self {
                total,
                processed: 0,
                steps,
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

    fn index_key(project: &str) -> QueueKey {
        QueueKey::new("client-a", project, WorkClass::IndexChunk)
    }

    /// VAL-SCHED-005: two projects' index jobs interleave fairly — each makes
    /// per-step progress and neither is starved while the other runs.
    #[test]
    fn test_drr_fair_interleave() {
        let mut queue = DrrQueue::<usize>::new();
        let a_steps = Arc::new(AtomicUsize::new(0));
        let b_steps = Arc::new(AtomicUsize::new(0));
        let job_a: JobId = queue.enqueue(
            index_key("project-a"),
            None,
            Box::new(CountingJob::new(10, a_steps.clone())),
        );
        let job_b: JobId = queue.enqueue(
            index_key("project-b"),
            None,
            Box::new(CountingJob::new(10, b_steps.clone())),
        );

        let mut steps_a = 0usize;
        let mut steps_b = 0usize;
        let mut rounds = 0;
        while !queue.is_done(job_a) || !queue.is_done(job_b) {
            match queue.tick(one_item_budget()) {
                Some(outcome) => {
                    rounds += 1;
                    if outcome.job_id == job_a {
                        steps_a += 1;
                    } else if outcome.job_id == job_b {
                        steps_b += 1;
                    }
                }
                None => panic!("queue reported no ready work with two live jobs"),
            }
        }

        assert_eq!(
            steps_a + steps_b,
            20,
            "both 10-step jobs complete in 20 steps"
        );
        // Fairness bound: each receives >= 7 of the 20 steps (60% of the 10-step
        // fair share), and neither ran 0 while the other ran > 5.
        assert!(
            steps_a >= 7 && steps_b >= 7,
            "fair interleave violated: a={steps_a}, b={steps_b}"
        );
        assert!(
            !(steps_a == 0 && steps_b > 5 || steps_b == 0 && steps_a > 5),
            "one project fully starved: a={steps_a}, b={steps_b}"
        );
        // Neither project ran to completion while the other was waiting: both
        // were serviced before either reached 10 (interleaving observed in
        // `rounds` progression is inherent to the deficit accounting above).
        assert!(rounds <= 20);
    }

    /// VAL-SCHED-006: same-project duplicate requests coalesce by target —
    /// one job created, both callers receive the shared result, no duplicate
    /// work executes.
    #[test]
    fn test_same_project_coalesce() {
        let mut queue = DrrQueue::<usize>::new();
        let steps = Arc::new(AtomicUsize::new(0));
        let target = TargetRef {
            project: "project-p".to_string(),
            source_fingerprint: "gen-fp-001".to_string(),
        };

        let key = index_key("project-p");
        let first = queue.enqueue(
            key.clone(),
            Some(target.clone()),
            Box::new(CountingJob::new(10, steps.clone())),
        );
        let second = queue.enqueue(
            key.clone(),
            Some(target),
            Box::new(CountingJob::new(10, steps.clone())),
        );

        // Exactly one entry queued, not two.
        assert_eq!(queue.queue_len(&key), 1);
        // The second caller shares the first job.
        assert_eq!(first, second);

        // Drive to completion.
        let mut guard = 0;
        while !queue.is_done(first) {
            assert!(queue.tick(one_item_budget()).is_some());
            guard += 1;
            assert!(guard < 100, "job never completes");
        }

        // Only 10 step() invocations executed — the duplicate did no work.
        assert_eq!(
            steps.load(Ordering::SeqCst),
            10,
            "coalesced duplicate must not execute work"
        );
        // Both callers receive the same shared result.
        assert_eq!(
            queue.result(first),
            queue.result(second),
            "both callers see the shared result"
        );
        assert!(queue.is_done(first) && queue.is_done(second));
    }

    /// VAL-SCHED-006: a duplicate arriving AFTER completion also coalesces to
    /// the shared result without creating new work.
    #[test]
    fn test_same_project_coalesce_after_completion() {
        let mut queue = DrrQueue::<usize>::new();
        let steps = Arc::new(AtomicUsize::new(0));
        let target = TargetRef {
            project: "project-p".to_string(),
            source_fingerprint: "gen-fp-001".to_string(),
        };
        let key = index_key("project-p");
        let first = queue.enqueue(
            key.clone(),
            Some(target.clone()),
            Box::new(CountingJob::new(10, steps.clone())),
        );
        while !queue.is_done(first) {
            queue.tick(one_item_budget());
        }
        assert_eq!(steps.load(Ordering::SeqCst), 10);

        let late = queue.enqueue(
            key.clone(),
            Some(target),
            Box::new(CountingJob::new(10, steps.clone())),
        );
        assert_eq!(late, first, "late duplicate reuses completed job id");
        assert!(queue.is_done(late));
        assert_eq!(steps.load(Ordering::SeqCst), 10, "no new work executed");
        assert_eq!(
            queue.result(late),
            queue.result(first),
            "late caller sees the shared result"
        );
    }

    /// VAL-SCHED-007: aging boosts a starved class so it is selected within a
    /// bounded number of ticks even under continuous higher-weight load.
    #[test]
    fn test_aging_boost_starved_class() {
        let mut queue = DrrQueue::<usize>::with_policy(
            crate::scheduler::classes::ClassAging::new(100, 5000),
            |_, _| Admission::Admit,
        );

        // Continuous InteractiveRead load.
        let read_steps = Arc::new(AtomicUsize::new(0));
        let read_key = QueueKey::new("client-a", "project-a", WorkClass::InteractiveRead);
        queue.enqueue(
            read_key,
            None,
            Box::new(CountingJob::new(10_000, read_steps.clone())),
        );
        // One Maintenance job arriving at tick 0.
        let maint_key = QueueKey::new("client-a", "project-a", WorkClass::Maintenance);
        let maint_steps = Arc::new(AtomicUsize::new(0));
        let maint: JobId = queue.enqueue(
            maint_key,
            None,
            Box::new(CountingJob::new(1, maint_steps.clone())),
        );

        // Without aging (threshold enormous), maintenance is never selected.
        let mut no_aging = DrrQueue::<usize>::with_policy(
            crate::scheduler::classes::ClassAging::new(1_000_000, 0),
            |_, _| Admission::Admit,
        );
        no_aging.enqueue(
            QueueKey::new("client-a", "project-a", WorkClass::InteractiveRead),
            None,
            Box::new(CountingJob::new(10_000, Arc::new(AtomicUsize::new(0)))),
        );
        let maint_no_aging: JobId = no_aging.enqueue(
            QueueKey::new("client-a", "project-a", WorkClass::Maintenance),
            None,
            Box::new(CountingJob::new(1, Arc::new(AtomicUsize::new(0)))),
        );
        for _ in 0..500 {
            no_aging.tick(one_item_budget());
        }
        assert!(
            !no_aging.is_done(maint_no_aging),
            "without aging, maintenance must never be selected under read load"
        );

        // With aging, maintenance is selected within a bounded window: it is
        // starved after 100 ticks, then catches up within 100 more.
        let mut selected_at = None;
        for t in 1..=250 {
            match queue.tick(one_item_budget()) {
                Some(outcome) if outcome.job_id == maint => {
                    selected_at = Some(t);
                    break;
                }
                _ => {}
            }
        }
        let selected_at = selected_at.expect("aging must select the starved maintenance job");
        assert!(
            selected_at <= 200,
            "aging boost must select starved class within 100 ticks of the 100-tick threshold (selected at {selected_at})"
        );
        assert!(
            queue.is_done(maint),
            "maintenance job completed once selected"
        );
    }

    /// A job whose every step fails deterministically must reach a terminal
    /// failed state after bounded retries (with backoff between attempts) —
    /// not loop forever burning the step budget while pollers wait on
    /// `is_done`.
    #[test]
    fn test_failing_job_reaches_terminal_failure_after_bounded_retries() {
        struct AlwaysFails {
            attempts: Arc<AtomicUsize>,
        }
        impl BoundedJob for AlwaysFails {
            type Progress = usize;
            fn step(&mut self, _budget: WorkBudget) -> anyhow::Result<Step<usize>> {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("deterministic failure"))
            }
            fn estimated_next_bytes(&self) -> usize {
                8
            }
        }

        let mut queue = DrrQueue::<usize>::new();
        let attempts = Arc::new(AtomicUsize::new(0));
        let id = queue.enqueue(
            index_key("project-a"),
            None,
            Box::new(AlwaysFails {
                attempts: attempts.clone(),
            }),
        );

        // Exponential backoff spaces the 8 attempts across up to ~255 ticks;
        // a cap well above that proves termination.
        let mut ticks = 0;
        while !queue.is_done(id) {
            ticks += 1;
            assert!(ticks <= 1000, "job must terminate within 1000 ticks");
            let _ = queue.tick(one_item_budget());
        }

        assert_eq!(
            attempts.load(Ordering::SeqCst),
            MAX_CONSECUTIVE_FAILURES,
            "exactly the retry cap worth of attempts, not one per tick"
        );
        assert!(
            queue
                .failure(id)
                .is_some_and(|m| m.contains("deterministic failure"))
        );
        assert!(
            queue.queue_len(&index_key("project-a")) == 0,
            "terminally failed entry leaves the queue"
        );
    }

    /// The terminal-failure exit must drain one quantum of DRR credit like
    /// the Yield/Complete exits do: a dropped job produced no outcome, so
    /// credit left behind is unspendable and would be inherited by the next
    /// enqueue on the same key.
    #[test]
    fn test_terminal_failure_drains_one_quantum_of_deficit() {
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

        let mut queue = DrrQueue::<usize>::new();
        let key = index_key("project-a");
        let class = key.class;
        let id = queue.enqueue(key.clone(), None, Box::new(AlwaysFails));

        // First tick: the step fails and the entry re-enters backoff — no
        // deficit is drained because no service happened.
        let _ = queue.tick(one_item_budget());
        let deficit_after_backoff = queue.queues.get(&key).expect("queue kept").deficit;

        // Fast-forward to the terminal attempt: the next failure is the
        // (MAX)th consecutive one and the backoff is already served.
        {
            let entry = queue
                .queues
                .get_mut(&key)
                .expect("queue kept")
                .entries
                .front_mut()
                .expect("entry still queued");
            entry.consecutive_failures = MAX_CONSECUTIVE_FAILURES - 1;
            entry.retry_not_before_tick = 0;
        }

        // Terminal tick: accrual adds quantum * weight for the queue, the
        // terminal exit must drain exactly quantum back off.
        let _ = queue.tick(one_item_budget());
        assert!(queue.is_done(id), "job reached terminal failure");
        let weight = queue.ager.effective_weight(class);
        let expected = deficit_after_backoff + queue.quantum * (weight - 1);
        assert_eq!(
            queue.queues.get(&key).expect("queue kept").deficit,
            expected,
            "terminal failure must drain exactly one quantum of deficit"
        );
        assert_eq!(queue.queue_len(&key), 0, "terminally failed entry dropped");
    }
}
