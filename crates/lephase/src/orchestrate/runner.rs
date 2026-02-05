use anyhow::Result;

use crate::{run_phase_analysis, PhaseAnalysisReport, PhaseOptions, PhaseSelection};

/// Abstraction for phase-analysis execution, enabling test doubles.
pub trait PhaseRunner {
    /// Execute one run.
    fn run(&self, options: PhaseOptions, selection: PhaseSelection) -> Result<PhaseAnalysisReport>;
}

/// Default runner backed by `run_phase_analysis`.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultPhaseRunner;

impl PhaseRunner for DefaultPhaseRunner {
    fn run(&self, options: PhaseOptions, selection: PhaseSelection) -> Result<PhaseAnalysisReport> {
        run_phase_analysis(options, selection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn default_runner_executes_phase_analysis() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn run()->bool{true}\n").expect("write");

        let runner = DefaultPhaseRunner;
        let report = runner
            .run(
                PhaseOptions {
                    root: dir.path().to_path_buf(),
                    ..PhaseOptions::default()
                },
                PhaseSelection::Single(1),
            )
            .expect("runner report");

        assert_eq!(report.executed_phases, vec![1]);
    }
}
