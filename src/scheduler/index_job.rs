//! Stepped, resumable indexing job (WS5 Task 5).
//!
//! Turns the indexing pipeline into a [`BoundedJob`]. Each `step()` runs one
//! *phase chunk* (scan / parse / PDG / lexical / publish-core / neural /
//! publish-enhanced) and returns `Yield`, so the caller (the scheduler or a
//! direct run-loop) controls interleaving and can check the admission gate
//! between chunks. A checkpoint marker is written at every yield point — a
//! phase only writes its marker after it has fully completed — so if the
//! process is killed mid-step, the next run's `resume` path skips every phase
//! whose marker exists and re-runs only the interrupted phase onward (spec
//! §11.2, §5.2 "cancellation at yield" = checkpoint boundaries).
//!
//! The heavy lifting for each phase is delegated to a [`PhaseExecutor`] (in the
//! real daemon this is the indexer's `run_scan`/`run_parse`/`run_pdg`/
//! `run_lexical`/`publish_generation`/`run_neural`; the MCP server wires that
//! adapter and owns `&mut self` across steps). The rollout is feature-flagged:
//! with `LEINDEX_FEATURE_BOUNDED_SCHEDULER` OFF the legacy `index_project_inner`
//! runs unstepped exactly as before.

use std::path::{Path, PathBuf};

use crate::scheduler::budget::{BoundedJob, Step, WorkBudget};

/// The indexing pipeline phases, in dependency order. Each is one steppable
/// chunk with a checkpoint boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IndexPhaseId {
    /// Rescan the project tree and diff against the previous snapshot.
    Scan,
    /// Parse changed/new source files into ASTs.
    Parse,
    /// Merge parsed files into the PDG.
    Pdg,
    /// Build the lexical/TF-IDF index over the PDG.
    Lexical,
    /// Publish the core generation (db + tfidf + pdg + symbols) to CAS.
    PublishCore,
    /// Run the neural embedding pass over new/changed documents.
    Neural,
    /// Publish the enhanced generation with the neural layer to CAS.
    PublishEnhanced,
    /// Terminal marker — indexing is complete; no more chunks.
    Complete,
}

impl IndexPhaseId {
    /// The chunk that runs after `self`.
    pub fn next(self) -> Self {
        match self {
            IndexPhaseId::Scan => IndexPhaseId::Parse,
            IndexPhaseId::Parse => IndexPhaseId::Pdg,
            IndexPhaseId::Pdg => IndexPhaseId::Lexical,
            IndexPhaseId::Lexical => IndexPhaseId::PublishCore,
            IndexPhaseId::PublishCore => IndexPhaseId::Neural,
            IndexPhaseId::Neural => IndexPhaseId::PublishEnhanced,
            IndexPhaseId::PublishEnhanced | IndexPhaseId::Complete => IndexPhaseId::Complete,
        }
    }

    /// Marker file name for this phase's checkpoint.
    pub fn marker_name(self) -> String {
        format!("{}.done", self.as_str())
    }

    /// Stable identifier used in the marker filename.
    pub fn as_str(self) -> &'static str {
        match self {
            IndexPhaseId::Scan => "scan",
            IndexPhaseId::Parse => "parse",
            IndexPhaseId::Pdg => "pdg",
            IndexPhaseId::Lexical => "lexical",
            IndexPhaseId::PublishCore => "publish-core",
            IndexPhaseId::Neural => "neural",
            IndexPhaseId::PublishEnhanced => "publish-enhanced",
            IndexPhaseId::Complete => "complete",
        }
    }
}

/// Progress reported by an [`IndexJob`] at a yield point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexJobProgress {
    /// The phase that just completed and produced this yield.
    pub completed_phase: IndexPhaseId,
    /// The next phase that will run on the following `step()`.
    pub next_phase: IndexPhaseId,
    /// Cumulative items processed so far (sum of each completed phase's count).
    pub items_done: usize,
    /// The checkpoint marker path written for the completed phase.
    pub checkpoint: PathBuf,
}

/// Executes a single indexing phase chunk. Implemented by the daemon on behalf
/// of the real indexer so each phase can be stepped and resumed independently.
pub trait PhaseExecutor {
    /// Error type surfaced from a phase run.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Run the entire `phase` chunk and return how many units of work it
    /// processed (files, parsed units, edges, vectors — semantics per phase).
    fn run_phase(&mut self, phase: IndexPhaseId) -> Result<usize, Self::Error>;

    /// Project the byte footprint the next `step()` would consume, so the
    /// admission gate can decide Admit/Defer/Reduce before dequeueing an
    /// IndexChunk.
    fn estimated_next_bytes(&self, phase: IndexPhaseId) -> usize;
}

/// A steppable wrapper over the indexing pipeline.
pub struct IndexJob<E: PhaseExecutor> {
    executor: E,
    phase: IndexPhaseId,
    checkpoint_dir: PathBuf,
    items_done: usize,
    done: bool,
}

impl<E: PhaseExecutor> IndexJob<E> {
    /// Create an indexing job. When `force` is false and a prior run left
    /// checkpoint markers, `step()` resumes at the first un-marked phase — a
    /// process killed mid-step restarts from the last completed checkpoint
    /// (VAL-SCHED-012). When `force` is true, all phases re-run regardless.
    pub fn new(executor: E, checkpoint_dir: impl Into<PathBuf>, force: bool) -> Self {
        let checkpoint_dir = checkpoint_dir.into();
        let phase = if force {
            IndexPhaseId::Scan
        } else {
            Self::first_incomplete(&checkpoint_dir)
        };
        Self {
            executor,
            phase,
            checkpoint_dir,
            items_done: 0,
            done: false,
        }
    }

    /// The next phase that will run (diagnostics / admission).
    pub fn current_phase(&self) -> IndexPhaseId {
        self.phase
    }

    /// Path where the next phase's checkpoint marker will be written.
    pub fn next_checkpoint(&self) -> PathBuf {
        self.checkpoint_dir.join(self.phase.marker_name())
    }

    fn write_checkpoint(&mut self, completed: IndexPhaseId) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.checkpoint_dir)?;
        let marker = self.checkpoint_dir.join(completed.marker_name());
        std::fs::write(&marker, b"")?;
        self.phase = completed.next();
        Ok(())
    }

    /// Rewind to the most recent completed marker so a caller can force re-run
    /// from a given phase (useful when a later chunk was externally invalidated).
    pub fn rewind_to(&mut self, phase: IndexPhaseId) {
        self.phase = phase;
        self.done = false;
    }
}

impl<E: PhaseExecutor> IndexJob<E> {
    /// The first phase whose checkpoint marker is absent, given existing markers.
    fn first_incomplete(dir: &Path) -> IndexPhaseId {
        let order = [
            IndexPhaseId::Scan,
            IndexPhaseId::Parse,
            IndexPhaseId::Pdg,
            IndexPhaseId::Lexical,
            IndexPhaseId::PublishCore,
            IndexPhaseId::Neural,
            IndexPhaseId::PublishEnhanced,
        ];
        for phase in order {
            if !dir.join(phase.marker_name()).exists() {
                return phase;
            }
        }
        // Everything marked: resume logically starts at the terminal state.
        IndexPhaseId::Complete
    }
}

impl<E: PhaseExecutor + Send> BoundedJob for IndexJob<E> {
    type Progress = IndexJobProgress;

    fn step(&mut self, _budget: WorkBudget) -> anyhow::Result<Step<Self::Progress>> {
        if self.done || self.phase == IndexPhaseId::Complete {
            self.done = true;
            return Ok(Step::Complete);
        }
        let phase = self.phase;
        let items = self
            .executor
            .run_phase(phase)
            .map_err(|e| anyhow::anyhow!("index phase {} failed: {e}", phase.as_str()))?;
        self.items_done += items;
        // Checkpoint aligned with the yield point: the marker is written only
        // after the phase fully completes, so a mid-step kill at most re-runs
        // this one chunk on resume.
        self.write_checkpoint(phase)?;
        let next = phase.next();
        if next == IndexPhaseId::Complete {
            self.done = true;
            Ok(Step::Complete)
        } else {
            Ok(Step::Yield(IndexJobProgress {
                completed_phase: phase,
                next_phase: next,
                items_done: self.items_done,
                checkpoint: self.checkpoint_dir.join(phase.marker_name()),
            }))
        }
    }

    fn estimated_next_bytes(&self) -> usize {
        self.executor.estimated_next_bytes(self.phase)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::collections::{BTreeSet, HashMap};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn max_budget() -> WorkBudget {
        WorkBudget::new(
            usize::MAX,
            usize::MAX,
            usize::MAX,
            Duration::from_secs(3600),
        )
    }

    /// Records which phases have run. Reusing the same record across jobs lets
    /// us assert that resume does not re-execute already-checkpointed phases.
    #[derive(Default)]
    struct Recorder {
        ran: Mutex<Vec<IndexPhaseId>>,
    }

    struct FakeExecutor {
        recorder: Arc<Recorder>,
    }

    impl PhaseExecutor for FakeExecutor {
        type Error = std::io::Error;

        fn run_phase(&mut self, phase: IndexPhaseId) -> Result<usize, Self::Error> {
            self.recorder.ran.lock().unwrap().push(phase);
            Ok(7) // arbitrary units per chunk
        }

        fn estimated_next_bytes(&self, _phase: IndexPhaseId) -> usize {
            4096
        }
    }

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "leindex-indexjob-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// VAL-SCHED-002 + Task 5: one `step()` runs exactly one phase chunk,
    /// yields between chunks, writes a checkpoint at the yield, and `step()`
    /// after the final chunk returns `Complete`.
    #[test]
    fn test_step_progresses_through_phases_one_chunk_at_a_time() {
        let dir = tempdir("step-progress");
        let recorder = Arc::new(Recorder::default());
        let mut job = IndexJob::new(
            FakeExecutor {
                recorder: recorder.clone(),
            },
            dir.clone(),
            true,
        );

        let mut seen: Vec<IndexPhaseId> = Vec::new();
        let mut total_items = 0;
        while let Step::Yield(progress) = job.step(max_budget()).unwrap() {
            seen.push(progress.completed_phase);
            total_items = progress.items_done;
            // Checkpoint marker for the completed phase exists at the
            // yield point.
            assert!(
                progress.checkpoint.exists(),
                "checkpoint must exist at yield"
            );
        }
        // Six phases yield one-per-step; the final phase (PublishEnhanced)
        // completes in the seventh step, which returns `Complete` (yield on
        // all-but-final is the contract, spec §5.2/§11.2).
        assert_eq!(
            seen,
            vec![
                IndexPhaseId::Scan,
                IndexPhaseId::Parse,
                IndexPhaseId::Pdg,
                IndexPhaseId::Lexical,
                IndexPhaseId::PublishCore,
                IndexPhaseId::Neural,
            ]
        );
        assert_eq!(seen.len(), 6, "one yield per non-final chunk");
        assert_eq!(total_items, 42, "6 yielded phases x 7 units");
        assert_eq!(
            recorder.ran.lock().unwrap().len(),
            7,
            "final phase runs in the Complete step"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// spec §11.2 / VAL-SCHED-012: a job killed mid-step resumes at the last
    /// checkpoint — completed phases are not re-executed; the next run starts
    /// at the interrupted phase.
    #[test]
    fn test_killed_mid_step_resumes_at_last_checkpoint() {
        let dir = tempdir("resume");
        let recorder = Arc::new(Recorder::default());
        {
            // First run: step through Scan + Parse, then simulate a kill
            // (the job is dropped mid-way, having checkpointed Scan and Parse).
            let mut job = IndexJob::new(
                FakeExecutor {
                    recorder: recorder.clone(),
                },
                dir.clone(),
                false,
            );
            assert_eq!(
                job.step(max_budget()).unwrap(),
                Step::Yield(IndexJobProgress {
                    completed_phase: IndexPhaseId::Scan,
                    next_phase: IndexPhaseId::Parse,
                    items_done: 7,
                    checkpoint: dir.join("scan.done"),
                })
            );
            // Kill after aborting during the Parse chunk's checkpoint write is
            // impossible at this boundary; instead drop after Scan+Parse both
            // yielded, mirroring a kill between parse and pdg.
            assert!(matches!(job.step(max_budget()).unwrap(), Step::Yield(..)));
            // (job dropped here — the "kill")
        }

        // Resume: a fresh job over the same checkpoint dir must start at Pdg.
        let mut resume = IndexJob::new(
            FakeExecutor {
                recorder: recorder.clone(),
            },
            dir.clone(),
            false,
        );
        assert_eq!(
            resume.current_phase(),
            IndexPhaseId::Pdg,
            "resume starts at the phase after the last checkpoint"
        );
        let mut resumed_phases = Vec::new();
        while let Step::Yield(p) = resume.step(max_budget()).unwrap() {
            resumed_phases.push(p.completed_phase);
        }
        assert_eq!(
            resumed_phases,
            vec![
                IndexPhaseId::Pdg,
                IndexPhaseId::Lexical,
                IndexPhaseId::PublishCore,
                IndexPhaseId::Neural,
            ],
            "only the un-checkpointed phases run on resume"
        );
        // Each phase executed at most once across kill + resume: the recorder
        // saw scan/parse in run 1 and pdg/lexical/publish-core/neural/
        // publish-enhanced in run 2 — exactly seven unique entries.
        {
            let r = recorder.ran.lock().unwrap();
            let unique: BTreeSet<_> = r.iter().copied().collect();
            assert_eq!(r.len(), 7, "no phase re-executed after resume");
            assert_eq!(unique.len(), 7, "every phase ran exactly once");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--force` rebuild ignores existing checkpoints and re-runs every phase.
    #[test]
    fn test_force_ignores_checkpoints() {
        let dir = tempdir("force");
        let recorder = Arc::new(Recorder::default());
        {
            let mut job = IndexJob::new(
                FakeExecutor {
                    recorder: recorder.clone(),
                },
                dir.clone(),
                false,
            );
            // Complete a full pass so all markers exist.
            while let Step::Yield(_) = job.step(max_budget()).unwrap() {}
        }
        let fresh: Vec<IndexPhaseId> = recorder.ran.lock().unwrap().iter().copied().collect();
        assert_eq!(fresh.len(), 7);

        // Forced rebuild ignores the markers and starts from Scan again.
        recorder.ran.lock().unwrap().clear();
        let mut force = IndexJob::new(
            FakeExecutor {
                recorder: recorder.clone(),
            },
            dir.clone(),
            true,
        );
        assert_eq!(force.current_phase(), IndexPhaseId::Scan);
        while let Step::Yield(_) = force.step(max_budget()).unwrap() {}
        assert_eq!(
            recorder.ran.lock().unwrap().len(),
            7,
            "force re-runs every phase"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The `estimated_next_bytes` delegation drives accompaniment admission
    /// sizing (VAL-SCHED-003).
    #[test]
    fn test_estimated_next_bytes_delegates() {
        let dir = tempdir("estimate");
        let job = IndexJob::new(
            FakeExecutor {
                recorder: Arc::default(),
            },
            dir.clone(),
            false,
        );
        assert_eq!(job.estimated_next_bytes(), 4096);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `IndexPhaseId` marker names are unique and map one-to-one to phases.
    #[test]
    fn test_phase_ids_unique_markers() {
        let mut names = HashMap::new();
        for p in [
            IndexPhaseId::Scan,
            IndexPhaseId::Parse,
            IndexPhaseId::Pdg,
            IndexPhaseId::Lexical,
            IndexPhaseId::PublishCore,
            IndexPhaseId::Neural,
            IndexPhaseId::PublishEnhanced,
        ] {
            names.insert(p.marker_name(), p);
        }
        assert_eq!(names.len(), 7, "marker names must be unique");
    }
}
