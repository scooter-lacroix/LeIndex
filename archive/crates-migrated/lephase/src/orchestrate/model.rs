use crate::{FormatMode, PhaseSelection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Parsed orchestration request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationRequest {
    /// Which phase(s) to execute.
    pub selection: PhaseSelection,
    /// Optional output mode override.
    pub mode: Option<FormatMode>,
    /// Optional root override.
    pub path: Option<PathBuf>,
}

impl Default for OrchestrationRequest {
    fn default() -> Self {
        Self {
            selection: PhaseSelection::All,
            mode: None,
            path: None,
        }
    }
}
