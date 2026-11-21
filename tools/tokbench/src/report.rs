//! Report rendering: the markdown table for stdout, the JSON report file,
//! and the gate verdict lines (brief §4.6 / §7 output contract).

use crate::metrics::{task_passes_gate, token_reduction_pct};
use crate::tasks::CorpusRun;
use crate::tokens::Counter;
use serde_json::json;

/// Aggregate totals across tasks: (native tokens, leindex tokens, native
/// round trips, leindex round trips).
pub fn totals(run: &CorpusRun) -> (usize, usize, usize, usize) {
    let mut t = (0usize, 0usize, 0usize, 0usize);
    for (native, leindex) in &run.tasks {
        t.0 += native.tokens;
        t.1 += leindex.tokens;
        t.2 += native.round_trips;
        t.3 += leindex.round_trips;
    }
    t
}

/// Sum one mode across tasks for one arm (true = native).
pub fn mode_total(run: &CorpusRun, native_arm: bool, mode: &str) -> usize {
    run.tasks
        .iter()
        .map(|(n, l)| {
            let m = if native_arm { n } else { l };
            m.by_mode.get(mode).copied().unwrap_or(0)
        })
        .sum()
}

/// Render the full markdown report.
pub fn markdown(run: &CorpusRun, counter: &Counter) -> String {
    let mut out = String::new();
    out.push_str("# tokbench: LeIndex vs Claude Code native tools\n\n");
    out.push_str(&format!(
        "Corpus hash: `{}` · Tokens: {} · Fixture: generated pinned LLM-gateway workspace (7 tasks, brief §4.6)\n\n",
        crate::corpus::corpus_hash(),
        counter.provenance(),
    ));
    out.push_str(
        "Round trips = tool calls; tokens = tool OUTPUT tokens (what the model pays to re-read every turn). \
Task 3 and task 4 LEINDEX degrade loudly on master (`UNSUPPORTED in this build`) — they need brief \
§4.4 `run` (lane F) and §5 git mutations (lane D), wired at integration.\n\n",
    );
    out.push_str("| task | NATIVE tok | LEINDEX tok | reduction | NATIVE rt | LEINDEX rt | native ok | leindex ok | verdict |\n");
    out.push_str("|---|---:|---:|---:|---:|---:|:---:|:---:|:---:|\n");
    for (native, leindex) in &run.tasks {
        let reduction = token_reduction_pct(native.tokens, leindex.tokens);
        let verdict = if task_passes_gate(native, leindex) {
            "PASS"
        } else {
            "FAIL"
        };
        let note = if !leindex.note.is_empty() {
            leindex.note.as_str()
        } else if !native.note.is_empty() {
            native.note.as_str()
        } else {
            "—"
        };
        out.push_str(&format!(
            "| {} | {} | {} | {reduction:.1}% | {} | {} | {} | {} | {verdict} — {note} |\n",
            leindex.task,
            native.tokens,
            leindex.tokens,
            native.round_trips,
            leindex.round_trips,
            native.success,
            leindex.success,
        ));
    }
    let (native_tok, leindex_tok, native_rt, leindex_rt) = totals(run);
    out.push_str(&format!(
        "| **total** | **{native_tok}** | **{leindex_tok}** | **{:.1}%** | **{native_rt}** | **{leindex_rt}** | | | |\n\n",
        token_reduction_pct(native_tok, leindex_tok),
    ));

    out.push_str("## Per-mode token share\n\n");
    out.push_str("| mode | NATIVE tok | LEINDEX tok | reduction |\n|---|---:|---:|---:|\n");
    for mode in ["read", "find", "edit", "git", "run"] {
        let n = mode_total(run, true, mode);
        let l = mode_total(run, false, mode);
        out.push_str(&format!(
            "| {mode} | {n} | {l} | {:.1}% |\n",
            token_reduction_pct(n, l)
        ));
    }
    out
}

/// The JSON report value.
pub fn json_report(run: &CorpusRun, counter: &Counter) -> serde_json::Value {
    let (native_tok, leindex_tok, native_rt, leindex_rt) = totals(run);
    json!({
        "corpus_hash": crate::corpus::corpus_hash(),
        "token_counter": counter.kind(),
        "token_counter_provenance": counter.provenance(),
        "tasks": run.tasks.iter().map(|(n, l)| json!({
            "task": n.task,
            "native": n,
            "leindex": l,
            "gate_pass": task_passes_gate(n, l),
            "token_reduction_pct": token_reduction_pct(n.tokens, l.tokens),
        })).collect::<Vec<_>>(),
        "totals": {
            "native_tokens": native_tok,
            "leindex_tokens": leindex_tok,
            "native_round_trips": native_rt,
            "leindex_round_trips": leindex_rt,
            "token_reduction_pct": token_reduction_pct(native_tok, leindex_tok),
        },
        "gate_pass": run.tasks.iter().all(|(n, l)| task_passes_gate(n, l)),
    })
}

/// The gate verdict lines (printed in gate mode, one per task plus overall).
pub fn gate_lines(run: &CorpusRun) -> Vec<String> {
    let mut lines = Vec::new();
    for (native, leindex) in &run.tasks {
        let pass = task_passes_gate(native, leindex);
        lines.push(format!(
            "GATE task {}: {} (tokens {} -> {}, reduction {:.1}%, round trips {} -> {}, success {} -> {}{})",
            leindex.task,
            if pass { "PASS" } else { "FAIL" },
            native.tokens,
            leindex.tokens,
            token_reduction_pct(native.tokens, leindex.tokens),
            native.round_trips,
            leindex.round_trips,
            native.success,
            leindex.success,
            if leindex.note.is_empty() {
                String::new()
            } else {
                format!(", note: {}", leindex.note)
            },
        ));
    }
    let all_pass = run.tasks.iter().all(|(n, l)| task_passes_gate(n, l));
    lines.push(format!(
        "GATE overall: {}",
        if all_pass {
            "PASS"
        } else {
            "FAIL — at least one task missing ≥40% token reduction, more round trips, lower success, or UNSUPPORTED in this build"
        }
    ));
    lines
}
