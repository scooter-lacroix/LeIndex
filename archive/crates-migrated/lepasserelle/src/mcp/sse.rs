//! SSE (Server-Sent Events) support.
//!
//! The implementation lives in `mcp::server` so HTTP and stdio modes share
//! one indexing path and one consolidation strategy.

pub use super::server::{index_stream_handler, index_with_progress};
