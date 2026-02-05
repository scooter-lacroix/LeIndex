use std::{thread, time::Duration};

use anyhow::Result;

use crate::PhaseSelection;

use super::{
    context::OrchestrationContext,
    model::OrchestrationRequest,
    runner::{DefaultPhaseRunner, PhaseRunner},
    state::{OrchestrationState, RunStatus},
};

/// Output of an orchestration run.
#[derive(Debug, Clone)]
pub struct OrchestrationRunReport {
    /// Final state snapshot.
    pub state: OrchestrationState,
    /// Last successful formatted report if available.
    pub formatted_output: Option<String>,
}

/// Engine that coordinates retries and status tracking.
pub struct OrchestrationEngine<R: PhaseRunner = DefaultPhaseRunner> {
    /// Mutable lifecycle state.
    pub state: OrchestrationState,
    runner: R,
}

impl Default for OrchestrationEngine<DefaultPhaseRunner> {
    fn default() -> Self {
        Self::new(DefaultPhaseRunner)
    }
}

impl<R: PhaseRunner> OrchestrationEngine<R> {
    /// Create a new orchestration engine with a custom runner.
    pub fn new(runner: R) -> Self {
        Self {
            state: OrchestrationState::default(),
            runner,
        }
    }

    /// Execute one orchestration request with retries.
    pub fn run(
        &mut self,
        orchestration: OrchestrationContext,
        request: OrchestrationRequest,
    ) -> OrchestrationRunReport {
        if request.selection == PhaseSelection::All
            && self.state.status == RunStatus::Succeeded
            && self.state.completed_phases == vec![1, 2, 3, 4, 5]
        {
            return OrchestrationRunReport {
                state: self.state.clone(),
                formatted_output: None,
            };
        }

        self.state.status = RunStatus::Running;
        self.state.attempts = 0;
        self.state.last_error = None;
        self.state.completed_phases.clear();

        let start = std::time::Instant::now();
        let mut formatted_output = None;

        for attempt in 1..=orchestration.max_retries.max(1) {
            self.state.attempts = attempt;

            if let Some(timeout_ms) = orchestration.timeout_ms {
                if start.elapsed().as_millis() >= timeout_ms as u128 {
                    self.state.last_error =
                        Some(format!("timeout budget exceeded after {} ms", timeout_ms));
                    break;
                }
            }

            let mut options = orchestration.options.clone();
            if let Some(mode) = request.mode {
                options.mode = mode;
            }
            if let Some(path) = &request.path {
                options.root = path.clone();
            }

            let selection = request.selection;
            let outcome = self.runner.run(options, selection);

            match outcome {
                Ok(report) => {
                    self.state.status = RunStatus::Succeeded;
                    self.state.last_error = None;
                    self.state.completed_phases = report.executed_phases.clone();
                    formatted_output = Some(report.formatted_output);
                    return OrchestrationRunReport {
                        state: self.state.clone(),
                        formatted_output,
                    };
                }
                Err(err) => {
                    self.state.last_error = Some(err.to_string());
                    if attempt < orchestration.max_retries.max(1) {
                        thread::sleep(Duration::from_millis(orchestration.retry_delay_ms));
                    }
                }
            }
        }

        self.state.status = RunStatus::Failed;
        OrchestrationRunReport {
            state: self.state.clone(),
            formatted_output,
        }
    }

    /// Restore engine state from a previously persisted snapshot.
    ///
    /// This does not imply phase-level continuation semantics; it restores lifecycle metadata.
    pub fn restore_state_snapshot(&mut self, state: OrchestrationState) {
        self.state = state;
    }

    /// Backward-compatible alias for `restore_state_snapshot`.
    pub fn resume_from_state(&mut self, state: OrchestrationState) {
        self.restore_state_snapshot(state);
    }

    /// Convenience helper for direct single/all phase execution.
    pub fn run_selection(
        &mut self,
        options: crate::PhaseOptions,
        selection: PhaseSelection,
    ) -> Result<crate::PhaseAnalysisReport> {
        self.runner.run(options, selection)
    }
}

#[cfg(test)]
mod tests {
    use anyhow::{anyhow, Result};

    use crate::{
        format::FormatMode,
        options::PhaseOptions,
        orchestrate::{model::OrchestrationRequest, state::RunStatus},
        phase1::Phase1Summary,
        PhaseAnalysisReport, PhaseSelection,
    };

    use super::*;

    struct FlakyRunner {
        fail_attempts: usize,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl PhaseRunner for FlakyRunner {
        fn run(
            &self,
            _options: crate::PhaseOptions,
            _selection: PhaseSelection,
        ) -> Result<PhaseAnalysisReport> {
            let current = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

            if current <= self.fail_attempts {
                Err(anyhow!("transient error"))
            } else {
                Ok(PhaseAnalysisReport {
                    project_id: "test".to_string(),
                    generation: "gen".to_string(),
                    executed_phases: vec![1],
                    cache_hit: false,
                    changed_files: 0,
                    deleted_files: 0,
                    phase1: Some(Phase1Summary::default()),
                    phase2: None,
                    phase3: None,
                    phase4: None,
                    phase5: None,
                    formatted_output: "ok".to_string(),
                })
            }
        }
    }

    #[test]
    fn retries_until_success() {
        let runner = FlakyRunner {
            fail_attempts: 1,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let mut engine = OrchestrationEngine::new(runner);

        let mut options = PhaseOptions::default();
        options.mode = FormatMode::Ultra;

        let context = OrchestrationContext {
            options,
            max_retries: 3,
            retry_delay_ms: 1,
            timeout_ms: None,
        };

        let request = OrchestrationRequest::default();
        let report = engine.run(context, request);

        assert_eq!(report.state.status, RunStatus::Succeeded);
        assert_eq!(report.state.attempts, 2);
        assert_eq!(report.formatted_output.as_deref(), Some("ok"));
    }

    #[test]
    fn marks_failed_after_retry_budget() {
        let runner = FlakyRunner {
            fail_attempts: 3,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let mut engine = OrchestrationEngine::new(runner);
        let context = OrchestrationContext::new(PhaseOptions::default());

        let report = engine.run(context, OrchestrationRequest::default());
        assert_eq!(report.state.status, RunStatus::Failed);
    }

    struct TimeoutRunner;

    impl PhaseRunner for TimeoutRunner {
        fn run(
            &self,
            _options: crate::PhaseOptions,
            _selection: PhaseSelection,
        ) -> Result<PhaseAnalysisReport> {
            Err(anyhow!("timeout while awaiting analysis"))
        }
    }

    #[test]
    fn timeout_errors_are_retried_and_then_fail() {
        let runner = TimeoutRunner;
        let mut engine = OrchestrationEngine::new(runner);
        let context = OrchestrationContext {
            options: PhaseOptions::default(),
            max_retries: 2,
            retry_delay_ms: 1,
            timeout_ms: None,
        };

        let report = engine.run(context, OrchestrationRequest::default());
        assert_eq!(report.state.status, RunStatus::Failed);
        assert_eq!(report.state.attempts, 2);
        assert!(
            report
                .state
                .last_error
                .as_deref()
                .unwrap_or_default()
                .contains("timeout"),
            "expected timeout error details to be preserved"
        );
    }

    #[test]
    fn restore_state_snapshot_updates_engine() {
        let runner = FlakyRunner {
            fail_attempts: 0,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let mut engine = OrchestrationEngine::new(runner);

        let resumed = crate::orchestrate::state::OrchestrationState {
            status: RunStatus::Failed,
            attempts: 4,
            last_error: Some("previous timeout".to_string()),
            completed_phases: vec![1, 2],
        };

        engine.restore_state_snapshot(resumed.clone());
        assert_eq!(engine.state.status, RunStatus::Failed);
        assert_eq!(engine.state.attempts, 4);
        assert_eq!(engine.state.last_error, resumed.last_error);
        assert_eq!(engine.state.completed_phases, vec![1, 2]);
    }

    #[test]
    fn resume_from_state_alias_still_restores_state() {
        let runner = FlakyRunner {
            fail_attempts: 0,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let mut engine = OrchestrationEngine::new(runner);

        let resumed = crate::orchestrate::state::OrchestrationState {
            status: RunStatus::Succeeded,
            attempts: 1,
            last_error: None,
            completed_phases: vec![1],
        };

        engine.resume_from_state(resumed.clone());
        assert_eq!(engine.state.status, RunStatus::Succeeded);
        assert_eq!(engine.state.completed_phases, vec![1]);
    }

    struct SlowRunner;

    impl PhaseRunner for SlowRunner {
        fn run(
            &self,
            _options: crate::PhaseOptions,
            _selection: PhaseSelection,
        ) -> Result<PhaseAnalysisReport> {
            std::thread::sleep(std::time::Duration::from_millis(5));
            Err(anyhow!("slow error"))
        }
    }

    #[test]
    fn wallclock_timeout_budget_stops_retries() {
        let runner = SlowRunner;
        let mut engine = OrchestrationEngine::new(runner);

        let context = OrchestrationContext {
            options: PhaseOptions::default(),
            max_retries: 5,
            retry_delay_ms: 1,
            timeout_ms: Some(1),
        };

        let report = engine.run(context, OrchestrationRequest::default());
        assert_eq!(report.state.status, RunStatus::Failed);
        assert!(
            report
                .state
                .last_error
                .as_deref()
                .unwrap_or_default()
                .contains("timeout budget exceeded")
                || report
                    .state
                    .last_error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("slow error")
        );
    }

    #[test]
    fn already_completed_all_phases_short_circuits() {
        let runner = FlakyRunner {
            fail_attempts: 0,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let mut engine = OrchestrationEngine::new(runner);
        engine.state = crate::orchestrate::state::OrchestrationState {
            status: RunStatus::Succeeded,
            attempts: 1,
            last_error: None,
            completed_phases: vec![1, 2, 3, 4, 5],
        };

        let report = engine.run(
            OrchestrationContext::new(PhaseOptions::default()),
            OrchestrationRequest::default(),
        );

        assert_eq!(report.state.status, RunStatus::Succeeded);
        assert!(report.formatted_output.is_none());
    }

    #[test]
    fn timeout_budget_zero_fails_before_attempt_execution() {
        let runner = FlakyRunner {
            fail_attempts: 0,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let mut engine = OrchestrationEngine::new(runner);

        let report = engine.run(
            OrchestrationContext {
                options: PhaseOptions::default(),
                max_retries: 3,
                retry_delay_ms: 1,
                timeout_ms: Some(0),
            },
            OrchestrationRequest::default(),
        );

        assert_eq!(report.state.status, RunStatus::Failed);
        assert!(report
            .state
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("timeout budget exceeded"));
    }

    #[test]
    fn run_selection_executes_runner_path() {
        let runner = FlakyRunner {
            fail_attempts: 0,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let mut engine = OrchestrationEngine::new(runner);

        let report = engine
            .run_selection(PhaseOptions::default(), PhaseSelection::Single(1))
            .expect("selection report");
        assert_eq!(report.executed_phases, vec![1]);
    }
}
