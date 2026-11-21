//! Metrics: per-call records, per-mode token shares, task rollups, gate math.
//!
//! One [`Call`] per tool invocation. Tasks roll calls up into [`TaskMetrics`]
//! with the per-mode split (read/find/edit/git/run) the brief §4.6 asks for;
//! the gate (brief §7) requires ≥40% fewer tokens, ≤ round trips and equal
//! success per task.

use crate::leindex_arm::UNSUPPORTED_IN_THIS_BUILD;
use serde::Serialize;
use std::collections::BTreeMap;

/// Which arm produced a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Arm {
    /// Claude Code native tools, emulated byte-faithfully.
    Native,
    /// LeIndex MCP handlers, in-process, model-visible text.
    LeIndex,
}

impl Arm {
    /// Column label.
    pub fn label(self) -> &'static str {
        match self {
            Arm::Native => "NATIVE",
            Arm::LeIndex => "LEINDEX",
        }
    }
}

/// The tool mode of a call (brief §4.6 per-mode token share).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Read / read_file.
    Read,
    /// Grep / find / search.
    Find,
    /// Edit / edit_preview + edit_apply.
    Edit,
    /// git status/diff/log/show/add/commit.
    Git,
    /// Test/build output reads (brief §4.4 `run`).
    Run,
}

impl Mode {
    /// Key used for per-mode tables.
    pub fn key(self) -> &'static str {
        match self {
            Mode::Read => "read",
            Mode::Find => "find",
            Mode::Edit => "edit",
            Mode::Git => "git",
            Mode::Run => "run",
        }
    }
}

/// One tool call's measured record.
#[derive(Debug, Clone, Serialize)]
pub struct Call {
    /// Mode bucket.
    pub mode: Mode,
    /// Tool name the agent invoked (`Read`, `Grep`, `leindex_find`, …).
    pub tool: String,
    /// Measured output tokens (real tokenizer or labelled chars/4).
    pub tokens: usize,
    /// Output chars, kept so chars/4 numbers stay reproducible.
    pub chars: usize,
}

/// Per-task rollup for one arm.
#[derive(Debug, Clone, Serialize)]
pub struct TaskMetrics {
    /// Task number (1-7).
    pub task: usize,
    /// Total output tokens across the task's calls.
    pub tokens: usize,
    /// Round trips (tool calls) — the session-cost driver of brief §2.
    pub round_trips: usize,
    /// Per-mode token shares.
    pub by_mode: BTreeMap<String, usize>,
    /// Scripted correctness assertion result.
    pub success: bool,
    /// Why success is false, or why the task could not run in this build.
    pub note: String,
}

impl TaskMetrics {
    /// Empty rollup for a task.
    pub fn new(task: usize) -> Self {
        Self {
            task,
            tokens: 0,
            round_trips: 0,
            by_mode: BTreeMap::new(),
            success: false,
            note: String::new(),
        }
    }

    /// Record one call into the rollup.
    pub fn record(&mut self, call: &Call) {
        self.tokens += call.tokens;
        self.round_trips += 1;
        *self
            .by_mode
            .entry(call.mode.key().to_string())
            .or_default() += call.tokens;
    }

    /// Mark the task unsupported in this build (loud, never faked). The
    /// marker names the missing cross-lane feature so the report says WHY.
    pub fn unsupported(task: usize, missing_feature: &str) -> Self {
        let mut metrics = Self::new(task);
        metrics.note = format!("{UNSUPPORTED_IN_THIS_BUILD} (missing: {missing_feature})");
        metrics
    }
}

/// Reduction vs native, in percent (100.0 when native is 0).
pub fn token_reduction_pct(native: usize, leindex: usize) -> f64 {
    if native == 0 {
        return if leindex == 0 { 100.0 } else { 0.0 };
    }
    100.0 * (native.saturating_sub(leindex)) as f64 / native as f64
}

/// The gate verdict for one task: ≥40% fewer tokens, ≤ round trips, equal
/// success. Unsupported tasks always fail the gate loudly.
pub fn task_passes_gate(native: &TaskMetrics, leindex: &TaskMetrics) -> bool {
    // A task the LEINDEX arm could not run fails the gate; the note says why.
    if leindex.note.contains(UNSUPPORTED_IN_THIS_BUILD) {
        return false;
    }
    // Equal-or-better success: both scripted assertions must hold.
    if !native.success || !leindex.success {
        return false;
    }
    // No more round trips.
    if leindex.round_trips > native.round_trips {
        return false;
    }
    // At least 40% fewer tool-output tokens.
    token_reduction_pct(native.tokens, leindex.tokens) >= 40.0
}
