use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Run status for orchestration lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RunStatus {
    /// Run has not started.
    Idle,
    /// Run currently in progress.
    Running,
    /// Run finished successfully.
    Succeeded,
    /// Run failed after retries.
    Failed,
}

/// Mutable orchestration state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationState {
    /// Current status.
    pub status: RunStatus,
    /// Number of attempts used by the latest run.
    pub attempts: usize,
    /// Optional terminal error message.
    pub last_error: Option<String>,
    /// Phases completed by the last successful run.
    pub completed_phases: Vec<u8>,
}

impl Default for OrchestrationState {
    fn default() -> Self {
        Self {
            status: RunStatus::Idle,
            attempts: 0,
            last_error: None,
            completed_phases: Vec::new(),
        }
    }
}

impl OrchestrationState {
    /// Persist state to JSON file.
    pub fn save_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    /// Load state from JSON file.
    pub fn load_from_path(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn state_roundtrip_save_and_load() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("state.json");

        let state = OrchestrationState {
            status: RunStatus::Failed,
            attempts: 3,
            last_error: Some("timeout".to_string()),
            completed_phases: vec![1, 2],
        };

        state.save_to_path(&file).expect("save state");
        let loaded = OrchestrationState::load_from_path(&file).expect("load state");

        assert_eq!(loaded.status, RunStatus::Failed);
        assert_eq!(loaded.attempts, 3);
        assert_eq!(loaded.last_error.as_deref(), Some("timeout"));
        assert_eq!(loaded.completed_phases, vec![1, 2]);
    }

    #[test]
    fn loading_missing_state_returns_error() {
        let dir = tempdir().expect("tempdir");
        let missing = dir.path().join("missing.json");

        let err = OrchestrationState::load_from_path(&missing)
            .err()
            .expect("must fail");
        assert!(err.to_string().contains("No such file") || err.to_string().contains("os error"));
    }
}
