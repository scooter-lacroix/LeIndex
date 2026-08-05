//! Global admission decisions (§5.3).
//!
//! The scheduler gates heavy work (index chunks, maintenance) through an
//! admission decision before dequeueing it. The invariant enforced here — and
//! by the full `AdmissionController` that builds on this module — is that
//! admission NEVER errors: it returns exactly one of [`Admission::Admit`],
//! [`Admission::Defer`], or [`Admission::Reduce`]. A memory cap must prevent
//! overlapping peaks, not convert valid work into errors (spec §5.3,
//! anti-cheat §2.1 #10).

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

#[cfg(test)]
mod test {
    use super::*;

    /// VAL-SCHED-008 (foundation): the admission outcome is one of exactly
    /// Admit / Defer / Reduce, never an error variant. Every input to the in-
    /// memory policies produces a valid outcome.
    #[test]
    fn test_admission_enum_never_errors() {
        for estimate in [0usize, 1, 100, usize::MAX] {
            let decision = defer_above(50)(estimate);
            match decision {
                Admission::Admit | Admission::Defer => {}
                Admission::Reduce { .. } => {}
            }
        }
        let decision = always_admit()(99);
        assert_eq!(decision, Admission::Admit);
    }

    /// The defer-above policy yields Defer above its threshold and Admit below
    /// (used by the scheduler's admission-gated dequeue test).
    #[test]
    fn test_defer_above_policy() {
        let decider = defer_above(64);
        assert_eq!(decider(10), Admission::Admit);
        assert_eq!(decider(1000), Admission::Defer);
    }

    /// BatchShape reduction halves the item ceiling and has an empty floor.
    #[test]
    fn test_batch_shape_reduction() {
        assert_eq!(BatchShape::half_of(100).max_items, 50);
        assert_eq!(BatchShape::empty().max_items, 0);
    }
}
