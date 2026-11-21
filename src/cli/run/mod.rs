//! The `run` capability (§4.4): sandboxed, allow-listed command execution with
//! a structured output summary, scoped diagnostics, and how-to-test discovery.
//!
//! Modules (each lands in its own step commit):
//! - [`parsers`]: pure functions that turn tool output into a [`parsers::Summary`].
//! - `sandbox`: allow-list validation and process execution with a scrubbed,
//!   size-capped log on disk.
//! - `diagnostics`: `cargo check` / `tsc` scoped to the owning package, with
//!   the last run's diagnostics persisted as JSON beside the logs.
//! - `how_to_test`: project-aware gate-command discovery.

pub(crate) mod parsers;

#[cfg(test)]
mod parsers_test;
