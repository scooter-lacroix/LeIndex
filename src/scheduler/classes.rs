//! Work classes and DRR weights (§5.1).
//!
//! Every schedulable operation belongs to exactly one [`WorkClass`]. The class
//! determines the base deficit-round-robin (DRR) weight: interactive reads get
//! the highest weight so query latency is protected from indexing and
//! maintenance, per the priority invariant in architecture §3.4.

use std::collections::HashMap;

/// The five work classes from spec §5.1.
///
/// Ordering is significant: reads rank above indexing, and indexing above
/// background maintenance. The variant order mirrors the weight ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkClass {
    /// Interactive read: search / symbol lookup / file summary issued by a
    /// client that is waiting on the result.
    InteractiveRead,
    /// Interactive compute: heavier read-side computation (deep analyze,
    /// cross-language resolution) still waiting on a response.
    InteractiveCompute,
    /// Mutation: write-path operations other than chunked indexing
    /// (metadata writes, cleanup-triggered writes, project settings).
    Mutation,
    /// IndexChunk: one bounded chunk of an indexing job.
    IndexChunk,
    /// Maintenance: background work with no interactive waiter (GC, retention,
    /// compaction, cache eviction).
    Maintenance,
}

/// DRR base weights for each class.
///
/// Invariant (VAL-SCHED-004): `InteractiveRead >= InteractiveCompute >=
/// Mutation >= IndexChunk >= Maintenance`, with at least one strict
/// inequality. Higher weight = larger quantum per round, so reads dominate the
/// scheduler while indexing and maintenance proceed at a reduced rate.
const WEIGHTS: [(WorkClass, u64); 5] = [
    (WorkClass::InteractiveRead, 100),
    (WorkClass::InteractiveCompute, 80),
    (WorkClass::Mutation, 60),
    (WorkClass::IndexChunk, 40),
    (WorkClass::Maintenance, 20),
];

impl WorkClass {
    /// The base DRR weight for this class.
    pub fn weight(self) -> u64 {
        WEIGHTS
            .iter()
            .find_map(|(class, weight)| (*class == self).then_some(*weight))
            .expect("all work classes have weights")
    }

    /// Ordering rank used to break ties (lower = higher priority).
    pub fn rank(self) -> u8 {
        match self {
            WorkClass::InteractiveRead => 0,
            WorkClass::InteractiveCompute => 1,
            WorkClass::Mutation => 2,
            WorkClass::IndexChunk => 3,
            WorkClass::Maintenance => 4,
        }
    }
}

/// Aging policy for starved work classes (§5.2).
///
/// If a class has not been serviced for `threshold_ticks` scheduler ticks, its
/// effective DRR weight is boosted by `boost` so it accumulates deficit faster
/// and is eventually selected — no class may starve indefinitely under load
/// from higher-weight classes.
#[derive(Debug, Clone)]
pub struct ClassAging {
    /// Number of ticks without service before a class is considered starved.
    threshold_ticks: u64,
    /// Weight added to a starved class's effective weight.
    boost: u64,
    /// Tick counter (advanced once per scheduler round).
    tick: u64,
    /// Last tick each class was serviced.
    last_served: HashMap<WorkClass, u64>,
    /// First tick each class became eligible (had work queued). Starvation is
    /// measured from eligibility, so a freshly-queued class is not instantly
    /// treated as starved.
    eligible_tick: HashMap<WorkClass, u64>,
}

impl Default for ClassAging {
    fn default() -> Self {
        Self::new(100, 200)
    }
}

impl ClassAging {
    /// Create an ager with the given starvation threshold and weight boost.
    pub fn new(threshold_ticks: u64, boost: u64) -> Self {
        Self {
            threshold_ticks,
            boost,
            tick: 0,
            last_served: HashMap::new(),
            eligible_tick: HashMap::new(),
        }
    }

    /// Advance the scheduler clock by one tick.
    pub fn advance(&mut self) {
        self.tick += 1;
    }

    /// Record that `class` became eligible (had work queued) at the current
    /// tick. A no-op if the class was already eligible.
    pub fn note_eligible(&mut self, class: WorkClass) {
        self.eligible_tick.entry(class).or_insert(self.tick);
    }

    /// Record that `class` was serviced on the current tick.
    pub fn note_served(&mut self, class: WorkClass) {
        self.last_served.insert(class, self.tick);
    }

    /// Number of ticks since `class` was last serviced (measured from the
    /// later of its eligibility or last service).
    pub fn ticks_since_served(&self, class: WorkClass) -> u64 {
        let baseline = self
            .last_served
            .get(&class)
            .copied()
            .or_else(|| self.eligible_tick.get(&class).copied())
            .unwrap_or(self.tick);
        self.tick.saturating_sub(baseline)
    }

    /// Whether `class` is currently considered starved (over the threshold).
    pub fn is_starved(&self, class: WorkClass) -> bool {
        self.ticks_since_served(class) >= self.threshold_ticks
    }

    /// The effective weight for `class`: base weight plus the aging boost if
    /// the class is starved.
    pub fn effective_weight(&self, class: WorkClass) -> u64 {
        if self.is_starved(class) {
            class.weight() + self.boost
        } else {
            class.weight()
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// VAL-SCHED-004: work class weights are ordered read > compute > mutation
    /// > index > maintenance, with at least one strict inequality.
    #[test]
    fn test_work_class_weight_ordering() {
        let read = WorkClass::InteractiveRead.weight();
        let compute = WorkClass::InteractiveCompute.weight();
        let mutation = WorkClass::Mutation.weight();
        let index = WorkClass::IndexChunk.weight();
        let maintenance = WorkClass::Maintenance.weight();
        assert!(
            read >= compute && compute >= mutation && mutation >= index && index >= maintenance,
            "weight ordering violated: {read} >= {compute} >= {mutation} >= {index} >= {maintenance}"
        );
        // At least one strict inequality (weights are not all equal).
        assert!(
            read > maintenance,
            "weights must not all be equal (read={read}, maintenance={maintenance})"
        );
        // Documented values.
        assert_eq!(
            (read, compute, mutation, index, maintenance),
            (100, 80, 60, 40, 20)
        );
    }

    /// VAL-SCHED-004: all five classes are distinct and ordered.
    #[test]
    fn test_work_class_enum_has_five_distinct_classes() {
        let all = [
            WorkClass::InteractiveRead,
            WorkClass::InteractiveCompute,
            WorkClass::Mutation,
            WorkClass::IndexChunk,
            WorkClass::Maintenance,
        ];
        assert_eq!(all.len(), 5);
        let mut sorted = all;
        sorted.sort();
        assert_eq!(sorted, all, "ordering matches priority ranking");
    }

    /// VAL-SCHED-007 (unit half): aging boosts a starved class's effective
    /// weight past its base.
    #[test]
    fn test_aging_boosts_starved_class_weight() {
        let mut ager = ClassAging::new(10, 100);
        assert_eq!(ager.effective_weight(WorkClass::Maintenance), 20);
        // Maintenance becomes eligible (work queued) at tick 0.
        ager.note_eligible(WorkClass::Maintenance);
        // Service InteractiveRead every tick; Maintenance is never served.
        for _ in 0..10 {
            ager.advance();
            ager.note_served(WorkClass::InteractiveRead);
        }
        assert!(ager.is_starved(WorkClass::Maintenance));
        // Starved maintenance now outranks even reads: 20 + 100 = 120 > 100.
        assert!(ager.effective_weight(WorkClass::Maintenance) > 100);
    }

    /// VAL-SCHED-007: servicing a class clears its starved state.
    #[test]
    fn test_aging_resets_on_service() {
        let mut ager = ClassAging::new(5, 100);
        ager.note_eligible(WorkClass::IndexChunk);
        for _ in 0..5 {
            ager.advance();
        }
        assert!(ager.is_starved(WorkClass::IndexChunk));
        ager.note_served(WorkClass::IndexChunk);
        assert!(!ager.is_starved(WorkClass::IndexChunk));
        assert_eq!(ager.effective_weight(WorkClass::IndexChunk), 40);
    }
}
