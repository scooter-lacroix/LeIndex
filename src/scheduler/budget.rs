//! Bounded execution primitives for the fair scheduler (§5.2).
//!
//! Heavy operations implement [`BoundedJob`] and drive their work in coarse,
//! yielding chunks through [`BoundedJob::step`]. The [`WorkBudget`] passed to
//! each step caps how much a single `step` call may do (input bytes observed,
//! output bytes produced, items handled, and CPU time spent) so that no one
//! job can monopolize the daemon. Between steps the scheduler may interleave
//! reads, other projects, or other work classes.

use std::time::Duration;

/// Resource limits applied to a single [`BoundedJob::step`] call.
///
/// A step may do *at most* `max_items` of whatever unit it counts, and should
/// stay within the given byte and CPU budgets when it can. These are ceilings,
/// not targets: a step returns [`Step::Yield`] as soon as it exhausts any cap
/// and there is still work left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkBudget {
    /// Upper bound on input bytes the step will read in this call.
    pub max_input_bytes: usize,
    /// Upper bound on output bytes the step will produce in this call.
    pub max_output_bytes: usize,
    /// Upper bound on discrete items the step will process in this call.
    pub max_items: usize,
    /// Upper bound on wall/CPU time the step should spend before yielding.
    pub max_cpu_time: Duration,
}

impl WorkBudget {
    /// Construct a budget with explicit caps.
    pub fn new(
        max_input_bytes: usize,
        max_output_bytes: usize,
        max_items: usize,
        max_cpu_time: Duration,
    ) -> Self {
        Self {
            max_input_bytes,
            max_output_bytes,
            max_items,
            max_cpu_time,
        }
    }

    /// A budget with all caps at their maximum (no limit). Used by tests and
    /// by direct, non-scheduled execution.
    pub fn unlimited() -> Self {
        Self {
            max_input_bytes: usize::MAX,
            max_output_bytes: usize::MAX,
            max_items: usize::MAX,
            max_cpu_time: Duration::from_secs(3600),
        }
    }
}

/// Outcome of a single [`BoundedJob::step`] call.
///
/// - [`Step::Yield`] carries a `Progress` describing how far the job advanced;
///   more work remains and the scheduler should call `step` again (later, after
///   other work has had a chance).
/// - [`Step::Complete`] means the job is done. A completed job is terminal and
///   idempotent: a further `step` returns `Complete` again without doing work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step<Progress> {
    /// The job advanced and still has work remaining.
    Yield(Progress),
    /// The job finished all of its work.
    Complete,
}

/// A unit of schedulable, bounded work.
///
/// Implementations must honor the invariant from §5.2: a single `step` call
/// performs a bounded amount of work (respecting [`WorkBudget`]), then either
/// yields (via [`Step::Yield`]) so the scheduler can service other clients,
/// projects, and classes, or returns [`Step::Complete`] once all work is done.
pub trait BoundedJob: Send {
    /// Type describing how far the job has advanced (used for progress /
    /// checkpoint reporting).
    type Progress: Send;

    /// Advance the job by at most one budgeted chunk.
    fn step(&mut self, budget: WorkBudget) -> anyhow::Result<Step<Self::Progress>>;

    /// Estimate how many input bytes the *next* `step` call is likely to
    /// consume. The estimate may be heuristic and imprecise, but it must be
    /// **non-zero** while the job is not finished so the admission controller
    /// can reason about upcoming work. A finished job may report zero.
    fn estimated_next_bytes(&self) -> usize;
}

#[cfg(test)]
mod test {
    use super::*;

    /// A toy job that counts `total` items, processing at most `max_items` per
    /// step. Progress reports the running count of processed items.
    struct CountingJob {
        total: usize,
        processed: usize,
        bytes_per_item: usize,
    }

    impl CountingJob {
        fn new(total: usize) -> Self {
            Self {
                total,
                processed: 0,
                bytes_per_item: 10,
            }
        }
    }

    impl BoundedJob for CountingJob {
        type Progress = usize;

        fn step(&mut self, budget: WorkBudget) -> anyhow::Result<Step<Self::Progress>> {
            if self.processed >= self.total {
                return Ok(Step::Complete);
            }
            let take = budget.max_items.min(self.total - self.processed);
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
                (self.total - self.processed) * self.bytes_per_item
            }
        }
    }

    fn budget_max_items(items: usize) -> WorkBudget {
        WorkBudget::new(usize::MAX, usize::MAX, items, Duration::from_secs(3600))
    }

    /// VAL-SCHED-001: WorkBudget bounds step execution. A toy job that counts
    /// items respects `max_items` — no single `step` processes more than the
    /// budget allows.
    #[test]
    fn test_work_budget_bounds_step() {
        let mut job = CountingJob::new(100);
        let budget = budget_max_items(30);

        let seen = [
            job.step(budget).unwrap(),
            job.step(budget).unwrap(),
            job.step(budget).unwrap(),
            job.step(budget).unwrap(),
        ];
        // First three steps process exactly 30 each, leaving work; fourth
        // finishes the remaining 10.
        assert_eq!(seen[0], Step::Yield(30));
        assert_eq!(seen[1], Step::Yield(60));
        assert_eq!(seen[2], Step::Yield(90));
        assert_eq!(seen[3], Step::Complete);
        // No single step overshot the 30-item budget.
        assert_eq!(job.processed, 100);
    }

    /// VAL-SCHED-001: a step respects max_items even when the process count
    /// measurement is taken per call (no overstepping within one step).
    #[test]
    fn test_work_budget_never_oversteps() {
        let budget = budget_max_items(7);
        // 23 items with max_items 7: 7,7,7,2 (last yields remain... completes).
        let mut job = CountingJob::new(23);
        assert_eq!(job.step(budget).unwrap(), Step::Yield(7));
        assert_eq!(job.step(budget).unwrap(), Step::Yield(14));
        assert_eq!(job.step(budget).unwrap(), Step::Yield(21));
        // Remaining 2 fit under the 7 budget -> complete.
        assert_eq!(job.step(budget).unwrap(), Step::Complete);
    }

    /// VAL-SCHED-002: Step Yield vs Complete semantics.
    ///
    /// A `Complete` return is an idempotent terminal state — a further `step`
    /// also returns `Complete` and does no work.
    #[test]
    fn test_step_yield_complete_semantics() {
        let mut job = CountingJob::new(10);
        let budget = budget_max_items(5);
        // 10 items / 5 per step: step 1 processes 5 and yields; step 2
        // processes the final 5 and completes.
        assert_eq!(job.step(budget).unwrap(), Step::Yield(5));
        assert_eq!(job.step(budget).unwrap(), Step::Complete);
        // Terminal idempotency: further steps also Complete without advancing.
        let processed_before = job.processed;
        assert_eq!(job.step(budget).unwrap(), Step::Complete);
        assert_eq!(job.processed, processed_before);
    }

    /// VAL-SCHED-002: complete must only be reached when all work is done.
    #[test]
    fn test_complete_only_when_all_done() {
        let mut job = CountingJob::new(5);
        let budget = budget_max_items(2);
        assert_eq!(job.step(budget).unwrap(), Step::Yield(2));
        assert_eq!(job.step(budget).unwrap(), Step::Yield(4));
        // The last item must complete this step, not yield spuriously.
        assert_eq!(job.step(budget).unwrap(), Step::Complete);
    }

    /// VAL-SCHED-003: estimated_next_bytes is non-zero while work remains and
    /// zero after completion.
    #[test]
    fn test_estimated_next_bytes() {
        let mut job = CountingJob::new(5);
        assert!(
            job.estimated_next_bytes() > 0,
            "non-complete job must estimate > 0"
        );
        let budget = WorkBudget::unlimited();
        loop {
            if job.step(budget).unwrap() == Step::Complete {
                break;
            }
        }
        assert_eq!(job.estimated_next_bytes(), 0, "complete job estimates 0");
    }

    /// VAL-SCHED-003: estimate never changes sign semantics as work advances —
    /// monotonically reflects remaining input.
    #[test]
    fn test_estimate_reflects_remaining() {
        let mut job = CountingJob::new(10);
        let first = job.estimated_next_bytes();
        job.step(budget_max_items(4)).unwrap();
        let second = job.estimated_next_bytes();
        assert!(second < first, "estimate shrinks as work advances");
    }
}
