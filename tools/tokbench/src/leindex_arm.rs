//! LEINDEX arm: the exact text the model would see over the MCP wire.
//!
//! ## Why in-process handler calls (not `leindex tools run`)
//!
//! `cargo xtask tokbench` builds tokbench against the `leindex` library and
//! drives `leindex::cli::mcp::server::handle_tool_call` directly — the same
//! public entry point `benches/mcp_tool_latency.rs` uses. That function IS
//! the MCP transport's dispatch path: it resolves the five public routers
//! (`grouped::resolve_call`), takes the response tier, hydrates the project
//! registry exactly as a live session does, trims the handler payload with
//! `trim_llm_payload`, and renders with `render_tool_output_plain` — the
//! uncolored renderer whose output is the `content[0].text` the model
//! receives. Running the built binary (`leindex tools run`) instead would be
//! LESS faithful and more expensive: `tools run` prints through the CLI's
//! COLORED renderer (`render_tool_output`, ANSI included) and pays one full
//! process spawn + registry rebuild per call, while a real agent session
//! amortizes one server across every call — which is also what the round-trip
//! metric means. Errors are captured the same way the transport captures
//! them: `Error: <message_with_hint>` with `isError: true`.
//!
//! ## Loud degradation when a capability is missing
//!
//! On the integrated roadmap tree every capability the corpus reaches for is
//! real (lane F's `run`, lane E's session dedup, lane D's git mutations). The
//! `UNSUPPORTED in this build` marker stays in [`metrics`] so a build that
//! loses one records it honestly instead of faking a result.

use anyhow::{Context, Result};
use leindex::cli::ProjectRegistry;
use leindex::cli::mcp::handlers::{ToolHandler, all_tool_handlers};
use leindex::cli::mcp::protocol::JsonRpcRequest;
use serde_json::{Value, json};
use std::sync::Arc;

/// Marker recorded when a task step needs a capability master does not have.
/// Never silently swallowed: the task runner surfaces it and marks the task
/// unsupported, which fails the gate for that task.
pub const UNSUPPORTED_IN_THIS_BUILD: &str = "UNSUPPORTED in this build";

/// Marker recorded when the NATIVE-arm equivalent could not run (e.g. no
/// ripgrep); used by the report to explain a skipped step.
pub const SKIPPED_NO_RG: &str = "skipped: rg not available";

/// One live LeIndex session: one registry, many calls, exactly like a real
/// MCP server lifetime.
pub struct LeIndexArm {
    registry: Arc<ProjectRegistry>,
    handlers: Vec<ToolHandler>,
}

impl LeIndexArm {
    /// Build the arm around a fresh registry (the per-process project state
    /// a real `leindex mcp` server owns).
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            registry: Arc::new(ProjectRegistry::new(4)),
            handlers: all_tool_handlers(),
        })
    }

    /// Build the index for `project_path` (plain index, never forced — the
    /// lane env forbids force reindex).
    pub fn index(&self, rt: &tokio::runtime::Runtime, project_path: &str) -> Result<()> {
        rt.block_on(self.registry.index_project(Some(project_path), false))
            .context("leindex index failed")?;
        Ok(())
    }

    /// Execute one tool call and return the model-visible text.
    ///
    /// `tool` uses the canonical handler name (`leindex_find`, …) or a router
    /// name (`leindex_git`) with `mode`/`action` in `args` — routing and
    /// normalization happen inside `handle_tool_call`, identical to the wire.
    pub fn call(&self, rt: &tokio::runtime::Runtime, tool: &str, args: Value) -> Result<String> {
        Ok(response_text(&self.call_raw(rt, tool, args)?))
    }

    /// Execute one tool call and return the full MCP response value (for
    /// flows that need structured fields — e.g. a git mutation plan's
    /// `preview_token` — before the text render).
    pub fn call_raw(&self, rt: &tokio::runtime::Runtime, tool: &str, args: Value) -> Result<Value> {
        let request: JsonRpcRequest = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": args }
        }))
        .context("build tool request")?;
        let response = rt
            .block_on(leindex::cli::mcp::server::handle_tool_call(
                &self.registry,
                &self.handlers,
                &request,
            ))
            .context("handle_tool_call failed")?;
        Ok(response)
    }
}

/// The `content[0].text` the MCP transport would put on the wire, including
/// the `Error: …` form transport errors take.
pub(crate) fn response_text(response: &Value) -> String {
    response
        .get("content")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
#[path = "leindex_arm_test.rs"]
mod tests;
