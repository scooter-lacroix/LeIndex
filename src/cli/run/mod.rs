//! The `run` capability (§4.4): sandboxed, allow-listed command execution with
//! a structured output summary, scoped diagnostics, and how-to-test discovery.

pub(crate) mod actions;
pub(crate) mod diagnostics;
pub(crate) mod how_to_test;
pub(crate) mod parsers;
pub(crate) mod run_log;
pub(crate) mod sandbox;

#[cfg(test)]
mod parsers_test;
#[cfg(test)]
mod sandbox_test;
